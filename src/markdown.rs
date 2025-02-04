use pulldown_cmark::{Parser, Event, Tag};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Span, Line},
};
use syntect::{
    easy::HighlightLines,
    highlighting::ThemeSet,
    parsing::SyntaxSet,
};
use std::collections::HashMap;
use log::debug;
use std::convert::TryInto;

lazy_static::lazy_static! {
    static ref LANGUAGE_ALIASES: HashMap<&'static str, &'static str> = {
        let mut m = HashMap::new();
        m.insert("js", "JavaScript");
        m.insert("ts", "TypeScript");
        m.insert("py", "Python");
        m.insert("rs", "Rust");
        m.insert("cpp", "C++");
        m.insert("hpp", "C++");
        m.insert("c", "C");
        m.insert("h", "C");
        m.insert("go", "Go");
        m.insert("rb", "Ruby");
        m.insert("php", "PHP");
        m.insert("java", "Java");
        m.insert("sh", "Bash");
        m.insert("bash", "Bash");
        m.insert("yaml", "YAML");
        m.insert("yml", "YAML");
        m.insert("json", "JSON");
        m.insert("md", "Markdown");
        m.insert("sql", "SQL");
        m.insert("html", "HTML");
        m.insert("css", "CSS");
        m.insert("toml", "TOML");
        m.insert("rust", "Rust");
        m.insert("dockerfile", "Dockerfile");
        m
    };
}

fn detect_language(lang_hint: &str) -> Option<&'static str> {
    let lang_lower = lang_hint.to_lowercase();
    
    // Special handling for HTML
    if lang_lower == "html" || lang_lower.starts_with("html:") {
        return Some("HTML");
    }
    
    LANGUAGE_ALIASES.get(lang_lower.as_str()).copied()
}

fn handle_code_block(
    text: &str,
    syntax: &syntect::parsing::SyntaxReference,
    theme: &syntect::highlighting::Theme,
    ps: &syntect::parsing::SyntaxSet,
    lines: &mut Vec<Line<'static>>
) {
    let mut h = HighlightLines::new(syntax, theme);
    
    // Split text while preserving empty lines and indentation
    let text_lines: Vec<&str> = text.lines().collect();
    
    for line in text_lines {
        let mut line_spans = Vec::new();
        
        // Highlight the entire line exactly as it appears
        let ranges = h.highlight_line(line, ps)
            .unwrap_or_default();
            
        for (style, text) in ranges {
            let color = Style::default()
                .fg(convert_syntect_color(style.foreground))
                .add_modifier(if style.font_style.contains(syntect::highlighting::FontStyle::BOLD) {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });
                
            line_spans.push(Span::styled(text.to_string(), color));
        }
        
        lines.push(Line::from(line_spans));
    }
}

pub fn markdown_to_lines(markdown: &str) -> Vec<Line<'static>> {
    debug!("Markdown to lines: {:?}", markdown);
    // Initialize syntax highlighting
    let ps = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    let theme = &ts.themes["base16-ocean.dark"];
    
    let parser = Parser::new(markdown);
    let mut lines: Vec<Line> = Vec::new();
    let mut current_spans: Vec<Span> = Vec::new();
    let mut current_style = Style::default();
    let mut code_block = false;
    let mut current_language = String::new();
    let mut list_level = 0;
    let mut list_start_numbers: Vec<usize> = Vec::new(); // Track start numbers for each list level

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading(level) => {
                    if !current_spans.is_empty() {
                        lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                    }
                    current_style = match level {
                        1 => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        2 => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                        3 => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                        _ => Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
                    };
                }
                Tag::CodeBlock(kind) => {
                    if !current_spans.is_empty() {
                        lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                    }
                    code_block = true;
                    
                    current_language = match kind {
                        pulldown_cmark::CodeBlockKind::Fenced(lang) => {
                            let lang_str = lang.to_string();
                            let lang_token = lang_str.split(':').next()
                                .unwrap_or(&lang_str)
                                .trim();
                            
                            detect_language(lang_token).unwrap_or("Plain Text").to_string()
                        }
                        _ => "Plain Text".to_string(),
                    };
                    
                    // Add empty line before code block
                    lines.push(Line::from(Vec::new()));
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
                    current_spans.push(Span::styled("│ ", current_style));
                }
                Tag::List(start) => {
                    if !current_spans.is_empty() {
                        lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                    }
                    list_level += 1;
                    // Only track numbering for ordered lists
                    if let Some(start_num) = start {
                        list_start_numbers.push(start_num.try_into().unwrap());
                    }
                }
                Tag::Item => {
                    if list_level > 0 {
                        let indent = "  ".repeat(list_level - 1);
                        current_spans.push(Span::raw(indent));
                        
                        // Use number only if list is ordered, otherwise use bullet
                        if let Some(start_num) = list_start_numbers.last_mut() {
                            current_spans.push(Span::styled(format!("{}. ", *start_num), current_style));
                            *start_num += 1; // Increment for the next item
                        } else {
                            current_spans.push(Span::styled("• ", current_style));
                        }
                    }
                }
                Tag::Link(_, _, _) => {
                    current_style = current_style
                        .fg(Color::Blue)
                        .add_modifier(Modifier::UNDERLINED);
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                Tag::CodeBlock(_) => {
                    if !current_spans.is_empty() {
                        lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                    }
                    // Add empty line after code block
                    lines.push(Line::from(Vec::new()));
                    code_block = false;
                }
                Tag::Heading(_) | Tag::BlockQuote | Tag::Paragraph => {
                    if !current_spans.is_empty() {
                        lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                    }
                    lines.push(Line::from(Vec::new()));
                }
                Tag::Item => {
                    if !current_spans.is_empty() {
                        lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                    }
                }
                Tag::List(_) => {
                    if !current_spans.is_empty() {
                        lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                    }
                    list_level = list_level.saturating_sub(1);
                    list_start_numbers.pop(); // Remove the start number for the current list level
                }
                _ => {}
            },
            Event::Text(text) => {
                if code_block {
                    let syntax = if current_language == "Plain Text" {
                        ps.find_syntax_plain_text()
                    } else {
                        ps.find_syntax_by_token(&current_language)
                            .or_else(|| ps.find_syntax_by_extension(&current_language.to_lowercase()))
                            .unwrap_or_else(|| ps.find_syntax_plain_text())
                    };
                    
                    handle_code_block(&text, syntax, theme, &ps, &mut lines);
                } else {
                    // Split text on newlines and preserve them
                    let text_lines: Vec<&str> = text.split('\n').collect();
                    for (i, line) in text_lines.iter().enumerate() {
                        if !line.is_empty() {
                            current_spans.push(Span::styled(line.to_string(), current_style));
                        }
                        if i < text_lines.len() - 1 {
                            // Add a new line after each line except the last one
                            lines.push(Line::from(current_spans.drain(..).collect::<Vec<_>>()));
                        }
                    }
                }
            }
            Event::Code(text) => {
                current_spans.push(Span::styled(
                    text.to_string(),
                    Style::default().fg(Color::Gray).bg(Color::DarkGray),
                ));
            }
            Event::SoftBreak => {
                current_spans.push(Span::raw(" "));
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

fn convert_syntect_color(color: syntect::highlighting::Color) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}
