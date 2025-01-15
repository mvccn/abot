use anyhow::Result;
use crossterm::{
    cursor, execute,
    terminal::{Clear, ClearType},
};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::stream;
use futures::{Stream, StreamExt};
use log::{debug, error, info, trace, warn, Log, Metadata, Record, LevelFilter};
use ratatui::style::Color as RatatuiColor;
use ratatui::widgets::BorderType;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::{stdout, Write};
use std::pin::Pin;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use uuid::Uuid;
use std::sync::Mutex;
use std::sync::Arc;
use termimad::MadSkin;
use ratatui::symbols;
use ratatui::widgets::Scrollbar;
use ratatui::widgets::ScrollbarOrientation;
use ratatui::widgets::ScrollbarState;
mod llama;
mod web_search;
use web_search::WebSearch;
mod markdown;

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

// Define a simple message enum
#[derive(Debug, Clone)]
enum Message {
    Info(String),
    Error(String),
}

// Define a simple message bus
#[derive(Default)]
struct MessageBus {
    messages: std::sync::Mutex<Vec<Message>>,
}

impl MessageBus {
    fn send(&self, message: Message) {
        let mut messages = self.messages.lock().unwrap();
        messages.push(message);
    }

    fn clear(&self) {
        let mut messages = self.messages.lock().unwrap();
        messages.clear();
    }

    fn get_messages(&self) -> Vec<Message> {
        let messages = self.messages.lock().unwrap();
        messages.clone()
    }
}

struct ChatBot {
    history: Vec<llama::Message>,
    config: Config,
    current_provider: String,
    llama_client: llama::LlamaClient,
    web_search: WebSearch,
    conversation_id: String,
    message_bus: std::sync::Arc<MessageBus>, // Add message bus
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
                temperature: None, // Will use default
                max_tokens: None,  // Will use default
                stream: None,      // Will use default
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

        info!("Loading config from: {}", config_path.display());
        // Read and parse existing config file
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

// First, let's define a type alias for our stream
type MessageStream = Pin<Box<dyn Stream<Item = Result<String>> + Send>>;

impl ChatBot {
    async fn new(config: Config, message_bus: std::sync::Arc<MessageBus>) -> Result<Self> {
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
            llama_client_for_search,
        )
        .await?;

        // Create main LlamaClient with default provider
        let llama_client = llama::LlamaClient::new(config.deepseek.clone())?;

        let mut bot = Self {
            history: Vec::new(),
            current_provider: config.default_provider.clone(),
            llama_client,
            config: config.clone(),
            web_search,
            conversation_id,
            message_bus, // Initialize message bus
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

    async fn send_message(&mut self, message: &str) -> Result<MessageStream> {
        self.add_message("user", message); // Add user message to history

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

        // Add debug print for request
        // println!("Sending request to: {}", self.llama_client.config.api_url);
        #[cfg(debug_assertions)]
        {
            debug!("Sending request to: {}", self.llama_client.config.api_url);
        }

        // Pass the entire history to generate
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
            // Handle non-streaming case
            let response_text = llama::LlamaClient::get_response_text(response).await?;
            self.add_message("assistant", &response_text);
            Ok(Box::pin(stream::once(async move { Ok(response_text) })))
        }
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

    fn save_all_history(&self) -> Result<()> {
        if self.history.is_empty() {
            self.message_bus
                .send(Message::Info("No conversation to save yet.".to_string()));
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
            content.push_str(&format!("{}:{}\n\n", message.role, message.content));
        }

        fs::write(&filename, content)?;

        self.message_bus.send(Message::Info(format!(
            "Saved full conversation to: {}",
            filename.display()
        )));

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

// Add this struct for logging
struct UiLogger {
    buffer: Arc<Mutex<Vec<String>>>,
}

impl Log for UiLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let message = format!("[{}] {}", record.level(), record.args());
        if let Ok(mut buffer) = self.buffer.lock() {
            buffer.push(message);
        }
    }

    fn flush(&self) {}
}

// Modify App struct to include log buffer
struct App {
    chatbot: ChatBot,
    input: String,
    messages: Vec<String>,
    scroll: usize,          // This will now represent the line number we're scrolled to
    current_response: String,
    info_message: String,
    message_bus: std::sync::Arc<MessageBus>,
    log_buffer: Arc<Mutex<Vec<String>>>,
    visible_height: u16,
}

impl App {
    async fn new(config: Config) -> Result<Self> {
        let message_bus = std::sync::Arc::new(MessageBus::default());
        let log_buffer = Arc::new(Mutex::new(Vec::new()));
        
        // Initialize logger
        let logger = UiLogger {
            buffer: log_buffer.clone(),
        };
        
        // Set up the logger
        log::set_boxed_logger(Box::new(logger))
            .map(|()| log::set_max_level(LevelFilter::Info))?;

        let chatbot = ChatBot::new(config, message_bus.clone()).await?;
        
        Ok(Self {
            chatbot,
            input: String::new(),
            messages: Vec::new(),
            scroll: 0,
            current_response: String::new(),
            info_message: String::new(),
            message_bus,
            log_buffer,
            visible_height: 0,
        })
    }

    fn update_current_response(&mut self, content: &str) {
        self.current_response.push_str(content);
        // Update the last message if it's from the assistant
        if let Some(last) = self.messages.last_mut() {
            if last.starts_with("Assistant:") {
                *last = format!("Assistant: {}", self.current_response);
                // Auto-scroll to bottom when updating response
                self.scroll = usize::MAX; // This will be clamped to max_scroll in ui()
            }
        }

        // When the response is complete, add it to the chatbot's history
        if content.trim().is_empty() {
            self.chatbot.add_message("assistant", &self.current_response);
            self.current_response.clear(); // Clear the current response buffer
        }
    }

    fn print_info(&mut self, message: String) {
        self.info_message = message;
    }

    fn process_messages(&mut self) {
        let messages = self.message_bus.get_messages();
        if let Some(last_message) = messages.last() {
            self.info_message = match last_message {
                Message::Info(msg) => format!(
                    "{}[{}]{}",
                    "\x1b[32m", // Dark green
                    msg,
                    "\x1b[0m" // Reset color
                ),
                Message::Error(msg) => format!(
                    "{}[Error: {}]{}",
                    "\x1b[31m", // Red
                    msg,
                    "\x1b[0m" // Reset color
                ),
            };
            self.message_bus.clear();
        }
    }
}

struct UiWriter {
    buffer: Arc<Mutex<Vec<String>>>,
}

impl Write for UiWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(s) = String::from_utf8(buf.to_vec()) {
            if let Ok(mut buffer) = self.buffer.lock() {
                buffer.push(s);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state
    let config = Config::load()?;
    let mut app = App::new(config).await?;

    // Main loop
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Esc => break,
                        KeyCode::Enter => {
                            if !app.input.is_empty() {
                                let input = std::mem::take(&mut app.input);

                                // Handle commands
                                if input.starts_with("/") {
                                    let command = input
                                        .trim_start_matches("/")
                                        .split_whitespace()
                                        .collect::<Vec<_>>();
                                    match command[0] {
                                        "save" => {
                                            if let Err(e) = app.chatbot.save_last_interaction() {
                                                app.messages.push(format!(
                                                    "Error saving last interaction: {}",
                                                    e
                                                ));
                                            }
                                        }
                                        "saveall" => {
                                            if let Err(e) = app.chatbot.save_all_history() {
                                                app.messages.push(format!(
                                                    "Error saving all history: {}",
                                                    e
                                                ));
                                            }
                                        }
                                        "model" => {
                                            if command.len() > 1 {
                                                let provider = command[1];
                                                if let Err(e) = app.chatbot.set_provider(provider) {
                                                    app.messages.push(format!(
                                                        "Error setting provider: {}",
                                                        e
                                                    ));
                                                } else {
                                                    app.print_info(format!(
                                                        "Switched to provider: {}",
                                                        provider
                                                    ));
                                                }
                                            } else {
                                                app.messages
                                                    .push("Usage: /model <provider>".to_string());
                                            }
                                        }
                                        _ => {
                                            app.messages
                                                .push(format!("Unknown command: {}", input));
                                        }
                                    }
                                } else {
                                    // Immediately display user message
                                    app.messages.push(format!("You: {}", input));
                                    // Force a redraw to show the user message
                                    terminal.draw(|f| ui(f, &mut app))?;
                                    match app.chatbot.send_message(&input).await {
                                        Ok(mut stream) => {
                                            // Reset scroll to bottom for new conversation
                                            app.scroll = usize::MAX;
                                            app.current_response.clear();
                                            
                                            // Only add the "Assistant: " prefix when we start receiving content
                                            let mut is_first_chunk = true;
                                            while let Some(chunk_result) = stream.next().await {
                                                match chunk_result {
                                                    Ok(content) => {
                                                        if !content.is_empty() {
                                                            if is_first_chunk {
                                                                app.messages.push("Assistant: ".to_string());
                                                                is_first_chunk = false;
                                                            }
                                                            app.update_current_response(&content);
                                                            terminal.draw(|f| ui(f, &mut app))?;
                                                        }
                                                    }
                                                    Err(e) => {
                                                        app.messages.push(format!("Error: {}", e));
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            app.messages.push(format!("Error: {}", e));
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Char(c) => {
                            app.input.push(c);
                        }
                        KeyCode::Backspace => {
                            app.input.pop();
                        }
                        KeyCode::Up => {
                            app.scroll = app.scroll.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            app.scroll = app.scroll.saturating_add(1); // Will be clamped in ui()
                        }
                        KeyCode::PageUp => {
                            let scroll_amount = (app.visible_height as usize / 2).max(1);
                            app.scroll = app.scroll.saturating_sub(scroll_amount);
                        }
                        KeyCode::PageDown => {
                            let scroll_amount = (app.visible_height as usize / 2).max(1);
                            app.scroll = app.scroll.saturating_add(scroll_amount); // Will be clamped in ui()
                        }
                        _ => {}
                    }
                }
            }
        }
        app.process_messages();
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

fn ui(f: &mut Frame, app: &mut App) {
    // Create the custom skin for markdown
    let md_skin = ChatBot::create_custom_skin();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),     // Messages area
            Constraint::Length(6),   // Log area
            Constraint::Length(3),   // Input area
            Constraint::Length(1),   // Status bar
        ])
        .split(f.size());

    // Calculate scroll position to keep the latest messages visible
    let messages_text = app.messages.iter()
        .flat_map(|msg| {
            if msg.starts_with("Assistant:") {
                let content = msg.trim_start_matches("Assistant:").trim();
                let mut lines = vec![Line::from(vec![
                    Span::styled("Assistant: ".to_string(), Style::default().fg(Color::Green))
                ])];
                lines.extend(markdown::markdown_to_lines(content));
                lines
            } else if msg.starts_with("You:") {
                vec![Line::from(vec![
                    Span::styled("You: ".to_string(), Style::default().fg(Color::Blue)),
                    Span::raw(msg.trim_start_matches("You:").trim().to_string()),
                ])]
            } else {
                vec![Line::from(msg.clone())]
            }
        })
        .collect::<Vec<_>>();

    // Calculate scroll and content metrics
    let total_message_height = messages_text.len();
    let visible_height = chunks[0].height.saturating_sub(2) as usize; // Subtract 2 for borders
    let max_scroll = if total_message_height > visible_height {
        total_message_height - visible_height
    } else {
        0
    };

    // Clamp scroll value to valid range
    app.scroll = app.scroll.min(max_scroll);

    // Create message area with scrollbar space
    let message_area = chunks[0];
    let (msg_area, scrollbar_area) = {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(message_area);
        (chunks[0], chunks[1])
    };

    // Render messages with the current scroll position
    let messages = Paragraph::new(messages_text.clone())
        .block(Block::default()
            .title("Messages")
            .borders(Borders::LEFT | Borders::RIGHT | Borders::TOP)
            .border_type(BorderType::Rounded))
        .wrap(Wrap { trim: true })
        .scroll((app.scroll as u16, 0));
    f.render_widget(messages, msg_area);

    // Update scrollbar to reflect current position
    let scrollbar = Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .begin_symbol(Some("↑"))
        .end_symbol(Some("↓"));

    f.render_stateful_widget(
        scrollbar,
        scrollbar_area,
        &mut ScrollbarState::new(total_message_height as usize)
            .position(app.scroll),
    );

    // Log area with dimmed text
    let log_content = if let Ok(buffer) = app.log_buffer.lock() {
        // Take last 5 log messages
        buffer.iter().rev().take(5).rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        String::from("Unable to access log buffer")
    };

    let collapsed_set = symbols::border::Set {
        top_left: symbols::line::NORMAL.vertical_right,
        top_right: symbols::line::NORMAL.vertical_left,
        ..symbols::border::PLAIN
    };
    let collapsed_set_input = symbols::border::Set {
        top_left: symbols::line::NORMAL.vertical_right,
        top_right: symbols::line::NORMAL.vertical_left,
        ..symbols::border::ROUNDED
    };
    // Log area with modified borders
    let logs = Paragraph::new(log_content)
        .block(Block::default()
            .title("Logs")
            .borders(Borders::LEFT | Borders::RIGHT | Borders::TOP)
            .border_set(collapsed_set))  // Apply custom border set
        .wrap(Wrap { trim: true });
    f.render_widget(logs, chunks[1]);

    // Input area with modified borders
    let input = Paragraph::new(app.input.as_str())
        .block(Block::default()
            .title("Input")
            .borders(Borders::ALL)
            .border_set(collapsed_set_input))  // Apply custom border set
        .wrap(Wrap { trim: true });
    f.render_widget(input, chunks[2]);

    // Status Bar with smaller text
    let status_text = format!(
        "Provider: {} | {}",
        app.chatbot.current_provider, app.info_message
    );
    let status_bar = Paragraph::new(status_text)
        .block(Block::default().borders(Borders::NONE))
        .style(Style::default().add_modifier(Modifier::DIM));  // Makes the text appear less prominent
    f.render_widget(status_bar, chunks[3]);

    // Cursor position
    let cursor_x = chunks[2].x + 1 + (app.input.len() as u16 % chunks[2].width);
    let cursor_y = chunks[2].y + 1 + (app.input.len() as u16 / chunks[2].width);
    f.set_cursor(cursor_x, cursor_y);

    // Update app's visible height
    app.visible_height = chunks[0].height;
}
