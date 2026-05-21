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
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};
use std::{io, time::Duration};
use telia_agent::{Agent, TokenCounts, TurnEvent};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Hint text shown at the right of the status bar; varies by mode so the
/// keys advertised are actually useful in the current context.
fn mode_hints(mode: Mode) -> &'static str {
    match mode {
        Mode::Insert => "↵ send · esc normal · tab accept · /help",
        Mode::Normal => "i insert · : command · G bottom · q quit",
        Mode::Command => "↵ run · esc cancel",
    }
}

/// Slash commands in their canonical form (aliases like `/q`, `/rm`,
/// `/exit`, `/info` are accepted by `handle_slash` but not surfaced by
/// autocomplete to avoid suggesting ambiguous short prefixes).
const SLASH_COMMANDS: &[&str] = &[
    "clear", "delete", "exit", "help", "list", "load", "model", "quit", "reset", "save", "show",
    "theme",
];

#[derive(Debug, Clone, Copy)]
struct Theme {
    cyan: Color,
    purple: Color,
    yellow: Color,
    red: Color,
    blue: Color,
    green: Color,
    dim: Color,
    fg: Color,
    bg: Color,
    bg_hl: Color,
}

const TOKYO_NIGHT: Theme = Theme {
    cyan: Color::Rgb(125, 207, 255),
    purple: Color::Rgb(187, 154, 247),
    yellow: Color::Rgb(224, 175, 104),
    red: Color::Rgb(247, 118, 142),
    blue: Color::Rgb(122, 162, 247),
    green: Color::Rgb(158, 206, 106),
    dim: Color::Rgb(86, 95, 137),
    fg: Color::Rgb(192, 202, 245),
    bg: Color::Rgb(26, 27, 38),
    bg_hl: Color::Rgb(40, 52, 87),
};

const CATPPUCCIN: Theme = Theme {
    cyan: Color::Rgb(148, 226, 213),   // teal
    purple: Color::Rgb(203, 166, 247), // mauve
    yellow: Color::Rgb(249, 226, 175),
    red: Color::Rgb(243, 139, 168),
    blue: Color::Rgb(137, 180, 250),
    green: Color::Rgb(166, 227, 161),
    dim: Color::Rgb(108, 112, 134), // overlay1
    fg: Color::Rgb(205, 214, 244),  // text
    bg: Color::Rgb(30, 30, 46),     // base
    bg_hl: Color::Rgb(49, 50, 68),  // surface0
};

const DRACULA: Theme = Theme {
    cyan: Color::Rgb(139, 233, 253),
    purple: Color::Rgb(189, 147, 249),
    yellow: Color::Rgb(241, 250, 140),
    red: Color::Rgb(255, 85, 85),
    blue: Color::Rgb(139, 233, 253), // Dracula reuses cyan-ish for accent
    green: Color::Rgb(80, 250, 123),
    dim: Color::Rgb(98, 114, 164), // comment
    fg: Color::Rgb(248, 248, 242),
    bg: Color::Rgb(40, 42, 54),
    bg_hl: Color::Rgb(68, 71, 90), // current line
};

const THEMES: &[(&str, &Theme)] = &[
    ("tokyo-night", &TOKYO_NIGHT),
    ("catppuccin", &CATPPUCCIN),
    ("dracula", &DRACULA),
];

static CURRENT_THEME: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn theme() -> &'static Theme {
    let idx = CURRENT_THEME.load(std::sync::atomic::Ordering::Relaxed);
    THEMES[idx.min(THEMES.len() - 1)].1
}

/// Switch the active theme by name. Returns the matched canonical name on
/// success, `None` if `name` doesn't correspond to a known theme.
pub fn set_theme(name: &str) -> Option<&'static str> {
    let (idx, &(canonical, _)) = THEMES.iter().enumerate().find(|(_, (n, _))| *n == name)?;
    CURRENT_THEME.store(idx, std::sync::atomic::Ordering::Relaxed);
    Some(canonical)
}

pub fn theme_names() -> Vec<&'static str> {
    THEMES.iter().map(|(n, _)| *n).collect()
}

fn current_theme_name() -> &'static str {
    let idx = CURRENT_THEME.load(std::sync::atomic::Ordering::Relaxed);
    THEMES[idx.min(THEMES.len() - 1)].0
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuKind {
    /// Slash command list — selecting an item replaces the whole input
    /// with `/cmd ` (trailing space if the command takes an arg).
    Command,
    /// Alias name list — selecting an item replaces the arg portion of
    /// the input (everything after the first space).
    Alias,
    /// Theme name list — same arg-replacement semantics as Alias, just
    /// with its own title in the dropdown.
    Theme,
    /// Ex command list (Command mode) — selecting replaces command_buf
    /// with the chosen ex command + trailing space if it takes an arg.
    Ex,
    /// Ollama-installed model list — surfaced from agent.available_models();
    /// arg-replacement like Alias/Theme but with a distinct title.
    Model,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Menu {
    items: Vec<String>,
    selected: usize,
    kind: MenuKind,
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
    frame: usize, // monotonic tick driving the spinner animation
    tokens: TokenCounts,
    suggestion: Option<Suggestion>,
    menu: Option<Menu>,
    mode: Mode,
    command_buf: String,
    command_cursor: usize,
    /// Shell-style readline history: every non-empty submission, in order.
    /// Consecutive duplicates are deduplicated.
    input_history: Vec<String>,
    /// Index into `input_history` when the user is browsing past inputs via
    /// Up/Down. `None` means "not currently recalling".
    recall_idx: Option<usize>,
    /// Host this telia process is running on — surfaced in the log block
    /// title so remote sessions are visually distinct from local ones.
    hostname: String,
    /// Login name of the user driving the session — used as the header on
    /// their own transcript turns.
    username: String,
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
            frame: 0,
            tokens: TokenCounts::default(),
            suggestion: None,
            menu: None,
            mode: Mode::Insert,
            command_buf: String::new(),
            command_cursor: 0,
            input_history: Vec::new(),
            recall_idx: None,
            hostname: hostname(),
            username: username(),
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
        state.frame = state.frame.wrapping_add(1);
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
                    KeyCode::Esc if state.menu.take().is_none() => {
                        state.mode = Mode::Normal;
                    }
                    KeyCode::Esc => {}
                    KeyCode::Up if state.menu.is_some() => {
                        if let Some(m) = state.menu.as_mut() {
                            m.selected = m.selected.saturating_sub(1);
                        }
                    }
                    KeyCode::Down if state.menu.is_some() => {
                        if let Some(m) = state.menu.as_mut() {
                            if m.selected + 1 < m.items.len() {
                                m.selected += 1;
                            }
                        }
                    }
                    KeyCode::Up if recall_up_possible(state) => recall_up(state),
                    KeyCode::Down if state.recall_idx.is_some() => recall_down(state),
                    KeyCode::PageUp => {
                        state.scroll = state.scroll.saturating_add(5);
                    }
                    KeyCode::PageDown => {
                        state.scroll = state.scroll.saturating_sub(5);
                    }
                    KeyCode::Up => {
                        state.scroll = state.scroll.saturating_add(1);
                    }
                    KeyCode::Down => {
                        state.scroll = state.scroll.saturating_sub(1);
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
                        if accept_menu(state) {
                            // accepted from menu
                        } else if let Some(s) = state.suggestion.clone() {
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
                                    state.recall_idx = None;
                                }
                                'w' => {
                                    delete_word_before_cursor(state);
                                    state.recall_idx = None;
                                }
                                _ => {}
                            }
                        } else {
                            state.input.insert(state.input_cursor, c);
                            state.input_cursor += c.len_utf8();
                            state.recall_idx = None;
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
                        state.recall_idx = None;
                    }
                    KeyCode::Delete if state.input_cursor < state.input.len() => {
                        state.input.remove(state.input_cursor);
                        state.recall_idx = None;
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
                    // Jump scrollback to the latest entries (vim's G).
                    KeyCode::Char('G') => state.scroll = 0,
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
                KeyCode::Tab => {
                    // Accept the highlighted item from the ex dropdown.
                    accept_menu(state);
                }
                KeyCode::Up if state.menu.is_some() => {
                    if let Some(m) = state.menu.as_mut() {
                        m.selected = m.selected.saturating_sub(1);
                    }
                }
                KeyCode::Down if state.menu.is_some() => {
                    if let Some(m) = state.menu.as_mut() {
                        if m.selected + 1 < m.items.len() {
                            m.selected += 1;
                        }
                    }
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
        refresh_menu(state, agent);
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
    state.recall_idx = None;
    state.mode = Mode::Insert;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return;
    }
    // Record into readline-style history (dedupe consecutive repeats).
    if state.input_history.last().map(String::as_str) != Some(trimmed) {
        state.input_history.push(trimmed.to_string());
    }
    if let Some(cmd) = trimmed.strip_prefix('/') {
        handle_slash(state, agent, cmd);
        return;
    }
    state.push(Entry::User(trimmed.to_string()));
    state.working = true;
    run_turn(terminal, state, agent, trimmed.to_string()).await;
    state.working = false;
    state.tokens = agent.tokens();
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
        "theme" | "colorscheme" | "colo" => format!("theme {arg}"),
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
        "theme" => Some(" [NAME]"),
        _ => None,
    }
}

/// Compute the dropdown menu (if any) for the current input. Pure function
/// over `(input, aliases)` so the dispatch from `refresh_menu` can decide
/// when to hit the store.
fn compute_menu(input: &str, aliases: &[String], models: &[String]) -> Option<Menu> {
    let rest = input.strip_prefix('/')?;

    if let Some(space) = rest.find(' ') {
        let cmd = &rest[..space];
        let arg = rest[space + 1..].trim_start();
        if matches!(cmd, "load" | "delete" | "rm") {
            let items: Vec<String> = aliases
                .iter()
                .filter(|n| n.starts_with(arg))
                .cloned()
                .collect();
            if items.is_empty() {
                return None;
            }
            return Some(Menu {
                items,
                selected: 0,
                kind: MenuKind::Alias,
            });
        }
        if cmd == "theme" {
            let items: Vec<String> = theme_names()
                .into_iter()
                .filter(|n| n.starts_with(arg))
                .map(String::from)
                .collect();
            if items.is_empty() {
                return None;
            }
            return Some(Menu {
                items,
                selected: 0,
                kind: MenuKind::Theme,
            });
        }
        if cmd == "model" {
            let items: Vec<String> = models
                .iter()
                .filter(|n| n.starts_with(arg))
                .cloned()
                .collect();
            if items.is_empty() {
                return None;
            }
            return Some(Menu {
                items,
                selected: 0,
                kind: MenuKind::Model,
            });
        }
        return None;
    }

    let items: Vec<String> = SLASH_COMMANDS
        .iter()
        .filter(|c| c.starts_with(rest))
        .map(|s| s.to_string())
        .collect();
    if items.is_empty() {
        return None;
    }
    Some(Menu {
        items,
        selected: 0,
        kind: MenuKind::Command,
    })
}

/// Ex commands surfaced by the Command-mode dropdown. Canonical names
/// only; short aliases like `q`/`w` stay valid input but aren't listed
/// (they'd clutter the menu with ambiguous prefixes).
const EX_COMMANDS: &[&str] = &[
    "clear",
    "colorscheme",
    "delete",
    "edit",
    "help",
    "info",
    "list",
    "load",
    "model",
    "quit",
    "reset",
    "show",
    "theme",
    "write",
];

fn ex_arg_placeholder(cmd: &str) -> Option<&'static str> {
    match cmd {
        "write" | "load" | "edit" | "delete" => Some(" NAME"),
        "model" | "theme" | "colorscheme" => Some(" [NAME]"),
        _ => None,
    }
}

/// Command-mode dropdown: filters EX_COMMANDS by the command_buf prefix
/// up to the first space.
fn compute_ex_menu(command_buf: &str) -> Option<Menu> {
    if command_buf.contains(' ') {
        return None;
    }
    let items: Vec<String> = EX_COMMANDS
        .iter()
        .filter(|c| c.starts_with(command_buf))
        .map(|s| s.to_string())
        .collect();
    if items.is_empty() {
        return None;
    }
    Some(Menu {
        items,
        selected: 0,
        kind: MenuKind::Ex,
    })
}

/// Refresh state.menu based on current input, preserving the highlighted
/// selection when the new menu has the same kind and the index is still
/// in range. Only hits the store for the load/delete/rm arg case.
fn refresh_menu(state: &mut State, agent: &Agent) {
    let next = match state.mode {
        Mode::Insert => {
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
            compute_menu(&state.input, &aliases, agent.available_models())
        }
        Mode::Command => compute_ex_menu(&state.command_buf),
        Mode::Normal => None,
    };

    state.menu = match (state.menu.take(), next) {
        (Some(prev), Some(mut new)) if prev.kind == new.kind => {
            new.selected = prev.selected.min(new.items.len().saturating_sub(1));
            Some(new)
        }
        (_, next) => next,
    };
}

/// Apply a Tab/Enter acceptance of the current menu selection, mutating
/// state.input + cursor. Returns true if a selection was applied.
fn accept_menu(state: &mut State) -> bool {
    let menu = match state.menu.take() {
        Some(m) => m,
        None => return false,
    };
    let item = match menu.items.get(menu.selected) {
        Some(s) => s.clone(),
        None => return false,
    };
    match menu.kind {
        MenuKind::Command => {
            let trailing = if arg_placeholder(&item).is_some() {
                " "
            } else {
                ""
            };
            state.input = format!("/{item}{trailing}");
            state.input_cursor = state.input.len();
        }
        MenuKind::Alias | MenuKind::Theme | MenuKind::Model => {
            if let Some(space) = state.input.find(' ') {
                let cmd_prefix = state.input[..=space].to_string();
                state.input = format!("{cmd_prefix}{item}");
                state.input_cursor = state.input.len();
            }
        }
        MenuKind::Ex => {
            let trailing = if ex_arg_placeholder(&item).is_some() {
                " "
            } else {
                ""
            };
            state.command_buf = format!("{item}{trailing}");
            state.command_cursor = state.command_buf.len();
        }
    }
    true
}

/// Up is intercepted for input-history recall when the menu isn't already
/// claiming it. We start recalling if the input is empty (no draft to
/// lose), or continue going further back if we're already recalling.
fn recall_up_possible(state: &State) -> bool {
    if state.input_history.is_empty() {
        return false;
    }
    state.recall_idx.is_some() || state.input.is_empty()
}

/// Move one step further back in input history.
fn recall_up(state: &mut State) {
    let next = match state.recall_idx {
        Some(0) => return, // already at the oldest entry
        Some(i) => i - 1,
        None => state.input_history.len() - 1,
    };
    state.recall_idx = Some(next);
    state.input = state.input_history[next].clone();
    state.input_cursor = state.input.len();
}

/// Move forward toward the most recent input; from the newest entry, this
/// clears the input and exits recall mode.
fn recall_down(state: &mut State) {
    let Some(i) = state.recall_idx else {
        return;
    };
    if i + 1 < state.input_history.len() {
        let next = i + 1;
        state.recall_idx = Some(next);
        state.input = state.input_history[next].clone();
        state.input_cursor = state.input.len();
    } else {
        state.recall_idx = None;
        state.input.clear();
        state.input_cursor = 0;
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
                state.tokens = TokenCounts::default();
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
                    state.tokens = TokenCounts::default();
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
        "theme" => {
            if arg.is_empty() {
                let names = theme_names().join(", ");
                state.push(Entry::Info(format!(
                    "current theme: {}\navailable: {}",
                    current_theme_name(),
                    names
                )));
            } else if let Some(canonical) = set_theme(arg) {
                state.push(Entry::Info(format!("switched theme to {canonical}")));
            } else {
                state.push(Entry::Error(format!(
                    "unknown theme '{arg}'. try one of: {}",
                    theme_names().join(", ")
                )));
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
                "commands: /reset · /clear · /save NAME · /load NAME · /delete NAME · /list · /model [NAME] · /theme [NAME] · /show · /help · /quit"
                    .into(),
            ));
        }
        other => {
            state.push(Entry::Error(format!("unknown command: /{other}")));
        }
    }
}

fn draw(f: &mut ratatui::Frame, state: &State) {
    let th = theme();
    // Up to 6 menu items inline above the input. Includes 2 for the border.
    let menu_height: u16 = state
        .menu
        .as_ref()
        .map(|m| (m.items.len().min(6) as u16) + 2)
        .unwrap_or(0);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(menu_height),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(f.area());

    let frame = state.frame;
    let lines: Vec<Line> = if state.history.is_empty() {
        welcome_banner(chunks[0].width, frame)
    } else {
        state
            .history
            .iter()
            .flat_map(|e| render_entry(e, frame, &state.username))
            .collect()
    };
    let visible = chunks[0].height.saturating_sub(2) as usize;
    let total = lines.len();
    let max_offset = total.saturating_sub(visible) as u16;
    let offset = max_offset.saturating_sub(state.scroll);

    let log = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(th.dim))
                .title(Line::from(vec![
                    Span::styled(
                        " τέλεια ",
                        Style::default().fg(th.purple).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("@ ", Style::default().fg(th.dim)),
                    Span::styled(
                        format!("{} ", state.hostname),
                        Style::default().fg(th.cyan).add_modifier(Modifier::ITALIC),
                    ),
                ])),
        )
        .wrap(Wrap { trim: false })
        .scroll((offset, 0));
    f.render_widget(log, chunks[0]);

    // Render the menu (if any) directly above the input.
    if let Some(menu) = &state.menu {
        let title = match menu.kind {
            MenuKind::Command => " commands ",
            MenuKind::Alias => " aliases ",
            MenuKind::Theme => " themes ",
            MenuKind::Ex => " ex ",
            MenuKind::Model => " models ",
        };
        let items: Vec<ListItem> = menu
            .items
            .iter()
            .take(6)
            .map(|s| ListItem::new(s.clone()))
            .collect();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(th.dim))
                    .title(Span::styled(
                        title,
                        Style::default().fg(th.blue).add_modifier(Modifier::BOLD),
                    )),
            )
            .highlight_style(
                Style::default()
                    .fg(th.fg)
                    .bg(th.bg_hl)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("❯ ");
        let mut list_state = ListState::default();
        list_state.select(Some(menu.selected.min(menu.items.len().saturating_sub(1))));
        f.render_stateful_widget(list, chunks[1], &mut list_state);
    }

    // Pick prompt + active buffer per mode.
    let (prompt, prompt_color, buf, buf_cursor) = match state.mode {
        Mode::Insert => ("> ", th.cyan, &state.input, state.input_cursor),
        Mode::Normal => ("> ", th.green, &state.input, state.input_cursor),
        Mode::Command => (": ", th.yellow, &state.command_buf, state.command_cursor),
    };

    // Horizontally scroll so the cursor is always visible.
    let inside = chunks[2];
    let visible_width = inside.width.saturating_sub(4) as usize; // 2 borders + prefix (2)
    let cursor_chars = buf[..buf_cursor].chars().count();
    let start_char = cursor_chars.saturating_sub(visible_width.saturating_sub(1));
    let visible_text: String = buf.chars().skip(start_char).take(visible_width).collect();

    let body_style = if state.working {
        Style::default().fg(th.dim)
    } else {
        Style::default().fg(th.fg)
    };
    let mut spans = vec![
        Span::styled(prompt, Style::default().fg(prompt_color)),
        Span::styled(visible_text, body_style),
    ];
    // Ghost-text suggestion only in Insert when the cursor is at the end
    // and the menu isn't already showing the same matches.
    if state.mode == Mode::Insert && state.menu.is_none() && state.input_cursor == state.input.len()
    {
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
                spans.push(Span::styled(ghost, Style::default().fg(th.dim)));
            }
        }
    }
    let input_widget = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(prompt_color)),
    );
    f.render_widget(input_widget, chunks[2]);

    // ratatui hides the cursor by default; positioning it each frame makes
    // it visible at the edit point within the scrolled view.
    let cursor_x = inside.x + 1 /* border */ + 2 /* prefix */ + (cursor_chars - start_char) as u16;
    let cursor_y = inside.y + 1;
    f.set_cursor_position((cursor_x, cursor_y));

    let (mode_label, mode_color) = match state.mode {
        Mode::Insert => ("INS", th.cyan),
        Mode::Normal => ("NOR", th.green),
        Mode::Command => ("CMD", th.yellow),
    };
    // Chip-style mode badge: dark fg on the mode colour for contrast.
    let mut status_spans = vec![
        Span::styled(
            format!(" {mode_label} "),
            Style::default()
                .fg(th.bg)
                .bg(mode_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", Style::default().fg(th.dim)),
    ];
    if state.working {
        let frame = SPINNER[state.frame % SPINNER.len()];
        status_spans.push(Span::styled(frame, Style::default().fg(th.purple)));
        status_spans.push(Span::raw(" "));
        status_spans.push(Span::styled(
            "thinking",
            Style::default()
                .fg(th.purple)
                .add_modifier(Modifier::ITALIC),
        ));
        // Three dots, one bright at a time, sweeping left → right. Same
        // 3-column footprint at every frame so the segments after it
        // don't shift.
        let active = (state.frame / 5) % 3;
        for i in 0..3 {
            let style = if i == active {
                Style::default().fg(th.purple).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(th.dim)
            };
            status_spans.push(Span::styled(".", style));
        }
    } else {
        status_spans.push(Span::styled(
            &state.status,
            Style::default().fg(th.dim).add_modifier(Modifier::ITALIC),
        ));
    }
    status_spans.push(Span::styled(" · ", Style::default().fg(th.dim)));
    status_spans.push(Span::styled(
        short_model(&state.model),
        Style::default().fg(th.blue).add_modifier(Modifier::ITALIC),
    ));
    status_spans.push(Span::styled(" · ", Style::default().fg(th.dim)));
    status_spans.push(Span::styled(
        format!(
            "↑{} ↓{}",
            format_count(state.tokens.prompt),
            format_count(state.tokens.completion)
        ),
        Style::default().fg(th.yellow),
    ));
    status_spans.push(Span::raw("   "));
    status_spans.push(Span::styled(
        mode_hints(state.mode),
        Style::default().fg(th.dim),
    ));
    f.render_widget(Paragraph::new(Line::from(status_spans)), chunks[3]);
}

/// Compact token count: 0..999 as-is, 1k..999k as "1k"/"45k", 1M+ as "1.2M".
fn format_count(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{}k", n / 1_000)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Compact "hf.co/FoolDev/Thanatos-27B:Q4_K_M" → "Thanatos-27B" for status-
/// bar use: take the segment after the last '/', then drop everything from
/// the first ':'. Leaves short names like "llama3" alone.
fn short_model(model: &str) -> &str {
    let tail = model.rsplit('/').next().unwrap_or(model);
    tail.split(':').next().unwrap_or(tail)
}

/// Read the machine's hostname. Tries `$HOSTNAME` first (set by most shells),
/// falls back to `/etc/hostname`, and finally to "localhost". Returns a
/// trimmed string with no trailing newline.
fn hostname() -> String {
    if let Some(h) = std::env::var_os("HOSTNAME") {
        let s = h.to_string_lossy();
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "localhost".to_string()
}

/// Login name of the current user. Tries `$USER` first (set by login
/// shells), falls back to `$LOGNAME`, then "user".
fn username() -> String {
    for var in ["USER", "LOGNAME"] {
        if let Some(u) = std::env::var_os(var) {
            let s = u.to_string_lossy();
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    "user".to_string()
}

/// Empty-state welcome banner: an ASCII-art "TELEIA" logo that shimmers
/// between th.purple and th.blue per character, with a blinking red Greek
/// period below — same motif as the SVG banner in the README. Centred
/// horizontally to the available `width`.
fn welcome_banner(width: u16, frame: usize) -> Vec<Line<'static>> {
    let th = theme();
    // ΤΕΛΕΙΑ. Same ANSI-Shadow figlet shape as before; the only structural
    // difference from a Latin "TELEIA" is the third letter: capital lambda
    // (Λ) rendered as an A without its crossbar.
    const LOGO: &[&str] = &[
        "████████╗███████╗ █████╗ ███████╗██╗ █████╗ ",
        "╚══██╔══╝██╔════╝██╔══██╗██╔════╝██║██╔══██╗",
        "   ██║   █████╗  ██║  ██║█████╗  ██║███████║",
        "   ██║   ██╔══╝  ██║  ██║██╔══╝  ██║██╔══██║",
        "   ██║   ███████╗██║  ██║███████╗██║██║  ██║",
        "   ╚═╝   ╚══════╝╚═╝  ╚═╝╚══════╝╚═╝╚═╝  ╚═╝",
    ];
    let logo_width = LOGO[0].chars().count();
    let logo_pad = (width as usize).saturating_sub(logo_width) / 2;
    let logo_indent = " ".repeat(logo_pad);

    // Typewriter reveal: each event-loop tick uncovers another fraction of
    // the logo width. ≈ 50 frames × 50 ms = 2.5 s to fully type out. After
    // it completes the existing shimmer + dot/tagline take over.
    const REVEAL_FRAMES: usize = 50;
    let revealed = if frame >= REVEAL_FRAMES {
        logo_width
    } else {
        frame * logo_width / REVEAL_FRAMES
    };
    let done_revealing = revealed >= logo_width;

    let mut out = Vec::new();
    out.push(Line::from(""));
    out.push(Line::from(""));

    for row in LOGO {
        let mut spans: Vec<Span<'static>> = vec![Span::raw(logo_indent.clone())];
        for (i, c) in row.chars().enumerate().take(revealed) {
            // Per-character colour alternation; the phase shifts with frame
            // to give a slow shimmer reading left-to-right.
            let color = if (i + frame / 4).is_multiple_of(2) {
                th.purple
            } else {
                th.blue
            };
            spans.push(Span::styled(
                c.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        }
        // A leading caret rides the reveal edge so the type-on reads as
        // an active cursor sweeping across.
        if !done_revealing {
            spans.push(Span::styled(
                "▌",
                Style::default().fg(th.red).add_modifier(Modifier::BOLD),
            ));
        }
        out.push(Line::from(spans));
    }

    if !done_revealing {
        // Suppress the dot/tagline/hint until the reveal finishes — keeps
        // the bottom of the banner clean during the type-on.
        return out;
    }

    // Greek period (●) blinks under the end of the logo at ~2 Hz.
    let dot_visible = (frame / 10).is_multiple_of(2);
    let dot_col = logo_pad + logo_width.saturating_sub(2);
    if dot_visible && dot_col < width as usize {
        out.push(Line::from(vec![
            Span::raw(" ".repeat(dot_col)),
            Span::styled("●", Style::default().fg(th.red)),
        ]));
    } else {
        out.push(Line::from(""));
    }

    out.push(Line::from(""));

    let center = |s: &str, style: Style| -> Line<'static> {
        let pad = (width as usize).saturating_sub(s.chars().count()) / 2;
        Line::from(Span::styled(format!("{}{s}", " ".repeat(pad)), style))
    };
    out.push(center(
        "the distilled coding agent",
        Style::default().fg(th.cyan).add_modifier(Modifier::ITALIC),
    ));
    out.push(center(
        "τέλεια · full stop",
        Style::default().fg(th.dim).add_modifier(Modifier::ITALIC),
    ));
    out.push(Line::from(""));
    out.push(Line::from(""));
    out.push(center(
        "Type to start · /help for commands · esc for normal mode",
        Style::default().fg(th.dim),
    ));

    out
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

fn render_entry(entry: &Entry, frame: usize, username: &str) -> Vec<Line<'static>> {
    let th = theme();
    let mut out = Vec::new();
    match entry {
        Entry::User(text) => {
            out.push(Line::from(Span::styled(
                username.to_string(),
                Style::default().fg(th.cyan).add_modifier(Modifier::BOLD),
            )));
            for line in text.lines() {
                out.push(Line::from(line.to_string()));
            }
            out.push(Line::from(""));
        }
        Entry::Assistant { text, complete } => {
            // Blink "▌" while streaming. Frame ticks every ~50ms; /10 gives
            // ~2 Hz.
            let header = if *complete {
                "τέλεια"
            } else if (frame / 10).is_multiple_of(2) {
                "τέλεια ▌"
            } else {
                "τέλεια  "
            };
            out.push(Line::from(Span::styled(
                header,
                Style::default().fg(th.purple).add_modifier(Modifier::BOLD),
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
            // While the tool is running, cycle the spinner next to the gear
            // so the user can see it's still going.
            let marker = if *complete {
                "⚙".to_string()
            } else {
                format!("⚙ {}", SPINNER[(frame / 5) % SPINNER.len()])
            };
            out.push(Line::from(Span::styled(
                format!("{marker} {name}({args})"),
                Style::default().fg(th.yellow),
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
                        Style::default().fg(th.dim),
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
                Style::default().fg(th.red),
            )));
            out.push(Line::from(""));
        }
        Entry::Info(text) => {
            let mut first = true;
            for line in text.lines() {
                let prefix = if first { "· " } else { "  " };
                out.push(Line::from(Span::styled(
                    format!("{prefix}{line}"),
                    Style::default().fg(th.blue),
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

    #[test]
    fn menu_command_list_filters_by_prefix() {
        let m = compute_menu("/sa", &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Command);
        assert_eq!(m.items, vec!["save"]);
    }

    #[test]
    fn menu_command_list_returns_all_for_lone_slash() {
        let m = compute_menu("/", &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Command);
        assert_eq!(m.items.len(), SLASH_COMMANDS.len());
    }

    #[test]
    fn menu_command_list_none_for_unknown_prefix() {
        assert!(compute_menu("/zzz", &[], &[]).is_none());
    }

    #[test]
    fn menu_alias_filters_by_prefix() {
        let aliases = vec![
            "audit-pass-1".to_string(),
            "audit-pass-2".to_string(),
            "draft".to_string(),
        ];
        let m = compute_menu("/load aud", &aliases, &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Alias);
        assert_eq!(m.items, vec!["audit-pass-1", "audit-pass-2"]);
    }

    #[test]
    fn menu_alias_shows_all_on_empty_arg() {
        let aliases = vec!["foo".to_string(), "bar".to_string()];
        let m = compute_menu("/load ", &aliases, &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Alias);
        assert_eq!(m.items.len(), 2);
    }

    #[test]
    fn menu_alias_none_when_no_aliases_match() {
        let aliases = vec!["foo".to_string()];
        assert!(compute_menu("/load zzz", &aliases, &[]).is_none());
    }

    #[test]
    fn menu_none_for_non_alias_commands_with_space() {
        // /help takes no arg; once a space is typed, no menu.
        assert!(compute_menu("/help ", &[], &[]).is_none());
    }

    #[test]
    fn menu_none_for_empty_or_no_slash() {
        assert!(compute_menu("", &[], &[]).is_none());
        assert!(compute_menu("hello", &[], &[]).is_none());
    }

    #[test]
    fn format_count_keeps_small_numbers_literal() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(42), "42");
        assert_eq!(format_count(999), "999");
    }

    #[test]
    fn format_count_uses_k_suffix_under_a_million() {
        assert_eq!(format_count(1_000), "1k");
        assert_eq!(format_count(12_345), "12k");
        assert_eq!(format_count(999_999), "999k");
    }

    #[test]
    fn format_count_uses_decimal_m_at_a_million_plus() {
        assert_eq!(format_count(1_000_000), "1.0M");
        assert_eq!(format_count(2_500_000), "2.5M");
    }

    fn recall_state(history: &[&str]) -> State {
        let mut s = State::new("dummy-session-id", "dummy-model");
        s.input_history = history.iter().map(|x| x.to_string()).collect();
        s
    }

    #[test]
    fn recall_starts_at_most_recent_entry() {
        let mut s = recall_state(&["one", "two", "three"]);
        recall_up(&mut s);
        assert_eq!(s.input, "three");
        assert_eq!(s.recall_idx, Some(2));
    }

    #[test]
    fn recall_up_walks_back_then_stops_at_oldest() {
        let mut s = recall_state(&["one", "two", "three"]);
        recall_up(&mut s);
        recall_up(&mut s);
        recall_up(&mut s);
        assert_eq!(s.input, "one");
        assert_eq!(s.recall_idx, Some(0));
        // Already at oldest — no further movement
        recall_up(&mut s);
        assert_eq!(s.input, "one");
        assert_eq!(s.recall_idx, Some(0));
    }

    #[test]
    fn recall_down_steps_forward_then_clears() {
        let mut s = recall_state(&["one", "two"]);
        recall_up(&mut s); // "two"
        recall_up(&mut s); // "one"
        recall_down(&mut s);
        assert_eq!(s.input, "two");
        assert_eq!(s.recall_idx, Some(1));
        // Stepping past the newest entry clears the input and exits recall
        recall_down(&mut s);
        assert_eq!(s.input, "");
        assert_eq!(s.recall_idx, None);
    }

    #[test]
    fn recall_up_possible_requires_empty_input_or_existing_recall() {
        let mut s = recall_state(&["one"]);
        assert!(recall_up_possible(&s));
        s.input = "draft".into();
        assert!(!recall_up_possible(&s));
        // Once recalling, even non-empty input keeps Up active.
        s.input.clear();
        recall_up(&mut s);
        s.input = "modified".into();
        // Note: real code resets recall_idx on edits; here we just check the
        // predicate against state.
        s.recall_idx = Some(0);
        assert!(recall_up_possible(&s));
    }

    #[test]
    fn recall_up_possible_false_for_empty_history() {
        let s = recall_state(&[]);
        assert!(!recall_up_possible(&s));
    }

    #[test]
    fn set_theme_returns_canonical_for_known() {
        assert_eq!(set_theme("tokyo-night"), Some("tokyo-night"));
        assert_eq!(set_theme("catppuccin"), Some("catppuccin"));
        assert_eq!(set_theme("dracula"), Some("dracula"));
    }

    #[test]
    fn set_theme_returns_none_for_unknown() {
        assert_eq!(set_theme("solarized"), None);
        assert_eq!(set_theme(""), None);
    }

    #[test]
    fn theme_names_lists_all_three() {
        let names = theme_names();
        assert!(names.contains(&"tokyo-night"));
        assert!(names.contains(&"catppuccin"));
        assert!(names.contains(&"dracula"));
    }

    #[test]
    fn ex_translates_theme_command() {
        assert_eq!(translate_ex("theme").unwrap(), "theme ");
        assert_eq!(
            translate_ex("theme catppuccin").unwrap(),
            "theme catppuccin"
        );
        // vim "colo" / "colorscheme" alias map to the same slash command
        assert_eq!(translate_ex("colo dracula").unwrap(), "theme dracula");
        assert_eq!(
            translate_ex("colorscheme tokyo-night").unwrap(),
            "theme tokyo-night"
        );
    }

    #[test]
    fn menu_theme_filters_by_prefix() {
        let m = compute_menu("/theme dra", &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Theme);
        assert_eq!(m.items, vec!["dracula"]);
    }

    #[test]
    fn menu_theme_lists_all_when_arg_empty() {
        let m = compute_menu("/theme ", &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Theme);
        // Three themes today: tokyo-night, catppuccin, dracula
        assert_eq!(m.items.len(), 3);
    }

    #[test]
    fn ex_menu_filters_by_prefix() {
        let m = compute_ex_menu("th").unwrap();
        assert_eq!(m.kind, MenuKind::Ex);
        assert_eq!(m.items, vec!["theme"]);
    }

    #[test]
    fn ex_menu_lone_buf_returns_all() {
        let m = compute_ex_menu("").unwrap();
        assert_eq!(m.kind, MenuKind::Ex);
        assert_eq!(m.items.len(), EX_COMMANDS.len());
    }

    #[test]
    fn ex_menu_none_after_space() {
        // Once the command name is followed by anything, the menu hides
        // so it doesn't fight with arg entry.
        assert!(compute_ex_menu("theme dra").is_none());
        assert!(compute_ex_menu("q ").is_none());
    }

    #[test]
    fn menu_model_filters_by_prefix() {
        let models = vec![
            "llama3:latest".to_string(),
            "hf.co/FoolDev/Thanatos-27B:Q4_K_M".to_string(),
            "hf.co/FoolDev/Janus-35B:Q4_K_M".to_string(),
        ];
        let m = compute_menu("/model hf.co", &[], &models).unwrap();
        assert_eq!(m.kind, MenuKind::Model);
        assert_eq!(m.items.len(), 2);
    }

    #[test]
    fn menu_model_shows_all_on_empty_arg() {
        let models = vec!["a".to_string(), "b".to_string()];
        let m = compute_menu("/model ", &[], &models).unwrap();
        assert_eq!(m.kind, MenuKind::Model);
        assert_eq!(m.items.len(), 2);
    }

    #[test]
    fn menu_model_none_when_no_models_cached() {
        // Empty model list (Ollama unreachable at startup) → no menu.
        assert!(compute_menu("/model anything", &[], &[]).is_none());
    }
}
