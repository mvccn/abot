use anyhow::Result;
use futures::StreamExt;
use rustyline::DefaultEditor;
use serde_json::Value;
use termimad::MadSkin;
use crossterm::{
    execute,
    terminal::{Clear, ClearType},
    cursor,
};
use std::io::{stdout, Write};
use serde::{Deserialize, Serialize};
use std::fs;
use uuid::Uuid;
use log::{trace, debug, info, warn, error};
mod web_search;
mod llama;
use web_search::WebSearch;



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

struct ChatBot {
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
            default_provider: String::from("deepseek"),
            deepseek: ModelConfig {
                api_url: String::from("https://api.deepseek.com/v1/chat/completions"),
                api_key: Some(String::from("your-deepseek-key")),
                model: String::from("deepseek-chat"),
                temperature: None,  // Will use default
                max_tokens: None,   // Will use default
                stream: None,       // Will use default
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
            web_search: WebSearchConfig {
                result_limit: 10,
            },
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ModelConfig {
    api_url: String,
    api_key: Option<String>,
    model: String,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    stream: Option<bool>,
}

impl ModelConfig {
    fn get_temperature(&self, defaults: &DefaultConfig) -> f32 {
        self.temperature.unwrap_or(defaults.temperature)
    }

    fn get_max_tokens(&self, defaults: &DefaultConfig) -> u32 {
        self.max_tokens.unwrap_or(defaults.max_tokens)
    }

    fn get_stream(&self, defaults: &DefaultConfig) -> bool {
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
    fn load() -> Result<Self> {
        let config_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
            .join(".config")
            .join("abot");
            
        let config_path = config_dir.join("config.toml");

        // Create config directory if it doesn't exist
        if !config_dir.exists() {
            println!("Creating config directory: {}", config_dir.display());
            fs::create_dir_all(&config_dir)?;
        }

        // If config file doesn't exist, create it with default values
        if !config_path.exists() {
            println!("Creating default config file: {}", config_path.display());
            let default_config = Config::default();
            let toml = toml::to_string_pretty(&default_config)?;
            fs::write(&config_path, toml)?;
            println!("Please set your API key in the config file or DEEPSEEK_API_KEY environment variable");
            println!("You can edit the config file at: {}", config_path.display());
            return Ok(default_config);
        }

        println!("Loading config from: {}", config_path.display());
        // Read and parse existing config file
        let config_str = fs::read_to_string(&config_path)?;
        let config: Config = toml::from_str(&config_str)?;

        if config.deepseek.api_key.is_none() && std::env::var("DEEPSEEK_API_KEY").is_err() {
            println!("Warning: No API key found in config file or DEEPSEEK_API_KEY environment variable");
            println!("Please set your API key in: {}", config_path.display());
            println!("Or set the DEEPSEEK_API_KEY environment variable");
        }

        Ok(config)
    }
}

impl ChatBot {
    async fn new(config: Config) -> Result<Self> {
        let conversation_id = Uuid::new_v4().to_string();
        
        // Create conversation directory
        let cache_dir = dirs::cache_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find cache directory"))?
            .join("abot")
            .join(&conversation_id);
        
        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir)?;
        }

        // Create a LlamaClient for web search
        let llama_config = config.llamacpp.clone();
        let llama_client_for_search = llama::LlamaClient::new(llama_config)?;

        let web_search = WebSearch::new(
            &conversation_id, 
            config.web_search.result_limit,
            llama_client_for_search
        ).await?;

        // Create main LlamaClient with default provider
        let llama_client = llama::LlamaClient::new(config.deepseek.clone())?;

        let mut bot = Self {
            history: Vec::new(),
            current_provider: config.default_provider.clone(),
            llama_client,
            config: config.clone(),
            web_search,
            conversation_id,
        };

        // Add initial system prompt
        let initial_prompt = bot.config.default.initial_prompt.clone();
        bot.add_message("system", &initial_prompt);
        
        Ok(bot)
    }

    fn add_message(&mut self, role: &str, content: &str) {
        self.history.push(llama::Message {
            role: role.to_string(),
            content: content.to_string(),
        });
    }

    fn create_custom_skin() -> MadSkin {
        let mut skin = MadSkin::default();
        skin.set_headers_fg(termimad::rgb(255, 187, 0));
        skin.bold.set_fg(termimad::rgb(255, 187, 0));
        skin.italic.set_fg(termimad::rgb(215, 255, 135));
        skin.bullet.set_fg(termimad::rgb(255, 187, 0));
        skin.code_block.set_fg(termimad::rgb(187, 187, 187));
        skin.code_block.set_bg(termimad::rgb(45, 45, 45));
        skin.quote_mark.set_fg(termimad::rgb(150, 150, 150));
        skin
    }

    async fn send_message(&mut self, message: &str) -> Result<()> {
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

        self.add_message("user", &message);
        
        // Add debug print for request
        // println!("Sending request to: {}", self.llama_client.config.api_url);
        
        // Pass the entire history to generate
        let response = match self.llama_client.generate(&self.history).await {
            Ok(resp) => resp,
            Err(e) => {
                println!("Error generating response: {}", e);
                return Err(e);
            }
        };
        
        if self.config.default.stream {
            // Handle streaming response
            let mut stream = response.bytes_stream();
            let mut current_message = String::new();
            let mut current_block = String::new();
            let mut rendered_length = 0;
            let mut _lines_printed = 0;
            let skin = Self::create_custom_skin();

            // Print the Assistant prefix and get initial cursor position
            print!("Assistant: ");
            stdout().flush()?;
            let mut initial_position = cursor::position()?;
            println!();  // Move to next line after the prefix

            while let Some(chunk_result) = stream.next().await {
                let chunk = chunk_result?;
                let chunk_str = String::from_utf8_lossy(&chunk);

                #[cfg(debug_assertions)]
                {
                    trace!("Chunk: {}", chunk_str);
                }
                
                for line in chunk_str.lines() {
                    if line.starts_with("data: ") {
                        let data = &line["data: ".len()..];
                        if data == "[DONE]" { continue; }
                        
                        if let Ok(json) = serde_json::from_str::<Value>(data) {
                            if let Some(content) = json["choices"][0]["delta"]["content"].as_str() {
                                current_message.push_str(content);
                                current_block.push_str(content);
                                _lines_printed += content.matches('\n').count();

                                if content.contains("\n\n") || content.contains("```") {
                                    execute!(
                                        stdout(),
                                        cursor::MoveTo(initial_position.0, initial_position.1),
                                        Clear(ClearType::FromCursorDown)
                                    )?;
                                    
                                    skin.print_text(&current_message);
                                    rendered_length = current_message.len();
                                    current_block.clear();
                                    
                                    initial_position = cursor::position()?;
                                    _lines_printed = 0;
                                    
                                    stdout().flush()?;
                                } else {
                                    #[cfg(debug_assertions)]
                                    {
                                        trace!("content: {}", content);
                                        trace!("current_block: {}", current_block);
                                    }
                                    if current_block.len() == content.len() {
                                        execute!(stdout(), cursor::MoveToColumn(0))?;
                                        _lines_printed = 0;
                                    }
                                    print!("{}", content);
                                    stdout().flush()?;
                                }
                            }
                        }
                    }
                }
            }

            if rendered_length < current_message.len() {
                execute!(
                    stdout(),
                    cursor::MoveTo(initial_position.0, initial_position.1),
                    Clear(ClearType::FromCursorDown)
                )?;
                
                skin.print_text(&current_message);
                println!();
            }
            
            self.add_message("assistant", &current_message);
        } else {
            // Handle non-streaming response
            let response_text = llama::LlamaClient::get_response_text(response).await?;
            println!("Assistant: ");
            let skin = Self::create_custom_skin();
            skin.print_text(&response_text);
            println!();
            self.add_message("assistant", &response_text);
        }

        Ok(())
    }

    fn save_last_interaction(&self) -> Result<()> {
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

        let last_user_msg = self.history.iter().rev()
            .find(|msg| msg.role == "user")
            .ok_or_else(|| anyhow::anyhow!("No user message found"))?;

        let last_assistant_msg = self.history.iter().rev()
            .find(|msg| msg.role == "assistant")
            .ok_or_else(|| anyhow::anyhow!("No assistant message found"))?;

        let content = format!(
            "User:{}\nAssistant:{}\n\n",
            last_user_msg.content,
            last_assistant_msg.content
        );

        fs::write(&filename, content)?;
        println!("Saved conversation to: {}", filename.display());
        Ok(())
    }

    fn save_all_history(&self) -> Result<()> {
        if self.history.is_empty() {
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

        let filename = save_dir.join("saveall.md");
        let mut content = String::new();

        // Skip the first system message
        for message in self.history.iter().skip(1) {
            content.push_str(&format!("{}:{}\n\n", 
                message.role,
                message.content
            ));
        }

        fs::write(&filename, content)?;
        println!("Saved full conversation to: {}", filename.display());
        Ok(())
    }

    pub fn set_provider(&mut self, provider: &str) -> Result<()> {
        // Only create a new client if we're switching to a different provider
        if self.current_provider != provider {
            self.llama_client = llama::LlamaClient::set_provider(&self.config, provider)?;
            self.current_provider = provider.to_string();
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .format(|buf, record| {
            let level_color = match record.level() {
                log::Level::Error => "\x1b[1;31m", // Bold Red
                log::Level::Warn => "\x1b[1;33m",  // Bold Yellow
                log::Level::Info => "\x1b[1;32m",  // Bold Green
                log::Level::Debug => "\x1b[1;34m", // Bold Blue
                log::Level::Trace => "\x1b[1;35m", // Bold Purple
            };
            let reset = "\x1b[0m";

            writeln!(buf,
                "[{}{}{} {}:{}] {}",
                level_color,
                record.level(),
                reset,
                // record.target(),
                record.file().unwrap_or("unknown"),
                record.line().unwrap_or(0),
                record.args()
            )
        })
        .init();
    let config = Config::load()?;
    let mut chatbot = ChatBot::new(config).await?;
    let mut rl = DefaultEditor::new()?;

    println!("Welcome to the Abot! Type 'quit' or 'exit' to exit.");
    
    loop {
        let readline = rl.readline("You: ");
        match readline {
            Ok(line) => {
                let line = line.trim();
                if line.eq_ignore_ascii_case("quit") || line.eq_ignore_ascii_case("exit") {
                    break;
                }
                
                // Handle commands
                if line.starts_with('/') {
                    match line.split_whitespace().next().unwrap() {
                        "/save" => {
                            if let Err(e) = chatbot.save_last_interaction() {
                                println!("Error saving conversation: {}", e);
                            }
                        }
                        "/saveall" => {
                            if let Err(e) = chatbot.save_all_history() {
                                println!("Error saving conversation: {}", e);
                            }
                        }
                        "/model" => {
                            match line.split_whitespace().nth(1) {
                                Some(provider) => {
                                    match chatbot.set_provider(provider) {
                                        Ok(_) => (),
                                        Err(e) => println!("Error setting provider: {}", e),
                                    }
                                }
                                None => println!("Available providers: deepseek, openai, llamacpp, ollama\nCurrent provider: {}", chatbot.current_provider),
                            }
                        }
                        _ => println!("Unknown command. Available commands: /save, /saveall, /model"),
                    }
                    continue;
                }
                
                println!("Assistant: ");
                chatbot.send_message(&line).await?;
            }
            Err(_) => break,
        }
    }

    Ok(())
}