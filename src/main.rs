use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use log::{LevelFilter, Log, Metadata, Record, info, error};
use std::any::Any;
use ratatui::{
    prelude::*,
    style::Style,
    widgets::{
        Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
        BorderType,
    },
    Terminal,
};
use std::io::stdout;
use std::sync::{Arc, Mutex, Once};
use std::str::FromStr;

mod chatbot;
mod config;
use chatbot::ChatBot;
use config::Config;
mod llama;
mod web_search;
mod markdown;

#[derive(Clone)]
struct UiLogger {
    buffer: Arc<Mutex<Vec<String>>>,
    max_lines: usize,
}

impl UiLogger {
    fn new(max_lines: usize) -> Self {
        Self {
            buffer: Arc::new(Mutex::new(Vec::new())),
            max_lines,
        }
    }

    pub fn get_line_count(&self) -> usize {
        self.buffer.lock().unwrap().len()
    }
}

impl UiLogger {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Log for UiLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let message = format!("[{}] {}", record.level(), record.args());
        if let Ok(mut buffer) = self.buffer.lock() {
            let len = buffer.len();
            buffer.push(message);
            // Keep only the last max_lines messages
            if len > self.max_lines {
                buffer.drain(0..len - self.max_lines);
            }
            // Notify about new log message
            unsafe {
                if let Some(app) = APP.as_mut() {
                app.log_scroll = usize::MAX; // Auto-scroll to bottom
            }
        }
    }

    fn flush(&self) {}
}

// Modify App struct to include log buffer
static mut APP: Option<App> = None;

#[derive(Debug)]
struct App {
    chatbot: ChatBot,
    input: String,
    messages: Vec<String>,
    scroll: usize,          // This will now represent the line number we're scrolled to
    current_response: String,
    info_message: String,
    log_buffer: Arc<Mutex<Vec<String>>>,
    visible_height: u16,
    log_scroll: usize,
    is_log_focused: bool,
    last_log_count: usize,  // Track number of log lines to detect new messages
}

impl App {
    async fn new(config: Config, log_buffer: Arc<Mutex<Vec<String>>>) -> Result<Self> {
        let chatbot = ChatBot::new(config).await?;
        
        Ok(Self {
            chatbot,
            input: String::new(),
            messages: Vec::new(),
            scroll: 0,
            current_response: String::new(),
            info_message: String::new(),
            log_buffer,
            visible_height: 0,
            log_scroll: 0,
            is_log_focused: false,
            last_log_count: 0,
        })
    }

    fn update_current_response(&mut self, content: &str) {
        self.current_response.push_str(content);
        
        // Update the last message if it's from the assistant
        if let Some(last_msg) = self.chatbot.conversation.last_message_mut() {
            if last_msg.role == "assistant" {
                // Update both raw and rendered content
                last_msg.raw_content = self.current_response.clone();
                last_msg.rendered_content = markdown::markdown_to_lines(&self.current_response);
                
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
}

static INIT: Once = Once::new();

#[tokio::main]
async fn main() -> Result<()> {

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Initialize logger first
    let logger = UiLogger::new(100); // Keep last 100 log messages
    let log_buffer = logger.buffer.clone();
    
    // Initialize the logger only once
    INIT.call_once(|| {
        log::set_boxed_logger(Box::new(logger.clone()))
            .map(|()| log::set_max_level(LevelFilter::Debug))
            .expect("Failed to set logger");
    });

    // Create and store app state
    let config = Config::load()?;
    let app = App::new(config, log_buffer.clone()).await?;
    unsafe {
        APP = Some(app);
    }

    // Main loop
    loop {
        let app = unsafe { APP.as_mut().unwrap() };
        // Check for new log messages and auto-scroll if needed
        let current_log_count = log_buffer.lock().unwrap().len();
        if current_log_count > app.last_log_count {
            app.log_scroll = usize::MAX; // Auto-scroll to bottom
            app.last_log_count = current_log_count;
        }

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
                                                error!("Error saving last interaction: {}", e);
                     
                                            }
                                        }
                                        //add /quit or /exit to quit the app
                                        "quit" | "exit" => {
                                            break;
                                        }
                                        //add /log logging_level to set the logging level
                                        "log" => {
                                            if command.len() > 1 {
                                                let logging_level = command[1];
                                                if let Ok(level) = LevelFilter::from_str(logging_level) {
                                                    log::set_max_level(level);
                                                    info!("Logging level set to: {}", logging_level);
                                                } else {
                                                    error!("Invalid logging level: {}", logging_level);
                                                }
                                            }
                                        }
                                        "saveall" => {
                                            if let Err(e) = app.chatbot.save_all_history() {
                                                error!("Error saving all history: {}", e);
                  
                                            }
                                        }
                                        "model" => {
                                            if command.len() > 1 {
                                                let provider = command[1];
                                                if let Err(e) = app.chatbot.set_provider(provider) {
                                                    error!("Failed to switch to provider '{}': {}", provider, e);
                               
                                                } else {
                                                   info!("Successfully switched to provider: {}", provider);
                                                }
                                            } else {
                                                error!("Usage: /model <provider>");
                                            }
                                        }
                                        _ => {
                                            error!("Unknown command: {}", input);
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
                                                                app.messages.push("▌".to_string()); // Typing indicator
                                                                is_first_chunk = false;
                                                            } else {
                                                                // Remove typing indicator if present
                                                                if let Some(last) = app.messages.last_mut() {
                                                                    if last == "▌" {
                                                                        app.messages.pop();
                                                                    }
                                                                }
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
                            if app.is_log_focused {
                                app.log_scroll = app.log_scroll.saturating_sub(1);
                            } else {
                                app.scroll = app.scroll.saturating_sub(1);
                            }
                        }
                        KeyCode::Down => {
                            if app.is_log_focused {
                                app.log_scroll = app.log_scroll.saturating_add(1);
                            } else {
                                app.scroll = app.scroll.saturating_add(1);
                            }
                        }
                        KeyCode::PageUp => {
                            let scroll_amount = (app.visible_height as usize / 2).max(1);
                            if app.is_log_focused {
                                app.log_scroll = app.log_scroll.saturating_sub(scroll_amount);
                            } else {
                                app.scroll = app.scroll.saturating_sub(scroll_amount);
                            }
                        }
                        KeyCode::PageDown => {
                            let scroll_amount = (app.visible_height as usize / 2).max(1);
                            if app.is_log_focused {
                                app.log_scroll = app.log_scroll.saturating_add(scroll_amount);
                            } else {
                                app.scroll = app.scroll.saturating_add(scroll_amount);
                            }
                        }
                        KeyCode::Tab => {
                            unsafe {
                                let app = APP.as_mut().unwrap();
                                app.is_log_focused = !app.is_log_focused;
                            }
                            if app.is_log_focused {
                                // Auto-scroll to bottom when focusing logs
                                app.log_scroll = usize::MAX;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

fn ui(f: &mut Frame, app: &mut App) {
    // Remove or define create_custom_skin if needed
    // let _md_skin = ChatBot::create_custom_skin();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),     // Messages area
            Constraint::Length(6),   // Log area
            Constraint::Length(3),   // Input area
            Constraint::Length(1),   // Status bar
        ])
        .split(f.size());

    // Get pre-rendered messages from conversation
    let messages_text = app.chatbot.conversation.get_rendered_messages();

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

    // Log area with scrollbar
    let log_content = if let Ok(buffer) = app.log_buffer.lock() {
        buffer.join("\n")
    } else {
        String::from("Unable to access log buffer")
    };

    // Calculate log scroll metrics
    let log_lines: Vec<&str> = log_content.lines().collect();
    let log_height = chunks[1].height.saturating_sub(2) as usize;
    let max_log_scroll = if log_lines.len() > log_height {
        log_lines.len() - log_height
    } else {
        0
    };
    
    // Clamp log scroll value
    app.log_scroll = app.log_scroll.min(max_log_scroll);
    
    // Get visible log lines
    let visible_logs = log_lines
        .iter()
        .skip(app.log_scroll)
        .take(log_height)
        .map(|line| Line::from(*line))
        .collect::<Vec<_>>();

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
    // Split log area into content and scrollbar
    let (log_area, log_scrollbar_area) = {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(chunks[1]);
        (chunks[0], chunks[1])
    };

    let logs = Paragraph::new(visible_logs)
        .block(Block::default()
            .title("Logs")
            .borders(Borders::LEFT | Borders::RIGHT | Borders::TOP)
            .border_set(collapsed_set)
            .style(if app.is_log_focused {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            }))
        .wrap(Wrap { trim: true })
        .scroll((0, 0));
    f.render_widget(logs, log_area);

    // Render log scrollbar
    let log_scrollbar = Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .begin_symbol(Some("↑"))
        .end_symbol(Some("↓"));

    f.render_stateful_widget(
        log_scrollbar,
        log_scrollbar_area,
        &mut ScrollbarState::new(log_lines.len())
            .position(app.log_scroll),
    );

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
