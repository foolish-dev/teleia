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

const HINTS: &str = "enter send · esc normal · :q quit · tab accept · /help";

/// Slash commands in their canonical form (aliases like `/q`, `/rm`,
/// `/exit`, `/info` are accepted by `handle_slash` but not surfaced by
/// autocomplete to avoid suggesting ambiguous short prefixes).
const SLASH_COMMANDS: &[&str] = &[
    "clear", "delete", "exit", "help", "list", "load", "model", "quit", "reset", "save", "show",
];

// Tokyo Night palette
const TN_CYAN: Color = Color::Rgb(125, 207, 255); // #7dcfff
const TN_PURPLE: Color = Color::Rgb(187, 154, 247); // #bb9af7
const TN_YELLOW: Color = Color::Rgb(224, 175, 104); // #e0af68
const TN_RED: Color = Color::Rgb(247, 118, 142); // #f7768e
const TN_BLUE: Color = Color::Rgb(122, 162, 247); // #7aa2f7
const TN_GREEN: Color = Color::Rgb(158, 206, 106); // #9ece6a
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

/// Ghost-text autocomplete suggestion shown after the cursor.
/// `completion` is what gets appended when the user presses Tab;
/// `placeholder` is shown for hint (e.g. " NAME") but never typed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Suggestion {
    completion: String,
    placeholder: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Insert,
    Normal,
    Command,
}

struct State {
    input: String,
    input_cursor: usize, // byte offset into input
    history: Vec<Entry>,
    status: String,
    model: String,
    scroll: u16, // offset from auto-scroll bottom; 0 = follow
    working: bool,
    should_quit: bool,
    hop: usize, // 0..=MAX_TOOL_HOPS, 0 = idle
    suggestion: Option<Suggestion>,
    mode: Mode,
    command_buf: String,
    command_cursor: usize,
}

impl State {
    fn new(session_id: &str, model: &str) -> Self {
        Self {
            input: String::new(),
            input_cursor: 0,
            history: Vec::new(),
            status: format!(
                "session {} · ready",
                &session_id[..session_id.len().min(12)]
            ),
            model: model.to_string(),
            scroll: 0,
            working: false,
            should_quit: false,
            hop: 0,
            suggestion: None,
            mode: Mode::Insert,
            command_buf: String::new(),
            command_cursor: 0,
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

    let mut state = State::new(agent.session_id(), agent.model());
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

        match state.mode {
            Mode::Insert => {
                match key.code {
                    KeyCode::Esc => {
                        state.mode = Mode::Normal;
                    }
                    KeyCode::Up | KeyCode::PageUp => {
                        state.scroll = state
                            .scroll
                            .saturating_add(if key.code == KeyCode::PageUp { 5 } else { 1 });
                    }
                    KeyCode::Down | KeyCode::PageDown => {
                        state.scroll = state
                            .scroll
                            .saturating_sub(if key.code == KeyCode::PageDown { 5 } else { 1 });
                    }
                    KeyCode::Left => {
                        if let Some((i, _)) =
                            state.input[..state.input_cursor].char_indices().next_back()
                        {
                            state.input_cursor = i;
                        }
                    }
                    KeyCode::Right => {
                        if let Some(c) = state.input[state.input_cursor..].chars().next() {
                            state.input_cursor += c.len_utf8();
                        }
                    }
                    KeyCode::Home => state.input_cursor = 0,
                    KeyCode::End => state.input_cursor = state.input.len(),
                    KeyCode::Tab => {
                        if let Some(s) = state.suggestion.clone() {
                            state.input.push_str(&s.completion);
                            state.input_cursor = state.input.len();
                        }
                    }
                    KeyCode::Char(c) => {
                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                            match c {
                                'a' => state.input_cursor = 0,
                                'e' => state.input_cursor = state.input.len(),
                                'u' => {
                                    state.input.clear();
                                    state.input_cursor = 0;
                                }
                                'w' => delete_word_before_cursor(state),
                                _ => {}
                            }
                        } else {
                            state.input.insert(state.input_cursor, c);
                            state.input_cursor += c.len_utf8();
                        }
                    }
                    KeyCode::Backspace if state.input_cursor > 0 => {
                        let prev = state.input[..state.input_cursor]
                            .chars()
                            .next_back()
                            .expect("cursor > 0 implies at least one char before");
                        let new_cursor = state.input_cursor - prev.len_utf8();
                        state.input.remove(new_cursor);
                        state.input_cursor = new_cursor;
                    }
                    KeyCode::Delete if state.input_cursor < state.input.len() => {
                        state.input.remove(state.input_cursor);
                    }
                    KeyCode::Enter => {
                        submit_input(terminal, state, agent).await;
                    }
                    _ => {}
                }
            }

            Mode::Normal => {
                match key.code {
                    // Mode transitions into Insert
                    KeyCode::Char('i') => state.mode = Mode::Insert,
                    KeyCode::Char('a') => {
                        if let Some(c) = state.input[state.input_cursor..].chars().next() {
                            state.input_cursor += c.len_utf8();
                        }
                        state.mode = Mode::Insert;
                    }
                    KeyCode::Char('I') => {
                        state.input_cursor = 0;
                        state.mode = Mode::Insert;
                    }
                    KeyCode::Char('A') => {
                        state.input_cursor = state.input.len();
                        state.mode = Mode::Insert;
                    }
                    // Cursor motion
                    KeyCode::Char('h') | KeyCode::Left => {
                        if let Some((i, _)) =
                            state.input[..state.input_cursor].char_indices().next_back()
                        {
                            state.input_cursor = i;
                        }
                    }
                    KeyCode::Char('l') | KeyCode::Right => {
                        if let Some(c) = state.input[state.input_cursor..].chars().next() {
                            state.input_cursor += c.len_utf8();
                        }
                    }
                    KeyCode::Char('0') | KeyCode::Home => state.input_cursor = 0,
                    KeyCode::Char('$') | KeyCode::End => state.input_cursor = state.input.len(),
                    // History scroll
                    KeyCode::Char('j') | KeyCode::Down | KeyCode::PageDown => {
                        state.scroll = state
                            .scroll
                            .saturating_sub(if key.code == KeyCode::PageDown { 5 } else { 1 });
                    }
                    KeyCode::Char('k') | KeyCode::Up | KeyCode::PageUp => {
                        state.scroll = state
                            .scroll
                            .saturating_add(if key.code == KeyCode::PageUp { 5 } else { 1 });
                    }
                    // Editing
                    KeyCode::Char('x') if state.input_cursor < state.input.len() => {
                        state.input.remove(state.input_cursor);
                    }
                    // Enter command mode
                    KeyCode::Char(':') => {
                        state.mode = Mode::Command;
                        state.command_buf.clear();
                        state.command_cursor = 0;
                    }
                    // Tab still accepts a suggestion (rare in Normal but harmless)
                    KeyCode::Tab => {
                        if let Some(s) = state.suggestion.clone() {
                            state.input.push_str(&s.completion);
                            state.input_cursor = state.input.len();
                        }
                    }
                    // Submit also works from Normal
                    KeyCode::Enter => {
                        submit_input(terminal, state, agent).await;
                    }
                    KeyCode::Esc => {}
                    _ => {}
                }
            }

            Mode::Command => match key.code {
                KeyCode::Esc => {
                    state.mode = Mode::Normal;
                    state.command_buf.clear();
                    state.command_cursor = 0;
                }
                KeyCode::Char(c) => {
                    state.command_buf.insert(state.command_cursor, c);
                    state.command_cursor += c.len_utf8();
                }
                KeyCode::Left => {
                    if let Some((i, _)) = state.command_buf[..state.command_cursor]
                        .char_indices()
                        .next_back()
                    {
                        state.command_cursor = i;
                    }
                }
                KeyCode::Right => {
                    if let Some(c) = state.command_buf[state.command_cursor..].chars().next() {
                        state.command_cursor += c.len_utf8();
                    }
                }
                KeyCode::Home => state.command_cursor = 0,
                KeyCode::End => state.command_cursor = state.command_buf.len(),
                KeyCode::Backspace if state.command_cursor > 0 => {
                    let prev = state.command_buf[..state.command_cursor]
                        .chars()
                        .next_back()
                        .expect("cursor > 0 implies at least one char before");
                    let new_cursor = state.command_cursor - prev.len_utf8();
                    state.command_buf.remove(new_cursor);
                    state.command_cursor = new_cursor;
                }
                KeyCode::Enter => {
                    let cmd = std::mem::take(&mut state.command_buf);
                    state.command_cursor = 0;
                    state.mode = Mode::Normal;
                    execute_ex(state, agent, &cmd);
                }
                _ => {}
            },
        }

        refresh_suggestion(state, agent);
    }
}

/// Submit the contents of state.input — slash command or chat turn.
/// Extracted so both Insert-Enter and Normal-Enter can call it.
async fn submit_input<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut State,
    agent: &mut Agent,
) {
    let raw = std::mem::take(&mut state.input);
    state.input_cursor = 0;
    state.mode = Mode::Insert;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return;
    }
    if let Some(cmd) = trimmed.strip_prefix('/') {
        handle_slash(state, agent, cmd);
        return;
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

/// Translate a vim-style ex command (without the leading ":") into the
/// equivalent slash command, then dispatch through `handle_slash`. Returns
/// an error entry for unknown commands.
fn execute_ex(state: &mut State, agent: &mut Agent, cmd: &str) {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return;
    }
    match translate_ex(cmd) {
        Ok(slash) => handle_slash(state, agent, &slash),
        Err(msg) => state.push(Entry::Error(msg)),
    }
}

/// Pure translation from ex-style ":<name> [arg]" to the matching slash
/// command string ("save NAME", "quit", etc.). Returns Err with an
/// already-formatted message for unknown names.
fn translate_ex(cmd: &str) -> Result<String, String> {
    let mut parts = cmd.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();
    let translated = match name {
        "q" | "quit" => "quit".to_string(),
        "w" | "write" => format!("save {arg}"),
        "wq" => format!("save {arg}"), // close-enough analogue: save, no quit
        "e" | "edit" | "l" | "load" => format!("load {arg}"),
        "d" | "bd" | "delete" => format!("delete {arg}"),
        "ls" | "list" => "list".to_string(),
        "model" => format!("model {arg}"),
        "help" | "h" => "help".to_string(),
        "reset" => "reset".to_string(),
        "clear" => "clear".to_string(),
        "show" | "info" => "show".to_string(),
        other => return Err(format!("unknown ex command: :{other}")),
    };
    Ok(translated)
}

/// Update state.suggestion based on the current input. Hits the store only
/// for /load /delete /rm — otherwise pure prefix logic.
fn refresh_suggestion(state: &mut State, agent: &Agent) {
    let needs_aliases = matches!(
        state
            .input
            .strip_prefix('/')
            .and_then(|r| r.split_once(' '))
            .map(|(c, _)| c),
        Some("load") | Some("delete") | Some("rm")
    );
    let aliases: Vec<String> = if needs_aliases {
        agent
            .list_aliases()
            .ok()
            .unwrap_or_default()
            .into_iter()
            .map(|(n, _, _)| n)
            .collect()
    } else {
        Vec::new()
    };
    state.suggestion = compute_suggestion(&state.input, &aliases);
}

fn compute_suggestion(input: &str, aliases: &[String]) -> Option<Suggestion> {
    let rest = input.strip_prefix('/')?;

    // Already past the command name: try to complete the argument.
    if let Some(space) = rest.find(' ') {
        let cmd = &rest[..space];
        let arg = rest[space + 1..].trim_start();
        return arg_suggestion(cmd, arg, aliases);
    }

    if rest.is_empty() {
        return None;
    }

    // Prefix matches a longer command.
    if let Some(cmd) = SLASH_COMMANDS
        .iter()
        .find(|c| c.starts_with(rest) && **c != rest)
    {
        let completion = cmd[rest.len()..].to_string();
        let placeholder = arg_placeholder(cmd).unwrap_or("").to_string();
        return Some(Suggestion {
            completion,
            placeholder,
        });
    }

    // Exact match of a command that takes an argument: show the placeholder
    // so the user knows what's expected.
    if SLASH_COMMANDS.contains(&rest) {
        return arg_placeholder(rest).map(|p| Suggestion {
            completion: String::new(),
            placeholder: p.to_string(),
        });
    }

    None
}

fn arg_suggestion(cmd: &str, arg: &str, aliases: &[String]) -> Option<Suggestion> {
    match cmd {
        "load" | "delete" | "rm" => {
            let matching = aliases
                .iter()
                .find(|name| name.starts_with(arg) && name.as_str() != arg)?;
            Some(Suggestion {
                completion: matching[arg.len()..].to_string(),
                placeholder: String::new(),
            })
        }
        _ => None,
    }
}

fn arg_placeholder(cmd: &str) -> Option<&'static str> {
    match cmd {
        "save" | "load" | "delete" | "rm" => Some(" NAME"),
        "model" => Some(" [NAME]"),
        _ => None,
    }
}

/// Ctrl+W: walk back from the cursor past any trailing whitespace, then past
/// the word it sits in, and delete that span.
fn delete_word_before_cursor(state: &mut State) {
    if state.input_cursor == 0 {
        return;
    }
    let chars_before: Vec<(usize, char)> =
        state.input[..state.input_cursor].char_indices().collect();
    let mut idx = chars_before.len();
    // Skip trailing whitespace
    while idx > 0 && chars_before[idx - 1].1.is_whitespace() {
        idx -= 1;
    }
    // Skip the word
    while idx > 0 && !chars_before[idx - 1].1.is_whitespace() {
        idx -= 1;
    }
    let new_cursor = chars_before.get(idx).map(|&(i, _)| i).unwrap_or(0);
    state
        .input
        .replace_range(new_cursor..state.input_cursor, "");
    state.input_cursor = new_cursor;
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
                state.model = arg.to_string();
                state.push(Entry::Info(format!("switched model to {arg}")));
            }
        }
        "show" | "info" => {
            let session_id = agent.session_id().to_string();
            let model = agent.model().to_string();
            let aliases_here: Vec<String> = agent
                .list_aliases()
                .ok()
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, sid, _)| *sid == session_id)
                .map(|(n, _, _)| n)
                .collect();
            let mut text = format!("session: {session_id}\nmodel:   {model}");
            if !aliases_here.is_empty() {
                text.push_str(&format!("\naliases: {}", aliases_here.join(", ")));
            }
            state.push(Entry::Info(text));
        }
        "quit" | "exit" | "q" => {
            state.should_quit = true;
        }
        "help" | "?" => {
            state.push(Entry::Info(
                "commands: /reset · /clear · /save NAME · /load NAME · /delete NAME · /list · /model [NAME] · /show · /help · /quit"
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

    // Pick prompt + active buffer per mode.
    let (prompt, prompt_color, buf, buf_cursor) = match state.mode {
        Mode::Insert => ("> ", TN_CYAN, &state.input, state.input_cursor),
        Mode::Normal => ("> ", TN_GREEN, &state.input, state.input_cursor),
        Mode::Command => (": ", TN_YELLOW, &state.command_buf, state.command_cursor),
    };

    // Horizontally scroll so the cursor is always visible.
    let inside = chunks[1];
    let visible_width = inside.width.saturating_sub(4) as usize; // 2 borders + prefix (2)
    let cursor_chars = buf[..buf_cursor].chars().count();
    let start_char = cursor_chars.saturating_sub(visible_width.saturating_sub(1));
    let visible_text: String = buf.chars().skip(start_char).take(visible_width).collect();

    let body_style = if state.working {
        Style::default().fg(TN_DIM)
    } else {
        Style::default().fg(TN_FG)
    };
    let mut spans = vec![
        Span::styled(prompt, Style::default().fg(prompt_color)),
        Span::styled(visible_text, body_style),
    ];
    // Ghost-text suggestion only in Insert when the cursor is at the end.
    if state.mode == Mode::Insert && state.input_cursor == state.input.len() {
        if let Some(s) = &state.suggestion {
            let used = cursor_chars - start_char;
            let remaining = visible_width.saturating_sub(used);
            let ghost: String = s
                .completion
                .chars()
                .chain(s.placeholder.chars())
                .take(remaining)
                .collect();
            if !ghost.is_empty() {
                spans.push(Span::styled(ghost, Style::default().fg(TN_DIM)));
            }
        }
    }
    let input_widget =
        Paragraph::new(Line::from(spans)).block(Block::default().borders(Borders::ALL));
    f.render_widget(input_widget, chunks[1]);

    // ratatui hides the cursor by default; positioning it each frame makes
    // it visible at the edit point within the scrolled view.
    let cursor_x = inside.x + 1 /* border */ + 2 /* prefix */ + (cursor_chars - start_char) as u16;
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

    let (mode_label, mode_color) = match state.mode {
        Mode::Insert => ("INS", TN_CYAN),
        Mode::Normal => ("NOR", TN_GREEN),
        Mode::Command => ("CMD", TN_YELLOW),
    };
    let status_line = Line::from(vec![
        Span::styled(
            mode_label,
            Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" · "),
        Span::styled(
            &state.status,
            Style::default().fg(TN_DIM).add_modifier(Modifier::ITALIC),
        ),
        Span::raw(" · "),
        Span::styled(
            short_model(&state.model),
            Style::default().fg(TN_DIM).add_modifier(Modifier::ITALIC),
        ),
        Span::raw("   "),
        Span::styled(HINTS, Style::default().fg(TN_DIM)),
    ]);
    f.render_widget(Paragraph::new(status_line), chunks[3]);
}

/// Compact "hf.co/FoolDev/Thanatos-27B:Q4_K_M" → "Thanatos-27B" for status-
/// bar use: take the segment after the last '/', then drop everything from
/// the first ':'. Leaves short names like "llama3" alone.
fn short_model(model: &str) -> &str {
    let tail = model.rsplit('/').next().unwrap_or(model);
    tail.split(':').next().unwrap_or(tail)
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

    fn state_with(input: &str, cursor: usize) -> State {
        let mut s = State::new("dummy-session-id", "dummy-model");
        s.input = input.into();
        s.input_cursor = cursor;
        s
    }

    #[test]
    fn delete_word_at_cursor_0_is_noop() {
        let mut s = state_with("hello", 0);
        delete_word_before_cursor(&mut s);
        assert_eq!(s.input, "hello");
        assert_eq!(s.input_cursor, 0);
    }

    #[test]
    fn delete_word_removes_trailing_word_and_one_space() {
        let mut s = state_with("hello world", 11);
        delete_word_before_cursor(&mut s);
        assert_eq!(s.input, "hello ");
        assert_eq!(s.input_cursor, 6);
    }

    #[test]
    fn delete_word_skips_trailing_whitespace_first() {
        let mut s = state_with("hello world  ", 13);
        delete_word_before_cursor(&mut s);
        assert_eq!(s.input, "hello ");
        assert_eq!(s.input_cursor, 6);
    }

    #[test]
    fn delete_word_with_only_whitespace_clears_to_start() {
        let mut s = state_with("   ", 3);
        delete_word_before_cursor(&mut s);
        assert_eq!(s.input, "");
        assert_eq!(s.input_cursor, 0);
    }

    #[test]
    fn delete_word_in_middle_keeps_suffix() {
        // cursor between "hello" and " world"
        let mut s = state_with("hello world", 5);
        delete_word_before_cursor(&mut s);
        assert_eq!(s.input, " world");
        assert_eq!(s.input_cursor, 0);
    }

    #[test]
    fn short_model_strips_hub_prefix_and_quant_suffix() {
        assert_eq!(
            short_model("hf.co/FoolDev/Thanatos-27B:Q4_K_M"),
            "Thanatos-27B"
        );
    }

    #[test]
    fn short_model_leaves_simple_names_alone() {
        assert_eq!(short_model("llama3"), "llama3");
        assert_eq!(short_model("qwen2.5"), "qwen2.5");
    }

    #[test]
    fn short_model_handles_slash_only_or_colon_only() {
        assert_eq!(short_model("a/b"), "b");
        assert_eq!(short_model("model:tag"), "model");
    }

    #[test]
    fn suggest_completes_command_prefix() {
        let s = compute_suggestion("/sa", &[]).unwrap();
        assert_eq!(s.completion, "ve");
        assert_eq!(s.placeholder, " NAME");
    }

    #[test]
    fn suggest_finds_unambiguous_short_prefix() {
        // "re" matches only "reset" — completes uniquely.
        let s = compute_suggestion("/re", &[]).unwrap();
        assert_eq!(s.completion, "set");
        assert_eq!(s.placeholder, ""); // /reset takes no arg
    }

    #[test]
    fn suggest_exact_command_shows_arg_placeholder() {
        let s = compute_suggestion("/save", &[]).unwrap();
        assert_eq!(s.completion, "");
        assert_eq!(s.placeholder, " NAME");
    }

    #[test]
    fn suggest_exact_no_arg_command_returns_none() {
        assert!(compute_suggestion("/reset", &[]).is_none());
        assert!(compute_suggestion("/clear", &[]).is_none());
    }

    #[test]
    fn suggest_returns_none_for_empty_or_lone_slash() {
        assert!(compute_suggestion("", &[]).is_none());
        assert!(compute_suggestion("/", &[]).is_none());
    }

    #[test]
    fn suggest_returns_none_for_unknown_command_prefix() {
        assert!(compute_suggestion("/zzz", &[]).is_none());
    }

    #[test]
    fn suggest_completes_alias_argument_with_prefix() {
        let aliases = vec!["audit-pass-1".to_string(), "foo".to_string()];
        let s = compute_suggestion("/load aud", &aliases).unwrap();
        assert_eq!(s.completion, "it-pass-1");
        assert_eq!(s.placeholder, "");
    }

    #[test]
    fn suggest_first_alias_when_arg_empty() {
        let aliases = vec!["audit-pass-1".to_string(), "foo".to_string()];
        let s = compute_suggestion("/load ", &aliases).unwrap();
        assert_eq!(s.completion, "audit-pass-1");
    }

    #[test]
    fn suggest_none_when_alias_arg_already_matches_exactly() {
        let aliases = vec!["foo".to_string()];
        assert!(compute_suggestion("/load foo", &aliases).is_none());
    }

    #[test]
    fn suggest_rm_alias_aliases_works_like_delete() {
        let aliases = vec!["foo".to_string()];
        let s = compute_suggestion("/rm f", &aliases).unwrap();
        assert_eq!(s.completion, "oo");
    }

    #[test]
    fn ex_translates_quit_aliases() {
        assert_eq!(translate_ex("q").unwrap(), "quit");
        assert_eq!(translate_ex("quit").unwrap(), "quit");
    }

    #[test]
    fn ex_translates_write_and_load() {
        assert_eq!(translate_ex("w foo").unwrap(), "save foo");
        assert_eq!(translate_ex("write foo").unwrap(), "save foo");
        assert_eq!(translate_ex("wq foo").unwrap(), "save foo");
        assert_eq!(translate_ex("e foo").unwrap(), "load foo");
        assert_eq!(translate_ex("edit foo").unwrap(), "load foo");
        assert_eq!(translate_ex("l foo").unwrap(), "load foo");
    }

    #[test]
    fn ex_translates_delete_variants() {
        assert_eq!(translate_ex("d foo").unwrap(), "delete foo");
        assert_eq!(translate_ex("bd foo").unwrap(), "delete foo");
        assert_eq!(translate_ex("delete foo").unwrap(), "delete foo");
    }

    #[test]
    fn ex_translates_no_arg_commands() {
        assert_eq!(translate_ex("ls").unwrap(), "list");
        assert_eq!(translate_ex("list").unwrap(), "list");
        assert_eq!(translate_ex("help").unwrap(), "help");
        assert_eq!(translate_ex("h").unwrap(), "help");
        assert_eq!(translate_ex("reset").unwrap(), "reset");
        assert_eq!(translate_ex("clear").unwrap(), "clear");
    }

    #[test]
    fn ex_translates_model() {
        assert_eq!(translate_ex("model llama3").unwrap(), "model llama3");
        assert_eq!(translate_ex("model").unwrap(), "model ");
    }

    #[test]
    fn ex_rejects_unknown_command() {
        let err = translate_ex("nonsense").unwrap_err();
        assert!(err.contains("nonsense"));
    }

    #[test]
    fn ex_translates_show_and_info() {
        assert_eq!(translate_ex("show").unwrap(), "show");
        assert_eq!(translate_ex("info").unwrap(), "show");
    }
}
