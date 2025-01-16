use anyhow::Result;
use futures::stream;
use futures::{Stream, StreamExt};
use log::{debug, error, info};
use serde_json::Value;
use std::fs;
use std::pin::Pin;
use uuid::Uuid;
use crate::llama;
use crate::web_search::WebSearch;
use bytes::Bytes;
use crate::config::Config;
use ratatui::prelude::{Line, Span, Style, Color};
use crate::markdown;

#[derive(Debug)]
pub struct Message {
    pub role: String,
    pub raw_content: String,
    pub rendered_content: Vec<Line<'static>>,
}

impl Message {
    pub fn new(role: &str, content: &str) -> Self {
        Self::with_rendered(role, content, role == "assistant")
    }

    pub fn with_rendered(role: &str, content: &str, _should_render: bool) -> Self {
        let rendered = if role == "assistant" {
            markdown::markdown_to_lines(content)
        } else {
            vec![Line::from(content.to_string())]
        };
        
        Self {
            role: role.to_string(),
            raw_content: content.to_string(),
            rendered_content: rendered,
        }
    }
}

#[derive(Debug)]
pub struct Conversation {
    messages: Vec<Message>,
}

impl Conversation {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    pub fn add_message(&mut self, role: &str, content: &str) {
        let message = Message::new(role, content);
        self.messages.push(message);
    }

    pub fn get_rendered_messages(&self) -> Vec<Line<'static>> {
        self.messages.iter()
            .flat_map(|msg| {
                let mut lines = vec![Line::from(vec![
                    Span::styled(
                        format!("{}: ", msg.role),
                        match msg.role.as_str() {
                            "assistant" => Style::default().fg(Color::Green),
                            "user" => Style::default().fg(Color::Blue),
                            _ => Style::default(),
                        }
                    )
                ])];
                lines.extend(msg.rendered_content.clone());
                lines
            })
            .collect()
    }

    pub fn get_raw_messages(&self) -> Vec<llama::Message> {
        self.messages.iter()
            .map(|msg| llama::Message {
                role: msg.role.clone(),
                content: msg.raw_content.clone(),
            })
            .collect()
    }

    pub fn last_message_mut(&mut self) -> Option<&mut Message> {
        self.messages.last_mut()
    }
}

pub struct ChatBot {
    pub conversation: Conversation,
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
            fs::create_dir_all(&cache_dir)?;
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
            conversation: Conversation::new(),
            current_provider: config.default_provider.clone(),
            llama_client,
            config: config.clone(),
            web_search,
            conversation_id,
        };

        let initial_prompt = bot.config.default.initial_prompt.clone();
        bot.conversation.add_message("system", &initial_prompt);

        Ok(bot)
    }

    pub fn add_message(&mut self, role: &str, content: &str) {
        self.conversation.add_message(role, content);
    }

    pub async fn send_message(&mut self, message: &str) -> Result<MessageStream> {
        self.add_message("user", message);

        let is_web_search = message.contains("@web");

        let query = message
            .split_whitespace()
            .filter(|word| !word.starts_with('#') && !word.starts_with('@'))
            .collect::<Vec<_>>()
            .join(" ");

        let _message = if is_web_search {
            info!("Performing a web search for: '{}'", query);
            let web_results = self.web_search.search(&query).await?;
            format!(
                "Based on the following web search results, please answer the question: '{}'\n\nSearch Results:\n{}",
                query,
                web_results
            )
        } else {
            query
        };

        #[cfg(debug_assertions)]
        {
            debug!("Sending request to: {}", self.llama_client.config.api_url);
        }

        let response = match self.llama_client.generate(&self.conversation.get_raw_messages()).await {
            Ok(resp) => resp,
            Err(e) => {
                error!("Error generating response: {}", e);
                return Err(e);
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
        if self.conversation.messages.len() < 2 {
            info!("No conversation to save yet.");
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
            .conversation.messages
            .iter()
            .rev()
            .find(|msg| msg.role == "user")
            .ok_or_else(|| anyhow::anyhow!("No user message found"))?;

        let last_assistant_msg = self
            .conversation.messages
            .iter()
            .rev()
            .find(|msg| msg.role == "assistant")
            .ok_or_else(|| anyhow::anyhow!("No assistant message found"))?;

        let content = format!(
            "User:{}\nAssistant:{}\n\n",
            last_user_msg.raw_content, last_assistant_msg.raw_content
        );

        fs::write(&filename, content)?;
        info!("Saved conversation to: {}", filename.display());
        Ok(())
    }

    pub fn save_all_history(&self) -> Result<()> {
        if self.conversation.messages.is_empty() {
            info!("No conversation to save yet.");
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

        for message in self.conversation.messages.iter().skip(1) {
            content.push_str(&format!("{}:{}\n\n", message.role, message.raw_content));
        }

        fs::write(&filename, content)?;
        info!("Saved full conversation to: {}", filename.display());

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
