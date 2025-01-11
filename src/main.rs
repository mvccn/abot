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

struct ChatBot {
    client: reqwest::Client,
    history: Vec<Message>,
    api_key: String,
}

#[derive(Clone, serde::Serialize)]
struct Message {
    role: String,
    content: String,
}

impl ChatBot {
    fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            history: Vec::new(),
            api_key,
        }
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
                "model": "deepseek-chat",
                "messages": self.history,
                "stream": true
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
    dotenv::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY must be set");

    let mut chatbot = ChatBot::new(api_key);
    let mut rl = DefaultEditor::new()?;
    let skin = MadSkin::default();
    // let mut terminal = Terminal::new(skin);

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