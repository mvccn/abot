use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyEvent},
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
    log_scroll: Arc<Mutex<usize>>,
}

impl UiLogger {
    fn new(max_lines: usize) -> Self {
        Self {
            buffer: Arc::new(Mutex::new(Vec::new())),
            max_lines,
            log_scroll: Arc::new(Mutex::new(0)),
        }
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
        let message = format!(
            "[{}] {}:{} - {}",
            record.level(),
            record.file().unwrap_or("unknown"),
            record.line().unwrap_or(0),
            record.args()
        );
        if let Ok(mut buffer) = self.buffer.lock() {
            let len = buffer.len();
            buffer.push(message);
            // Keep only the last max_lines messages
            if len > self.max_lines {
                buffer.drain(0..len - self.max_lines);
            }
            // Notify about new log message
            if let Ok(mut scroll) = self.log_scroll.lock() {
                *scroll = usize::MAX; // Auto-scroll to bottom
            }
        }
    }

    fn flush(&self) {}
}

#[derive(Debug)]
struct App {
    chatbot: ChatBot,
    input: String,
    // messages: Vec<String>,
    scroll: usize,          // This will now represent the line number we're scrolled to
    current_response: String,
    info_message: String,
    log_buffer: Arc<Mutex<Vec<String>>>,
    visible_height: u16,
    log_scroll: usize,
    is_log_focused: bool,
    last_log_count: usize,  // Track number of log lines to detect new messages
    last_message_count: usize,  // Add this new field to track message count
    raw_mode: bool,         // Whether to show raw content instead of rendered markdown
    follow_mode: bool,  // follow mode scrolling: auto scroll to bottom when new content is added,
                        // but manual scrolling will disable the follow mode
                        // and re-enable it when we scroll to the bottom
    is_streaming: bool,  // Add this new field
    log_scroll_shared: Arc<Mutex<usize>>,
}

impl App {
    async fn new(config: Config, log_buffer: Arc<Mutex<Vec<String>>>) -> Result<Self> {
        let chatbot = ChatBot::new(config).await?;
        
        Ok(Self {
            chatbot,
            input: String::new(),
            // messages: Vec::new(),
            scroll: 0,
            current_response: String::new(),
            info_message: String::new(),
            log_buffer,
            visible_height: 0,
            log_scroll: 0,
            is_log_focused: false,
            last_log_count: 0,
            last_message_count: 0,
            raw_mode: false,
            follow_mode: true,  // Start in follow mode
            is_streaming: false,  // Initialize the new field
            log_scroll_shared: Arc::new(Mutex::new(0)),
        })
    }

    fn handle_input(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::PageUp | KeyCode::Up => {
                if !self.is_log_focused {
                    self.scroll = self.scroll.saturating_sub(if key.code == KeyCode::PageUp { 10 } else { 1 });
                    // Disable follow mode when manually scrolling up
                    self.follow_mode = false;
                }
            }
            KeyCode::PageDown | KeyCode::Down => {
                if !self.is_log_focused {
                    self.scroll = self.scroll.saturating_add(if key.code == KeyCode::PageDown { 10 } else { 1 });
                    // Re-enable follow mode if we scroll to bottom
                    if self.is_at_bottom(self.scroll) {
                        self.follow_mode = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn is_at_bottom(&self, max_scroll: usize) -> bool {
        self.scroll >= max_scroll.saturating_sub(1)
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
    let logger = UiLogger::new(1000); // Keep last 1000 log messages
    let log_buffer = logger.buffer.clone();
    
    // Initialize the logger only once
    INIT.call_once(|| {
        log::set_boxed_logger(Box::new(logger.clone()))
            .map(|()| log::set_max_level(LevelFilter::Debug))
            .expect("Failed to set logger");
    });

    // Create app state locally
    let config = Config::load()?;
    let mut app = App::new(config, log_buffer.clone()).await?;

    // Main loop
    loop {
        // Use app directly without unsafe
        let current_log_count = log_buffer.lock().unwrap().len();
        if current_log_count > app.last_log_count {
            app.log_scroll = usize::MAX;
            app.last_log_count = current_log_count;
        }

        // Check for new messages and auto-scroll if needed
        let current_message_count = app.chatbot.messages.len();
        if current_message_count > app.last_message_count {
            app.scroll = usize::MAX; // Auto-scroll to bottom
            app.last_message_count = current_message_count;
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
                                        "raw" => {
                                            app.raw_mode = !app.raw_mode;
                                            app.info_message = format!("Raw mode {}", if app.raw_mode { "enabled" } else { "disabled" });
                                        }
                                        _ => {
                                            error!("Unknown command: {}", input);
                                        }
                                    }
                                } else {
                                    // Immediately display user message
                                    // app.messages.push(format!("You: {}", input));
                                    app.chatbot.add_message("user", &input);
                                    // Force a redraw to show the user message
                                    terminal.draw(|f| ui(f, &mut app))?;
                                    match app.chatbot.querry(&input).await {
                                        Ok(mut stream) => {
                                            app.chatbot.add_message("assistant", "");
                                            app.current_response.clear();
                                            app.is_streaming = true;
                                            terminal.hide_cursor()?;
                                            
                                            while let Some(chunk_result) = stream.next().await {
                                                match chunk_result {
                                                    Ok(content) => {
                                                        if !content.is_empty() {
                                                            app.current_response.push_str(&content);
                                                            app.chatbot.update_last_message(&app.current_response);
                                                            
                                                            // Only auto-scroll if in follow mode
                                                            if app.follow_mode {
                                                                app.scroll = usize::MAX;
                                                            }
                                                            
                                                            terminal.draw(|f| ui(f, &mut app))?;
                                                        }
                                                    }
                                                    Err(e) => {
                                                        error!("Error receiving chunk: {}", e);
                                                        break;
                                                    }
                                                }
                                            }
                                            
                                            app.is_streaming = false;
                                            terminal.show_cursor()?;
                                            app.current_response.clear();
                                        }
                                        Err(e) => {
                                            error!("Failed to send message: {}", e);
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
                            if !app.is_log_focused {
                                // Scroll up by a larger amount (e.g., 10 lines)
                                app.scroll = app.scroll.saturating_sub(10);
                            }
                        }
                        KeyCode::PageDown => {
                            if !app.is_log_focused {
                                // Scroll down by a larger amount
                                app.scroll = app.scroll.saturating_add(10);
                            }
                        }
                        KeyCode::Tab => {
                            app.is_log_focused = !app.is_log_focused;
                            if app.is_log_focused {
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

//ui code will be called every time app draw is called
fn ui(f: &mut Frame, app: &mut App) {
    // Remove or define create_custom_skin if needed
    // let _md_skin = ChatBot::create_custom_skin();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),     // Messages area
            Constraint::Ratio(3, 10),   // Log area (30% of screen height)
            Constraint::Length(3),   // Input area
            Constraint::Length(1),   // Status bar
        ])
        .split(f.size());

    // Get all chatbot messages to render
    let mut messages_to_display = Vec::new();
    
    // Add all completed messages
    for message in &app.chatbot.messages {
        // Add role prefix
        let prefix = match message.role.as_str() {
            "assistant" => Span::styled("Assistant: ", Style::default().fg(Color::Green)),
            "user" => Span::styled("User: ", Style::default().fg(Color::Blue)),
            _ => Span::raw("System: "),
        };
        messages_to_display.push(Line::from(vec![prefix]));
        
        // Show raw content if raw mode is enabled
        if app.raw_mode {
            messages_to_display.push(Line::from(message.raw_content.as_str()));
        } else if message.role == "assistant" {
            messages_to_display.extend(message.rendered_content.clone());
        } else {
            messages_to_display.push(Line::from(message.raw_content.as_str()));
        }
    }
    
    // If there's a current response being streamed, update the last message
    // if !app.current_response.is_empty() {
    //     app.chatbot.update_last_message(&app.current_response);
    // }

    // Calculate scroll and content metrics
    let total_message_height = messages_to_display.iter()
        .map(|line| {
            let line_width = chunks[0].width.saturating_sub(2) as usize; // Subtract 2 for borders
            let rendered_width = if app.raw_mode {
                line.width()
            } else {
                // For rendered content, we need to consider the actual rendered width
                // which might include formatting, code blocks, etc.
                match line.spans.first() {
                    Some(span) if span.content.starts_with("```") => {
                        // Code blocks typically need more height
                        (line.width() as f32 / (line_width - 2) as f32).ceil() as usize + 2
                    }
                    Some(span) if span.content.starts_with(">") => {
                        // Blockquotes might wrap differently
                        (line.width() as f32 / (line_width - 2) as f32).ceil() as usize
                    }
                    _ => {
                        // Regular text
                        (line.width() as f32 / line_width as f32).ceil() as usize
                    }
                }
            };
            rendered_width
        })
        .sum::<usize>();

    // Add extra padding to ensure we can scroll to the very end
    let total_message_height = total_message_height + 5; // Add some extra lines of padding

    let visible_height = chunks[0].height.saturating_sub(2) as usize;
    let max_scroll = if total_message_height > visible_height {
        total_message_height - visible_height
    } else {
        0
    };

    // Add debug logging
    // if app.scroll == usize::MAX || app.scroll == max_scroll {
    //     info!(
    //         "Scroll metrics - Total: {}, Visible: {}, Max: {}, Current: {}",
    //         total_message_height, visible_height, max_scroll, app.scroll
    //     );
    // }

    // Clamp scroll value to valid range
    if app.scroll == usize::MAX {
        app.scroll = max_scroll;
    } else {
        app.scroll = app.scroll.min(max_scroll);
    }

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

    // Calculate available width for text (accounting for borders and padding)
    let text_width = msg_area.width.saturating_sub(2); // 1 char padding on each side
    
    // Render messages with proper wrapping
    let messages = Paragraph::new(messages_to_display.clone())
        .block(Block::default()
            .title("Chat")
            .borders(Borders::LEFT | Borders::RIGHT | Borders::TOP)
            .border_type(BorderType::Rounded))
        // .wrap(Wrap { trim: false })
        .scroll((app.scroll as u16, 0))
        .style(Style::default().fg(Color::White));
    
    // Use a custom render method to handle wrapping properly
    f.render_widget(messages, msg_area.inner(&Margin {
        horizontal: 1,
        vertical: 0,
    }));

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
                Style::default().add_modifier(Modifier::DIM)
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

    // Only set cursor position if not streaming
    if !app.is_streaming {
        let cursor_x = chunks[2].x + 1 + (app.input.len() as u16 % chunks[2].width);
        let cursor_y = chunks[2].y + 1 + (app.input.len() as u16 / chunks[2].width);
        f.set_cursor(cursor_x, cursor_y);
    }

    // Update app's visible height
    app.visible_height = chunks[0].height;
}
