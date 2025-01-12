use anyhow::Result;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use rustyline::DefaultEditor;
use serde_json::{json, Value};
// use termimad::crossterm::style::Stylize;
use termimad::MadSkin;
use crossterm::{
    execute,
    terminal::{Clear, ClearType},
    cursor,
};
use std::io::{stdout, Write};
use serde::{Deserialize, Serialize};
use std::fs;

struct ChatBot {
    client: reqwest::Client,
    history: Vec<Message>,
    api_key: String,
    config: Config,
}

#[derive(Clone, serde::Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    deepseek: DeepseekConfig,
    ollama: OllamaConfig,
    initial_prompt: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct DeepseekConfig {
    api_key: Option<String>,
    model: String,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaConfig {
    url: String,
    model: String,
    temperature: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            deepseek: DeepseekConfig {
                api_key: Some(String::from("Your deepseek key")),
                model: String::from("deepseek-chat"),
                temperature: 0.7,
                max_tokens: 2000,
            },
            ollama: OllamaConfig {
                url: String::from("http://localhost:11434"),
                model: String::from("llama2"),
                temperature: 0.7,
            },
            initial_prompt: String::from(
                "You are an intelligent AI assistant. Please be concise and helpful in your responses."
            ),
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
    fn new(config: Config) -> Result<Self> {
        let api_key = config.deepseek.api_key.clone()
            .or_else(|| std::env::var("DEEPSEEK_API_KEY").ok())
            .ok_or_else(|| anyhow::anyhow!("API key must be set in config or DEEPSEEK_API_KEY environment variable"))?;

        let mut bot = Self {
            client: reqwest::Client::new(),
            history: Vec::new(),
            api_key,
            config,
        };

        let initial_prompt = bot.config.initial_prompt.clone();
        bot.add_message("system", &initial_prompt);
        Ok(bot)
    }

    fn add_message(&mut self, role: &str, content: &str) {
        self.history.push(Message {
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
        let is_ollama = message.starts_with("#ollama");
        let message = if is_ollama {
            message.trim_start_matches("#ollama").trim()
        } else {
            message
        };
        
        self.add_message("user", message);

        let headers = {
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            if !is_ollama {
                headers.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", self.api_key))?);
            }
            headers
        };

        let url = if is_ollama {
            format!("{}/api/chat", self.config.ollama.url)
        } else {
            "https://api.deepseek.com/v1/chat/completions".to_string()
        };

        let payload = if is_ollama {
            json!({
                "model": self.config.ollama.model,
                "messages": self.history,
                "stream": true,
                "temperature": self.config.ollama.temperature,
            })
        } else {
            json!({ 
                "model": self.config.deepseek.model,
                "messages": self.history,
                "stream": true,
                "temperature": self.config.deepseek.temperature,
                "max_tokens": self.config.deepseek.max_tokens
            })
        };

        let response = self.client
            .post(url)
            .headers(headers)
            .json(&payload)
            .send()
            .await?;

        let mut stream = response.bytes_stream();
        let mut current_message = String::new();
        let mut current_block = String::new();
        let mut rendered_length = 0;
        let mut lines_printed = 0;
        let skin = Self::create_custom_skin();

        // Print the Assistant prefix and save position
        println!("");
        stdout().flush()?;
        // let initial_position = cursor::position()?;

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            
            for line in chunk_str.lines() {
                if line.starts_with("data: ") {
                    let data = &line["data: ".len()..];
                    if data == "[DONE]" { continue; }
                    
                    if let Ok(json) = serde_json::from_str::<Value>(data) {
                        if let Some(content) = json["choices"][0]["delta"]["content"].as_str() {
                            current_message.push_str(content);
                            current_block.push_str(content);
                            lines_printed += content.matches('\n').count();

                            if content.contains("\n\n") || content.contains("```") {
                                // Move to start of the raw text block and clear everything below
                                execute!(
                                    stdout(),
                                    cursor::MoveToColumn(0),
                                    cursor::MoveUp(lines_printed as u16),
                                    Clear(ClearType::FromCursorDown)
                                )?;
                                
                                // Print the new content
                                let new_content = &current_message[rendered_length..];
                                skin.print_text(new_content);
                                rendered_length = current_message.len();
                                current_block.clear();
                                lines_printed = 0;
                                
                                // Ensure we're at the start of a new line
                                execute!(stdout(), cursor::MoveToColumn(0))?;
                                stdout().flush()?;
                            } else {
                                if current_block.len() == content.len() {
                                    // Start of new block, ensure we're at line start
                                    execute!(stdout(), cursor::MoveToColumn(0))?;
                                    lines_printed = 0;
                                }
                                print!("{}", content);
                                stdout().flush()?;
                            }
                        }
                    }
                }
            }
        }

        // Final render for any remaining content
        if rendered_length < current_message.len() {
            execute!(
                stdout(),
                cursor::MoveToColumn(0),
                cursor::MoveUp(lines_printed as u16),
                Clear(ClearType::FromCursorDown)
            )?;
            
            let remaining_content = &current_message[rendered_length..];
            skin.print_text(remaining_content);
            println!();
        }
        
        self.add_message("assistant", &current_message);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::load()?;
    let mut chatbot = ChatBot::new(config)?;
    let mut rl = DefaultEditor::new()?;

    println!("Welcome to the Abot! Type 'quit' or 'exit' to exit.");
    
    loop {
        let readline = rl.readline("You: ");
        match readline {
            Ok(line) => {
                if line.trim().eq_ignore_ascii_case("quit") || line.trim().eq_ignore_ascii_case("exit") {
                    break;
                }
                
                println!("Assistant: ");
                chatbot.send_message(&line).await?;
            }
            Err(_) => break,
        }
    }

    Ok(())
}