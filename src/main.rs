use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use log::{debug, error, info, LevelFilter, Log, Metadata, Record, SetLoggerError};
use ratatui::{
    prelude::*,
    style::Style,
    widgets::{
        Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Wrap,
    },
    Terminal,
};
use std::io::stdout;
use std::str::FromStr;
use std::sync::{Arc, Mutex, Once};
use simplelog::{Config as SimpleLogConfig, WriteLogger};
use std::fs::File;
use tokio::time::{sleep, Duration};

mod chatbot;
mod config;
use chatbot::ChatBot;
use config::Config;
mod llama;
mod markdown;
mod web_search;
mod llama_function;

const APP_LOG_FILTER: &str = "abot=debug,chatbot=debug,llama=debug,html5ever=error, *=error";

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
    scroll: usize, // This will now represent the line number we're scrolled to
    log_scroll: usize, // Add this new field for log scrolling
    current_response: String,
    info_message: String,
    log_buffer: Arc<Mutex<Vec<String>>>,
    visible_height: u16,
    is_log_focused: bool,
    raw_mode: bool,        // Whether to show raw content instead of rendered markdown
    follow_mode: bool, // follow mode scrolling: auto scroll to bottom when new content is added,
    // but manual scrolling will disable the follow mode
    // and re-enable it when we scroll to the bottom
    is_streaming: bool, // Add this new field
}

impl App {
    async fn new(config: Config, log_buffer: Arc<Mutex<Vec<String>>>) -> Result<Self> {
        let chatbot = ChatBot::new(config).await?;

        Ok(Self {
            chatbot,
            input: String::new(),
            scroll: 0,
            log_scroll: 0, // Initialize the new field
            current_response: String::new(),
            info_message: String::new(),
            log_buffer,
            visible_height: 0,
            is_log_focused: false,
            raw_mode: false,
            follow_mode: true,   // Start in follow mode
            is_streaming: false, // Initialize the new field
        })
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
            .map(|()| {
                log::set_max_level(
                    LevelFilter::from_str(APP_LOG_FILTER).unwrap_or(LevelFilter::Info),
                )
            })
            .expect("Failed to set logger");

        // // Set up file logger
        // let file = File::create("abot.log").expect("Failed to create log file");
        // WriteLogger::init(LevelFilter::Info, SimpleLogConfig::default(), file)
        //     .expect("Failed to initialize file logger");

        //composite logger
        // init_loggers().expect("Failed to initialize loggers");
    });

    // Create app state locally
    let config = Config::load()?;
    let mut app = App::new(config, log_buffer.clone()).await?;

    // Main loop
    loop {
        // Draw UI first
        terminal.draw(|f| ui(f, &mut app))?;

        tokio::select! {
            _ = sleep(Duration::from_millis(16)) => {
                // Timer tick for UI updates
            }
            result = tokio::task::spawn_blocking(|| event::poll(Duration::from_millis(1))) => {
                if let Ok(Ok(true)) = result {
                    if let Ok(Event::Key(key)) = event::read() {
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
                                                "quit" | "exit" => {
                                                    break;
                                                }
                                                "log" => {
                                                    if command.len() > 1 {
                                                        let logging_level = command[1];
                                                        if let Ok(level) =
                                                            LevelFilter::from_str(logging_level)
                                                        {
                                                            log::set_max_level(level);
                                                            info!(
                                                                "Logging level set to: {}",
                                                                logging_level
                                                            );
                                                        } else {
                                                            error!(
                                                                "Invalid logging level: {}",
                                                                logging_level
                                                            );
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
                                                            error!(
                                                                "Failed to switch to provider '{}': {}",
                                                                provider, e
                                                            );
                                                        } else {
                                                            info!(
                                                                "Successfully switched to provider: {}",
                                                                provider
                                                            );
                                                        }
                                                    } else {
                                                        error!("Usage: /model <provider>");
                                                    }
                                                }
                                                "raw" => {
                                                    app.raw_mode = !app.raw_mode;
                                                    app.info_message = format!(
                                                        "Raw mode {}",
                                                        if app.raw_mode { "enabled" } else { "disabled" }
                                                    );
                                                }
                                                "reset" => {
                                                    app.chatbot.messages.clear();
                                                    info!("Chat history and context have been reset.");
                                                }
                                                "topic" => {
                                                    if command.len() > 1 {
                                                        let topic = command[1..].join(" ");
                                                        match app.chatbot.set_topic(&topic) {
                                                            Ok(sanitized_topic) => {
                                                                info!("Topic set to '{}'", sanitized_topic);
                                                            }
                                                            Err(e) => {
                                                                error!("Failed to set topic '{}': {}", topic, e);
                                                            }
                                                        }
                                                    } else {
                                                        error!("No topic specified");
                                                    }
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
                                            match app.chatbot.query(&input).await {
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
                                                                    app.chatbot.update_last_message(
                                                                        &app.current_response,
                                                                    );

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
                                        // Scroll up by the visible height of the chat area
                                        // let scroll_amount = app.visible_height as usize;
                                        debug!(
                                            "Scroll up by 10, scroll: {}, visible_height: {}",
                                            app.scroll, app.visible_height
                                        );
                                        app.scroll = app.scroll.saturating_sub(10);
                                        // Disable follow mode when manually scrolling up
                                        app.follow_mode = false;
                                    }
                                }
                                KeyCode::PageDown => {
                                    if !app.is_log_focused {
                                        let scroll_amount = app.visible_height as usize;
                                        app.scroll = app.scroll.saturating_add(scroll_amount);
                                        debug!("Scroll down by 10, scroll:{}", app.scroll);
                                        // if app.scroll >= max_scroll {
                                        //     app.scroll = max_scroll;
                                        //     app.follow_mode = true;
                                        // }
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
            Constraint::Min(1),       // Messages area
            Constraint::Ratio(3, 10), // Log area (30% of screen height)
            Constraint::Length(3),    // Input area
            Constraint::Length(1),    // Status bar
        ])
        .split(f.size());

    // Get all chatbot messages to render
    let mut messages_buffer = Vec::new();

    // Add all completed messages
    for message in &app.chatbot.messages {
        // Add role prefix
        let prefix = match message.role.as_str() {
            "assistant" => Span::styled("Assistant: ", Style::default().fg(Color::Green)),
            "user" => Span::styled("User: ", Style::default().fg(Color::Blue)),
            _ => Span::raw("System: "),
        };
        messages_buffer.push(Line::from(vec![prefix]));

        // Show raw content if raw mode is enabled
        if app.raw_mode {
            messages_buffer.push(Line::from(message.raw_content.as_str()));
        } else if message.role == "assistant" {
            messages_buffer.extend(message.rendered_content.clone());
        } else {
            messages_buffer.push(Line::from(message.raw_content.as_str()));
        }
    }
    // debug!(
    //     "messages: {}",
    //     app.chatbot.messages
    //         .iter()
    //         .map(|msg| format!("[{}]: {}", msg.role, msg.raw_content))
    //         .collect::<Vec<_>>()
    //         .join("\n")
    // );

    let visible_width = chunks[0].width.saturating_sub(2) as usize;
    // If there's a current response being streamed, update the last message
    // if !app.current_response.is_empty() {
    //     app.chatbot.update_last_message(&app.current_response);
    // }

    // Calculate scroll and content metrics
    let total_message_height = app.chatbot.messages
        .iter()
        .map(|message| {
            message.raw_content.lines().map(|line| {
                (line.len() as f32 / visible_width as f32).ceil() as usize
            }).sum::<usize>()
        })
        .sum::<usize>() + 5;

    debug!("total_message_height: {}", total_message_height);

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
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(message_area);
        (chunks[0], chunks[1])
    };

    // Calculate available width for text (accounting for borders and padding)
    // let text_width = msg_area.width.saturating_sub(2); // 1 char padding on each side

    // Render messages with exact formatting
    let messages = Paragraph::new(messages_buffer.clone())
        .block(
            Block::default()
                .title("Chat")
                .borders(Borders::LEFT | Borders::RIGHT | Borders::TOP)
                .border_type(BorderType::Rounded),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.scroll as u16, 0))
        .style(Style::default().fg(Color::White));

    // Remove the inner margin when rendering the messages
    f.render_widget(messages, msg_area);

    // Update scrollbar to reflect current position
    let scrollbar = Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .begin_symbol(Some("↑"))
        .end_symbol(Some("↓"));

    f.render_stateful_widget(
        scrollbar,
        scrollbar_area,
        &mut ScrollbarState::new(total_message_height as usize).position(app.scroll),
    );

    // Log area with scrollbar
    let log_content = if let Ok(buffer) = app.log_buffer.lock() {
        buffer.join("\n")
    } else {
        String::from("Unable to access log buffer")
    };

    let log_lines: Vec<&str> = log_content.lines().collect();
    let log_height = chunks[1].height.saturating_sub(2) as usize;
    let max_log_scroll = if log_lines.len() > log_height {
        log_lines.len() - log_height
    } else {
        0
    };

    // Ensure log_scroll is set to show the latest logs
    if app.log_scroll == usize::MAX {
        app.log_scroll = max_log_scroll;
    }

    // Clamp log scroll value to valid range
    app.log_scroll = app.log_scroll.min(max_log_scroll);

    // Get visible log lines
    let visible_logs = log_lines
        .iter()
        .skip(app.log_scroll)
        .take(log_height)
        .map(|line| Line::from(*line))
        .collect::<Vec<_>>();

    let _collapsed_set = symbols::border::Set {
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
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(chunks[1]);
        (chunks[0], chunks[1])
    };

    let logs = Paragraph::new(visible_logs)
        .block(
            Block::default()
                .title("Logs")
                .borders(Borders::LEFT | Borders::RIGHT | Borders::TOP)
                .border_type(BorderType::Plain)
                .style(if app.is_log_focused {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().add_modifier(Modifier::DIM)
                }),
        )
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
        &mut ScrollbarState::new(log_lines.len()).position(app.log_scroll),
    );

    // Input area with modified borders
    let input = Paragraph::new(app.input.as_str())
        .block(
            Block::default()
                .title("Input")
                .borders(Borders::ALL)
                .border_set(collapsed_set_input),
        ) // Apply custom border set
        .wrap(Wrap { trim: true });
    f.render_widget(input, chunks[2]);

    // Status Bar with smaller text
    let status_text = format!(
        "Provider: {} | Topic: {}",
        app.chatbot.current_provider, app.chatbot.conversation_id
    );
    let status_bar = Paragraph::new(status_text)
        .block(Block::default().borders(Borders::NONE))
        .style(Style::default().add_modifier(Modifier::DIM)); // Makes the text appear less prominent
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

struct CompositeLogger {
    ui_logger: UiLogger,
    file_logger: WriteLogger<File>,
}

impl Log for CompositeLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.ui_logger.enabled(metadata) || self.file_logger.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if self.ui_logger.enabled(record.metadata()) {
            self.ui_logger.log(record);
        }
        if self.file_logger.enabled(record.metadata()) {
            self.file_logger.log(record);
        }
    }

    fn flush(&self) {
        self.ui_logger.flush();
        self.file_logger.flush();
    }
}

fn init_loggers() -> Result<(), SetLoggerError> {
    let ui_logger = UiLogger::new(1000);
    let file = File::create("abot.log").expect("Failed to create log file");
    let file_logger = WriteLogger::new(LevelFilter::Debug, SimpleLogConfig::default(), file);

    let composite_logger = CompositeLogger {
        ui_logger,
        file_logger: *file_logger, // Dereference the Box to get the WriteLogger
    };

    log::set_boxed_logger(Box::new(composite_logger))?;
    log::set_max_level(LevelFilter::from_str(APP_LOG_FILTER).unwrap_or(LevelFilter::Debug));

    Ok(())
}
