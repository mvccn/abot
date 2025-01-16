use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use log::{LevelFilter, Log, Metadata, Record, info, error};
use ratatui::{
    prelude::*,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
        BorderType,
    },
    Terminal,
};
use std::io::stdout;
use std::sync::{Arc, Mutex};

mod chatbot;
mod config;
use chatbot::ChatBot;
use config::Config;
mod llama;
mod web_search;
mod markdown;

struct UiLogger {
    buffer: Arc<Mutex<Vec<String>>>,
}

impl Log for UiLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
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
    log_buffer: Arc<Mutex<Vec<String>>>,
    visible_height: u16,
}

impl App {
    async fn new(config: Config) -> Result<Self> {
        let log_buffer = Arc::new(Mutex::new(Vec::new()));
        
        // Initialize logger
        let logger = UiLogger {
            buffer: log_buffer.clone(),
        };
        
        // Set up the logger
        log::set_boxed_logger(Box::new(logger))
            .map(|()| log::set_max_level(LevelFilter::Info))?;

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
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    let log_buffer = Arc::new(Mutex::new(Vec::new()));
    let logger = UiLogger {
        buffer: log_buffer.clone(),
    };
    
    log::set_boxed_logger(Box::new(logger))
        .map(|()| log::set_max_level(LevelFilter::Info))?;

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
                                                error!("Error saving last interaction: {}", e);
                     
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
                                                    error!("Error setting provider: {}", e);
                               
                                                } else {
                                                   info!("Switched to provider: {}", provider);
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
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

fn ui(f: &mut Frame, app: &mut App) {
    // Create the custom skin for markdown
    let _md_skin = ChatBot::create_custom_skin();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),     // Messages area
            Constraint::Length(6),   // Log area
            Constraint::Length(3),   // Input area
            Constraint::Length(1),   // Status bar
        ])
        .split(f.size());

    // Calculate messages text using cached renderings
    let messages_text: Vec<Line> = app.messages.iter()
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
        .collect();

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
