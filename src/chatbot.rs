use anyhow::Result;
use futures::stream;
use futures::{Stream, StreamExt};
use log::{debug, error, info};
use anyhow::Context;
use serde_json::Value;
use std::fs;
use std::pin::Pin;
use uuid::Uuid;
use crate::llama;
use crate::web_search::WebSearch;
use bytes::Bytes;
use crate::config::Config;
use ratatui::prelude::Line;
use std::path::PathBuf;
use crate::markdown;
use crate::web_search::SearchResult;

// Make the type alias public so that it can be referenced in main.rs:
pub type MessageStream = Pin<Box<dyn Stream<Item = Result<String>> + Send>>;

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub raw_content: String,
    pub rendered_content: Vec<Line<'static>>,
    // cached_rendered_content: Option<Vec<Line<'static>>>,
}

impl ChatMessage {
    pub fn new(role: &str, content: &str) -> Self {
        Self {
            role: role.to_string(),
            raw_content: content.to_string(),
            rendered_content: Vec::new(),
            // cached_rendered_content: None,
        }
    }
}

  
#[derive(Debug)]
pub struct ChatBot {
    pub messages: Vec<ChatMessage>,
    config: Config,
    pub current_provider: String,
    llama_client: llama::LlamaClient,
    web_search: WebSearch,
    pub conversation_id: String,
    search_results_rx: Option<tokio::sync::mpsc::Receiver<Result<Vec<SearchResult>>>>,
}

impl ChatBot {
    pub async fn new(config: Config) -> Result<Self> {
        let conversation_id = Uuid::new_v4().to_string();

        let cache_dir = dirs::cache_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find cache directory"))?
            .join("abot")
            .join(&conversation_id);

        if !cache_dir.exists() {
            debug!("Creating cache directory: {}", cache_dir.display());
            fs::create_dir_all(&cache_dir)
                .with_context(|| format!("Failed to create cache directory at {}", cache_dir.display()))?;
        }

        // let llama_config = config.llamacpp.clone();
        // let llama_client_for_search = llama::LlamaClient::new(llama_config)?;

        let web_search = WebSearch::new(
            &conversation_id,
            config.web_search.result_limit,
        ).await?;

        let llama_client = llama::LlamaClient::new(config.deepseek.clone())?;

        let mut bot = Self {
            messages: Vec::new(),
            current_provider: config.default_provider.clone(),
            llama_client,
            config: config.clone(),
            web_search,
            conversation_id,
            search_results_rx: None,
        };

        let initial_prompt = bot.config.default.initial_prompt.clone();
        bot.add_message("system", &initial_prompt);

        Ok(bot)
    }

    pub fn cache_dir(&self) -> PathBuf {
        dirs::home_dir().unwrap()
            .join(".cache")
            .join("abot")
            .join(&self.conversation_id)
    }

    pub fn add_message(&mut self, role: &str, content: &str) {
        let message = ChatMessage::new(role, content);
        self.messages.push(message);
    }

    pub fn update_last_message(&mut self, content: &str) {
        if let Some(last_msg) = self.messages.last_mut() {
            last_msg.raw_content = content.to_string();
            last_msg.rendered_content = markdown::markdown_to_lines(content);
        }
    }

	fn get_raw_messages(&self) -> Vec<llama::Message> {
        self.messages.iter()
            .map(|msg| llama::Message {
                role: msg.role.clone(),
                content: msg.raw_content.clone(),
            })
            .collect()
    }

    pub async fn query(&mut self, message: &str) -> Result<MessageStream> {
        let is_web_search = message.contains("@web");
        let query_text = message
            .split_whitespace()
            .filter(|word| !word.starts_with('#') && !word.starts_with('@'))
            .collect::<Vec<_>>()
            .join(" ");

        if is_web_search {
            info!("🔍 Web search initiated for: '{}'", query_text);

            // Clear existing results.
            {
                let mut results = self.web_search.results.write().await;
                results.clear();
            }

            // Await complete research with a timeout.
            let results = match tokio::time::timeout(
                std::time::Duration::from_secs(30),
                self.web_search.research(&query_text, true)
            )
            .await {
                Ok(Ok(results)) => results,
                Ok(Err(e)) => {
                    error!("❌ Web search failed: {}", e);
                    vec![]
                }
                Err(_) => {
                    error!("Web search timed out");
                    vec![]
                }
            };

            if !results.is_empty() {
                info!("📚 Retrieved {} search results", results.len());
                let context = results
                    .iter()
                    .enumerate()
                    .map(|(i, result)| format!("Source {}: {}\nSummary: {}", i + 1, result.url, result.summary))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                info!("Context: {}", context);
                self.add_message("system", &format!(
                    "Here are relevant search results for your query:\n\n{}",
                    context
                ));
            } else {
                self.add_message("system", "No search results were found.");
            }
        }

        // Generate response using LLama    
        info!("Generating response using context from {:?} messages", self.messages); //display full message
        let response = self.llama_client.generate(&self.get_raw_messages()).await?;

        if self.config.default.stream {
            let stream = response.bytes_stream().map(|chunk_result| {
                chunk_result.map_err(anyhow::Error::from).and_then(|chunk: Bytes| {
                    let chunk_str = String::from_utf8_lossy(&chunk);
                    let mut content = String::new();

                    for line in chunk_str.lines() {
                        if line.starts_with("data: ") {
                            let data = &line["data: ".len()..];
                            if data == "[DONE]" {
                                continue;
                            }

                            if let Ok(json) = serde_json::from_str::<Value>(data) {
                                if let Some(delta_content) =
                                    json["choices"][0]["delta"]["content"].as_str()
                                {
                                    content.push_str(delta_content);
                                }
                            }
                        }
                    }
                    Ok(content)
                })
            });

            Ok(Box::pin(stream))
        } else {
            let response_text = llama::LlamaClient::get_response_text(response).await?;
            self.add_message("assistant", &response_text);
            Ok(Box::pin(stream::once(async move { Ok(response_text) })))
        }
    }

    pub fn save_last_interaction(&self) -> Result<()> {
        if self.messages.len() < 2 {
            debug!("No conversation to save - not enough messages yet");
            return Ok(());
        }

        let cache_dir = self.cache_dir();

        let save_dir = cache_dir.join("save");
        if !save_dir.exists() {
            fs::create_dir_all(&save_dir)?;
        }

        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let filename = save_dir.join(format!("interaction_{}.md", timestamp));

        let last_user_msg = self
            .messages
            .iter()
            .rev()
            .find(|msg| msg.role == "user")
            .ok_or_else(|| anyhow::anyhow!("No user message found"))?;

        let last_assistant_msg = self
            .messages
            .iter()
            .rev()
            .find(|msg| msg.role == "assistant")
            .ok_or_else(|| anyhow::anyhow!("No assistant message found"))?;

        let content = format!(
            "User:{}\nAssistant:{}\n\n",
            last_user_msg.raw_content, last_assistant_msg.raw_content
        );

        fs::write(&filename, content)?;
        info!("Saved conversation interaction to: {}", filename.display());
        Ok(())
    }

    pub fn save_all_history(&self) -> Result<()> {
        if self.messages.is_empty() {
            debug!("No conversation history to save - conversation is empty");
            return Ok(());
        }
        let cache_dir = self.cache_dir();
        let save_dir = cache_dir.join("save");
        if !save_dir.exists() {
            fs::create_dir_all(&save_dir)?;
        }

        let filename = save_dir.join("saveall.md");
        let mut content = String::new();

        for message in self.messages.iter().skip(1) {
            content.push_str(&format!("{}:{}\n\n", message.role, message.raw_content));
        }

        fs::write(&filename, content)?;
        info!("Saved full conversation history to: {}", filename.display());

        Ok(())
    }

    pub fn set_provider(&mut self, provider: &str) -> Result<()> {
        if self.current_provider != provider {
            self.llama_client = llama::LlamaClient::set_provider(&self.config, provider)?;
            self.current_provider = provider.to_string();
        }
        Ok(())
    }

    pub fn set_topic(&mut self, topic: &str) -> Result<String> {
        // Sanitize the topic to be used as a directory name
        let sanitized_topic = topic.replace(" ", "_");
        // let old_conversation_id = self.conversation_id.clone();
        
        // Get the old and new cache directory paths
        let old_cache_dir = self.cache_dir();
        self.conversation_id = sanitized_topic.clone();
        let new_cache_dir = self.cache_dir();

        // Rename the cache directory if it exists
        if old_cache_dir.exists() {
            debug!("Renaming cache directory from {} to {}", old_cache_dir.display(), new_cache_dir.display());
            fs::rename(&old_cache_dir, &new_cache_dir)
                .with_context(|| format!("Failed to rename cache directory from {} to {}", old_cache_dir.display(), new_cache_dir.display()))?;
        } else {
            info!("Creating new cache directory for topic: {}", new_cache_dir.display());
            fs::create_dir_all(&new_cache_dir)
                .with_context(|| format!("Failed to create cache directory at {}", new_cache_dir.display()))?;
        }

        // Update the web search cache directory as well
        self.web_search.cache_dir = new_cache_dir.clone();

        info!("Conversation topic set to: {}, cache directory: {}", self.conversation_id, new_cache_dir.display());
        Ok(self.conversation_id.clone())
    }

}