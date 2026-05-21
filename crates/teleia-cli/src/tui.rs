use crate::highlight;
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::{pin_mut, StreamExt};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Wrap},
    Terminal,
};
use std::{io, time::Duration};
use teleia_agent::{Agent, TurnEvent, MAX_TOOL_HOPS};

const HINTS: &str = "enter send · ↑↓ scroll · /help cmds · ctrl-c quit";

// Tokyo Night palette
const TN_CYAN: Color = Color::Rgb(125, 207, 255); // #7dcfff
const TN_PURPLE: Color = Color::Rgb(187, 154, 247); // #bb9af7
const TN_YELLOW: Color = Color::Rgb(224, 175, 104); // #e0af68
const TN_RED: Color = Color::Rgb(247, 118, 142); // #f7768e
const TN_BLUE: Color = Color::Rgb(122, 162, 247); // #7aa2f7
const TN_DIM: Color = Color::Rgb(86, 95, 137); // #565f89
const TN_FG: Color = Color::Rgb(192, 202, 245); // #c0caf5

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
    should_quit: bool,
    hop: usize, // 0..=MAX_TOOL_HOPS, 0 = idle
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
            should_quit: false,
            hop: 0,
        }
    }

    fn push(&mut self, e: Entry) {
        self.history.push(e);
        self.scroll = 0; // auto-scroll on new content
    }

    fn apply(&mut self, evt: TurnEvent) {
        match evt {
            TurnEvent::AssistantStart => {
                self.hop = (self.hop + 1).min(MAX_TOOL_HOPS);
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
            TurnEvent::TurnEnd => {
                self.hop = 0;
            }
        }
    }
}

pub async fn run(mut agent: Agent) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = State::new(agent.session_id());
    let result = event_loop(&mut terminal, &mut state, &mut agent).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut State,
    agent: &mut Agent,
) -> Result<()> {
    loop {
        if state.should_quit {
            return Ok(());
        }
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
                state.hop = 0;
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
        "list" => match agent.list_aliases() {
            Ok(aliases) if aliases.is_empty() => {
                state.push(Entry::Info("no saved aliases".into()));
            }
            Ok(aliases) => {
                let mut text = format!("saved aliases ({}):", aliases.len());
                for (name, session_id, _) in aliases {
                    text.push_str(&format!(
                        "\n  {name} → {}",
                        &session_id[..session_id.len().min(12)]
                    ));
                }
                state.push(Entry::Info(text));
            }
            Err(e) => state.push(Entry::Error(format!("list: {e}"))),
        },
        "clear" => {
            state.history.clear();
            state.scroll = 0;
        }
        "delete" | "rm" => {
            if arg.is_empty() {
                state.push(Entry::Error("usage: /delete NAME".into()));
                return;
            }
            match agent.delete_alias(arg) {
                Ok(()) => state.push(Entry::Info(format!("deleted alias '{arg}'"))),
                Err(e) => state.push(Entry::Error(format!("delete: {e}"))),
            }
        }
        "model" => {
            if arg.is_empty() {
                state.push(Entry::Info(format!("current model: {}", agent.model())));
            } else {
                agent.set_model(arg.to_string());
                state.push(Entry::Info(format!("switched model to {arg}")));
            }
        }
        "quit" | "exit" | "q" => {
            state.should_quit = true;
        }
        "help" | "?" => {
            state.push(Entry::Info(
                "commands: /reset · /clear · /save NAME · /load NAME · /delete NAME · /list · /model [NAME] · /help · /quit"
                    .into(),
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
        Style::default().fg(TN_DIM)
    } else {
        Style::default().fg(TN_FG)
    };
    let input_widget = Paragraph::new(Line::from(vec![
        Span::styled("> ", Style::default().fg(TN_CYAN)),
        Span::styled(&state.input, prompt_style),
    ]))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(input_widget, chunks[1]);

    // Show the terminal cursor at the end of the input. ratatui hides the
    // cursor by default; calling set_cursor_position each frame makes it
    // visible at our chosen spot so users can see where they're typing.
    let inside = chunks[1];
    let cursor_x = inside.x + 1 /* border */ + 2 /* "> " */ + state.input.chars().count() as u16;
    let cursor_x = cursor_x.min(inside.x + inside.width.saturating_sub(2));
    let cursor_y = inside.y + 1;
    f.set_cursor_position((cursor_x, cursor_y));

    let ratio = (state.hop as f64 / MAX_TOOL_HOPS as f64).clamp(0.0, 1.0);
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(TN_PURPLE).bg(Color::Reset))
        .label(Span::styled(
            format!("hops {}/{}", state.hop, MAX_TOOL_HOPS),
            Style::default().fg(TN_FG),
        ))
        .ratio(ratio);
    f.render_widget(gauge, chunks[2]);

    let status_line = Line::from(vec![
        Span::styled(
            &state.status,
            Style::default().fg(TN_DIM).add_modifier(Modifier::ITALIC),
        ),
        Span::raw("   "),
        Span::styled(HINTS, Style::default().fg(TN_DIM)),
    ]);
    f.render_widget(Paragraph::new(status_line), chunks[3]);
}

/// Walk an assistant message looking for ```lang ... ``` code fences. Plain
/// prose lines render unchanged; fenced blocks are syntax-highlighted using
/// the language token (or unhighlighted if syntect doesn't recognise it).
fn render_assistant_lines(text: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut in_code = false;
    let mut code_lang = String::new();
    let mut code_buf = String::new();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("```") {
            if in_code {
                for hl in highlight::highlight(&code_buf, &code_lang, "") {
                    out.push(hl);
                }
                code_buf.clear();
                code_lang.clear();
                in_code = false;
            } else {
                code_lang = rest.trim().to_string();
                in_code = true;
            }
        } else if in_code {
            code_buf.push_str(line);
            code_buf.push('\n');
        } else {
            out.push(Line::from(line.to_string()));
        }
    }

    // If the message ended mid-fence (mid-stream), flush what we have so the
    // user sees the partial code rather than nothing.
    if in_code && !code_buf.is_empty() {
        for hl in highlight::highlight(&code_buf, &code_lang, "") {
            out.push(hl);
        }
    }

    out
}

fn render_entry(entry: &Entry) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    match entry {
        Entry::User(text) => {
            out.push(Line::from(Span::styled(
                "you",
                Style::default().fg(TN_CYAN).add_modifier(Modifier::BOLD),
            )));
            for line in text.lines() {
                out.push(Line::from(line.to_string()));
            }
            out.push(Line::from(""));
        }
        Entry::Assistant { text, complete } => {
            out.push(Line::from(Span::styled(
                if *complete { "teleia" } else { "teleia ▌" },
                Style::default().fg(TN_PURPLE).add_modifier(Modifier::BOLD),
            )));
            for line in render_assistant_lines(text) {
                out.push(line);
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
                Style::default().fg(TN_YELLOW),
            )));
            let highlighted = if name == "read" {
                highlight::extension_from_read_args(args)
                    .map(|ext| highlight::highlight(output, &ext, "  "))
            } else {
                None
            };
            let body: Box<dyn Iterator<Item = Line<'static>>> = match highlighted {
                Some(hl) => Box::new(hl.into_iter()),
                None => Box::new(output.lines().take(20).map(|line| {
                    Line::from(Span::styled(
                        format!("  {line}"),
                        Style::default().fg(TN_DIM),
                    ))
                })),
            };
            for line in body.take(20) {
                out.push(line);
            }
            out.push(Line::from(""));
        }
        Entry::Error(text) => {
            out.push(Line::from(Span::styled(
                format!("error: {text}"),
                Style::default().fg(TN_RED),
            )));
            out.push(Line::from(""));
        }
        Entry::Info(text) => {
            let mut first = true;
            for line in text.lines() {
                let prefix = if first { "· " } else { "  " };
                out.push(Line::from(Span::styled(
                    format!("{prefix}{line}"),
                    Style::default().fg(TN_BLUE),
                )));
                first = false;
            }
            out.push(Line::from(""));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_prose_passes_through_unchanged() {
        let lines = render_assistant_lines("hello\nworld");
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn fence_markers_are_stripped() {
        // The ``` lines are consumed; only the code content survives.
        let lines = render_assistant_lines("```rust\nfn x() {}\n```");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn prose_and_code_alternate() {
        let lines = render_assistant_lines("before\n```rust\ncode\n```\nafter");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn unclosed_fence_still_flushes() {
        // Mid-stream: the assistant hasn't emitted the closing ``` yet, so
        // we still render what we have rather than dropping the code on the
        // floor.
        let lines = render_assistant_lines("```rust\nfn x() {}\n");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn empty_text_renders_nothing() {
        assert_eq!(render_assistant_lines("").len(), 0);
    }

    #[test]
    fn unlabeled_fence_falls_back_to_plain() {
        // ``` without a language token should still be highlighted (as
        // plain text) and the fence markers removed.
        let lines = render_assistant_lines("```\nsome code\n```");
        assert_eq!(lines.len(), 1);
    }
}
