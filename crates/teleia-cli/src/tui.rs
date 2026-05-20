use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};
use std::{io, time::Duration};
use teleia_agent::{Agent, Step};

enum Entry {
    User(String),
    Assistant(String),
    Tool { name: String, input: String, output: String },
    Error(String),
}

pub async fn run(mut agent: Agent) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, &mut agent).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    result
}

async fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    agent: &mut Agent,
) -> Result<()> {
    let mut input = String::new();
    let mut history: Vec<Entry> = Vec::new();
    let mut status = format!("session {} ready · enter to send · ctrl-c to quit", agent.session_id());
    let mut working = false;

    loop {
        terminal.draw(|f| draw(f, &history, &input, &status, working))?;

        if working {
            continue;
        }

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
                    break;
                }
                match key.code {
                    KeyCode::Char(c) => input.push(c),
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Enter => {
                        if input.trim().is_empty() {
                            continue;
                        }
                        let prompt = std::mem::take(&mut input);
                        history.push(Entry::User(prompt.clone()));
                        status = "thinking…".to_string();
                        working = true;
                        terminal.draw(|f| draw(f, &history, &input, &status, working))?;

                        match agent.turn(prompt).await {
                            Ok(steps) => {
                                for step in steps {
                                    history.push(match step {
                                        Step::Assistant(t) => Entry::Assistant(t),
                                        Step::Tool { name, input, output } => {
                                            Entry::Tool { name, input, output }
                                        }
                                    });
                                }
                                status = "ready".to_string();
                            }
                            Err(e) => {
                                history.push(Entry::Error(format!("{e:#}")));
                                status = "error · ready".to_string();
                            }
                        }
                        working = false;
                    }
                    _ => {}
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
    Ok(())
}

fn draw(
    f: &mut ratatui::Frame,
    history: &[Entry],
    input: &str,
    status: &str,
    working: bool,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(f.area());

    let lines: Vec<Line> = history.iter().flat_map(render_entry).collect();
    let log = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" teleia "))
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset(history, chunks[0].height.saturating_sub(2) as usize), 0));
    f.render_widget(log, chunks[0]);

    let prompt_style = if working {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    let input_widget = Paragraph::new(Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Cyan)),
        Span::styled(input, prompt_style),
    ]))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(input_widget, chunks[1]);

    let status_widget = Paragraph::new(Span::styled(
        status,
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
    ));
    f.render_widget(status_widget, chunks[2]);
}

fn render_entry(entry: &Entry) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    match entry {
        Entry::User(text) => {
            out.push(Line::from(Span::styled(
                "you",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
            for line in text.lines() {
                out.push(Line::from(line.to_string()));
            }
            out.push(Line::from(""));
        }
        Entry::Assistant(text) => {
            out.push(Line::from(Span::styled(
                "teleia",
                Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            )));
            for line in text.lines() {
                out.push(Line::from(line.to_string()));
            }
            out.push(Line::from(""));
        }
        Entry::Tool { name, input, output } => {
            out.push(Line::from(Span::styled(
                format!("⚙ {name}({input})"),
                Style::default().fg(Color::Yellow),
            )));
            for line in output.lines().take(20) {
                out.push(Line::from(Span::styled(
                    format!("  {line}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            out.push(Line::from(""));
        }
        Entry::Error(text) => {
            out.push(Line::from(Span::styled(
                format!("error: {text}"),
                Style::default().fg(Color::Red),
            )));
            out.push(Line::from(""));
        }
    }
    out
}

fn scroll_offset(history: &[Entry], visible: usize) -> u16 {
    let total: usize = history
        .iter()
        .map(|e| match e {
            Entry::User(t) | Entry::Assistant(t) | Entry::Error(t) => t.lines().count() + 2,
            Entry::Tool { output, .. } => output.lines().count().min(20) + 2,
        })
        .sum();
    total.saturating_sub(visible) as u16
}
