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
};
use std::io::{stdout, Write};
use std::path::PathBuf;
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
    api_key: Option<String>,
    initial_prompt: String,
    model: String,
    temperature: f32,
    max_tokens: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_key: Some(String::from("Your deepseek key")),
            initial_prompt: String::from(
                "You are an intelligent AI assistant. Please be concise and helpful in your responses."
            ),
            model: String::from("deepseek-chat"),
            temperature: 0.7,
            max_tokens: 2000,
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

        if config.api_key.is_none() && std::env::var("DEEPSEEK_API_KEY").is_err() {
            println!("Warning: No API key found in config file or DEEPSEEK_API_KEY environment variable");
            println!("Please set your API key in: {}", config_path.display());
            println!("Or set the DEEPSEEK_API_KEY environment variable");
        }

        Ok(config)
    }
}

impl ChatBot {
    fn new(config: Config) -> Self {
        let api_key = config.api_key.clone()
            .or_else(|| std::env::var("DEEPSEEK_API_KEY").ok())
            .expect("API key must be set in config or DEEPSEEK_API_KEY environment variable");

        let mut bot = Self {
            client: reqwest::Client::new(),
            history: Vec::new(),
            api_key,
            config,
        };

        let initial_prompt = bot.config.initial_prompt.clone();
        bot.add_message("system", &initial_prompt);
        bot
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
        self.add_message("user", message);

        let headers = {
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", self.api_key))?);
            headers
        };

        let response = self.client
            .post("https://api.deepseek.com/v1/chat/completions")
            .headers(headers)
            .json(&json!({ 
                "model": self.config.model,
                "messages": self.history,
                "stream": true,
                "temperature": self.config.temperature,
                "max_tokens": self.config.max_tokens
            }))
            .send()
            .await?;

        let mut stream = response.bytes_stream();
        let mut current_message = String::new();
        let skin = Self::create_custom_skin();

        // Save cursor position before we start streaming
        execute!(
            stdout(),
            crossterm::cursor::SavePosition
        )?;

        // First phase: Stream the raw text
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            
            for line in chunk_str.lines() {
                if line.starts_with("data: ") {
                    let data = &line["data: ".len()..];
                    if data == "[DONE]" {
                        continue;
                    }
                    
                    if let Ok(json) = serde_json::from_str::<Value>(data) {
                        if let Some(content) = json["choices"][0]["delta"]["content"].as_str() {
                            current_message.push_str(content);
                            print!("{}", content);
                            stdout().flush()?;
                        }
                    }
                }
            }
        }

        // Second phase: Restore cursor and render with proper markdown
        execute!(
            stdout(),
            crossterm::cursor::RestorePosition,
            Clear(ClearType::FromCursorDown)
        )?;
        
        skin.print_text(&current_message);
        println!("\n");
        
        self.add_message("assistant", &current_message);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::load()?;
    let mut chatbot = ChatBot::new(config);
    let mut rl = DefaultEditor::new()?;

    println!("Welcome to the Abot! Type 'quit' or 'exit' to exit.");
    
    loop {
        let readline = rl.readline("You: ");
        match readline {
            Ok(line) => {
                if line.trim().eq_ignore_ascii_case("quit") || line.trim().eq_ignore_ascii_case("exit") {
                    break;
                }
                
                print!("Assistant: ");
                chatbot.send_message(&line).await?;
            }
            Err(_) => break,
        }
    }

    Ok(())
}