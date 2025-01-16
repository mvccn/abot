use pulldown_cmark::{Parser, Event, Tag};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Span, Line},
};

pub fn markdown_to_lines(markdown: &str) -> Vec<Line<'static>> {
    let parser = Parser::new(markdown);
    let mut lines: Vec<Line> = Vec::new();
    let mut current_spans: Vec<Span> = Vec::new();
    let mut current_style = Style::default();
    let mut code_block = false;

    for event in parser {
        match event {
            Event::Start(tag) => {
                match tag {
                    Tag::Heading(level) => {
                        if !current_spans.is_empty() {
                            lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                        }
                        current_style = match level {
                            1 => Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                            2 => Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                            _ => Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        };
                    }
                    Tag::CodeBlock(_) => {
                        if !current_spans.is_empty() {
                            lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                        }
                        code_block = true;
                        current_style = Style::default()
                            .fg(Color::Gray)
                            .bg(Color::DarkGray);
                    }
                    Tag::Emphasis => {
                        current_style = current_style.add_modifier(Modifier::ITALIC);
                    }
                    Tag::Strong => {
                        current_style = current_style.add_modifier(Modifier::BOLD);
                    }
                    Tag::BlockQuote => {
                        if !current_spans.is_empty() {
                            lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                        }
                        current_style = Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::ITALIC);
                        current_spans.push(Span::styled("│ ".to_string(), current_style));
                    }
                    Tag::List(_) => {
                        if !current_spans.is_empty() {
                            lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                        }
                    }
                    Tag::Item => {
                        current_spans.push(Span::styled("• ".to_string(), current_style));
                    }
                    Tag::Link(_, _, _) => {
                        current_style = current_style
                            .fg(Color::Blue)
                            .add_modifier(Modifier::UNDERLINED);
                    }
                    _ => {}
                }
            }
            Event::End(tag) => {
                match tag {
                    Tag::CodeBlock(_) => {
                        if !current_spans.is_empty() {
                            lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                        }
                        code_block = false;
                    }
                    Tag::Heading(_) | Tag::BlockQuote | Tag::Paragraph => {
                        if !current_spans.is_empty() {
                            lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                        }
                        lines.push(Line::from(Vec::new()));
                    }
                    _ => {}
                }
                current_style = Style::default();
            }
            Event::Text(text) => {
                if code_block {
                    for line in text.lines() {
                        if !current_spans.is_empty() {
                            lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                        }
                        current_spans.push(Span::styled(line.to_string(), current_style));
                    }
                } else {
                    current_spans.push(Span::styled(text.trim().to_string(), current_style));
                }
            }
            Event::Code(text) => {
                current_spans.push(Span::styled(
                    text.to_string(),
                    Style::default().fg(Color::Gray).bg(Color::DarkGray),
                ));
            }
            Event::SoftBreak => {
                current_spans.push(Span::raw(" ".to_string()));
            }
            Event::HardBreak => {
                if !current_spans.is_empty() {
                    lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                }
            }
            _ => {}
        }
    }

    if !current_spans.is_empty() {
        lines.push(Line::from(current_spans));
    }

    lines
} 
