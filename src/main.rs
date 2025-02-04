use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use log::{debug, error, info, LevelFilter};
use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    widgets::{Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
    Terminal,
};
use std::io::stdout;
use std::str::FromStr;
use std::sync::Once;
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

#[derive(Debug)]
struct App {
    chatbot: ChatBot,
    input: String,
    scroll: usize,       // for chat history
    log_scroll: usize,   // scroll offset for the logs area
    current_response: String,
    info_message: String,
    visible_height: u16,
    is_log_focused: bool,
    raw_mode: bool,      // show raw (unformatted) content
    follow_mode: bool,   // auto scroll when new content is added
    is_streaming: bool,  // whether streaming is in progress
}

impl App {
    async fn new(config: Config) -> Result<Self> {
        let chatbot = ChatBot::new(config).await?;
        Ok(Self {
            chatbot,
            input: String::new(),
            scroll: 0,
            log_scroll: 0,
            current_response: String::new(),
            info_message: String::new(),
            visible_height: 0,
            is_log_focused: false,
            raw_mode: false,
            follow_mode: true,
            is_streaming: false,
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

    // Initialize a file logger that writes to ~/abot.log
    {
        let home_dir = dirs::home_dir().expect("Could not get HOME directory");
        let log_path = home_dir.join("abot.log");
        let file = File::create(&log_path).expect("Failed to create log file");
        WriteLogger::init(
            LevelFilter::from_str(APP_LOG_FILTER).unwrap_or(LevelFilter::Info),
            SimpleLogConfig::default(),
            file,
        )
        .expect("Failed to initialize file logger");
    }

    // Create app state locally
    let config = Config::load()?;
    let mut app = App::new(config).await?;

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
                                        
                                        // Handle commands starting with /
                                        if input.starts_with("/") {
                                            let command = input.trim_start_matches("/").split_whitespace().collect::<Vec<_>>();
                                            match command[0] {
                                                "save" => {
                                                    if let Err(e) = app.chatbot.save_last_interaction() {
                                                        error!("Error saving last interaction: {}", e);
                                                    }
                                                }
                                                "quit" | "exit" => break,
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
                                            // Append user message and query the chatbot
                                            app.chatbot.add_message("user", &input);
                                            terminal.draw(|f| ui(f, &mut app))?;
                                            match app.chatbot.query(&input).await {
                                                Ok(mut stream) => {
                                                    app.chatbot.add_message("assistant", "");
                                                    app.current_response.clear();
                                                    app.is_streaming = true;
                                                    terminal.hide_cursor()?;
                                                    loop {
                                                        tokio::select! {
                                                            maybe_chunk = stream.next() => {
                                                                if let Some(chunk_result) = maybe_chunk {
                                                                    match chunk_result {
                                                                        Ok(content) => {
                                                                            if !content.is_empty() {
                                                                                app.current_response.push_str(&content);
                                                                                app.chatbot.update_last_message(&app.current_response);
                                                                                if app.follow_mode {
                                                                                    app.scroll = usize::MAX;
                                                                                }
                                                                            }
                                                                        },
                                                                        Err(e) => {
                                                                            error!("Error receiving chunk: {}", e);
                                                                            break;
                                                                        },
                                                                    }
                                                                } else {
                                                                    break;
                                                                }
                                                            },
                                                            _ = sleep(Duration::from_millis(16)) => {}
                                                        }
                                                        terminal.draw(|f| ui(f, &mut app))?;
                                                    }
                                                    app.is_streaming = false;
                                                    terminal.show_cursor()?;
                                                    app.current_response.clear();
                                                }
                                                Err(e) => error!("Failed to send message: {}", e),
                                            }
                                        }
                                    }
                                }
                                KeyCode::Char(c) => app.input.push(c),
                                KeyCode::Backspace => { app.input.pop(); },
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
                                        debug!("Scroll up by 10, scroll: {}, visible_height: {}", app.scroll, app.visible_height);
                                        app.scroll = app.scroll.saturating_sub(10);
                                        app.follow_mode = false;
                                    }
                                }
                                KeyCode::PageDown => {
                                    if !app.is_log_focused {
                                        let scroll_amount = app.visible_height as usize;
                                        app.scroll = app.scroll.saturating_add(scroll_amount);
                                        debug!("Scroll down by 10, scroll:{}", app.scroll);
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

fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),       // Chat messages area
            Constraint::Ratio(3, 10), // Log area (30% of screen height)
            Constraint::Length(3),    // Input area
            Constraint::Length(1),    // Status bar
        ])
        .split(f.size());

    // Prepare chat messages for display
    let mut messages_buffer = Vec::new();
    for message in &app.chatbot.messages {
        let prefix = match message.role.as_str() {
            "assistant" => Span::styled("Assistant: ", Style::default().fg(Color::Green)),
            "user" => Span::styled("User: ", Style::default().fg(Color::Blue)),
            _ => Span::raw("System: "),
        };
        messages_buffer.push(Line::from(vec![prefix]));
        if app.raw_mode {
            messages_buffer.push(Line::from(message.raw_content.as_str()));
        } else if message.role == "assistant" {
            messages_buffer.extend(message.rendered_content.clone());
        } else {
            messages_buffer.push(Line::from(message.raw_content.as_str()));
        }
    }

    // Compute available width for text display (subtracting borders)
    let available_width = chunks[0].width.saturating_sub(2) as usize;

    // Calculate an effective total height for all chat messages
    // by taking into account the prefix line (always 1)
    // plus the number of wrapped lines for the message content.
    let total_message_height: usize = app.chatbot.messages.iter().map(|message| {
        let prefix_height = 1;
        if app.raw_mode {
            let content_height: usize = message.raw_content
                .lines()
                .map(|line| ((line.len() as f32) / (available_width as f32)).ceil() as usize)
                .sum();
            prefix_height + content_height
        } else if message.role == "assistant" {
            // rendered_content is already wrapped.
            prefix_height + message.rendered_content.len()
        } else {
            let content_height: usize = message.raw_content
                .lines()
                .map(|line| ((line.len() as f32) / (available_width as f32)).ceil() as usize)
                .sum();
            prefix_height + content_height
        }
    }).sum();

    // Get the available inner height for the messages (after borders)
    let visible_height = chunks[0].height.saturating_sub(2) as usize;
    let max_scroll = if total_message_height > visible_height {
        total_message_height - visible_height
    } else {
        0
    };

    // Auto-scroll to bottom if follow_mode is enabled or scroll is set to ULONG_MAX.
    if app.follow_mode || app.scroll == usize::MAX {
        app.scroll = max_scroll;
    } else {
        app.scroll = app.scroll.min(max_scroll);
    }

    // --- Messages Widget ---
    let message_area = chunks[0];
    let (msg_area, scrollbar_area) = {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(message_area);
        (chunks[0], chunks[1])
    };

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
    f.render_widget(messages, msg_area);

    let scrollbar = Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .begin_symbol(Some("↑"))
        .end_symbol(Some("↓"));
    f.render_stateful_widget(
        scrollbar,
        scrollbar_area,
        &mut ScrollbarState::new(total_message_height).position(app.scroll),
    );

    // --- Logs Area ---
    // Read log file from ~/abot.log
    let home_dir = dirs::home_dir().unwrap();
    let log_file_path = home_dir.join("abot.log");
    let log_content = std::fs::read_to_string(&log_file_path)
        .unwrap_or_else(|_| "Unable to access log file".to_string());
    let log_lines: Vec<&str> = log_content.lines().collect();

    // Split the logs area horizontally into the actual log area and a scrollbar.
    let (log_area, log_scrollbar_area) = {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(chunks[1]);
        (chunks[0], chunks[1])
    };

    // Determine available width for the logs text.
    // Subtract 2 to account for left/right borders.
    let available_width = log_area.width.saturating_sub(2) as usize;

    // Wrap each log line manually.
    let mut wrapped_log_lines = Vec::new();
    for line in log_lines {
        if available_width == 0 {
            wrapped_log_lines.push(line.to_string());
        } else {
            // Break the line into chunks of at most available_width characters.
            let mut char_iter = line.chars().peekable();
            while char_iter.peek().is_some() {
                let chunk: String = char_iter.by_ref().take(available_width).collect();
                wrapped_log_lines.push(chunk);
            }
        }
    }

    // Compute the inner height available for the log content.
    // For a block with a top border and title, the inner height is the total height minus one.
    let inner_height = log_area.height.saturating_sub(1) as usize;

    // Calculate how many steps we can scroll.
    let max_log_scroll = if wrapped_log_lines.len() > inner_height {
        wrapped_log_lines.len() - inner_height
    } else {
        0
    };

    // Always scroll to the bottom.
    app.log_scroll = max_log_scroll;

    // Prepare the visible (wrapped) log lines.
    let visible_logs: Vec<Line> = wrapped_log_lines
        .iter()
        .skip(app.log_scroll)
        .take(inner_height)
        .map(|s| Line::from(s.as_str()))
        .collect();

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

    let log_scrollbar = Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .begin_symbol(Some("↑"))
        .end_symbol(Some("↓"));
    f.render_stateful_widget(
        log_scrollbar,
        log_scrollbar_area,
        &mut ScrollbarState::new(wrapped_log_lines.len()).position(app.log_scroll),
    );

    // --- End Logs Area ---

    // Input area
    let collapsed_set_input = symbols::border::Set {
        top_left: symbols::line::NORMAL.vertical_right,
        top_right: symbols::line::NORMAL.vertical_left,
        ..symbols::border::ROUNDED
    };
    let input = Paragraph::new(app.input.as_str())
        .block(
            Block::default()
                .title("Input")
                .borders(Borders::ALL)
                .border_set(collapsed_set_input),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(input, chunks[2]);

    // Status bar
    let status_text = format!(
        "Provider: {} | Topic: {}",
        app.chatbot.current_provider, app.chatbot.conversation_id
    );
    let status_bar = Paragraph::new(status_text)
        .block(Block::default().borders(Borders::NONE))
        .style(Style::default().add_modifier(Modifier::DIM));
    f.render_widget(status_bar, chunks[3]);

    // Set the cursor position for the input area
    if !app.is_streaming {
        let cursor_x = chunks[2].x + 1 + (app.input.len() as u16 % chunks[2].width);
        let cursor_y = chunks[2].y + 1 + (app.input.len() as u16 / chunks[2].width);
        f.set_cursor(cursor_x, cursor_y);
    }
    
    app.visible_height = chunks[0].height;
}
