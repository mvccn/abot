use anyhow::Result;
use futures::stream;
use futures::{Stream, StreamExt};
use log::{debug, error};
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
use crate::markdown;

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

    // pub fn get_rendered_content(&mut self) -> Vec<Line<'static>> {
    //     if let Some(cached) = &self.cached_rendered_content {
    //         cached.clone()
    //     } else {
    //         let rendered = if self.role == "assistant" {
    //             markdown::markdown_to_lines(&self.raw_content)
    //         } else {
    //             vec![Line::from(self.raw_content.to_string())]
    //         };
    //         self.cached_rendered_content = Some(rendered.clone());
    //         rendered
    //     }
    // }
}

// #[derive(Debug)]
// pub struct Conversation {
//     messages: Vec<ChatMessage>,
// }

// impl Conversation {
//     pub fn new() -> Self {
//         Self {
//             messages: Vec::new(),
//         }
//     }

//     pub fn add_message(&mut self, role: &str, content: &str) {
//         let message = ChatMessage::new(role, content);
//         self.messages.push(message);
//     }

//     pub fn finalize_streamed_response(&mut self, final_content: String) {
//         let mut message = ChatMessage::new("assistant", &final_content);
//         let rendered = markdown::markdown_to_lines(&final_content);
//         message.cached_rendered_content = Some(rendered);
//         self.messages.push(message);
//     }

//     pub fn get_rendered_messages(&self) -> Vec<Line<'static>> {
//         let mut rendered_messages = Vec::new();
        
//         for message in &self.messages {
//             let prefix = match message.role.as_str() {
//                 "assistant" => Span::styled("Assistant: ", Style::default().fg(Color::Green)),
//                 "user" => Span::styled("User: ", Style::default().fg(Color::Blue)),
//                 _ => Span::raw("System: "),
//             };
//             rendered_messages.push(Line::from(vec![prefix]));
            
//             if let Some(cached) = &message.cached_rendered_content {
//                 rendered_messages.extend(cached.clone());
//             } else {
//                 rendered_messages.extend(if message.role == "assistant" {
//                     markdown::markdown_to_lines(&message.raw_content)
//                 } else {
//                     vec![Line::from(message.raw_content.to_string())]
//                 });
//             }
//         }
        
//         rendered_messages
//     }

//     pub fn get_raw_messages(&self) -> Vec<llama::Message> {
//         self.messages.iter()
//             .map(|msg| llama::Message {
//                 role: msg.role.clone(),
//                 content: msg.raw_content.clone(),
//             })
//             .collect()
//     }

//     pub fn last_message_mut(&mut self) -> Option<&mut ChatMessage> {
//         self.messages.last_mut()
//     }
// }

#[derive(Debug, Clone)]
pub struct ChatBot {
    pub messages: Vec<ChatMessage>,
    config: Config,
    pub current_provider: String,
    llama_client: llama::LlamaClient,
    web_search: WebSearch,
    conversation_id: String,
}

type MessageStream = Pin<Box<dyn Stream<Item = Result<String>> + Send>>;

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

        let llama_config = config.llamacpp.clone();
        let llama_client_for_search = llama::LlamaClient::new(llama_config)?;

        let web_search = WebSearch::new(
            &conversation_id,
            config.web_search.result_limit,
            llama_client_for_search,
        ).await?;

        let llama_client = llama::LlamaClient::new(config.deepseek.clone())?;

        let mut bot = Self {
            messages: Vec::new(),
            current_provider: config.default_provider.clone(),
            llama_client,
            config: config.clone(),
            web_search,
            conversation_id,
        };

        let initial_prompt = bot.config.default.initial_prompt.clone();
        bot.add_message("system", &initial_prompt);

        Ok(bot)
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

    pub async fn querry(&mut self, message: &str) -> Result<MessageStream> {
        let is_web_search = message.contains("@web");
        let query = message
            .split_whitespace()
            .filter(|word| !word.starts_with('#') && !word.starts_with('@'))
            .collect::<Vec<_>>()
            .join(" ");

        // If it's a web search, spawn a background task
        if is_web_search {
            debug!("Performing web search for query: '{}'", query);
            let web_search = self.web_search.clone();
            let query_clone = query.clone();
            
            // Spawn the web search in a background task
            tokio::spawn(async move {
                if let Err(e) = web_search.search(&query_clone).await {
                    error!("Web search failed: {}", e);
                }
            });
        }

        let _message = query;

        debug!("Sending request to provider: {} at {}", self.current_provider, self.llama_client.config.api_url);

        let response = match self.llama_client.generate(&self.get_raw_messages()).await {
            Ok(resp) => resp,
            Err(e) => {
                error!("Failed to generate response from provider {}: {}", self.current_provider, e);
                return Err(e).context("Failed to generate response from LLM provider");
            }
        };

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

        let cache_dir = dirs::cache_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find cache directory"))?
            .join("abot")
            .join(&self.conversation_id);

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
        debug!("Saved conversation interaction to: {}", filename.display());
        Ok(())
    }

    pub fn save_all_history(&self) -> Result<()> {
        if self.messages.is_empty() {
            debug!("No conversation history to save - conversation is empty");
            return Ok(());
        }

        let cache_dir = dirs::cache_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find cache directory"))?
            .join("abot")
            .join(&self.conversation_id);

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
        debug!("Saved full conversation history to: {}", filename.display());

        Ok(())
    }

    pub fn set_provider(&mut self, provider: &str) -> Result<()> {
        if self.current_provider != provider {
            self.llama_client = llama::LlamaClient::set_provider(&self.config, provider)?;
            self.current_provider = provider.to_string();
        }
        Ok(())
    }
}
