use anyhow::Result;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::fs;
use url::Url;
use std::time::{SystemTime, UNIX_EPOCH};
use futures::future::join_all;
use percent_encoding::{percent_encode, NON_ALPHANUMERIC};
use crate::llama::{self, LlamaClient};
use log::{debug, info, error};

#[derive(Debug, Serialize, Deserialize)]
pub struct CachedDocument {
    url: String,
    content: String,
    timestamp: u64,
    summary: String,
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
        let use_llama = true;
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
        })
    }

    fn get_cache_path(&self, url: &str) -> PathBuf {
        // Encode URL to be filesystem safe
        let encoded_url = percent_encode(url.as_bytes(), NON_ALPHANUMERIC).to_string();
        self.cache_dir.join(encoded_url)
    }

    async fn fetch_and_cache_url(&self, url: &str) -> Result<CachedDocument> {
        // Validate URL first
        if let Err(e) = Url::parse(url) {
            error!("Warning: Invalid URL '{}': {}", url, e);
            return Err(anyhow::anyhow!("Invalid URL: {}", e));
        }

        let cache_path = self.get_cache_path(url);
        
        // Check cache first
        if cache_path.exists() {
            let cached: CachedDocument = serde_json::from_str(&fs::read_to_string(&cache_path)?)?;
            let age = SystemTime::now()
                .duration_since(UNIX_EPOCH)?
                .as_secs() - cached.timestamp;
            
            // Return cached version if less than 24 hours old
            if age < 24 * 60 * 60 {
                return Ok(cached);
            }
        }

        // Fetch new content with timeout
        let response = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.client.get(url).send()
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
            std::time::Duration::from_secs(5),
            tokio::task::spawn_blocking(move || {
                let document = Html::parse_document(&response);
                
                // Remove unwanted elements
                let selector_to_remove = Selector::parse("script, style, meta, link, noscript, iframe, svg").unwrap();
                let text_selectors = Selector::parse("p, h1, h2, h3, h4, h5, h6, article, section, main, div > text").unwrap();
                
                // Extract meaningful text content
                document
                    .select(&text_selectors)
                    .map(|element| {
                        // Skip if this element or its parent is in the removal list
                        if element.select(&selector_to_remove).next().is_some() {
                            return String::new();
                        }
                        
                        // Get text content, normalize whitespace
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
        };

        // Add debug output
        #[cfg(debug_assertions)]
        {
            debug!("Content from {}: {}", url, content);
        }

        // Modify the summary generation to check use_llama flag
        let summary = if self.use_llama {
            // Add timeout to LLM summary generation
            match tokio::time::timeout(
                std::time::Duration::from_secs(15),
                async {
                    let summary_prompt = vec![llama::Message {
                        role: "user".to_string(),
                        content: format!(
                            "Please provide a brief, factual summary of the following text in 2-3 sentences:\n\n{}",
                            content
                        ),
                    }];
                    
                    self.llama.generate(&summary_prompt).await
                }
            ).await {
                Ok(Ok(response)) => {
                    match LlamaClient::get_response_text(response).await {
                        Ok(text) => text,
                        Err(e) => {
                            error!("Warning: Failed to parse LLM response: {}. Using fallback.", e);
                            content.chars().take(500).collect::<String>().trim().to_string()
                        }
                    }
                },
                Ok(Err(e)) => {
                    error!("Warning: Failed to generate LLM summary: {}. Using fallback.", e);
                    content.chars().take(500).collect::<String>().trim().to_string()
                },
                Err(_) => {
                    error!("Timeout generating LLM summary. Using fallback.");
                    content.chars().take(500).collect::<String>().trim().to_string()
                }
            }
        } else {
            // Simple fallback summary when LLama is not available
            content.chars().take(500).collect::<String>().trim().to_string()
        };

        let cached_doc = CachedDocument {
            url: url.to_string(),
            content,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)?
                .as_secs(),
            summary,
        };

        // Save to cache
        fs::write(
            &cache_path,
            serde_json::to_string_pretty(&cached_doc)?,
        )?;

        Ok(cached_doc)
    }

    pub async fn search(&mut self, query: &str) -> Result<String> {
        #[cfg(debug_assertions)]
        debug!("Starting search with query: {}", query);

        // Save the query to self
        self.query = query.to_string();

        let search_url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding::encode(query)
        );
      
        info!("Fetching search results from DuckDuckGo...");
        let response = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
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

        #[cfg(debug_assertions)]
        debug!("Raw DuckDuckGo response length: {} bytes", response.len());

        let document = Html::parse_document(&response);
        
        // Define selectors for the search results structure
        let results_selector = Selector::parse(".result__extras").unwrap();
        let url_selector = Selector::parse(".result__url").unwrap();
        let snippet_selector = Selector::parse(".result__snippet").unwrap();
        
        let mut search_results = Vec::new();
        
        #[cfg(debug_assertions)]
        let mut result_count = 0;
        
        // Iterate directly over all result__extras elements
        for result in document.select(&results_selector) {
            #[cfg(debug_assertions)]
            {
                result_count += 1;
                debug!("Processing search result #{}", result_count);
            }

            let encoded_url = result
                .select(&url_selector)
                .next()
                .and_then(|el| Some(el.text().collect::<String>()))
                .unwrap_or_default();

            #[cfg(debug_assertions)]
            debug!("Found encoded URL: {}", encoded_url);

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

            info!("Fetching from: {}", real_url);

            let snippet = result
                .select(&snippet_selector)
                .next()
                .map(|el| el.text().collect::<String>())
                .unwrap_or_default();
                
            #[cfg(debug_assertions)]
            debug!("Result snippet: {}", snippet);

            if !real_url.is_empty() {
                search_results.push((real_url, snippet));
            }
        }

        info!("Found {} search results", search_results.len());

        #[cfg(debug_assertions)]
        {
            debug!("Search results: {:#?}", search_results);
            debug!("Limiting results to max_results: {}", self.max_results);
        }
        
        // Fetch and cache all URLs (limit to first max_results) in search results.
        // Process them in batches to avoid overwhelming the system
        let batch_size = 2; // Process 2 URLs at a time
        let mut results = Vec::new();
        
        for chunk in search_results.iter().take(self.max_results).collect::<Vec<_>>().chunks(batch_size) {
            let fetch_futures: Vec<_> = chunk.iter()
                .map(|(url, _)| {
                    debug!("Fetching content from: {}", url);
                    self.fetch_and_cache_url(url)
                })
                .collect();

            // Fetch batch of URLs concurrently
            let batch_results = join_all(fetch_futures).await;
            results.extend(batch_results);
            
            // Yield control back to the runtime after each batch
            tokio::task::yield_now().await;
        }
        
        println!("Processing search results...");
        
        // Process results
        let summaries: String = results.into_iter()
            .filter_map(|result| {
                result.ok().map(|doc| {
                    format!(
                        "Source: {}\nSummary: {}\n",
                        doc.url,
                        doc.summary
                    )
                })
            })
            .collect::<Vec<_>>()
            .join("\n");

        #[cfg(debug_assertions)]
        debug!("Final processed summaries length: {} bytes", summaries.len());

        println!("Search completed successfully!");
        Ok(summaries)
    }
} 
