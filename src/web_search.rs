use anyhow::Result;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::fs;
use url::Url;
use std::time::{SystemTime, UNIX_EPOCH};
use percent_encoding::{percent_encode, NON_ALPHANUMERIC};
use crate::llama::{self, LlamaClient};
use log::{debug, info, error};
use std::sync::Arc;
use tokio::sync::RwLock;

const SEARCH_TIMEOUT_SECS: u64 = 10;
const FETCH_TIMEOUT_SECS: u64 = 10;
const HTML_PARSE_TIMEOUT_SECS: u64 = 5;
const SUMMARY_TIMEOUT_SECS: u64 = 10;
const CACHE_MAX_AGE_SECS: u64 = 24 * 60 * 60;  // 24 hours
const MAX_CONTENT_LENGTH: usize = 20000;  // Max chars to keep from content
const BATCH_SIZE: usize = 4;  // Number of URLs to process simultaneously

#[derive(Debug, Serialize, Deserialize)]
pub struct CachedDocument {
    url: String,
    document: String,
    timestamp: u64,
    summary: String,
    snippet: String,
}

#[derive(Debug, Clone)]
pub struct WebSearch {
    pub client: Client,
    pub cache_dir: PathBuf,
    pub conversation_id: String,
    pub max_results: usize,
    pub llama: LlamaClient,
    pub query: String,
    pub use_llama: bool,
    pub results: Arc<RwLock<Vec<SearchResult>>>,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub url: String,
    pub snippet: String,
    pub document: String,
    pub summary: String,
}

impl WebSearch {
    pub async fn new(conversation_id: &str, max_results: usize, llama: LlamaClient) -> Result<Self> {
        let home_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
        let cache_dir = home_dir
            .join(".cache")
            .join("abot")
            .join(conversation_id)
            .join("web_cache");

        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir)?;
        }

        // Test LLama availability
        let use_llama = false;
        // let use_llama = match llama.test_availability().await {
        //     Ok(true) => true,
        //     Ok(false) => {
        //         warn!("LLama service is not available, falling back to simple summaries");
        //         false
        //     },
        //     Err(e) => {
        //         warn!("Error testing LLama availability: {}, falling back to simple summaries", e);
        //         false
        //     }
        // };

        Ok(Self {
            client: Client::new(),
            cache_dir,
            conversation_id: conversation_id.to_string(),
            max_results,
            llama,
            query: String::new(),
            use_llama,
            results: Arc::new(RwLock::new(Vec::new())),
        })
    }

    fn get_cache_path(&self, url: &str) -> PathBuf {
        // Encode URL to be filesystem safe
        let url_without_protocol = url.replace("https://", "").replace("http://", "");
        let cache_path = self.cache_dir.join(percent_encode(url_without_protocol.as_bytes(), NON_ALPHANUMERIC).to_string());
        cache_path
    }

    pub async fn research(&mut self, query: &str) -> Result<Arc<RwLock<Vec<SearchResult>>>> {
        self.query = query.to_string();

        let search_url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding::encode(query)
        );
        
        info!("Searching on DuckDuckGo...");
        let response = match tokio::time::timeout(
            std::time::Duration::from_secs(SEARCH_TIMEOUT_SECS),
            self.client.get(&search_url).send()
        ).await {
            Ok(Ok(resp)) => resp.text().await?,
            Ok(Err(e)) => {
                error!("Error fetching search results: {}", e);
                return Err(anyhow::anyhow!("Failed to fetch search results: {}", e));
            },
            Err(_) => {
                error!("Timeout fetching search results");
                return Err(anyhow::anyhow!("Timeout fetching search results"));
            }
        };

        // Move HTML parsing to a blocking task
        let search_results = tokio::task::spawn_blocking(move || {
            info!("Parsing DuckDuckGo response...");
            let document = Html::parse_document(&response);
            
            // Define selectors for the search results structure
            let results_selector = Selector::parse(".result__extras").unwrap();
            let url_selector = Selector::parse(".result__url").unwrap();
            let snippet_selector = Selector::parse(".result__snippet").unwrap();
            
            let mut results = Vec::new();
            
            // Iterate directly over all result__extras elements
            for result in document.select(&results_selector) {
                let encoded_url = result
                    .select(&url_selector)
                    .next()
                    .and_then(|el| Some(el.text().collect::<String>()))
                    .unwrap_or_default();

                // Extract the real URL by finding the uddg parameter
                let mut real_url = if encoded_url.contains("uddg=") {
                    let start_idx = encoded_url.find("uddg=").map(|i| i + 5).unwrap_or(0);
                    let end_idx = encoded_url.find("&rut=").unwrap_or(encoded_url.len());
                    let encoded_real_url = &encoded_url[start_idx..end_idx];
                    
                    urlencoding::decode(encoded_real_url)
                        .unwrap_or(encoded_real_url.into())
                        .into_owned()
                } else {
                    encoded_url
                };
                real_url = real_url.split_whitespace().collect::<String>();
                real_url = format!("https://{}", real_url);

                let snippet = result
                    .select(&snippet_selector)
                    .next()
                    .map(|el| el.text().collect::<String>())
                    .unwrap_or_default();

                results.push(SearchResult {
                    url: real_url,
                    snippet,
                    document: String::new(),
                    summary: String::new(),
                });
            }
            
            results
        }).await?;

        info!("Found {} search results", search_results.len());
        
        let batch_size = BATCH_SIZE;
        let results = self.results.clone();
        {
            let mut results_write = results.write().await;
            results_write.clear();
        }

        for chunk in search_results.iter().take(self.max_results).collect::<Vec<_>>().chunks(batch_size) {
            let fetch_futures: Vec<_> = chunk.iter()
                .map(|result| {
                    let url = result.url.clone();
                    let snippet = result.snippet.clone();
                    let results = results.clone();
                    let client = self.client.clone();
                    let cache_dir = self.cache_dir.clone();
                    let llama = self.llama.clone();
                    let use_llama = self.use_llama;
                    let query = self.query.clone();

                    async move {
                        let content = fetch_url(&client, &url, &cache_dir).await?;
                        info!("🏄 Got content {} chars for URL: {}", content.len(), url);
                        let summary = summarize_content(&llama, use_llama, &content, &query).await;
                        info!("�� Summarized content for URL: {}", summary);
                        
                        // Cache the result
                        let cached_doc = cache_result(&cache_dir, &url, &snippet, Some(content), Some(summary)).await?;
                        
                        let mut results_write = results.write().await;
                        if let Some(result) = results_write.iter_mut().find(|r| r.url == url) {
                            result.summary = cached_doc.summary;
                            result.document = cached_doc.document;
                            result.snippet = cached_doc.snippet;
                        }
                        Ok::<(), anyhow::Error>(())
                    }
                })
                .collect();

            futures::future::join_all(fetch_futures).await;
        }
        
        Ok(results.clone())
    }
}

async fn fetch_url(
    client: &reqwest::Client,
    url: &str,
    cache_dir: &std::path::Path,
) -> Result<String> {
    // Try to load from cache first
    let cache_path = cache_dir.join(percent_encode(url.as_bytes(), NON_ALPHANUMERIC).to_string());
    if cache_path.exists() {
        if let Ok(cached) = serde_json::from_str::<CachedDocument>(&fs::read_to_string(&cache_path)?) {
            let age = SystemTime::now()
                .duration_since(UNIX_EPOCH)?
                .as_secs() - cached.timestamp;
            
            if age < CACHE_MAX_AGE_SECS {
                debug!("💾 Cache hit for URL: {}", url);
                return Ok(cached.document);
            }
        }
    }

    // Rest of the existing fetch_url logic
    if let Err(e) = Url::parse(url) {
        error!("Warning: Invalid URL '{}': {}", url, e);
        return Err(anyhow::anyhow!("Invalid URL: {}", e));
    }

    // Fetch new content with timeout
    let response = match tokio::time::timeout(
        std::time::Duration::from_secs(FETCH_TIMEOUT_SECS),
        client.get(url).send()
    ).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => {
            error!("Error fetching URL '{}': {}", url, e);
            return Err(anyhow::anyhow!("Failed to fetch URL: {}", e));
        },
        Err(_) => {
            error!("Timeout fetching URL '{}'", url);
            return Err(anyhow::anyhow!("Timeout fetching URL"));
        }
    }.text().await?;
    
    // Parse HTML in a blocking task with timeout
    let content = match tokio::time::timeout(
        std::time::Duration::from_secs(HTML_PARSE_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            let document = Html::parse_document(&response);
            
            // Remove unwanted elements
            let selector_to_remove = Selector::parse("script, style, meta, link, noscript, iframe, svg").unwrap();
            let text_selectors = Selector::parse("p, h1, h2, h3, h4, h5, h6, article, section, main, div > text").unwrap();
            
            // Extract meaningful text content
            document
                .select(&text_selectors)
                .map(|element| {
                    if element.select(&selector_to_remove).next().is_some() {
                        return String::new();
                    }
                    element.text()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        })
    ).await {
        Ok(content) => content,
        Err(e) => return Err(anyhow::anyhow!("Failed to parse HTML: {}", e)),
    }?;

    Ok(content.chars().take(MAX_CONTENT_LENGTH).collect::<String>().trim().to_string())
}

async fn summarize_content(
    llama: &LlamaClient,
    use_llama: bool,
    content: &str,
    query: &str,
) -> String {
    if !use_llama {
        return String::new();
    }

    match tokio::time::timeout(
        std::time::Duration::from_secs(SUMMARY_TIMEOUT_SECS),
        async {
            let summary_prompt = vec![llama::Message {
                role: "user".to_string(),
                content: format!("our query is {}, please extract all the relevant information from the web page content:\n{}", query, content)
            }];
            llama.generate(&summary_prompt).await
        }
    ).await {
        Ok(Ok(response)) => {
            match LlamaClient::get_response_text(response).await {
                Ok(text) => text,
                Err(e) => {
                    error!("Warning: Failed to parse LLM response: {}. Using fallback.", e);
                    content.to_string()
                }
            }
        },
        Ok(Err(e)) => {
            error!("Warning: Failed to generate LLM summary: {}. Using fallback.", e);
            content.to_string()
        },
        Err(_) => {
            error!("Timeout generating LLM summary. Using fallback.");
            content.to_string()
        }
    }
}

async fn cache_result(
    cache_dir: &std::path::Path,
    url: &str,
    snippet: &str,
    content: Option<String>,
    summary: Option<String>,
) -> Result<CachedDocument> {
    let cache_path = cache_dir.join(percent_encode(url.as_bytes(), NON_ALPHANUMERIC).to_string());
    
    // Check cache first if no content provided
    if content.is_none() && cache_path.exists() {
        if let Ok(cached) = serde_json::from_str::<CachedDocument>(&fs::read_to_string(&cache_path)?) {
            let age = SystemTime::now()
                .duration_since(UNIX_EPOCH)?
                .as_secs() - cached.timestamp;
            
            if age < CACHE_MAX_AGE_SECS {
                return Ok(cached);
            }
        }
        return Err(anyhow::anyhow!("Cache expired or invalid"));
    }

    // Create new cache entry
    let cached_doc = CachedDocument {
        url: url.to_string(),
        snippet: snippet.to_string(),
        document: content.unwrap_or_default(),
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_secs(),
        summary: summary.unwrap_or_default(),
    };

    // Save to cache
    fs::write(
        &cache_path,
        serde_json::to_string_pretty(&cached_doc)?,
    )?;

    Ok(cached_doc)
} 
