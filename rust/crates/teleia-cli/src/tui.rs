use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::{pin_mut, StreamExt};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};
use std::{io, time::Duration};
use teleia_agent::{Agent, TurnEvent};

const HINTS: &str = "enter send · ↑↓ scroll · /help cmds · ctrl-c quit";

enum Entry {
    User(String),
    Assistant {
        text: String,
        complete: bool,
    },
    Tool {
        name: String,
        args: String,
        output: String,
        complete: bool,
    },
    Error(String),
    Info(String),
}

struct State {
    input: String,
    history: Vec<Entry>,
    status: String,
    scroll: u16, // offset from auto-scroll bottom; 0 = follow
    working: bool,
}

impl State {
    fn new(session_id: &str) -> Self {
        Self {
            input: String::new(),
            history: Vec::new(),
            status: format!(
                "session {} · ready",
                &session_id[..session_id.len().min(12)]
            ),
            scroll: 0,
            working: false,
        }
    }

    fn push(&mut self, e: Entry) {
        self.history.push(e);
        self.scroll = 0; // auto-scroll on new content
    }

    fn apply(&mut self, evt: TurnEvent) {
        match evt {
            TurnEvent::AssistantStart => {
                self.push(Entry::Assistant {
                    text: String::new(),
                    complete: false,
                });
            }
            TurnEvent::AssistantDelta(text) => {
                if let Some(Entry::Assistant {
                    text: t,
                    complete: false,
                }) = self.history.last_mut()
                {
                    t.push_str(&text);
                    self.scroll = 0;
                } else {
                    self.push(Entry::Assistant {
                        text,
                        complete: false,
                    });
                }
            }
            TurnEvent::AssistantEnd => {
                if let Some(Entry::Assistant { complete, text }) = self.history.last_mut() {
                    *complete = true;
                    if text.is_empty() {
                        self.history.pop();
                    }
                }
            }
            TurnEvent::ToolStart { name, arguments } => {
                self.push(Entry::Tool {
                    name,
                    args: arguments,
                    output: String::new(),
                    complete: false,
                });
            }
            TurnEvent::ToolEnd { output, .. } => {
                if let Some(Entry::Tool {
                    output: o,
                    complete,
                    ..
                }) = self.history.last_mut()
                {
                    *o = output;
                    *complete = true;
                    self.scroll = 0;
                }
            }
            TurnEvent::TurnEnd => {}
        }
    }
}

pub async fn run(mut agent: Agent) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = State::new(agent.session_id());
    let result = event_loop(&mut terminal, &mut state, &mut agent).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

async fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut State,
    agent: &mut Agent,
) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, state))?;

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return Ok(());
        }

        match key.code {
            KeyCode::Up | KeyCode::PageUp => {
                state.scroll =
                    state
                        .scroll
                        .saturating_add(if key.code == KeyCode::PageUp { 5 } else { 1 });
            }
            KeyCode::Down | KeyCode::PageDown => {
                state.scroll = state
                    .scroll
                    .saturating_sub(if key.code == KeyCode::PageDown { 5 } else { 1 });
            }
            KeyCode::Char(c) => state.input.push(c),
            KeyCode::Backspace => {
                state.input.pop();
            }
            KeyCode::Enter => {
                let raw = std::mem::take(&mut state.input);
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(cmd) = trimmed.strip_prefix('/') {
                    handle_slash(state, agent, cmd);
                    continue;
                }
                state.push(Entry::User(trimmed.to_string()));
                state.status = "thinking…".into();
                state.working = true;
                run_turn(terminal, state, agent, trimmed.to_string()).await;
                state.working = false;
                state.status = format!(
                    "session {} · ready",
                    &agent.session_id()[..agent.session_id().len().min(12)]
                );
            }
            _ => {}
        }
    }
}

async fn run_turn<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut State,
    agent: &mut Agent,
    input: String,
) {
    let stream = agent.turn(input);
    pin_mut!(stream);
    loop {
        if let Err(e) = terminal.draw(|f| draw(f, state)) {
            state.push(Entry::Error(format!("draw: {e:#}")));
            return;
        }

        tokio::select! {
            evt = stream.next() => {
                match evt {
                    Some(Ok(e)) => {
                        let is_end = matches!(e, TurnEvent::TurnEnd);
                        state.apply(e);
                        if is_end { return; }
                    }
                    Some(Err(e)) => {
                        state.push(Entry::Error(e.to_string()));
                        return;
                    }
                    None => return,
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(33)) => {
                // periodic redraw so deltas land smoothly
            }
        }
    }
}

fn handle_slash(state: &mut State, agent: &mut Agent, cmd: &str) {
    let mut parts = cmd.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim();
    let arg = parts.next().unwrap_or("").trim();
    match name {
        "reset" => match agent.reset() {
            Ok(()) => {
                state.history.clear();
                state.push(Entry::Info(format!(
                    "started new session {}",
                    &agent.session_id()[..agent.session_id().len().min(12)]
                )));
            }
            Err(e) => state.push(Entry::Error(format!("reset: {e}"))),
        },
        "save" => {
            if arg.is_empty() {
                state.push(Entry::Error("usage: /save NAME".into()));
                return;
            }
            match agent.save_alias(arg) {
                Ok(()) => state.push(Entry::Info(format!("saved current session as '{arg}'"))),
                Err(e) => state.push(Entry::Error(format!("save: {e}"))),
            }
        }
        "load" => {
            if arg.is_empty() {
                state.push(Entry::Error("usage: /load NAME".into()));
                return;
            }
            match agent.load_alias(arg) {
                Ok(id) => {
                    state.history.clear();
                    state.push(Entry::Info(format!(
                        "loaded '{arg}' → session {}",
                        &id[..id.len().min(12)]
                    )));
                }
                Err(e) => state.push(Entry::Error(format!("load: {e}"))),
            }
        }
        "help" | "?" => {
            state.push(Entry::Info(
                "commands: /reset · /save NAME · /load NAME · /help".into(),
            ));
        }
        other => {
            state.push(Entry::Error(format!("unknown command: /{other}")));
        }
    }
}

fn draw(f: &mut ratatui::Frame, state: &State) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(f.area());

    let lines: Vec<Line> = state.history.iter().flat_map(render_entry).collect();
    let visible = chunks[0].height.saturating_sub(2) as usize;
    let total = lines.len();
    let max_offset = total.saturating_sub(visible) as u16;
    let offset = max_offset.saturating_sub(state.scroll);

    let log = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" teleia "))
        .wrap(Wrap { trim: false })
        .scroll((offset, 0));
    f.render_widget(log, chunks[0]);

    let prompt_style = if state.working {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    let input_widget = Paragraph::new(Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Cyan)),
        Span::styled(&state.input, prompt_style),
    ]))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(input_widget, chunks[1]);

    let status_line = Line::from(vec![
        Span::styled(
            &state.status,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
        Span::raw("   "),
        Span::styled(HINTS, Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(status_line), chunks[2]);
}

fn render_entry(entry: &Entry) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    match entry {
        Entry::User(text) => {
            out.push(Line::from(Span::styled(
                "you",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            for line in text.lines() {
                out.push(Line::from(line.to_string()));
            }
            out.push(Line::from(""));
        }
        Entry::Assistant { text, complete } => {
            out.push(Line::from(Span::styled(
                if *complete { "teleia" } else { "teleia ▌" },
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )));
            for line in text.lines() {
                out.push(Line::from(line.to_string()));
            }
            out.push(Line::from(""));
        }
        Entry::Tool {
            name,
            args,
            output,
            complete,
        } => {
            let marker = if *complete { "⚙" } else { "⚙ …" };
            out.push(Line::from(Span::styled(
                format!("{marker} {name}({args})"),
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
        Entry::Info(text) => {
            out.push(Line::from(Span::styled(
                format!("· {text}"),
                Style::default().fg(Color::Blue),
            )));
            out.push(Line::from(""));
        }
    }
    out
}
