 use anyhow::Result;
use futures::stream;
use futures::{Stream, StreamExt};
use log::{debug, info};
use serde_json::Value;
use std::fs;
use uuid::Uuid;
use crate::llama;
use crate::web_search::WebSearch;

#[derive(Debug, Serialize, Deserialize, Clone)]
struct WebSearchConfig {
    result_limit: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Config {
    default: DefaultConfig,
    default_provider: String,
    deepseek: ModelConfig,
    openai: ModelConfig,
    llamacpp: ModelConfig,
    ollama: ModelConfig,
    web_search: WebSearchConfig,
}

pub struct ChatBot {
    history: Vec<llama::Message>,
    config: Config,
    current_provider: String,
    llama_client: llama::LlamaClient,
    web_search: WebSearch,
    conversation_id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct DefaultConfig {
    temperature: f32,
    max_tokens: u32,
    stream: bool,
    initial_prompt: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default: DefaultConfig {
                temperature: 0.7,
                max_tokens: 2000,
                stream: true,
                initial_prompt: String::from("You are a helpful AI assistant."),
            },
            default_provider: String::from("llamacpp"),
            deepseek: ModelConfig {
                api_url: String::from("https://api.deepseek.com/v1/chat/completions"),
                api_key: Some(String::from("your-deepseek-key")),
                model: String::from("deepseek-chat"),
                temperature: None,
                max_tokens: None,
                stream: None,
            },
            openai: ModelConfig {
                api_url: String::from("https://api.openai.com/v1/chat/completions"),
                api_key: Some(String::from("your-openai-key")),
                model: String::from("gpt-3.5-turbo"),
                temperature: None,
                max_tokens: None,
                stream: None,
            },
            llamacpp: ModelConfig {
                api_url: String::from("http://localhost:8080/v1/chat/completions"),
                api_key: None,
                model: String::from("phi4"),
                temperature: None,
                max_tokens: None,
                stream: None,
            },
            ollama: ModelConfig {
                api_url: String::from("http://localhost:11434/api/chat"),
                api_key: None,
                model: String::from("mistral"),
                temperature: None,
                max_tokens: None,
                stream: None,
            },
            web_search: WebSearchConfig { result_limit: 10 },
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModelConfig {
    api_url: String,
    api_key: Option<String>,
    model: String,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    stream: Option<bool>,
}

impl ModelConfig {
    pub fn get_temperature(&self, defaults: &DefaultConfig) -> f32 {
        self.temperature.unwrap_or(defaults.temperature)
    }

    pub fn get_max_tokens(&self, defaults: &DefaultConfig) -> u32 {
        self.max_tokens.unwrap_or(defaults.max_tokens)
    }

    pub fn get_stream(&self, defaults: &DefaultConfig) -> bool {
        self.stream.unwrap_or(defaults.stream)
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            api_url: String::new(),
            api_key: None,
            model: String::new(),
            temperature: None,
            max_tokens: None,
            stream: None,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
            .join(".config")
            .join("abot");

        let config_path = config_dir.join("config.toml");

        if !config_dir.exists() {
            println!("Creating config directory: {}", config_dir.display());
            fs::create_dir_all(&config_dir)?;
        }

        if !config_path.exists() {
            println!("Creating default config file: {}", config_path.display());
            let default_config = Config::default();
            let toml = toml::to_string_pretty(&default_config)?;
            fs::write(&config_path, toml)?;
            println!("Please set your API key in the config file or DEEPSEEK_API_KEY environment variable");
            println!("You can edit the config file at: {}", config_path.display());
            return Ok(default_config);
        }

        info!("Loading config from: {}", config_path.display());
        let config_str = fs::read_to_string(&config_path)?;
        let config: Config = toml::from_str(&config_str)?;

        if config.deepseek.api_key.is_none() && std::env::var("DEEPSEEK_API_KEY").is_err() {
            println!(
                "Warning: No API key found in config file or DEEPSEEK_API_KEY environment variable"
            );
            println!("Please set your API key in: {}", config_path.display());
            println!("Or set the DEEPSEEK_API_KEY environment variable");
        }

        Ok(config)
    }
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
            history: Vec::new(),
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
        self.history.push(llama::Message {
            role: role.to_string(),
            content: content.to_string(),
        });
    }

    pub fn create_custom_skin() -> termimad::MadSkin {
        let mut skin = termimad::MadSkin::default();
        skin.set_headers_fg(termimad::rgb(255, 187, 0));
        skin.bold.set_fg(termimad::rgb(255, 187, 0));
        skin.italic.set_fg(termimad::rgb(215, 255, 135));
        skin.bullet.set_fg(termimad::rgb(255, 187, 0));
        skin.code_block.set_fg(termimad::rgb(187, 187, 187));
        skin.code_block.set_bg(termimad::rgb(45, 45, 45));
        skin.quote_mark.set_fg(termimad::rgb(150, 150, 150));
        skin
    }

    pub async fn send_message(&mut self, message: &str) -> Result<MessageStream> {
        self.add_message("user", message);

        let is_web_search = message.contains("@web");

        let query = message
            .split_whitespace()
            .filter(|word| !word.starts_with('#') && !word.starts_with('@'))
            .collect::<Vec<_>>()
            .join(" ");

        let message = if is_web_search {
            println!("Performing a web search for: '{}'", query);
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

        let response = match self.llama_client.generate(&self.history).await {
            Ok(resp) => resp,
            Err(e) => {
                println!("Error generating response: {}", e);
                return Err(e);
            }
        };

        if self.config.default.stream {
            let stream = response.bytes_stream().map(|chunk_result| {
                chunk_result.map_err(anyhow::Error::from).and_then(|chunk| {
                    let chunk_str = String::from_utf8_lossy(&chunk);
                    let mut content = String::new();

                    for line in chunk_str.lines() {
                        if line.starts_with("data: ") {
                            let data = &line["data: ".len()..];
                            if data == "[DONE]" {
                                continue;
                            }

                            if let Ok(json) = serde_json::from_str::<Value>(data) {
                                debug!("json: {}", json);
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
        if self.history.len() < 2 {
            println!("No conversation to save yet.");
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
            .history
            .iter()
            .rev()
            .find(|msg| msg.role == "user")
            .ok_or_else(|| anyhow::anyhow!("No user message found"))?;

        let last_assistant_msg = self
            .history
            .iter()
            .rev()
            .find(|msg| msg.role == "assistant")
            .ok_or_else(|| anyhow::anyhow!("No assistant message found"))?;

        let content = format!(
            "User:{}\nAssistant:{}\n\n",
            last_user_msg.content, last_assistant_msg.content
        );

        fs::write(&filename, content)?;
        println!("Saved conversation to: {}", filename.display());
        Ok(())
    }

    pub fn save_all_history(&self) -> Result<()> {
        if self.history.is_empty() {
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

        for message in self.history.iter().skip(1) {
            content.push_str(&format!("{}:{}\n\n", message.role, message.content));
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
