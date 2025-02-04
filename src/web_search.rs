use anyhow::Result;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::fs;
use url::Url;
use std::time::{SystemTime, UNIX_EPOCH};
use percent_encoding::{percent_encode, NON_ALPHANUMERIC};
// use crate::llama::{self, LlamaClient};
use log::{info,error,debug};
use std::sync::Arc;
use std::path::PathBuf;
use tokio::sync::RwLock;
use crate::llama_function::LlamaFunction;
const SEARCH_TIMEOUT_SECS: u64 = 10;
const FETCH_TIMEOUT_SECS: u64 = 10;
const HTML_PARSE_TIMEOUT_SECS: u64 = 5;
// const SUMMARY_TIMEOUT_SECS: u64 = 10;
// const CACHE_MAX_AGE_SECS: u64 = 24 * 60 * 60;  // 24 hours
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
    pub max_results: usize,
    // pub llama: Option<LlamaClient>,
    pub query: String,
    // pub use_llama: bool,
    pub results: Arc<RwLock<Vec<SearchResult>>>,
    pub conversation_id: String,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub url: String,
    pub snippet: String,
    pub document: String,
    pub summary: String,
}

impl WebSearch {
    pub async fn new(conversation_id: &str, max_results: usize) -> Result<Self> {
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

        // Determine if Llama should be used
        // let use_llama = llama.is_some();
        // let use_llama = false;
        Ok(Self {
            client: Client::new(),
            cache_dir,
            max_results,
            // llama,
            query: String::new(),
            // use_llama,
            results: Arc::new(RwLock::new(Vec::new())),
            conversation_id: conversation_id.to_string(),
        })
    }

    pub fn cache_dir(&self) -> PathBuf {
        dirs::home_dir().unwrap()
            .join(".cache")
            .join("abot")
            .join(&self.conversation_id)
    }

    pub async fn research(&mut self, query: &str, llama: bool) -> Result<Vec<SearchResult>> {
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

        debug!("DuckDuckGo response: {}", response);

        // Parse the search results on a blocking task.
        let search_results = tokio::task::spawn_blocking(move || {
            info!("Parsing DuckDuckGo response...");
            let document = Html::parse_document(&response);
            
            // Define selectors for the search results structure
            let results_selector = Selector::parse(".result__extras").unwrap();
            let url_selector = Selector::parse(".result__url").unwrap();
            let snippet_selector = Selector::parse(".result__snippet").unwrap();
            
            let mut results = Vec::new();
            
            for result in document.select(&results_selector) {
                let encoded_url = result
                    .select(&url_selector)
                    .next()
                    .map(|el| el.text().collect::<String>())
                    .unwrap_or_default();

                // Extract the real URL from the "uddg" parameter.
                let mut real_url = if encoded_url.contains("uddg=") {
                    let start_idx = encoded_url.find("uddg=").map(|i| i + 5).unwrap_or(0);
                    let end_idx = encoded_url.find("&rut=").unwrap_or(encoded_url.len());
                    let encoded_real_url = &encoded_url[start_idx..end_idx];
                    urlencoding::decode(encoded_real_url)
                        .unwrap_or_else(|_| encoded_real_url.into())
                        .into_owned()
                } else {
                    encoded_url
                };
                real_url = real_url.split_whitespace().collect::<String>();
                real_url = format!("https://{}", real_url);

                let snippet = result
                    .select(&snippet_selector)
                    .next()
                    .map(|el| el.inner_html())
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
        
        {
            let mut results_write = self.results.write().await;
            results_write.clear();
            results_write.extend(search_results.into_iter());
        }

        // Launch extraction (summarization) tasks concurrently.
        use futures::stream::{FuturesUnordered, StreamExt};
        let mut tasks = FuturesUnordered::new();

        {
            // Capture self.results for use inside the tasks.
            let results_clone = self.results.clone();
            // Iterate over a clone of the current search results.
            for result in self.results.read().await.iter().take(self.max_results).cloned() {
                let url = result.url;
                let snippet = result.snippet;
                let results_ptr = results_clone.clone();
                let client = self.client.clone();
                let cache_dir = self.cache_dir();
                let query = self.query.clone();

                tasks.push(async move {
                    let content = fetch_url(&client, &url).await?;
                    info!("🏄 Got content {} chars for URL: {}", content.len(), url);
                    let summary = if llama {
                        let llama_function = LlamaFunction::new("test", None, "");
                        let summary = match llama_function.extract_nodes(&query, &content).await {
                            Ok(summary) => summary,
                            Err(e) => {
                                error!("Failed to extract nodes: {}", e);
                                String::new()
                            }
                        };
                        info!("💾 LLama summary: {}", summary);
                        if summary.is_empty() {
                            error!("Failed to summarize content");
                        }
                        summary
                    } else {
                        info!("💾 Content: {}", content);
                        content.split_whitespace().take(1000).collect::<Vec<_>>().join(" ")
                    };

                    // Cache the result.
                    let cached_doc = cache_result(&cache_dir, &url, &snippet, Some(content), Some(summary)).await?;
                    let mut results_write = results_ptr.write().await;
                    if let Some(res) = results_write.iter_mut().find(|r| r.url == url) {
                        res.summary = cached_doc.summary;
                        res.document = cached_doc.document;
                        res.snippet = cached_doc.snippet;
                    }
                    Ok::<(), anyhow::Error>(())
                });
            }
        }

        // Wait for all extraction tasks to complete before proceeding.
        while let Some(task_result) = tasks.next().await {
            task_result?;
        }

        // Clone the finished results into a concrete Vec.
        let final_results = {
            let results_read = self.results.read().await;
            results_read.clone()
        };

        Ok(final_results)
    }
}

pub async fn fetch_url(
    client: &reqwest::Client,
    url: &str,
) -> Result<String> {
    // Try to load from cache first
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

async fn cache_result(
    cache_dir: &std::path::Path,
    url: &str,
    snippet: &str,
    content: Option<String>,
    summary: Option<String>,
) -> Result<CachedDocument> {
    
    let cache_path = cache_dir.join("web_cache").join(percent_encode(url.as_bytes(), NON_ALPHANUMERIC).to_string());
    
    // // Check cache first if no content provided
    // if content.is_none() && cache_path.exists() {
    //     if let Ok(cached) = serde_json::from_str::<CachedDocument>(&fs::read_to_string(&cache_path)?) {
    //         let age = SystemTime::now()
    //             .duration_since(UNIX_EPOCH)?
    //             .as_secs() - cached.timestamp;
            
    //         if age < CACHE_MAX_AGE_SECS {
    //             return Ok(cached);
    //         }
    //     }
    //     return Err(anyhow::anyhow!("Cache expired or invalid"));
    // }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llama_function::LlamaFunction;
    // use crate::llama_function::summarize_content;
    // use crate::llama::LlamaClient;
    // use crate::config::ModelConfig;

    #[tokio::test]
    async fn test_duckduckgo_search_parsing() {
        // Create a new WebSearch instance
        let mut web_search = WebSearch::new("test_conversation", 5 ).await.unwrap();

        // Perform a search query
        let query = "Rust programming language";
        let results = web_search.research(query).await.unwrap();

        // Check if results are returned
        let results_read = results.iter();
        if results_read.is_empty() {
            println!("No search results found for query: {}", query);
        }
        // println!("Results: {:?}", results_read);
        // assert!(!results_read.is_empty(), "Expected search results, got none");

        // Check if URLs and snippets are correctly parsed
        for result in results_read {
            println!("Result URL: {}", result.url);
            println!("Result Snippet: {}", result.snippet);
            assert!(!result.url.is_empty(), "Result URL should not be empty");
            assert!(!result.snippet.is_empty(), "Result snippet should not be empty");
        }
    }

    #[tokio::test]
    async fn test_fetch_url_with_real_url() {
        // Use a real URL for testing
        let url = "https://www.example.com";

        // Create a new reqwest client
        let client = Client::new();

        // Call the fetch_url function
        let result = fetch_url(&client, url).await;

        // Assert that the result is Ok
        assert!(result.is_ok());

        // Check if the content contains expected text
        let content = result.unwrap();
        assert!(content.contains("Example Domain"));
        println!("Got Content: {}", content);
        //let summary = extract_nodes(query, &content).await.expect("Failed to extract nodes");
        let query = "Rust programming language";
        let llama_function = LlamaFunction::new("test", None, "");
        let summary = llama_function.extract_nodes(&query, &content).await.expect("Failed to extract nodes");
        println!("Summary: {}", summary);
        assert!(!summary.is_empty());
    }
} 