use crate::highlight;
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::{pin_mut, StreamExt};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, List, ListItem, ListState, Padding, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
    Terminal,
};
use std::{io, time::Duration};
use teleia_agent::{Agent, PermissionMode, TokenCounts, ToolApproval, TurnEvent};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Hint text shown at the right of the status bar; varies by mode so the
/// keys advertised are actually useful in the current context.
fn mode_hints(mode: Mode) -> &'static str {
    match mode {
        Mode::Insert => "↵ send · esc normal · tab accept · /help",
        Mode::Normal => {
            "i insert · v/V visual · R replace · u undo · / search · n/N · : cmd · y yank"
        }
        Mode::Visual => "h j k l move · y yank · esc cancel",
        Mode::Command => "↵ run · esc cancel",
        Mode::Replace => "type overwrites · esc normal · ← back",
        Mode::Search => "↵ search · esc cancel",
    }
}

/// Slash commands in their canonical form (aliases like `/q`, `/rm`,
/// `/exit`, `/info` are accepted by `handle_slash` but not surfaced by
/// autocomplete to avoid suggesting ambiguous short prefixes).
const SLASH_COMMANDS: &[&str] = &[
    "ask",
    "auto",
    "autocompact",
    "build",
    "cd",
    "clear",
    "compact",
    "context",
    "copy",
    "delete",
    "effort",
    "entropy",
    "exit",
    "help",
    "key",
    "keys",
    "list",
    "load",
    "loop",
    "lsps",
    "mcps",
    "model",
    "notify",
    "plan",
    "prompt",
    "pwd",
    "quit",
    "reset",
    "save",
    "show",
    "theme",
    "thinking",
    "tools",
    "transparent",
    "update",
    "version",
];

/// Sync the permission-mode change across the agent + the State mirror
/// and surface a one-line confirmation in the chat log. Centralised so
/// the slash commands and Shift+Tab handler stay tight.
/// Persist a pref and surface a transcript error if the write fails, so a
/// setting that silently didn't save (a full or locked DB) is visible instead
/// of quietly reverting on the next launch.
fn set_pref_warn(state: &mut State, agent: &Agent, key: &str, value: &str) {
    if let Err(e) = agent.set_pref(key, value) {
        state.push(Entry::Error(format!("couldn't save '{key}': {e}")));
    }
}

fn set_mode(state: &mut State, agent: &mut Agent, mode: PermissionMode) {
    if state.permission_mode == mode {
        return;
    }
    agent.set_permission_mode(mode);
    state.permission_mode = mode;
    set_pref_warn(state, agent, "permission_mode", mode.label_canonical());
    let blurb = match mode {
        PermissionMode::Plan => {
            "plan mode — read/list/glob/grep run; write/edit/bash are blocked. Use Shift+Tab or /build to execute."
        }
        PermissionMode::Build => "build mode — every tool call prompts y/n/a.",
        PermissionMode::Auto => {
            "auto mode — tool calls run without asking. Shift+Tab or /plan to step back."
        }
    };
    state.push(Entry::Info(format!("{} · {blurb}", mode.label())));
}

/// Reusable prompt templates surfaced via `/prompt NAME`. Selecting one
/// drops the template text into the input box; the user then appends
/// their content and submits. Plain text only — no agent-state mutation,
/// no system-prompt swap.
const PROMPT_TEMPLATES: &[(&str, &str)] = &[
    (
        "review",
        "Review the following for bugs, edge cases, and style — call out concrete issues, not vibes.\n\n",
    ),
    (
        "debug",
        "Help me debug this. Trace through the logic, name what's wrong, and propose the smallest fix.\n\n",
    ),
    (
        "explain",
        "Explain this code: what it does, how it does it, and any non-obvious choices worth flagging.\n\n",
    ),
    (
        "refactor",
        "Refactor for clarity. Keep behaviour identical; prefer fewer abstractions over more.\n\n",
    ),
    (
        "test",
        "Write tests covering the golden path plus the edge cases that actually break in practice.\n\n",
    ),
    (
        "docs",
        "Write or update documentation for this. Concise — what it does and when to use it, not how.\n\n",
    ),
    (
        "security",
        "Audit for security issues — injection, auth, secrets handling, OWASP-style problems.\n\n",
    ),
    (
        "perf",
        "Where is time or memory spent? Identify the cheapest wins; ignore micro-optimisations.\n\n",
    ),
    (
        "commit",
        "Write a concise commit message for the diff below. Focus on the WHY, 1–2 sentences max.\n\n",
    ),
    (
        "plan",
        "Sketch a plan before writing any code. Numbered steps with a verifiable check after each.\n\n",
    ),
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
static TRANSPARENT_BG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SHOW_THINKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

static CUSTOM_THEME: std::sync::OnceLock<std::sync::RwLock<Theme>> = std::sync::OnceLock::new();
static USE_CUSTOM_THEME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn theme() -> Theme {
    if USE_CUSTOM_THEME.load(std::sync::atomic::Ordering::Relaxed) {
        if let Some(lock) = CUSTOM_THEME.get() {
            return *lock.read().unwrap();
        }
    }
    let idx = CURRENT_THEME.load(std::sync::atomic::Ordering::Relaxed);
    *THEMES[idx.min(THEMES.len() - 1)].1
}

pub fn set_custom_palette_from_json(json: &str) -> bool {
    match parse_hex_palette(json) {
        Some(t) => {
            let lock = CUSTOM_THEME.get_or_init(|| std::sync::RwLock::new(t));
            *lock.write().unwrap() = t;
            USE_CUSTOM_THEME.store(true, std::sync::atomic::Ordering::Relaxed);
            true
        }
        None => false,
    }
}

pub fn clear_custom_theme() {
    USE_CUSTOM_THEME.store(false, std::sync::atomic::Ordering::Relaxed);
}

fn parse_hex_palette(s: &str) -> Option<Theme> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let get = |k: &str| -> Option<Color> {
        let hex = v.get(k)?.as_str()?.trim_start_matches('#');
        if hex.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some(Color::Rgb(r, g, b))
    };
    Some(Theme {
        bg: get("bg")?,
        bg_hl: get("bg_hl")?,
        fg: get("fg")?,
        dim: get("dim")?,
        red: get("red")?,
        green: get("green")?,
        yellow: get("yellow")?,
        blue: get("blue")?,
        purple: get("purple")?,
        cyan: get("cyan")?,
    })
}

/// Theme background, but `Color::Reset` when the transparent toggle is on.
/// Reset cells inherit the host terminal's background — so terminal alpha
/// + compositor blur bleed through every frame the TUI paints.
fn paint_bg() -> Color {
    if TRANSPARENT_BG.load(std::sync::atomic::Ordering::Relaxed) {
        Color::Reset
    } else {
        theme().bg
    }
}

pub fn set_transparent(on: bool) {
    TRANSPARENT_BG.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn is_transparent() -> bool {
    TRANSPARENT_BG.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether the model's reasoning/"thinking" stream is rendered. Toggled by
/// `/thinking`, persisted as the `show_thinking` pref; defaults on.
fn show_thinking() -> bool {
    SHOW_THINKING.load(std::sync::atomic::Ordering::Relaxed)
}

fn set_show_thinking(on: bool) {
    SHOW_THINKING.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Switch the active theme by name. Returns the matched canonical name on
/// success, `None` if `name` doesn't correspond to a known theme.
pub fn set_theme(name: &str) -> Option<&'static str> {
    let (idx, &(canonical, _)) = THEMES.iter().enumerate().find(|(_, (n, _))| *n == name)?;
    CURRENT_THEME.store(idx, std::sync::atomic::Ordering::Relaxed);
    USE_CUSTOM_THEME.store(false, std::sync::atomic::Ordering::Relaxed);
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
    /// The model's reasoning/"thinking" stream, rendered dimmed and kept
    /// distinct from the answer. Present for reasoning models; a
    /// reasoning-only turn keeps this so it isn't blank.
    Reasoning {
        text: String,
        complete: bool,
    },
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

impl Entry {
    /// The text that `/`-search and `*`/`#` look through. Returns the
    /// primary content of each entry: the prompt text for `User`, the
    /// reply text for `Assistant`, the captured stdout for `Tool`, and
    /// the message for `Info` / `Error`. Tool args (usually JSON noise)
    /// are skipped — a user grepping the scrollback wants reply text,
    /// not raw argument blobs.
    fn searchable_text(&self) -> &str {
        match self {
            Entry::User(t) => t,
            Entry::Reasoning { text, .. } => text,
            Entry::Assistant { text, .. } => text,
            Entry::Tool { output, .. } => output,
            Entry::Info(t) | Entry::Error(t) => t,
        }
    }
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
    /// MCP server name list — surfaced after `/mcps enable ` or `/mcps
    /// disable `. Acceptance replaces only the trailing token (after the
    /// LAST space), unlike Alias/Theme/Model which replace everything
    /// after the first.
    McpServer,
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
    Visual,
    Command,
    /// Overwrite-as-you-type mode (vim's `R`, entered with `R` from
    /// Normal). Char keys replace the char at the cursor instead of
    /// inserting; cursor advances past it. At end-of-buffer the keypress
    /// appends (matching vim). Esc returns to Normal. Backspace just
    /// moves the cursor left — vim restores the original char, but we
    /// don't track the pre-edit buffer, so we don't pretend to.
    Replace,
    /// Vim's `/` prompt — type a pattern, Enter to run, Esc to cancel.
    /// On Enter, [`run_search`] populates `state.search_matches` and
    /// `state.search_pattern`, jumps to the first match, returns to
    /// Normal. `n` / `N` cycle through matches afterward.
    Search,
}

/// First half of a two-key Normal-mode sequence (`gg`, `dd`) or a
/// pending operator that consumes the next char (`r`). Set when the
/// leader key is pressed, consumed by the next keypress; if the second
/// key doesn't complete a known sequence the operator cancels and the
/// key is re-processed as if pressed fresh (matching vim).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOp {
    G,
    D,
    R,
}

/// One hit produced by a chat-scrollback search. Indexes into
/// `state.history` and tags the byte offset of the match start within
/// that entry's text. `len` is in bytes (the match equals the current
/// search pattern, so it's `pattern.len()` — kept here for symmetry
/// with potential future case-insensitive / regex matches).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SearchHit {
    entry_idx: usize,
    byte_offset: usize,
    len: usize,
}

/// Shape of a Visual-mode (or mouse-drag) selection. `Char` is vim's
/// `v` — first row from anchor to end-of-row, last row from start-of-row
/// to cursor, middle rows full-width. `Line` is vim's `V` — every
/// touched row taken full-width regardless of column. `Block` is vim's
/// `Ctrl-v` — the rectangle bounded by min/max col and min/max row of
/// anchor and cursor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SelectionKind {
    #[default]
    Char,
    Line,
    Block,
}

/// Text-style (not rectangular) drag-selection: anchor is where the mouse
/// went down, cursor is the latest drag position. Coordinates are raw
/// terminal cells; clamping to the chat-area happens at render/extract time.
#[derive(Debug, Clone, Copy)]
struct Selection {
    anchor: (u16, u16),
    cursor: (u16, u16),
    kind: SelectionKind,
}

impl Selection {
    fn new(col: u16, row: u16) -> Self {
        Self::with_kind(SelectionKind::Char, col, row)
    }
    fn with_kind(kind: SelectionKind, col: u16, row: u16) -> Self {
        Self {
            anchor: (col, row),
            cursor: (col, row),
            kind,
        }
    }
    /// Returns `(start, end)` in reading order — start <= end where the
    /// ordering is row-first, col-second.
    fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        let a = (self.anchor.1, self.anchor.0);
        let b = (self.cursor.1, self.cursor.0);
        let (s, e) = if a <= b { (a, b) } else { (b, a) };
        ((s.1, s.0), (e.1, e.0))
    }
    fn is_point(&self) -> bool {
        self.anchor == self.cursor
    }
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
    /// Host this teleia process is running on — surfaced in the log block
    /// title so remote sessions are visually distinct from local ones.
    hostname: String,
    /// Login name of the user driving the session — used as the header on
    /// their own transcript turns.
    username: String,
    /// Whether to fire a desktop notification after each chat turn ends.
    /// Toggled at runtime via `/notify`; defaults to on.
    notify: bool,
    /// Sticky auto-prompting. While on, after a chat turn that used a
    /// tool, `submit_input` re-submits "continue" and runs again — driving
    /// the task to completion on its own — until a turn finishes without
    /// touching a tool, the user interrupts, or the [`LOOP_MAX`] cap is hit.
    /// Toggled via `/entropy` (its UI name; this field + pref key stay
    /// `autoprompt`), persisted across launches; defaults off.
    autoprompt: bool,
    /// When a turn overflows the model's context window, automatically
    /// `/compact` (summarize + reseed a fresh session) and retry the turn
    /// once, instead of surfacing a dead-end error. Toggled via
    /// `/autocompact`, persisted across launches; defaults on.
    auto_compact: bool,
    /// Set by `run_turn`: did the turn just streamed dispatch at least one
    /// tool? The auto-prompt loop reads it right after to decide whether the
    /// agent still has work to do. Reset at the start of every turn.
    turn_used_tool: bool,
    /// Active mouse drag-selection, if any. Set on mouse-down inside the
    /// chat area, updated on drag, cleared on the next mouse-down (or Esc).
    /// On mouse-up the selected text is copied to the system clipboard and
    /// the highlight stays visible until the next interaction.
    selection: Option<Selection>,
    /// The chat-area `Rect` from the most recent `draw()`. Event handlers
    /// (which run between draws) read this to clamp selection coordinates
    /// and decide whether a mouse click landed inside the log.
    log_area: Rect,
    /// Pending tool call awaiting user permission. Set by the
    /// `ToolApprovalRequest` event; cleared when the user presses y/n/a
    /// (or Esc, treated as deny). While `Some`, `run_turn` stops polling
    /// the agent stream — the agent is blocked on the matching `.await`.
    pending_approval: Option<PendingApproval>,
    /// Pending API-key entry from a mid-session `/model` switch. While
    /// `Some`, the input box renders a hidden-input prompt; typed chars
    /// land in `buf` (echoed as `•`), Enter commits the key onto the
    /// agent, Esc cancels.
    pending_key_entry: Option<KeyEntry>,
    /// Armed by `/loop N PROMPT` (a sync slash command that can't run
    /// turns itself). `submit_input` takes it right after `handle_slash`
    /// and hands it to `run_loop`, which re-submits the prompt `count`
    /// times. Always `None` once a loop finishes or is stopped.
    pending_loop: Option<LoopSpec>,
    /// Armed by `/compact` (same sync-handler constraint as
    /// `pending_loop`). `submit_input` takes it and drives
    /// `run_compact`, which summarizes the session via the model.
    pending_compact: bool,
    /// Armed by `/model` (same sync-handler constraint). The async path
    /// takes it and re-detects the local `num_ctx` for the new model, since
    /// the compaction budget is per-model. See `refresh_local_budget`.
    pending_redetect: bool,
    /// Mirror of `agent.permission_mode()`. Kept in sync so the status
    /// bar + Shift+Tab cycle don't need to touch the agent for reads.
    permission_mode: PermissionMode,
    /// Mirror of `agent.reasoning_effort()` for the status bar. `Some` only
    /// when a tier is active; the top tier `leetcode` renders as an
    /// animated rainbow badge.
    reasoning_effort: Option<String>,
    /// Pre-formatted summary of MCP servers booted at startup. `None`
    /// when no `[mcps.NAME]` entries were configured. Rendered by `/mcps`.
    mcp_summary: Option<String>,
    /// Pre-formatted summary of LSP entries from config. `None` when no
    /// `[lsps.NAME]` entries were configured. Rendered by `/lsps`.
    lsp_summary: Option<String>,
    /// Cached update-check result from startup. `/update` re-displays
    /// it; on startup an Info entry is auto-pushed (and a desktop
    /// notification fires) if a newer release is available.
    update_check: Option<crate::update::UpdateCheck>,
    /// True when the chat should snap to the bottom whenever new content
    /// arrives. Flipped to `false` the moment the user scrolls up to
    /// re-read prior turns and back to `true` when they return to the
    /// bottom (G in Normal mode, or PageDown / wheel-down to scroll==0).
    /// Without this, every streamed delta would yank the user back down.
    follow_bottom: bool,
    /// Total rendered-line count from the previous frame. `draw()` uses
    /// the delta against the new count to bump `scroll` when not
    /// following — keeping the visible window pinned to the same
    /// content rather than letting new entries push it up.
    last_total_lines: usize,
    /// Leader key of a two-key Normal-mode sequence (`gg`, `dd`), cleared
    /// by the next keypress.
    pending_op: Option<PendingOp>,
    /// Persistent arboard handle. Holding it across copies keeps the
    /// X11/macOS/Windows selection owner alive so paste actually returns
    /// the bytes we set — dropping the handle right after `set_text` lets
    /// the data source die before the display server can serve a request,
    /// leaving the next paste empty or stale. Wayland routes through
    /// `wl-copy` instead, which forks a daemon that owns the selection.
    clipboard: Option<arboard::Clipboard>,
    /// Undo / redo stacks for the input buffer. Each entry is a
    /// `(input, cursor)` snapshot taken right before a key dispatch that
    /// changed it. `u` swaps the current state onto `input_redo` and
    /// pops `input_undo` back into the live input; `Ctrl-r` mirrors that
    /// the other direction. Capped at [`INPUT_UNDO_CAP`] so long
    /// editing sessions don't grow unbounded. Both are cleared on
    /// submit — once the prompt is sent there's nothing meaningful to
    /// undo into.
    input_undo: Vec<(String, usize)>,
    input_redo: Vec<(String, usize)>,
    /// One-shot flag set by the `u` and `Ctrl-r` handlers in Normal so
    /// the outer event loop's diff-based undo recorder doesn't capture
    /// the restored state as a fresh edit. Reset to false after each
    /// dispatch.
    skip_input_undo_record: bool,
    /// Live buffer for the `/` prompt while in [`Mode::Search`]. On
    /// Enter it's promoted to `search_pattern` and a fresh match scan
    /// runs; on Esc it's discarded and the previously-committed
    /// pattern (if any) stays in effect.
    search_buf: String,
    search_cursor: usize,
    /// Most recently committed search pattern. `None` clears all
    /// highlights. Updated by [`run_search`] (from `/`, `*`, or `#`)
    /// and read by the rendering pass to colour matched substrings.
    search_pattern: Option<String>,
    /// Match list populated from `search_pattern` at the time of the
    /// most recent search. Each entry is the index into
    /// `state.history` plus the byte offset within that entry's text
    /// where the match begins. Ordered by history index, then byte
    /// offset, so `n` / `N` walk it linearly.
    search_matches: Vec<SearchHit>,
    /// Cursor into `search_matches`. `None` means "no active match"
    /// (either no search ran or the search matched nothing). `n`
    /// advances; `N` decrements; both wrap around like vim's default
    /// `set wrapscan`.
    search_idx: Option<usize>,
}

/// Maximum number of undo snapshots kept on the input buffer. Vim's
/// default is unbounded; we cap because a chat session can drift for
/// hours and the input is bounded anyway by terminal width × however
/// many lines.
const INPUT_UNDO_CAP: usize = 200;

pub struct PendingApproval {
    pub name: String,
    pub arguments: String,
    pub responder: tokio::sync::oneshot::Sender<ToolApproval>,
}

/// A bounded `/loop`: re-submit `prompt` as a chat turn up to `count`
/// times. See [`run_loop`].
struct LoopSpec {
    count: usize,
    prompt: String,
}

/// Hard cap on `/loop` iterations. Keeps a fat-fingered count (or a
/// runaway in Auto mode) from spinning far longer than intended; a
/// larger requested count is clamped to this and the banner shows it.
const LOOP_MAX: usize = 100;

/// The prompt re-submitted each round of sticky auto-prompting
/// (`/entropy`). Generic on purpose — the agent decides from its own
/// prior work what "continue" means, per the auto-prompting-loop guideline
/// carried in the system prompt.
const AUTO_CONTINUE_PROMPT: &str = "continue";

/// Whether sticky auto-prompting should fire another `continue` turn: the
/// mode is armed, the last turn finished cleanly *and* touched a tool (did
/// work rather than just answering), and we're under [`LOOP_MAX`].
fn should_auto_continue(armed: bool, outcome: TurnOutcome, used_tool: bool, steps: usize) -> bool {
    armed && outcome == TurnOutcome::Completed && used_tool && steps < LOOP_MAX
}

pub struct KeyEntry {
    pub provider: String,
    pub env_var: String,
    pub buf: String,
    /// True when a key is already saved for this provider — Enter with
    /// an empty buffer keeps it; the prompt wording also changes to
    /// "(already set · type new to replace or Esc to keep)".
    pub existing: bool,
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
            notify: true,
            autoprompt: false,
            auto_compact: true,
            turn_used_tool: false,
            selection: None,
            log_area: Rect::default(),
            pending_approval: None,
            pending_key_entry: None,
            pending_loop: None,
            pending_compact: false,
            pending_redetect: false,
            permission_mode: PermissionMode::default(),
            reasoning_effort: None,
            mcp_summary: None,
            lsp_summary: None,
            update_check: None,
            follow_bottom: true,
            last_total_lines: 0,
            pending_op: None,
            clipboard: None,
            input_undo: Vec::new(),
            input_redo: Vec::new(),
            skip_input_undo_record: false,
            search_buf: String::new(),
            search_cursor: 0,
            search_pattern: None,
            search_matches: Vec::new(),
            search_idx: None,
        }
    }

    fn push(&mut self, e: Entry) {
        self.history.push(e);
        if self.follow_bottom {
            self.scroll = 0;
        }
        // When the user is scrolled up, `draw()` will bump scroll by
        // the new-line delta so the visible window stays pinned to the
        // same content they were reading.
    }

    /// Mark this turn's trailing streamed entries (reasoning / answer /
    /// tool) complete, and drop an empty answer bubble. Called at the end
    /// of a turn — normal completion (`AssistantEnd`) AND every interrupt /
    /// error stop — so a halted turn never leaves a header blinking or a
    /// tool spinner spinning forever, which falsely implies the model is
    /// still working. Stops at the first already-complete/other entry, so
    /// earlier turns are untouched.
    fn finalize_trailing_stream(&mut self) {
        for entry in self.history.iter_mut().rev() {
            match entry {
                Entry::Assistant { complete, .. }
                | Entry::Reasoning { complete, .. }
                | Entry::Tool { complete, .. }
                    if !*complete =>
                {
                    *complete = true;
                }
                _ => break,
            }
        }
        if let Some(Entry::Assistant { text, .. }) = self.history.last() {
            if text.is_empty() {
                self.history.pop();
            }
        }
    }

    fn apply(&mut self, evt: TurnEvent) {
        match evt {
            TurnEvent::AssistantStart => {
                // The answer/reasoning bubbles are created lazily by the
                // first delta, so a reasoning-first turn renders the
                // thinking above the answer (and an all-reasoning turn
                // never leaves a stray empty answer bubble).
            }
            TurnEvent::ReasoningDelta(text) => {
                if let Some(Entry::Reasoning {
                    text: t,
                    complete: false,
                }) = self.history.last_mut()
                {
                    t.push_str(&text);
                } else {
                    self.push(Entry::Reasoning {
                        text,
                        complete: false,
                    });
                }
                if self.follow_bottom {
                    self.scroll = 0;
                }
            }
            TurnEvent::AssistantDelta(text) => {
                if let Some(Entry::Assistant {
                    text: t,
                    complete: false,
                }) = self.history.last_mut()
                {
                    t.push_str(&text);
                    if self.follow_bottom {
                        self.scroll = 0;
                    }
                } else {
                    self.push(Entry::Assistant {
                        text,
                        complete: false,
                    });
                }
            }
            TurnEvent::AssistantEnd => {
                self.finalize_trailing_stream();
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
                    if self.follow_bottom {
                        self.scroll = 0;
                    }
                }
            }
            TurnEvent::Notice(msg) => {
                self.push(Entry::Info(format!("⚠ {msg}")));
            }
            TurnEvent::TurnEnd => {}
            TurnEvent::ToolApprovalRequest {
                name,
                arguments,
                responder,
            } => {
                self.pending_approval = Some(PendingApproval {
                    name,
                    arguments,
                    responder,
                });
                if self.follow_bottom {
                    self.scroll = 0;
                }
            }
        }
    }
}

pub async fn run(
    mut agent: Agent,
    mcp_summary: Option<String>,
    lsp_summary: Option<String>,
    update_check: Option<crate::update::UpdateCheck>,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Mouse capture forwards wheel events to teleia so the log can scroll
    // with the wheel. The cost is that drag-to-select in the terminal
    // stops working as-is — most emulators expose Shift+drag (or a
    // similar modifier) as "send to terminal, ignore app capture" so
    // selection is still reachable.
    // Bracketed paste turns Ctrl+Shift+V (and most emulator paste-binds)
    // into a single `Event::Paste(String)` instead of a stream of synthetic
    // keystrokes that terminals + tmux variously drop, garble, or interpret
    // as control codes. Without this, paste into the input box silently
    // does nothing in most modern terminals.
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = State::new(agent.session_id(), agent.model());
    state.permission_mode = agent.permission_mode();
    state.reasoning_effort = agent.reasoning_effort().map(String::from);
    state.mcp_summary = mcp_summary;
    state.lsp_summary = lsp_summary;
    state.update_check = update_check;
    // Restore readline history + notify preference from prior runs.
    state.input_history = agent.input_history(500);
    if let Some(v) = agent.get_pref("notify") {
        state.notify = v == "on";
    }
    if let Some(v) = agent.get_pref("autoprompt") {
        state.autoprompt = v == "on";
    }
    if let Some(v) = agent.get_pref("auto_compact") {
        state.auto_compact = v == "on";
    }
    if let Some(v) = agent.get_pref("show_thinking") {
        set_show_thinking(v == "on");
    }
    // Transparent background: stored pref wins; otherwise honour
    // `TELEIA_TRANSPARENT=1` so users can opt in via their shell env.
    if let Some(v) = agent.get_pref("transparent") {
        set_transparent(v == "on");
    } else if matches!(
        std::env::var("TELEIA_TRANSPARENT").as_deref(),
        Ok("1") | Ok("on") | Ok("true")
    ) {
        set_transparent(true);
    }
    // If the startup check found a newer release, surface it in the
    // chat log and fire a desktop notification on first paint. Clone
    // out of the cache first so the subsequent `state.push` mutation
    // doesn't conflict with the immutable borrow.
    let new_release = state
        .update_check
        .as_ref()
        .filter(|u| u.newer)
        .map(|u| (u.latest.clone(), u.current.clone(), u.url.clone()));
    if let Some((latest, current, url)) = new_release {
        let msg = format!(
            "update available: teleia v{latest} (you're on v{current})\n  {url}\n  upgrade in place: teleia --self-update"
        );
        state.push(Entry::Info(msg));
        if state.notify {
            notify_user(
                "τέλεια update available",
                &format!("v{current} → v{latest}"),
            );
        }
    }
    // Size the proactive-compaction budget to the local model's real num_ctx
    // (read from Ollama's /api/show) rather than a guessed default — the guess
    // is what wedges a constrained host mid-turn.
    refresh_local_budget(&mut state, &mut agent).await;

    let result = event_loop(&mut terminal, &mut state, &mut agent).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;
    result
}

/// Ctrl-C at the idle prompt: discard whatever is being typed and return
/// `false`; return `true` (quit) only when there is nothing to clear, so a
/// stray Ctrl-C can't nuke the session. Mirrors each mode's Esc-cancel.
/// (During a running turn, Ctrl-C is handled separately as a hard
/// interrupt.) The caller `continue`s, skipping the post-key menu refresh,
/// so the dropdown/ghost-text are cleared here.
fn ctrl_c_should_quit(state: &mut State) -> bool {
    state.menu = None;
    state.suggestion = None;
    match state.mode {
        Mode::Command => {
            state.mode = Mode::Normal;
            state.command_buf.clear();
            state.command_cursor = 0;
            false
        }
        Mode::Search => {
            state.mode = Mode::Normal;
            state.search_buf.clear();
            state.search_cursor = 0;
            false
        }
        _ if !state.input.is_empty() => {
            // Record undo first, so an accidental clear is recoverable with
            // `u` — same as Ctrl-u. (The caller `continue`s past the normal
            // edit recorder.)
            record_input_undo(state);
            state.input.clear();
            state.input_cursor = 0;
            state.recall_idx = None;
            false
        }
        _ => true,
    }
}

/// Snapshot the current input onto the undo stack (capped, clearing redo),
/// so a following `u` restores it. Mirrors the main edit recorder.
fn record_input_undo(state: &mut State) {
    state
        .input_undo
        .push((state.input.clone(), state.input_cursor));
    if state.input_undo.len() > INPUT_UNDO_CAP {
        state.input_undo.remove(0);
    }
    state.input_redo.clear();
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
        let key = match event::read()? {
            Event::Key(k) => k,
            Event::Mouse(m) => {
                handle_mouse(terminal, state, m)?;
                continue;
            }
            Event::Paste(s) => {
                insert_paste(state, &s);
                continue;
            }
            _ => continue,
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            if ctrl_c_should_quit(state) {
                return Ok(());
            }
            continue;
        }

        // Ctrl+T: flip the reasoning stream's visibility from any mode,
        // persisting it like `/thinking`. Display-only — the model keeps
        // reasoning (see `/effort`).
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('t')) {
            let new = !show_thinking();
            set_show_thinking(new);
            set_pref_warn(
                state,
                agent,
                "show_thinking",
                if new { "on" } else { "off" },
            );
            state.push(Entry::Info(if new {
                "thinking on — showing the model's reasoning".to_string()
            } else {
                "thinking off — hiding the model's reasoning".to_string()
            }));
            continue;
        }

        // Hidden-input API-key entry takes precedence over normal mode
        // dispatch — typed chars must go to the masked buffer, not the
        // input box, until the user commits with Enter or cancels.
        if state.pending_key_entry.is_some() {
            handle_key_entry(state, agent, key);
            continue;
        }

        // Snapshot the input buffer + cursor before dispatch so we can
        // tell — *after* dispatch — whether the keypress actually
        // mutated it. Used to feed the undo stack without sprinkling
        // snapshot calls across every mutation site. `u` / `Ctrl-r`
        // themselves set `state.skip_input_undo_record` for one
        // iteration so an undo step doesn't itself get recorded as a
        // new edit.
        let pre_input_snapshot = (state.input.clone(), state.input_cursor);

        match state.mode {
            Mode::Insert => {
                match key.code {
                    // Esc precedence: dismiss menu, then clear drag-select
                    // highlight, then fall back to switching to Normal mode.
                    KeyCode::Esc
                        if state.menu.take().is_none() && state.selection.take().is_none() =>
                    {
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
                    KeyCode::PageUp => scroll_up(state, 5),
                    KeyCode::PageDown => scroll_down(state, 5),
                    KeyCode::Up => scroll_up(state, 1),
                    KeyCode::Down => scroll_down(state, 1),
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
                    KeyCode::BackTab => {
                        set_mode(state, agent, state.permission_mode.next());
                    }
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

            Mode::Normal => 'normal: {
                // Two-key sequences (gg / dd). On a non-matching second key
                // the operator cancels and the key falls through to the main
                // match so it acts as if pressed fresh — matches vim.
                if let Some(op) = state.pending_op.take() {
                    let handled = match (op, key.code) {
                        (PendingOp::G, KeyCode::Char('g')) => {
                            state.scroll = u16::MAX;
                            state.follow_bottom = false;
                            true
                        }
                        (PendingOp::D, KeyCode::Char('d')) => {
                            state.input.clear();
                            state.input_cursor = 0;
                            true
                        }
                        // r<char> — replace the input char at the
                        // cursor. No-op at end-of-buffer or on an
                        // empty input, matching vim. Any non-Char key
                        // (Esc, arrows, etc.) cancels the operator and
                        // re-dispatches normally — `_ => false` below.
                        (PendingOp::R, KeyCode::Char(c)) => {
                            replace_char_at_cursor(state, c);
                            true
                        }
                        _ => false,
                    };
                    if handled {
                        break 'normal;
                    }
                }
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
                    // Word motion
                    KeyCode::Char('w') => {
                        state.input_cursor = word_forward(&state.input, state.input_cursor);
                    }
                    KeyCode::Char('b') => {
                        state.input_cursor = word_back(&state.input, state.input_cursor);
                    }
                    KeyCode::Char('e') => {
                        state.input_cursor = word_end(&state.input, state.input_cursor);
                    }
                    // Jump scrollback to the latest entries (vim's G).
                    KeyCode::Char('G') => {
                        state.scroll = 0;
                        state.follow_bottom = true;
                    }
                    // Half-page scroll (vim's Ctrl-U / Ctrl-D).
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        scroll_up(state, 12);
                    }
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        scroll_down(state, 12);
                    }
                    // Undo / redo for the input buffer. Diff-based —
                    // every keypress that mutated the input was already
                    // captured by the outer event loop; here we swap
                    // between input_undo and input_redo.
                    KeyCode::Char('u') => {
                        undo_input(state);
                    }
                    KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        redo_input(state);
                    }
                    // History scroll
                    KeyCode::Char('j') | KeyCode::Down | KeyCode::PageDown => {
                        scroll_down(state, if key.code == KeyCode::PageDown { 5 } else { 1 });
                    }
                    KeyCode::Char('k') | KeyCode::Up | KeyCode::PageUp => {
                        scroll_up(state, if key.code == KeyCode::PageUp { 5 } else { 1 });
                    }
                    // Editing
                    KeyCode::Char('x') if state.input_cursor < state.input.len() => {
                        state.input.remove(state.input_cursor);
                    }
                    KeyCode::Char('X') if state.input_cursor > 0 => {
                        let prev = state.input[..state.input_cursor]
                            .chars()
                            .next_back()
                            .expect("cursor > 0 implies at least one char before");
                        let new_cursor = state.input_cursor - prev.len_utf8();
                        state.input.remove(new_cursor);
                        state.input_cursor = new_cursor;
                    }
                    KeyCode::Char('D') => {
                        state.input.truncate(state.input_cursor);
                    }
                    // Two-key sequence leaders (gg, dd) and r<char>.
                    KeyCode::Char('g') => state.pending_op = Some(PendingOp::G),
                    KeyCode::Char('d') => state.pending_op = Some(PendingOp::D),
                    KeyCode::Char('r') => state.pending_op = Some(PendingOp::R),
                    // Enter Replace mode (overwrite-as-you-type).
                    KeyCode::Char('R') => state.mode = Mode::Replace,
                    // Enter Visual mode. `v` is charwise (vim default),
                    // `V` is linewise, `Ctrl-v` is blockwise. Anchor
                    // lands at the top-left of the visible chat area
                    // and hjkl drives the selection from there.
                    KeyCode::Char('v') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        enter_visual(state, SelectionKind::Char)
                    }
                    KeyCode::Char('V') => enter_visual(state, SelectionKind::Line),
                    KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        enter_visual(state, SelectionKind::Block)
                    }
                    // Yank the most recent assistant response — same as the
                    // `:yank` / `:copy` ex-commands. Visual mode has its
                    // own `y` for selection yank; Normal-mode `y` is the
                    // no-selection shortcut.
                    KeyCode::Char('y') => yank_last_assistant(state),
                    // Enter command mode
                    KeyCode::Char(':') => {
                        state.mode = Mode::Command;
                        state.command_buf.clear();
                        state.command_cursor = 0;
                    }
                    // Enter Search mode (vim `/`). Esc inside Search
                    // cancels; Enter runs the search and returns to
                    // Normal.
                    KeyCode::Char('/') => {
                        state.mode = Mode::Search;
                        state.search_buf.clear();
                        state.search_cursor = 0;
                    }
                    // Cycle search matches (`n` next, `N` previous).
                    KeyCode::Char('n')
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && state.search_pattern.is_some() =>
                    {
                        search_next(state);
                    }
                    KeyCode::Char('N') => search_prev(state),
                    // Word-under-input-cursor search. `*` runs forward
                    // from the first match; `#` from the last. Both
                    // pull the word from `state.input` because that's
                    // where Normal-mode cursor motion happens — vim
                    // pulls from the buffer, our buffer is the prompt.
                    KeyCode::Char('*') => {
                        if let Some(word) = word_under_cursor(&state.input, state.input_cursor) {
                            run_search(state, word);
                        } else {
                            state.status = "no word under cursor".into();
                        }
                    }
                    KeyCode::Char('#') => {
                        if let Some(word) = word_under_cursor(&state.input, state.input_cursor) {
                            run_search(state, word);
                            // # is "search backward" in vim — jump from
                            // the first match to the last so subsequent
                            // n walks backward through occurrences.
                            if !state.search_matches.is_empty() {
                                state.search_idx = Some(state.search_matches.len() - 1);
                                scroll_to_current_match(state);
                            }
                        } else {
                            state.status = "no word under cursor".into();
                        }
                    }
                    // Tab still accepts a suggestion (rare in Normal but harmless)
                    KeyCode::Tab => {
                        if let Some(s) = state.suggestion.clone() {
                            state.input.push_str(&s.completion);
                            state.input_cursor = state.input.len();
                        }
                    }
                    KeyCode::BackTab => {
                        set_mode(state, agent, state.permission_mode.next());
                    }
                    // Submit also works from Normal
                    KeyCode::Enter => {
                        submit_input(terminal, state, agent).await;
                    }
                    KeyCode::Esc => {}
                    _ => {}
                }
            }

            Mode::Visual => match key.code {
                KeyCode::Esc => exit_visual(state, /* keep_selection */ false),
                KeyCode::Char('h') | KeyCode::Left => move_visual_cursor(state, -1, 0),
                KeyCode::Char('l') | KeyCode::Right => move_visual_cursor(state, 1, 0),
                KeyCode::Char('k') | KeyCode::Up => move_visual_cursor(state, 0, -1),
                KeyCode::Char('j') | KeyCode::Down => move_visual_cursor(state, 0, 1),
                KeyCode::Char('0') | KeyCode::Home => snap_visual_cursor_x(state, true),
                KeyCode::Char('$') | KeyCode::End => snap_visual_cursor_x(state, false),
                KeyCode::Char('y') => {
                    if let Some(sel) = state.selection {
                        // `terminal.current_buffer_mut()` returns the back
                        // buffer that was just reset by `swap_buffers()` at
                        // the end of the previous `draw()` — empty cells.
                        // To read what's actually on screen we re-issue a
                        // draw and read from the `CompletedFrame.buffer`,
                        // which is the just-rendered front buffer.
                        let cf = terminal.draw(|f| draw(f, state))?;
                        let inner = selection_inner_area(state.log_area);
                        let text = extract_selection_text(cf.buffer, sel, inner);
                        if !text.is_empty() {
                            copy_to_clipboard(&text, state);
                        }
                    }
                    exit_visual(state, /* keep_selection */ true);
                }
                _ => {}
            },

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
                    // `:loop` arms pending_loop via handle_slash but the sync
                    // ex path can't drive it — do it here, mirroring how
                    // submit_input drives `/loop`. Taking it also guarantees
                    // no stale spec survives to hijack a later submission.
                    if let Some(spec) = state.pending_loop.take() {
                        run_loop(terminal, state, agent, spec).await;
                        state.tokens = agent.tokens();
                        state.status = format!(
                            "session {} · ready",
                            &agent.session_id()[..agent.session_id().len().min(12)]
                        );
                    }
                    // Same deal for `:compact`.
                    if std::mem::take(&mut state.pending_compact) {
                        run_compact(terminal, state, agent).await;
                        state.status = format!(
                            "session {} · ready",
                            &agent.session_id()[..agent.session_id().len().min(12)]
                        );
                    }
                    // And `:model`, which arms a budget re-detect.
                    if std::mem::take(&mut state.pending_redetect) {
                        refresh_local_budget(state, agent).await;
                    }
                }
                _ => {}
            },

            Mode::Replace => match key.code {
                KeyCode::Esc => state.mode = Mode::Normal,
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
                KeyCode::Up => scroll_up(state, 1),
                KeyCode::Down => scroll_down(state, 1),
                KeyCode::PageUp => scroll_up(state, 5),
                KeyCode::PageDown => scroll_down(state, 5),
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    overwrite_char_at_cursor(state, c);
                    state.recall_idx = None;
                }
                KeyCode::Backspace if state.input_cursor > 0 => {
                    // Vim restores the original char on backspace; we
                    // don't track the pre-edit buffer, so we just back
                    // the cursor — the user can re-type to overwrite.
                    if let Some((i, _)) =
                        state.input[..state.input_cursor].char_indices().next_back()
                    {
                        state.input_cursor = i;
                    }
                }
                KeyCode::Enter => {
                    submit_input(terminal, state, agent).await;
                }
                _ => {}
            },

            Mode::Search => match key.code {
                KeyCode::Esc => {
                    state.mode = Mode::Normal;
                    state.search_buf.clear();
                    state.search_cursor = 0;
                }
                KeyCode::Char(c) => {
                    state.search_buf.insert(state.search_cursor, c);
                    state.search_cursor += c.len_utf8();
                }
                KeyCode::Left => {
                    if let Some((i, _)) = state.search_buf[..state.search_cursor]
                        .char_indices()
                        .next_back()
                    {
                        state.search_cursor = i;
                    }
                }
                KeyCode::Right => {
                    if let Some(c) = state.search_buf[state.search_cursor..].chars().next() {
                        state.search_cursor += c.len_utf8();
                    }
                }
                KeyCode::Home => state.search_cursor = 0,
                KeyCode::End => state.search_cursor = state.search_buf.len(),
                KeyCode::Backspace if state.search_cursor > 0 => {
                    let prev = state.search_buf[..state.search_cursor]
                        .chars()
                        .next_back()
                        .expect("cursor > 0 implies at least one char before");
                    let new_cursor = state.search_cursor - prev.len_utf8();
                    state.search_buf.remove(new_cursor);
                    state.search_cursor = new_cursor;
                }
                KeyCode::Enter => {
                    let pattern = std::mem::take(&mut state.search_buf);
                    state.search_cursor = 0;
                    state.mode = Mode::Normal;
                    run_search(state, pattern);
                }
                _ => {}
            },
        }

        // Diff-based undo recorder: any keypress that mutated the input
        // buffer text between snapshot and now pushes the old snapshot
        // onto the undo stack and clears the redo stack. Comparison is
        // string-only — pure cursor motion (`h`, `l`, `w`, `0`, `$`,
        // etc.) doesn't get its own undo step, matching vim. `u` /
        // `Ctrl-r` set `skip_input_undo_record` so their own swap
        // doesn't become a recorded edit.
        if state.skip_input_undo_record {
            state.skip_input_undo_record = false;
        } else if state.input != pre_input_snapshot.0 {
            state.input_undo.push(pre_input_snapshot);
            if state.input_undo.len() > INPUT_UNDO_CAP {
                state.input_undo.remove(0);
            }
            state.input_redo.clear();
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
    // The input is gone — there's nothing meaningful for `u` to
    // restore into anymore. Clearing keeps a post-submit `u` from
    // silently re-populating the box with the prompt that just went
    // out (which would also reintroduce orphan ghost-text / menu
    // state). The Up/Down recall ring is the right way to get the
    // last prompt back.
    state.input_undo.clear();
    state.input_redo.clear();
    // The dropdown menu, ghost-text completion, and any drag-select
    // highlight all key off the now-empty input — clearing them
    // immediately keeps a stale frame from showing up between submit
    // and the next outer-loop refresh.
    state.menu = None;
    state.suggestion = None;
    state.selection = None;
    // Anchor the chat to its bottom and re-engage bottom-following so
    // the just-submitted prompt + streaming reply are guaranteed
    // visible. Without flipping `follow_bottom` back on, the smart-
    // scroll delta-bump in draw() would shove scroll right back up to
    // wherever the user *was* reading, hiding the new turn.
    state.scroll = 0;
    state.follow_bottom = true;
    state.last_total_lines = 0;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return;
    }
    // Record into readline-style history (dedupe consecutive repeats).
    if state.input_history.last().map(String::as_str) != Some(trimmed) {
        state.input_history.push(trimmed.to_string());
        agent.push_input_history(trimmed);
    }
    if let Some(cmd) = trimmed.strip_prefix('/') {
        handle_slash(state, agent, cmd);
        // `/loop`, `/compact`, `/model` arm pending flags but can't run async
        // work from the sync slash handler — drive them here. `/model`'s
        // re-detect is independent of the turn-drivers below, so drain it
        // first (it doesn't run a turn, so the chain still falls through).
        if std::mem::take(&mut state.pending_redetect) {
            refresh_local_budget(state, agent).await;
        }
        if let Some(spec) = state.pending_loop.take() {
            run_loop(terminal, state, agent, spec).await;
        } else if std::mem::take(&mut state.pending_compact) {
            run_compact(terminal, state, agent).await;
        } else {
            return;
        }
    } else {
        state.push(Entry::User(trimmed.to_string()));
        state.working = true;
        let mut outcome = run_turn_ac(terminal, state, agent, trimmed.to_string()).await;
        // Sticky auto-prompting: keep the agent going on its own. After a
        // turn that used a tool (did work rather than just answering),
        // re-submit "continue" and run again — stopping as soon as a turn
        // finishes without touching a tool, the user interrupts, or the
        // safety cap is hit.
        let mut steps = 0;
        while should_auto_continue(state.autoprompt, outcome, state.turn_used_tool, steps) {
            steps += 1;
            state.push(Entry::Info(format!("entropy {steps}")));
            state.push(Entry::User(AUTO_CONTINUE_PROMPT.to_string()));
            outcome = run_turn_ac(terminal, state, agent, AUTO_CONTINUE_PROMPT.to_string()).await;
        }
        state.working = false;
    }
    state.tokens = agent.tokens();
    state.status = format!(
        "session {} · ready",
        &agent.session_id()[..agent.session_id().len().min(12)]
    );

    if state.notify {
        let preview: String = state
            .history
            .iter()
            .filter_map(|e| match e {
                Entry::Assistant { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .next_back()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        let body = if preview.is_empty() {
            "turn complete".to_string()
        } else {
            preview
        };
        notify_user("τέλεια", &body);
    }
}

/// Best-effort desktop notification — Linux via `notify-send`, macOS
/// via `osascript`, Windows via PowerShell BurntToast (silently
/// skipped if PowerShell isn't on PATH). All errors swallowed — a
/// missing notification daemon shouldn't break the turn.
fn notify_user(title: &str, body: &str) {
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .arg("-a")
            .arg("τέλεια")
            .arg(title)
            .arg(body)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(target_os = "macos")]
    {
        // AppleScript display notification "<body>" with title "<title>"
        let script = format!(
            "display notification {} with title {}",
            applescript_str(body),
            applescript_str(title)
        );
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(target_os = "windows")]
    {
        // PowerShell + BurntToast if available; user has to install it.
        // Falls back to a no-op when the module isn't present.
        let ps = format!(
            "Import-Module BurntToast -ErrorAction SilentlyContinue; \
             New-BurntToastNotification -Text '{}','{}' -ErrorAction SilentlyContinue",
            title.replace('\'', "''"),
            body.replace('\'', "''"),
        );
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (title, body);
    }
}

#[cfg(target_os = "macos")]
fn applescript_str(s: &str) -> String {
    // AppleScript string literal: double-quoted, backslash-escape both " and \.
    let esc = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{esc}\"")
}

/// Translate a vim-style ex command (without the leading ":") into the
/// equivalent slash command, then dispatch through `handle_slash`. A
/// few vim-natives (`:cd`, `:pwd`, `:noh`, `:!CMD`, `:version`) are
/// handled inline because they have no slash equivalent.
fn execute_ex(state: &mut State, agent: &mut Agent, cmd: &str) {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return;
    }

    // :!CMD  — shell out without a tool round-trip. Inheriting nothing
    // here; subprocess gets stdin /dev/null so a runaway can't lock the
    // TUI. Combined stdout/stderr piped back as an Info entry.
    if let Some(rest) = cmd.strip_prefix('!') {
        run_inline_shell(state, rest);
        return;
    }

    let mut parts = cmd.splitn(2, char::is_whitespace);
    let raw_name = parts.next().unwrap_or("");
    let name = raw_name.strip_suffix('!').unwrap_or(raw_name);
    let arg = parts.next().unwrap_or("").trim();
    match name {
        "noh" | "nohlsearch" => {
            // Clear both the drag-select highlight and the active
            // search-result highlight, mirroring vim's `:noh` which
            // hides hlsearch matches without forgetting the pattern.
            // We also drop the pattern itself — there's no `:set
            // hlsearch` knob to flip it back on, so a fresh `/` is
            // how you re-highlight.
            state.selection = None;
            clear_search(state);
        }
        "r" | "read" => {
            if arg.is_empty() {
                state.push(Entry::Error("usage: :read FILE".into()));
            } else {
                match std::fs::read_to_string(arg) {
                    Ok(body) => state.push(Entry::Info(format!("{arg}:\n{body}"))),
                    Err(e) => state.push(Entry::Error(format!("read {arg}: {e}"))),
                }
            }
        }
        _ => match translate_ex(cmd) {
            Ok(slash) => handle_slash(state, agent, &slash),
            Err(msg) => state.push(Entry::Error(msg)),
        },
    }
}

/// Run `cmd` via `/bin/sh -c` and push the combined stdout/stderr to
/// the chat. Same 30s timeout the `bash` tool uses; stdin is muted so
/// an interactive program can't lock the TUI.
fn run_inline_shell(state: &mut State, cmd: &str) {
    use std::process::{Command, Stdio};
    let out = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    match out {
        Ok(o) => {
            let mut body = String::from_utf8_lossy(&o.stdout).into_owned();
            if !o.stderr.is_empty() {
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(&String::from_utf8_lossy(&o.stderr));
            }
            if !o.status.success() {
                body.push_str(&format!("\n[exit {}]", o.status.code().unwrap_or(-1)));
            }
            let body = if body.is_empty() {
                "(no output)".to_string()
            } else {
                body
            };
            state.push(Entry::Info(format!("$ {cmd}\n{body}")));
        }
        Err(e) => state.push(Entry::Error(format!("!{cmd}: {e}"))),
    }
}

/// Pure translation from ex-style ":<name> [arg]" to the matching slash
/// command string ("save NAME", "quit", etc.). Returns Err with an
/// already-formatted message for unknown names.
fn translate_ex(cmd: &str) -> Result<String, String> {
    let mut parts = cmd.splitn(2, char::is_whitespace);
    let raw_name = parts.next().unwrap_or("");
    // Trailing `!` is vim's "force" suffix (`:q!`, `:wq!`, `:x!`). teleia has
    // no notion of "unsaved buffer" so the bang is purely cosmetic — strip it
    // and dispatch on the bare name.
    let name = raw_name.strip_suffix('!').unwrap_or(raw_name);
    let arg = parts.next().unwrap_or("").trim();
    let translated = match name {
        "q" | "quit" | "qa" | "qall" | "x" | "exit" | "clo" | "close" => "quit".to_string(),
        "w" | "write" | "wa" | "wall" | "sav" | "saveas" => format!("save {arg}"),
        "wq" => format!("save {arg}"), // close-enough analogue: save, no quit
        "e" | "edit" | "l" | "load" => format!("load {arg}"),
        "d" | "bd" | "delete" | "bw" | "bwipe" | "bwipeout" | "tabc" | "tabclose" => {
            format!("delete {arg}")
        }
        "ls" | "list" => "list".to_string(),
        "model" => format!("model {arg}"),
        "help" | "h" => "help".to_string(),
        "reset" | "enew" | "new" | "tabnew" | "tabe" | "tabedit" | "vnew" | "vne" => {
            "reset".to_string()
        }
        "clear" => "clear".to_string(),
        "show" | "info" | "f" | "file" | "mes" | "messages" => "show".to_string(),
        "theme" | "colorscheme" | "colo" => format!("theme {arg}"),
        "notify" | "notifications" => format!("notify {arg}"),
        "transparent" | "transparency" | "transp" => format!("transparent {arg}"),
        "entropy" => format!("entropy {arg}"),
        "thinking" => format!("thinking {arg}"),
        "autocompact" => format!("autocompact {arg}"),
        "cd" | "lcd" | "tcd" => format!("cd {arg}"),
        "pwd" => "pwd".to_string(),
        "version" => "version".to_string(),
        "tools" => "tools".to_string(),
        "context" => "context".to_string(),
        "compact" => "compact".to_string(),
        "copy" | "yank" | "y" => "copy".to_string(),
        "keys" => "keys".to_string(),
        "key" => format!("key {arg}"),
        "mcps" => format!("mcps {arg}"),
        "lsps" => "lsps".to_string(),
        "auto" => "auto".to_string(),
        "build" | "ask" => "build".to_string(),
        "plan" => "plan".to_string(),
        "prompt" => format!("prompt {arg}"),
        "effort" => format!("effort {arg}"),
        "loop" => format!("loop {arg}"),
        other => return Err(format!("unknown ex command: :{other}")),
    };
    Ok(translated)
}

/// Parse a `/loop` argument into `(count, prompt)`. The first
/// whitespace-delimited token is the iteration count (1..=[`LOOP_MAX`],
/// clamped); everything after it is the prompt re-submitted each round.
/// Returns a ready-to-show usage message on bad input.
fn parse_loop(arg: &str) -> Result<(usize, String), String> {
    const USAGE: &str = "usage: /loop N PROMPT  (N = 1–100 times, then the prompt to repeat)";
    let mut parts = arg.splitn(2, char::is_whitespace);
    let count_tok = parts.next().unwrap_or("").trim();
    let prompt = parts.next().unwrap_or("").trim();
    let count: usize = count_tok.parse().map_err(|_| USAGE.to_string())?;
    if count == 0 {
        return Err(USAGE.to_string());
    }
    if prompt.is_empty() {
        return Err(USAGE.to_string());
    }
    Ok((count.min(LOOP_MAX), prompt.to_string()))
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

/// Values offered by `/effort` completion: `off` plus the settable
/// reasoning tiers. Single source of truth: `teleia_agent::REASONING_EFFORTS`.
fn effort_values() -> Vec<&'static str> {
    std::iter::once("off")
        .chain(teleia_agent::REASONING_EFFORTS.iter().copied())
        .collect()
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
        "effort" => {
            let value = effort_values()
                .into_iter()
                .find(|v| v.starts_with(arg) && *v != arg)?;
            Some(Suggestion {
                completion: value[arg.len()..].to_string(),
                placeholder: String::new(),
            })
        }
        "thinking" | "autocompact" => {
            let value = ["on", "off"]
                .into_iter()
                .find(|v| v.starts_with(arg) && *v != arg)?;
            Some(Suggestion {
                completion: value[arg.len()..].to_string(),
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
        "notify" | "transparent" | "entropy" | "thinking" | "autocompact" => Some(" [on|off]"),
        "prompt" => Some(" [NAME]"),
        "key" => Some(" PROVIDER"),
        "cd" => Some(" PATH"),
        "mcps" => Some(" [enable|disable NAME]"),
        "effort" => Some(" [off|low|medium|high|xhigh|max|leetcode]"),
        "context" => Some(" [N|off]"),
        "loop" => Some(" N PROMPT"),
        _ => None,
    }
}

/// Compute the dropdown menu (if any) for the current input. Pure function
/// over `(input, aliases, models, mcp_servers)` so the dispatch from
/// `refresh_menu` can decide when to hit the store.
fn compute_menu(
    input: &str,
    aliases: &[String],
    models: &[String],
    mcp_servers: &[String],
) -> Option<Menu> {
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
        if cmd == "prompt" {
            let items: Vec<String> = PROMPT_TEMPLATES
                .iter()
                .filter(|(n, _)| n.starts_with(arg))
                .map(|(n, _)| (*n).to_string())
                .collect();
            if items.is_empty() {
                return None;
            }
            return Some(Menu {
                items,
                selected: 0,
                kind: MenuKind::Theme, // reuse arg-replacement semantics
            });
        }
        if cmd == "key" {
            // Provider names are TitleCase ("OpenAI", "OpenRouter") but
            // resolution elsewhere in teleia is case-insensitive, so the
            // dropdown matches that — `/key open` → both providers.
            let arg_lc = arg.to_ascii_lowercase();
            let items: Vec<String> = teleia_llm::PROVIDERS
                .iter()
                .map(|p| p.name.to_string())
                .filter(|n| n.to_ascii_lowercase().starts_with(&arg_lc))
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
        if matches!(
            cmd,
            "notify" | "transparent" | "entropy" | "thinking" | "autocompact"
        ) {
            let items: Vec<String> = ["on", "off"]
                .iter()
                .filter(|n| n.starts_with(arg))
                .map(|s| (*s).to_string())
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
        if cmd == "effort" {
            let items: Vec<String> = effort_values()
                .into_iter()
                .filter(|v| v.starts_with(arg))
                .map(String::from)
                .collect();
            if items.is_empty() {
                return None;
            }
            return Some(Menu {
                items,
                selected: 0,
                kind: MenuKind::Theme, // single-arg replacement
            });
        }
        if cmd == "mcps" {
            // Two-level: first token is action (enable/disable), second
            // is server name. `arg` is everything after `/mcps `.
            let mut sub = arg.splitn(2, char::is_whitespace);
            let action = sub.next().unwrap_or("");
            let rest_after_action = sub.next();
            match rest_after_action {
                None => {
                    // No second token yet — offer enable/disable filtered by
                    // the partial action.
                    let items: Vec<String> = ["enable", "disable"]
                        .iter()
                        .filter(|n| n.starts_with(action))
                        .map(|s| (*s).to_string())
                        .collect();
                    if items.is_empty() {
                        return None;
                    }
                    return Some(Menu {
                        items,
                        selected: 0,
                        kind: MenuKind::Theme, // single-arg replacement
                    });
                }
                Some(partial) if matches!(action, "enable" | "disable") => {
                    let partial = partial.trim_start();
                    let items: Vec<String> = mcp_servers
                        .iter()
                        .filter(|n| n.starts_with(partial))
                        .cloned()
                        .collect();
                    if items.is_empty() {
                        return None;
                    }
                    return Some(Menu {
                        items,
                        selected: 0,
                        kind: MenuKind::McpServer,
                    });
                }
                Some(_) => return None,
            }
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
    "auto",
    "autocompact",
    "build",
    "cd",
    "clear",
    "colorscheme",
    "compact",
    "context",
    "copy",
    "delete",
    "edit",
    "effort",
    "entropy",
    "exit",
    "file",
    "help",
    "info",
    "key",
    "keys",
    "list",
    "load",
    "loop",
    "lsps",
    "mcps",
    "model",
    "noh",
    "notify",
    "plan",
    "prompt",
    "pwd",
    "quit",
    "read",
    "reset",
    "show",
    "theme",
    "thinking",
    "tools",
    "transparent",
    "version",
    "write",
];

fn ex_arg_placeholder(cmd: &str) -> Option<&'static str> {
    match cmd {
        "write" | "load" | "edit" | "delete" => Some(" NAME"),
        "model" | "theme" | "colorscheme" => Some(" [NAME]"),
        "notify" | "transparent" | "entropy" | "thinking" | "autocompact" => Some(" [on|off]"),
        "mcps" => Some(" [enable|disable NAME]"),
        "effort" => Some(" [off|low|medium|high|xhigh|max|leetcode]"),
        "context" => Some(" [N|off]"),
        "loop" => Some(" N PROMPT"),
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
            let mcp_servers = agent.mcp_server_names();
            compute_menu(
                &state.input,
                &aliases,
                agent.available_models(),
                &mcp_servers,
            )
        }
        Mode::Command => compute_ex_menu(&state.command_buf),
        Mode::Normal | Mode::Visual | Mode::Replace | Mode::Search => None,
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
        MenuKind::McpServer => {
            // Replace only the trailing token (after the LAST space) so
            // `/mcps enable g` → `/mcps enable github`, not `/mcps github`.
            if let Some(space) = state.input.rfind(' ') {
                let prefix = state.input[..=space].to_string();
                state.input = format!("{prefix}{item}");
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
/// Scroll the chat up by `n` rows and disengage bottom-following so
/// streaming deltas don't yank the user back down mid-read.
fn scroll_up(state: &mut State, n: u16) {
    state.scroll = state.scroll.saturating_add(n);
    state.follow_bottom = false;
}

/// Scroll the chat down by `n` rows. Re-engages bottom-following the
/// instant the user reaches scroll == 0 so subsequent deltas auto-follow
/// again — no manual `G` needed.
fn scroll_down(state: &mut State, n: u16) {
    state.scroll = state.scroll.saturating_sub(n);
    if state.scroll == 0 {
        state.follow_bottom = true;
    }
}

/// Vim `w`: start of next whitespace-separated word (or end of input).
fn word_forward(s: &str, cursor: usize) -> usize {
    let suffix = &s[cursor..];
    let mut iter = suffix.char_indices().peekable();
    while iter.peek().is_some_and(|&(_, c)| !c.is_whitespace()) {
        iter.next();
    }
    while iter.peek().is_some_and(|&(_, c)| c.is_whitespace()) {
        iter.next();
    }
    cursor + iter.peek().map_or(suffix.len(), |&(i, _)| i)
}

/// Vim `b`: start of the current word, or of the previous word if already
/// at a word start / on whitespace.
fn word_back(s: &str, cursor: usize) -> usize {
    let prefix = &s[..cursor];
    let chars: Vec<(usize, char)> = prefix.char_indices().collect();
    if chars.is_empty() {
        return 0;
    }
    let mut idx = chars.len() - 1;
    while idx > 0 && chars[idx].1.is_whitespace() {
        idx -= 1;
    }
    while idx > 0 && !chars[idx - 1].1.is_whitespace() {
        idx -= 1;
    }
    chars[idx].0
}

/// Vim `e`: end of the current word, or of the next word if already at a
/// word end / on whitespace.
fn word_end(s: &str, cursor: usize) -> usize {
    if cursor >= s.len() {
        return cursor;
    }
    let suffix = &s[cursor..];
    let mut iter = suffix.char_indices().peekable();
    iter.next();
    while iter.peek().is_some_and(|&(_, c)| c.is_whitespace()) {
        iter.next();
    }
    let mut last = iter.peek().map_or(suffix.len(), |&(i, _)| i);
    while let Some(&(i, c)) = iter.peek() {
        if c.is_whitespace() {
            break;
        }
        last = i;
        iter.next();
    }
    cursor + last
}

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

/// Outcome of a single `run_turn`. `/loop` reads this to decide whether
/// to keep iterating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnOutcome {
    /// Reached `TurnEnd` (or the stream ended) on its own.
    Completed,
    /// Ended early — user interrupt (Esc / Ctrl-C) or a draw/stream
    /// error. `/loop` stops on this.
    Stopped,
    /// The request overflowed the model's context window. Internal signal
    /// consumed by [`run_turn_ac`], which auto-compacts + retries; callers
    /// that go through the wrapper never observe it.
    ContextOverflow,
}

/// Read the local model's real `num_ctx` (Ollama `/api/show`) and make it the
/// proactive-compaction budget, best-effort. Announces it only when it's the
/// active budget (the user hasn't pinned one with `/context`). Called at
/// startup and after a `/model` switch, since the window is per-model. A
/// non-Ollama backend or any failure clears the detected value and falls back
/// to the default budget.
async fn refresh_local_budget(state: &mut State, agent: &mut Agent) {
    if let Some(n) = agent.detect_context_window().await {
        if agent.get_pref("context_limit").is_none() {
            state.push(Entry::Info(format!(
                "context budget set to {n} tok (local model num_ctx) — /context N to override"
            )));
        }
    }
}

/// Heuristic: does this stream error look like a local backend crashing or
/// timing out under an oversized prompt (Ollama 5xx / runner death), rather
/// than a clean client error (auth, 400)? Used only to attach a
/// non-destructive recovery hint — never to trigger auto-compaction, so a
/// false positive costs at most one stray hint line.
fn looks_like_backend_crash(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("returned 5") // "backend returned 5xx: ..."
        || m.contains("runner")
        || m.contains("terminated")
        || m.contains("timed out")
        || m.contains("timeout")
}

async fn run_turn<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut State,
    agent: &mut Agent,
    input: String,
) -> TurnOutcome {
    // Reset the per-turn tool marker; the auto-prompt loop reads it after
    // this turn returns to decide whether the agent still has work to do.
    state.turn_used_tool = false;
    let stream = agent.turn(input);
    pin_mut!(stream);
    loop {
        // Tick the global frame counter so the status-bar spinner,
        // running-dot ellipsis, and any other frame-driven animations
        // keep moving while a turn is in flight. The outer event loop
        // is parked awaiting this function, so without bumping here
        // every animation would freeze for the duration of the turn.
        state.frame = state.frame.wrapping_add(1);

        if let Err(e) = terminal.draw(|f| draw(f, state)) {
            state.push(Entry::Error(format!("draw: {e:#}")));
            return TurnOutcome::Stopped;
        }

        // Drain whatever input events have piled up — without this, the
        // outer event loop is parked awaiting this function and the TUI
        // looks frozen: mouse wheel does nothing, Ctrl-C does nothing,
        // the terminal cursor sits stuck on the input. Non-blocking,
        // so a fast turn pays essentially nothing for this.
        while event::poll(Duration::from_millis(0)).unwrap_or(false) {
            let Ok(evt) = event::read() else { break };
            match evt {
                Event::Key(k) if k.kind == KeyEventKind::Release => {}
                // Ctrl-C is always a hard interrupt, even mid-approval.
                Event::Key(k)
                    if k.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(k.code, KeyCode::Char('c')) =>
                {
                    // Clear any outstanding tool-approval prompt before
                    // bailing. Otherwise the flag stays set, draw() keeps
                    // the approval prompt commandeering the input box, and
                    // it's wedged for the rest of the session. Dropping the
                    // responder denies the abandoned tool call.
                    state.pending_approval = None;
                    state.finalize_trailing_stream();
                    state.push(Entry::Info("(interrupted)".into()));
                    return TurnOutcome::Stopped;
                }
                Event::Key(k) if state.pending_approval.is_some() => {
                    // Single-keystroke gate: y/n/a (Esc = deny).
                    if let Some(decision) = approval_decision(k) {
                        if let Some(pa) = state.pending_approval.take() {
                            if matches!(decision, ToolApproval::AllowAll) {
                                state.permission_mode = PermissionMode::Auto;
                            }
                            // A dropped responder just means the agent
                            // gave up before we got back; treat as if
                            // we never had one.
                            let _ = pa.responder.send(decision);
                        }
                    }
                }
                Event::Key(k) if matches!(k.code, KeyCode::Esc) => {
                    state.finalize_trailing_stream();
                    state.push(Entry::Info("(interrupted)".into()));
                    return TurnOutcome::Stopped;
                }
                Event::Key(k) => {
                    // Let the user keep typing into the input box while a
                    // turn streams. Enter is intentionally dropped — the
                    // current turn owns the agent until it ends; user can
                    // Esc to interrupt then Enter to submit.
                    handle_input_edit(state, k);
                }
                Event::Mouse(m) => {
                    // Swallow handle_mouse errors during a streaming
                    // turn — a drag-copy failure shouldn't abort the
                    // turn. The outer event loop propagates the same
                    // errors when no turn is active.
                    let _ = handle_mouse(terminal, state, m);
                }
                Event::Paste(s) => insert_paste(state, &s),
                _ => {}
            }
        }

        // The agent is parked on the approval oneshot. Don't poll the
        // stream — that would block until the user answers anyway, but
        // *also* freeze our draw/event loop. Just spin and redraw so
        // animations + new keystrokes stay live.
        if state.pending_approval.is_some() {
            tokio::time::sleep(Duration::from_millis(33)).await;
            continue;
        }

        tokio::select! {
            evt = stream.next() => {
                match evt {
                    Some(Ok(e)) => {
                        if matches!(e, TurnEvent::ToolStart { .. }) {
                            state.turn_used_tool = true;
                        }
                        let is_end = matches!(e, TurnEvent::TurnEnd);
                        state.apply(e);
                        if is_end { return TurnOutcome::Completed; }
                    }
                    Some(Err(e)) => {
                        state.finalize_trailing_stream();
                        // A context-window overflow is handled by the caller
                        // (`run_turn_ac` auto-compacts + retries); everything
                        // else is a genuine stop shown in the transcript.
                        if teleia_agent::is_overflow_error(&e) {
                            return TurnOutcome::ContextOverflow;
                        }
                        let msg = e.to_string();
                        // A local backend (Ollama) can drop a turn with a bare
                        // 5xx / runner crash / timeout when the prompt overruns
                        // the model's real num_ctx, without the clean overflow
                        // phrasing `is_overflow_error` matches. Point at the
                        // recovery instead of leaving a bare error — a hint
                        // only, never a guessed auto-compaction.
                        let backend_crash = looks_like_backend_crash(&msg);
                        state.push(Entry::Error(msg));
                        if backend_crash {
                            state.push(Entry::Info(
                                "if this was a long session the prompt may have overrun the model's context — run /compact, or set /context to your model's num_ctx so teleia compacts before the backend fails".into(),
                            ));
                        }
                        return TurnOutcome::Stopped;
                    }
                    None => return TurnOutcome::Completed,
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(33)) => {
                // periodic redraw so deltas land smoothly
            }
        }
    }
}

/// Run one turn and, on a context-window overflow, auto-compact the
/// session (summarize + reseed a fresh one) and retry the same input
/// once — when `state.auto_compact` is on. This turns a dead-end
/// "context limit exceeded" into an automatic recovery. When it can't
/// recover it stops: with the toggle off — or a second overflow even
/// after compaction — it surfaces the standard guidance; if compaction
/// itself fails or is interrupted (session id unchanged) it stops
/// quietly, since `run_compact` has already reported why. Every turn
/// driver goes through this wrapper, so `TurnOutcome::ContextOverflow`
/// never leaks past it.
async fn run_turn_ac<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut State,
    agent: &mut Agent,
    input: String,
) -> TurnOutcome {
    // Proactive compaction: for backends that don't report overflow the way
    // hosted APIs do (a local Ollama model silently truncates or 500s), a
    // budget set via `/context N` lets us summarize *before* the turn
    // overflows, instead of waiting for an error that never arrives.
    if state.auto_compact && agent.should_compact() {
        let before = agent.session_id().to_string();
        run_compact(terminal, state, agent).await;
        if agent.session_id() != before {
            state.push(Entry::Info(
                "(auto-compacted: approaching the context limit)".into(),
            ));
        }
    }
    let outcome = run_turn(terminal, state, agent, input.clone()).await;
    if outcome != TurnOutcome::ContextOverflow {
        return outcome;
    }
    if !state.auto_compact {
        state.push(Entry::Error(
            teleia_agent::CONTEXT_OVERFLOW_HELP.to_string(),
        ));
        return TurnOutcome::Stopped;
    }
    // Summarize + reseed, then re-ask against the smaller session. Guard on
    // the session id: a cancelled/failed compaction leaves it unchanged, and
    // retrying would just overflow again — so bail instead of looping.
    let before = agent.session_id().to_string();
    run_compact(terminal, state, agent).await;
    if agent.session_id() == before {
        return TurnOutcome::Stopped;
    }
    // run_compact clears the visual log on success; note the automatic
    // compaction and restore the prompt above its incoming reply.
    state.push(Entry::Info(
        "(auto-compacted: the context window was full)".into(),
    ));
    state.push(Entry::User(input.clone()));
    state.working = true;
    let retry = run_turn(terminal, state, agent, input).await;
    if retry == TurnOutcome::ContextOverflow {
        state.push(Entry::Error(
            teleia_agent::CONTEXT_OVERFLOW_HELP.to_string(),
        ));
        return TurnOutcome::Stopped;
    }
    retry
}

/// Drive a bounded `/loop`: re-submit `spec.prompt` as a chat turn up to
/// `spec.count` times. Each iteration appends to the same session, so the
/// model sees its prior work as context. Esc / Ctrl-C during any turn
/// (or a stream error) returns [`TurnOutcome::Stopped`] and halts the
/// loop early.
async fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut State,
    agent: &mut Agent,
    spec: LoopSpec,
) {
    for i in 0..spec.count {
        state.push(Entry::Info(format!("loop {}/{}", i + 1, spec.count)));
        state.push(Entry::User(spec.prompt.clone()));
        state.working = true;
        let outcome = run_turn_ac(terminal, state, agent, spec.prompt.clone()).await;
        state.working = false;
        if outcome == TurnOutcome::Stopped {
            state.push(Entry::Info(format!(
                "loop stopped after {}/{}",
                i + 1,
                spec.count
            )));
            break;
        }
    }
}

/// Drive `agent.compact()` — summarize the session with the model and
/// continue in a fresh session seeded with the summary — while keeping
/// the TUI alive with the same redraw/interrupt loop as `run_turn`.
/// Esc / Ctrl-C cancels by dropping the future; the agent only swaps
/// its history *after* the summary has fully streamed, so a cancelled
/// compact leaves the session exactly as it was.
async fn run_compact<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut State,
    agent: &mut Agent,
) {
    let before = agent.context_estimate();
    state.push(Entry::Info("compacting session…".into()));
    state.working = true;
    let result = {
        let fut = agent.compact();
        pin_mut!(fut);
        loop {
            state.frame = state.frame.wrapping_add(1);
            if let Err(e) = terminal.draw(|f| draw(f, state)) {
                break Err(anyhow::anyhow!("draw: {e:#}"));
            }
            let mut interrupted = false;
            // Same event handling as `run_turn`: Esc / Ctrl-C interrupt,
            // typing and scrolling stay live while the summary streams.
            while event::poll(Duration::from_millis(0)).unwrap_or(false) {
                let Ok(evt) = event::read() else { break };
                match evt {
                    Event::Key(k) if k.kind == KeyEventKind::Release => {}
                    Event::Key(k)
                        if matches!(k.code, KeyCode::Esc)
                            || (k.modifiers.contains(KeyModifiers::CONTROL)
                                && matches!(k.code, KeyCode::Char('c'))) =>
                    {
                        interrupted = true;
                    }
                    Event::Key(k) => handle_input_edit(state, k),
                    Event::Mouse(m) => {
                        let _ = handle_mouse(terminal, state, m);
                    }
                    Event::Paste(s) => insert_paste(state, &s),
                    _ => {}
                }
            }
            if interrupted {
                // Dropping the future cancels the summarize call; the
                // agent hasn't touched its history yet.
                state.working = false;
                state.push(Entry::Info("(interrupted — session unchanged)".into()));
                return;
            }
            tokio::select! {
                r = &mut fut => break r,
                _ = tokio::time::sleep(Duration::from_millis(33)) => {}
            }
        }
    };
    state.working = false;
    match result {
        Ok(()) => {
            let after = agent.context_estimate();
            state.history.clear();
            clear_search(state);
            state.tokens = TokenCounts::default();
            state.push(Entry::Info(format!(
                "compacted: history ~{} → ~{} tokens · session {} · previous session recoverable via /load prev",
                before.history,
                after.history,
                &agent.session_id()[..agent.session_id().len().min(12)]
            )));
        }
        Err(e) => state.push(Entry::Error(format!("compact: {e:#}"))),
    }
}

/// Hidden-input key-entry dispatch. Active only while
/// `state.pending_key_entry` is `Some`. Enter commits the typed key
/// onto the agent (and updates the `/keys` mirror); Esc cancels.
/// Backspace pops one char; printable chars push.
fn handle_key_entry(state: &mut State, agent: &mut Agent, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            if let Some(ke) = state.pending_key_entry.take() {
                if ke.buf.is_empty() {
                    if ke.existing {
                        state.push(Entry::Info(format!("kept existing {} key", ke.provider)));
                    } else {
                        state.push(Entry::Info(format!(
                            "no key entered — {} requests will 401 until ${} is set",
                            ke.provider, ke.env_var
                        )));
                    }
                } else {
                    let chars = ke.buf.chars().count();
                    let pref = crate::pref_key_for(&ke.env_var);
                    set_pref_warn(state, agent, &pref, &ke.buf);
                    agent.set_api_key(Some(ke.buf));
                    state.push(Entry::Info(format!(
                        "stored {} key ({chars} chars) — saved for future launches too",
                        ke.provider
                    )));
                }
            }
        }
        KeyCode::Esc => {
            if let Some(ke) = state.pending_key_entry.take() {
                if ke.existing {
                    state.push(Entry::Info(format!("kept existing {} key", ke.provider)));
                } else {
                    state.push(Entry::Info(format!(
                        "key entry cancelled — {} requests will 401 until ${} is set",
                        ke.provider, ke.env_var
                    )));
                }
            }
        }
        KeyCode::Backspace => {
            if let Some(ke) = state.pending_key_entry.as_mut() {
                ke.buf.pop();
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(ke) = state.pending_key_entry.as_mut() {
                ke.buf.push(c);
            }
        }
        _ => {}
    }
}

/// Map a keystroke to a [`ToolApproval`] when the permission prompt is
/// up. `Esc` is treated as a deny so the user can dismiss the prompt
/// with the same key they'd use anywhere else; explicit `y`/`n`/`a`
/// (or their uppercase variants) take the obvious meanings. Other keys
/// return `None` so the loop keeps waiting.
fn approval_decision(k: KeyEvent) -> Option<ToolApproval> {
    match k.code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Some(ToolApproval::Allow),
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(ToolApproval::Deny),
        KeyCode::Char('a') | KeyCode::Char('A') => Some(ToolApproval::AllowAll),
        _ => None,
    }
}

/// Drop any active `/`-search state. `search_matches` index into
/// `state.history`, so this must run wherever the history is cleared or
/// replaced (`/clear`, `/reset`, `/load`, compaction) — otherwise the
/// overlay highlight and `n`/`N` point at entries that no longer exist.
fn clear_search(state: &mut State) {
    state.search_pattern = None;
    state.search_matches.clear();
    state.search_idx = None;
}

fn handle_slash(state: &mut State, agent: &mut Agent, cmd: &str) {
    let mut parts = cmd.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim();
    let arg = parts.next().unwrap_or("").trim();
    match name {
        "reset" => match agent.reset() {
            Ok(()) => {
                state.history.clear();
                clear_search(state);
                state.tokens = TokenCounts::default();
                state.push(Entry::Info(format!(
                    "started new session {}",
                    &agent.session_id()[..agent.session_id().len().min(12)]
                )));
            }
            Err(e) => state.push(Entry::Error(format!("reset: {e}"))),
        },
        "compact" => {
            // Only arms the flag; `submit_input` (and the ex path) take
            // `pending_compact` and drive `run_compact`, which needs the
            // terminal for its redraw/interrupt loop.
            state.pending_compact = true;
        }
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
                    clear_search(state);
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
            clear_search(state);
            state.scroll = 0;
            state.follow_bottom = true;
            state.last_total_lines = 0;
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
                set_pref_warn(state, agent, "current_model", arg);
                state.push(Entry::Info(format!("switched model to {arg}")));
                // The compaction budget is per-model; re-detect the new
                // model's num_ctx. Async, so the sync handler only arms it.
                state.pending_redetect = true;
                // For *every* cloud-model switch, drop into the
                // hidden-input prompt so the key for the new provider
                // can be confirmed or replaced. When a key is already
                // saved, Enter with an empty buffer keeps it; Esc
                // dismisses without touching it. Ollama models skip
                // this — no provider, no key needed.
                if let Some(prov) = teleia_llm::provider_for_model(arg) {
                    state.pending_key_entry = Some(KeyEntry {
                        provider: prov.name.to_string(),
                        env_var: prov.env_var.to_string(),
                        buf: String::new(),
                        existing: agent.has_api_key(),
                    });
                }
            }
        }
        "auto" => set_mode(state, agent, PermissionMode::Auto),
        "build" | "ask" => set_mode(state, agent, PermissionMode::Build),
        "plan" => set_mode(state, agent, PermissionMode::Plan),
        "update" => {
            // Re-display the cached check from startup. We don't re-hit
            // the network here — the slash handler is sync and the
            // check would block the event loop.
            let body = match &state.update_check {
                Some(uc) if uc.newer => format!(
                    "update available: teleia v{} (you're on v{})\n  {}\n\n  teleia --self-update   # download, verify, and replace in place\n  # or: cargo install --git https://github.com/foolish-dev/teleia teleia-cli --force",
                    uc.latest, uc.current, uc.url
                ),
                Some(uc) => format!(
                    "teleia v{} is the latest release · you're on v{}",
                    uc.latest, uc.current
                ),
                None => "update check unavailable (offline, rate-limited, or no releases yet)"
                    .to_string(),
            };
            state.push(Entry::Info(body));
        }
        "context" => {
            let a = arg.trim();
            if a.is_empty() {
                let est = agent.context_estimate();
                let total = est.total();
                let t = agent.tokens();
                let budget_line = match agent.context_limit() {
                    Some(l) => format!(
                        "\n  budget  {} tok  ({}% used · auto-compact at 85%)",
                        format_count(l),
                        total.saturating_mul(100) / l,
                    ),
                    None => "\n  budget  unset  (/context N to auto-compact before overflow)"
                        .to_string(),
                };
                state.push(Entry::Info(format!(
                    "context · {} · {} msgs\n  \
                     system prompt  ~{} tok\n  \
                     tools ({})  ~{} tok\n  \
                     history  ~{} tok\n  \
                     → ~{} tok sent per turn · session ↑{} ↓{}{}",
                    agent.model(),
                    est.messages,
                    format_count(est.system),
                    agent.tools().len(),
                    format_count(est.tools),
                    format_count(est.history),
                    format_count(total),
                    format_count(t.prompt),
                    format_count(t.completion),
                    budget_line,
                )));
            } else if a.eq_ignore_ascii_case("off") || a == "0" {
                match agent.set_context_limit(None) {
                    Ok(()) => state.push(Entry::Info(
                        "context budget cleared — proactive auto-compact off".into(),
                    )),
                    Err(e) => {
                        state.push(Entry::Error(format!("couldn't clear context budget: {e}")))
                    }
                }
            } else if let Ok(n) = a.replace('_', "").parse::<u64>() {
                match agent.set_context_limit(Some(n)) {
                    Ok(()) => state.push(Entry::Info(format!(
                        "context budget set to {} tok — teleia auto-compacts before a turn \
                         exceeds ~85% of it (match your local model's num_ctx)",
                        format_count(n),
                    ))),
                    Err(e) => {
                        state.push(Entry::Error(format!("couldn't save context budget: {e}")))
                    }
                }
            } else {
                state.push(Entry::Error(
                    "usage: /context [N | off]   (N = token budget, e.g. 32768)".into(),
                ));
            }
        }
        "mcps" => {
            let mut sub = arg.splitn(2, char::is_whitespace);
            let action = sub.next().unwrap_or("").trim();
            let target = sub.next().unwrap_or("").trim();
            match action {
                "" => {
                    let summary = state.mcp_summary.clone().unwrap_or_else(|| {
                        "no MCP servers configured. Add `[mcps.NAME]` entries to ~/.config/teleia/config.toml.".to_string()
                    });
                    let disabled = agent.disabled_mcps();
                    let mut text = summary;
                    if !agent.mcp_server_names().is_empty() {
                        if disabled.is_empty() {
                            text.push_str("\n\nall servers enabled  ·  /mcps disable NAME to hide a server's tools");
                        } else {
                            text.push_str(&format!(
                                "\n\ndisabled ({} of {}): {}  ·  /mcps enable NAME to restore",
                                disabled.len(),
                                agent.mcp_server_names().len(),
                                disabled.join(", ")
                            ));
                        }
                    }
                    state.push(Entry::Info(text));
                }
                "enable" | "disable" => {
                    if target.is_empty() {
                        state.push(Entry::Error(format!(
                            "usage: /mcps {action} NAME  (known: {})",
                            if agent.mcp_server_names().is_empty() {
                                "<no MCP servers configured>".to_string()
                            } else {
                                agent.mcp_server_names().join(", ")
                            }
                        )));
                        return;
                    }
                    let result = if action == "enable" {
                        agent.enable_mcp(target)
                    } else {
                        agent.disable_mcp(target)
                    };
                    match result {
                        Ok(true) => {
                            state.push(Entry::Info(format!("{action}d MCP server `{target}`")))
                        }
                        Ok(false) => state.push(Entry::Info(format!(
                            "MCP server `{target}` was already {action}d"
                        ))),
                        Err(e) => state.push(Entry::Error(format!("/mcps {action}: {e}"))),
                    }
                }
                other => {
                    state.push(Entry::Error(format!(
                        "unknown /mcps subcommand: '{other}'  (use: enable NAME · disable NAME · or no args for status)"
                    )));
                }
            }
        }
        "lsps" => {
            let text = state.lsp_summary.clone().unwrap_or_else(|| {
                "no LSP servers configured. Add `[lsps.NAME]` entries to ~/.config/teleia/config.toml.".to_string()
            });
            state.push(Entry::Info(text));
        }
        "prompt" => {
            if arg.is_empty() {
                let names: Vec<&str> = PROMPT_TEMPLATES.iter().map(|(n, _)| *n).collect();
                state.push(Entry::Info(format!(
                    "prompts: {}\n(use /prompt NAME to drop a template into the input)",
                    names.join(" · ")
                )));
            } else if let Some((_, text)) = PROMPT_TEMPLATES
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(arg))
            {
                state.input = (*text).to_string();
                state.input_cursor = state.input.len();
                state.recall_idx = None;
            } else {
                let names: Vec<&str> = PROMPT_TEMPLATES.iter().map(|(n, _)| *n).collect();
                state.push(Entry::Error(format!(
                    "unknown prompt '{arg}'. known: {}",
                    names.join(", ")
                )));
            }
        }
        "key" => {
            // /key PROVIDER  →  open the hidden-input prompt for that provider
            // /key           →  short usage hint pointing at /keys
            if arg.is_empty() {
                let names: Vec<&str> = teleia_llm::PROVIDERS.iter().map(|p| p.name).collect();
                state.push(Entry::Info(format!(
                    "usage: /key PROVIDER  (one of: {})\n  or /keys to list status",
                    names.join(", ")
                )));
            } else if let Some(prov) = teleia_llm::PROVIDERS
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(arg))
            {
                let existing = agent
                    .get_pref(&crate::pref_key_for(prov.env_var))
                    .map(|v| !v.is_empty())
                    .unwrap_or(false)
                    || std::env::var(prov.env_var)
                        .map(|v| !v.is_empty())
                        .unwrap_or(false);
                state.pending_key_entry = Some(KeyEntry {
                    provider: prov.name.to_string(),
                    env_var: prov.env_var.to_string(),
                    buf: String::new(),
                    existing,
                });
            } else {
                let names: Vec<&str> = teleia_llm::PROVIDERS.iter().map(|p| p.name).collect();
                state.push(Entry::Error(format!(
                    "unknown provider '{arg}'. known: {}",
                    names.join(", ")
                )));
            }
        }
        "keys" => {
            let active = teleia_llm::provider_for_model(&state.model);
            let mut text = String::from("api keys (env var → status)");
            for p in teleia_llm::PROVIDERS {
                let set = std::env::var(p.env_var)
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);
                let mark = if set { "✓ set" } else { "✗ unset" };
                let star = if active.map(|a| a.name == p.name).unwrap_or(false) {
                    " ← active"
                } else {
                    ""
                };
                text.push_str(&format!(
                    "\n  {:<10} {:<22} {mark}{star}",
                    p.name, p.env_var
                ));
            }
            if active.is_none() {
                text.push_str("\n\nactive: Ollama (no key needed)");
            }
            state.push(Entry::Info(text));
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
                set_pref_warn(state, agent, "theme", canonical);
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
        "notify" => {
            let new = match arg.to_ascii_lowercase().as_str() {
                "on" | "true" | "yes" | "1" => true,
                "off" | "false" | "no" | "0" => false,
                "" => !state.notify,
                _ => {
                    state.push(Entry::Error(format!(
                        "usage: /notify [on|off]  (current: {})",
                        if state.notify { "on" } else { "off" }
                    )));
                    return;
                }
            };
            state.notify = new;
            set_pref_warn(state, agent, "notify", if new { "on" } else { "off" });
            state.push(Entry::Info(format!(
                "desktop notifications {}",
                if new { "on" } else { "off" }
            )));
        }
        // UI name is `/entropy`; the internal field + pref key stay `autoprompt`.
        "entropy" => {
            let new = match arg.to_ascii_lowercase().as_str() {
                "on" | "true" | "yes" | "1" => true,
                "off" | "false" | "no" | "0" => false,
                "" => !state.autoprompt,
                _ => {
                    state.push(Entry::Error(format!(
                        "usage: /entropy [on|off]  (current: {})",
                        if state.autoprompt { "on" } else { "off" }
                    )));
                    return;
                }
            };
            state.autoprompt = new;
            set_pref_warn(state, agent, "autoprompt", if new { "on" } else { "off" });
            state.push(Entry::Info(
                if new {
                    "entropy on — after each turn I'll continue on my own until the task is done (esc/^c stops, cap 100)".to_string()
                } else {
                    "entropy off".to_string()
                },
            ));
        }
        "autocompact" => {
            let new = match arg.to_ascii_lowercase().as_str() {
                "on" | "true" | "yes" | "1" => true,
                "off" | "false" | "no" | "0" => false,
                "" => !state.auto_compact,
                _ => {
                    state.push(Entry::Error(format!(
                        "usage: /autocompact [on|off]  (current: {})",
                        if state.auto_compact { "on" } else { "off" }
                    )));
                    return;
                }
            };
            state.auto_compact = new;
            set_pref_warn(state, agent, "auto_compact", if new { "on" } else { "off" });
            state.push(Entry::Info(
                if new {
                    "autocompact on — a full context window auto-summarizes into a fresh session and the turn retries (previous recoverable via /load prev)".to_string()
                } else {
                    "autocompact off — a full context window surfaces an error; run /compact yourself".to_string()
                },
            ));
        }
        // Show/hide the model's reasoning stream. Display-only: the model
        // still reasons (that's `/effort`); this just gates rendering, so
        // toggling back on re-reveals the current session's thinking.
        "thinking" => {
            let new = match arg.to_ascii_lowercase().as_str() {
                "on" | "true" | "yes" | "1" => true,
                "off" | "false" | "no" | "0" => false,
                "" => !show_thinking(),
                _ => {
                    state.push(Entry::Error(format!(
                        "usage: /thinking [on|off]  (current: {})",
                        if show_thinking() { "on" } else { "off" }
                    )));
                    return;
                }
            };
            set_show_thinking(new);
            set_pref_warn(
                state,
                agent,
                "show_thinking",
                if new { "on" } else { "off" },
            );
            state.push(Entry::Info(if new {
                "thinking on — showing the model's reasoning".to_string()
            } else {
                "thinking off — hiding the model's reasoning".to_string()
            }));
        }
        "cd" => {
            let target = if arg.is_empty() {
                std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
            } else {
                arg.to_string()
            };
            match std::env::set_current_dir(&target) {
                Ok(()) => state.push(Entry::Info(format!("cd {target}"))),
                Err(e) => state.push(Entry::Error(format!("cd {target}: {e}"))),
            }
        }
        "pwd" => match std::env::current_dir() {
            Ok(p) => state.push(Entry::Info(p.display().to_string())),
            Err(e) => state.push(Entry::Error(format!("pwd: {e}"))),
        },
        "version" => {
            state.push(Entry::Info(format!(
                "τέλεια {} · ratatui · reqwest (rustls) · rusqlite (bundled)",
                env!("CARGO_PKG_VERSION")
            )));
        }
        "tools" => {
            let defs = agent.tools();
            let builtin_names: std::collections::HashSet<String> = teleia_tools::definitions()
                .into_iter()
                .map(|d| d.function.name)
                .collect();
            let (builtin, mcp): (Vec<_>, Vec<_>) = defs
                .iter()
                .partition(|d| builtin_names.contains(&d.function.name));
            let summarise = |d: &teleia_llm::ToolDef| {
                let first_line = d
                    .function
                    .description
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(70)
                    .collect::<String>();
                format!("  {:<14} {first_line}", d.function.name)
            };
            let mut text = format!(
                "tools · {} total\n\nbuilt-in ({})",
                defs.len(),
                builtin.len()
            );
            for d in &builtin {
                text.push('\n');
                text.push_str(&summarise(d));
            }
            if !mcp.is_empty() {
                text.push_str(&format!("\n\nmcp ({})", mcp.len()));
                for d in &mcp {
                    text.push('\n');
                    text.push_str(&summarise(d));
                }
            }
            state.push(Entry::Info(text));
        }
        "copy" | "yank" => yank_last_assistant(state),
        "transparent" | "transparency" => {
            let new = match arg.to_ascii_lowercase().as_str() {
                "on" | "true" | "yes" | "1" => true,
                "off" | "false" | "no" | "0" => false,
                "" => !is_transparent(),
                _ => {
                    state.push(Entry::Error(format!(
                        "usage: /transparent [on|off]  (current: {})",
                        if is_transparent() { "on" } else { "off" }
                    )));
                    return;
                }
            };
            set_transparent(new);
            set_pref_warn(state, agent, "transparent", if new { "on" } else { "off" });
            state.push(Entry::Info(format!(
                "transparent background {} — terminal alpha + compositor blur now {}",
                if new { "on" } else { "off" },
                if new { "pass through" } else { "are masked" }
            )));
        }
        "quit" | "exit" | "q" => {
            state.should_quit = true;
        }
        "effort" => {
            if arg.is_empty() {
                let cur = agent.reasoning_effort().unwrap_or("off");
                state.push(Entry::Info(format!(
                    "reasoning effort: {cur}  ·  /effort [off|low|medium|high|xhigh|max|leetcode]"
                )));
            } else {
                match arg {
                    "off" | "none" => {
                        agent.set_reasoning_effort(None);
                        set_pref_warn(state, agent, "reasoning_effort", "off");
                        state.reasoning_effort = None;
                        state.push(Entry::Info("reasoning effort: off".into()));
                    }
                    other if teleia_agent::is_reasoning_effort(other) => {
                        agent.set_reasoning_effort(Some(arg.to_string()));
                        set_pref_warn(state, agent, "reasoning_effort", arg);
                        state.reasoning_effort = Some(arg.to_string());
                        state.push(Entry::Info(format!(
                            "reasoning effort: {arg} (reasoning-capable models only)"
                        )));
                    }
                    other => state.push(Entry::Error(format!(
                        "unknown effort '{other}' (use: off · low · medium · high · xhigh · max · leetcode)"
                    ))),
                }
            }
        }
        "loop" => {
            // Only arms the loop; `submit_input` takes `pending_loop`
            // and drives the turns, since `handle_slash` is sync and
            // can't run a turn without freezing the event loop.
            match parse_loop(arg) {
                Ok((count, prompt)) => {
                    state.pending_loop = Some(LoopSpec { count, prompt });
                }
                Err(msg) => state.push(Entry::Error(msg)),
            }
        }
        "help" | "?" => {
            state.push(Entry::Info(
                "commands: /reset · /clear · /save NAME · /load NAME · /delete NAME · /list · /model [NAME] · /effort [off|low|medium|high|xhigh|max|leetcode] · /key PROVIDER · /keys · /mcps · /lsps · /tools · /context · /compact · /plan · /build · /auto · /loop N PROMPT · /prompt [NAME] · /theme [NAME] · /notify [on|off] · /entropy [on|off] · /autocompact [on|off] · /thinking [on|off] · /transparent [on|off] · /copy · /cd PATH · /pwd · /version · /show · /help · /quit"
                    .into(),
            ));
        }
        other => {
            state.push(Entry::Error(format!("unknown command: /{other}")));
        }
    }
}

/// Per-character colours for the animated `leetcode` effort badge at a
/// given frame. A six-hue palette cycled by char index + frame, so the
/// colours flow across the word as a rainbow wave — one hue every couple
/// of frames (the event loop redraws ~20fps even when idle).
fn leetcode_badge(th: &Theme, frame: usize) -> Vec<(char, Color)> {
    let palette = [th.red, th.yellow, th.green, th.cyan, th.blue, th.purple];
    "leetcode"
        .chars()
        .enumerate()
        .map(|(i, ch)| (ch, palette[(i + frame / 2) % palette.len()]))
        .collect()
}

/// Per-character colours for the animated `entropy` badge. Where the
/// leetcode badge flows an *ordered* rainbow, entropy *scatters*: each
/// glyph jumps around a cool decay palette by a hash of (index, frame),
/// so the colours jitter chaotically — visual disorder. Advances every
/// other frame so it churns rather than strobes (the loop redraws ~20fps).
fn entropy_badge(th: &Theme, frame: usize) -> Vec<(char, Color)> {
    let palette = [th.purple, th.blue, th.cyan, th.green, th.dim];
    "⟳ entropy"
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            let seed = (i as u64)
                .wrapping_mul(0x9E37_79B1)
                .wrapping_add((frame as u64 / 2).wrapping_mul(0x85EB_CA77));
            let idx = ((seed ^ (seed >> 13)) as usize) % palette.len();
            (ch, palette[idx])
        })
        .collect()
}

fn draw(f: &mut ratatui::Frame, state: &mut State) {
    let th = theme();
    // Paint the entire frame with the active theme bg + fg first. Any
    // widget rendered on top inherits the base colours unless it sets
    // its own style — so spans/lines without an explicit fg pick up
    // `th.fg` (Tokyo Night `#c0caf5`) instead of the terminal's
    // default white.
    f.render_widget(
        Block::default().style(Style::default().bg(paint_bg()).fg(th.fg)),
        f.area(),
    );
    // Up to 6 menu items inline above the input. Includes 2 for the border.
    // Dropdown shows up to 10 entries at a time; longer lists (the full
    // /model catalogue, ~200+ entries) scroll inside the panel via
    // ListState's offset-tracking, with `Up`/`Down` always keeping the
    // selected row in view.
    const MENU_VISIBLE: usize = 10;
    let menu_height: u16 = state
        .menu
        .as_ref()
        .map(|m| (m.items.len().min(MENU_VISIBLE) as u16) + 2)
        .unwrap_or(0);

    // Grow the input box vertically so long prompts wrap into view instead
    // of horizontally scrolling off the right edge. Clamped so it never
    // takes more than a third of the screen — the chat log keeps the rest.
    // While an approval request is pending, the same chunk renders the
    // approval prompt (3 content rows) instead of the input.
    let (active_buf, active_prefix_width) = match state.mode {
        Mode::Command => (state.command_buf.as_str(), 2usize), // ": "
        _ => (state.input.as_str(), 2usize),                   // "> "
    };
    let inner_input_width = (f.area().width as usize).saturating_sub(2); // borders
    let needed_rows = input_visual_rows(active_buf, inner_input_width, active_prefix_width);
    let max_input_rows = (f.area().height as usize / 3).max(1);
    let input_content_rows = if state.pending_approval.is_some() {
        3
    } else if state.pending_key_entry.is_some() {
        2
    } else {
        needed_rows.clamp(1, max_input_rows)
    };
    let input_height = (input_content_rows as u16) + 2; // + borders

    // No spacer rows around the prompt — the chat block's own border
    // One blank row sits between the chat bottom border and the input
    // top border — keeps the prompt visually distinct without burning
    // a second row below it. Drops to 0 on cramped (<24-row) terminals
    // so small screens keep their chat space.
    let gap: u16 = if f.area().height >= 24 { 1 } else { 0 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),               // 0 chat log
            Constraint::Length(menu_height),  // 1 menu
            Constraint::Length(gap),          // 2 spacer above input
            Constraint::Length(input_height), // 3 input box
            Constraint::Length(0),            // 4 (no spacer below input)
            Constraint::Length(1),            // 5 status bar
        ])
        .split(f.area());
    // Stash the chat-area rect so mouse handlers (which run between draws)
    // can map raw terminal coordinates back to log content.
    state.log_area = chunks[0];

    let frame = state.frame;
    let lines: Vec<Line> = if state.history.is_empty() {
        welcome_banner(chunks[0].width, frame)
    } else {
        render_history(&state.history, frame, &state.username)
    };
    // Chat log breathes: 1-char horizontal padding on terminals wide
    // enough to spare, 1-row top padding when tall enough. Auto-shrinks
    // to zero on cramped screens so the transcript keeps the space.
    let h_pad: u16 = if f.area().width >= 60 { 1 } else { 0 };
    // One-row top padding inside the chat block so the first entry
    // doesn't crash into the title bar. Drops to 0 on cramped (<24-row)
    // terminals so small screens keep their content space.
    let t_pad: u16 = if f.area().height >= 24 { 1 } else { 0 };
    // The visible content area = block height − borders (2) − top padding.
    // Without subtracting the padding, the auto-scroll math is off by one
    // and new streamed deltas can land behind the padding strip, looking
    // like the chat is frozen on the previous frame.
    let visible = chunks[0].height.saturating_sub(2 + t_pad) as usize;
    // Count *visual* rows after wrapping when the chat looks like it
    // might overflow; otherwise the cheap `lines.len()` is exact
    // enough (no wrap → no max_offset). `Paragraph::line_count` runs
    // ratatui's WordWrapper across every line, which is O(N) per
    // frame for long transcripts — skipping it when content trivially
    // fits keeps short chats / startup banners cheap. The factor of 2
    // gates the upgrade: even if every logical line wrapped to 2
    // visual rows, we'd still fit and the exact count wouldn't move
    // max_offset off 0.
    let wrap_width = chunks[0].width.saturating_sub(2 + h_pad * 2);
    let raw_lines = lines.len();
    let total = if raw_lines * 2 <= visible {
        raw_lines
    } else {
        Paragraph::new(lines.clone())
            .wrap(Wrap { trim: false })
            .line_count(wrap_width)
    };
    // Belt-and-braces: when bottom-following, `scroll` is meant to be
    // 0. apply() and push() set it, but a stale value from an earlier
    // state transition shouldn't strand the user away from the latest
    // content. Enforce it here so the invariant always holds.
    if state.follow_bottom {
        state.scroll = 0;
    } else if total > state.last_total_lines {
        // Compensate for new lines so the visible window stays pinned
        // to the same content the user was reading.
        let delta = (total - state.last_total_lines) as u16;
        state.scroll = state.scroll.saturating_add(delta);
    }
    state.last_total_lines = total;
    let max_offset = total.saturating_sub(visible) as u16;
    // Clamp scroll so it can't exceed what's representable in the
    // transcript (e.g. after a /reset shrinks history).
    if state.scroll > max_offset {
        state.scroll = max_offset;
        state.follow_bottom = state.scroll == 0;
    }
    let offset = max_offset.saturating_sub(state.scroll);
    let log = Paragraph::new(lines)
        .style(Style::default().bg(paint_bg()).fg(th.fg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(th.dim).bg(paint_bg()))
                .style(Style::default().bg(paint_bg()))
                .padding(Padding::new(h_pad, h_pad, t_pad, 0))
                .title(Line::from(vec![
                    // Nerd Font terminal glyph + λ — soft "rice"
                    // accent. Renders as a tiny box on terminals
                    // without a Nerd Font installed, otherwise as
                    // a stylized terminal icon. Title also gets a
                    // leading dimmed angle bracket for the
                    // Powerline-y aesthetic.
                    Span::styled("╭ ", Style::default().fg(th.dim).bg(paint_bg())),
                    Span::styled(" ", Style::default().fg(th.cyan).bg(paint_bg())),
                    Span::styled(
                        "λ ",
                        Style::default()
                            .fg(th.cyan)
                            .bg(paint_bg())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "τέλεια ",
                        Style::default()
                            .fg(th.purple)
                            .bg(paint_bg())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("@ ", Style::default().fg(th.dim).bg(paint_bg())),
                    Span::styled(
                        format!("{} ", state.hostname),
                        Style::default()
                            .fg(th.cyan)
                            .bg(paint_bg())
                            .add_modifier(Modifier::ITALIC),
                    ),
                ])),
        )
        .wrap(Wrap { trim: false })
        .scroll((offset, 0));
    f.render_widget(log, chunks[0]);

    // Search-result highlight overlay: walk the inner chat area
    // row-by-row, reconstruct the on-screen text from cell symbols, and
    // mark every cell whose column range falls inside a match of
    // `search_pattern`. Operates on what the user sees (post-wrap,
    // post-paragraph) so we don't have to plumb the pattern through
    // every render_entry call. Painted before the selection overlay so
    // a drag-select highlight wins over a match highlight when they
    // overlap.
    if let Some(pat) = state.search_pattern.as_deref().filter(|p| !p.is_empty()) {
        let inner = selection_inner_area(state.log_area);
        if inner.width > 0 && inner.height > 0 {
            let buf = f.buffer_mut();
            let th2 = th;
            for row in inner.y..inner.y + inner.height {
                // (byte offset in row_text where this cell starts, col index)
                let mut offsets: Vec<(usize, u16)> = Vec::new();
                let mut row_text = String::new();
                for col in inner.x..inner.x + inner.width {
                    if let Some(cell) = buf.cell((col, row)) {
                        let sym = cell.symbol();
                        offsets.push((row_text.len(), col));
                        row_text.push_str(sym);
                    }
                }
                let mut start = 0usize;
                while let Some(rel) = row_text[start..].find(pat) {
                    let m_start = start + rel;
                    let m_end = m_start + pat.len();
                    for &(off, col) in &offsets {
                        if off >= m_start && off < m_end {
                            if let Some(cell) = buf.cell_mut((col, row)) {
                                cell.set_bg(th2.yellow);
                                cell.set_fg(th2.bg);
                            }
                        }
                    }
                    start = m_end;
                    if start >= row_text.len() {
                        break;
                    }
                }
            }
        }
    }

    // Scrollbar on the right edge of the chat block — only when there's
    // overflow. We render it into a 1-col strip that sits *just inside*
    // the right border (in the right-padding column when `h_pad > 0`)
    // and stops short of both corners, so the rounded `╮` / `╯` glyphs
    // stay intact and the track doesn't overwrite the border line.
    if total > visible && chunks[0].width >= 4 && chunks[0].height >= 4 {
        let sb_x = if h_pad > 0 {
            chunks[0].x + chunks[0].width - 2
        } else {
            // No padding column to spare on narrow terminals; overwrite
            // the right border rather than skip the scrollbar entirely.
            chunks[0].x + chunks[0].width - 1
        };
        let sb_area = Rect {
            x: sb_x,
            y: chunks[0].y + 1,
            width: 1,
            height: chunks[0].height - 2,
        };
        let mut sb_state = ScrollbarState::new(total)
            .viewport_content_length(visible)
            .position(offset as usize);
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_style(Style::default().fg(th.dim).bg(paint_bg()))
            .thumb_style(Style::default().fg(th.purple).bg(paint_bg()));
        f.render_stateful_widget(sb, sb_area, &mut sb_state);
    }

    // Render the menu (if any) directly above the input.
    if let Some(menu) = &state.menu {
        let total = menu.items.len();
        let title_text = match menu.kind {
            MenuKind::Command => format!(" commands · {total} "),
            MenuKind::Alias => format!(" aliases · {total} "),
            MenuKind::Theme => format!(" themes · {total} "),
            MenuKind::Ex => format!(" ex · {total} "),
            MenuKind::Model => format!(" models · {total} "),
            MenuKind::McpServer => format!(" mcp servers · {total} "),
        };
        // Pass the whole list to ratatui — ListState's offset handling
        // scrolls long catalogues automatically and keeps `selected` in
        // the visible window.
        let items: Vec<ListItem> = menu
            .items
            .iter()
            .map(|s| ListItem::new(s.clone()).style(Style::default().fg(th.fg).bg(paint_bg())))
            .collect();
        let list = List::new(items)
            .style(Style::default().bg(paint_bg()).fg(th.fg))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(th.dim).bg(paint_bg()))
                    .style(Style::default().bg(paint_bg()))
                    .title(Span::styled(
                        title_text,
                        Style::default()
                            .fg(th.blue)
                            .bg(paint_bg())
                            .add_modifier(Modifier::BOLD),
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
        Mode::Visual => ("> ", th.purple, &state.input, state.input_cursor),
        Mode::Command => (": ", th.yellow, &state.command_buf, state.command_cursor),
        Mode::Replace => ("> ", th.red, &state.input, state.input_cursor),
        Mode::Search => ("/ ", th.blue, &state.search_buf, state.search_cursor),
    };

    let inside = chunks[3];
    let body_style = if state.working {
        Style::default().fg(th.dim)
    } else {
        Style::default().fg(th.fg)
    };

    if let Some(ke) = &state.pending_key_entry {
        // Hidden-input prompt. Echo each typed char as `•`; never store
        // the key in input_history.
        let mask: String = "•".repeat(ke.buf.chars().count());
        let suffix: Vec<Span> = if ke.existing {
            vec![
                Span::styled(
                    " api key (already set · type new to replace, ",
                    Style::default().fg(th.dim),
                ),
                Span::styled("Enter", Style::default().fg(th.green)),
                Span::styled("/", Style::default().fg(th.dim)),
                Span::styled("Esc", Style::default().fg(th.yellow)),
                Span::styled(" to keep)", Style::default().fg(th.dim)),
            ]
        } else {
            vec![
                Span::styled(" api key (will save to $", Style::default().fg(th.dim)),
                Span::styled(&ke.env_var, Style::default().fg(th.cyan)),
                Span::styled(" pref)", Style::default().fg(th.dim)),
            ]
        };
        let mut header = vec![
            Span::styled("enter ", Style::default().fg(th.dim)),
            Span::styled(
                &ke.provider,
                Style::default().fg(th.purple).add_modifier(Modifier::BOLD),
            ),
        ];
        header.extend(suffix);
        let lines = vec![
            Line::from(header),
            Line::from(vec![
                Span::styled("> ", Style::default().fg(th.yellow)),
                Span::styled(mask, Style::default().fg(th.fg)),
            ]),
        ];
        let widget = Paragraph::new(lines)
            .style(Style::default().bg(paint_bg()).fg(th.fg))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(th.yellow).bg(paint_bg()))
                    .style(Style::default().bg(paint_bg()))
                    .title(Span::styled(
                        " api key ",
                        Style::default()
                            .fg(th.yellow)
                            .bg(paint_bg())
                            .add_modifier(Modifier::BOLD),
                    )),
            );
        f.render_widget(widget, chunks[3]);
        // Cursor at end of the masked input.
        let cursor_x = inside.x + 1 + 2 + ke.buf.chars().count() as u16;
        let cursor_y = inside.y + 2;
        f.set_cursor_position((cursor_x, cursor_y));
    } else if let Some(pa) = &state.pending_approval {
        // Approval prompt commandeers the input chunk. The chat scrollback
        // shows the pending tool's name + args; here we just collect a
        // single keystroke (y/n/a).
        let preview = compact_tool_args(&pa.arguments, inside.width.saturating_sub(8) as usize);
        let lines = vec![
            Line::from(vec![
                Span::styled("approve ", Style::default().fg(th.dim)),
                Span::styled(
                    &pa.name,
                    Style::default().fg(th.purple).add_modifier(Modifier::BOLD),
                ),
                Span::styled("(", Style::default().fg(th.dim)),
                Span::styled(preview, Style::default().fg(th.fg)),
                Span::styled(") ?", Style::default().fg(th.dim)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "y",
                    Style::default().fg(th.green).add_modifier(Modifier::BOLD),
                ),
                Span::styled("es · ", Style::default().fg(th.dim)),
                Span::styled(
                    "n",
                    Style::default().fg(th.red).add_modifier(Modifier::BOLD),
                ),
                Span::styled("o · ", Style::default().fg(th.dim)),
                Span::styled(
                    "a",
                    Style::default().fg(th.yellow).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "uto (allow all for this session)",
                    Style::default().fg(th.dim),
                ),
            ]),
        ];
        let widget = Paragraph::new(lines)
            .style(Style::default().bg(paint_bg()).fg(th.fg))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(th.yellow).bg(paint_bg()))
                    .style(Style::default().bg(paint_bg()))
                    .title(Span::styled(
                        " permission ",
                        Style::default()
                            .fg(th.yellow)
                            .bg(paint_bg())
                            .add_modifier(Modifier::BOLD),
                    )),
            );
        f.render_widget(widget, chunks[3]);
        // No cursor while waiting for approval — the prompt isn't editable.
    } else if input_content_rows == 1 {
        // Single-line: keep the horizontal-scroll behaviour so the cursor
        // stays in view when typing past the right edge.
        let visible_width = inside.width.saturating_sub(4) as usize; // 2 borders + prefix (2)
        let cursor_chars = buf[..buf_cursor].chars().count();
        let start_char = cursor_chars.saturating_sub(visible_width.saturating_sub(1));
        let visible_text: String = buf.chars().skip(start_char).take(visible_width).collect();
        let mut spans = vec![
            Span::styled(prompt, Style::default().fg(prompt_color)),
            Span::styled(visible_text, body_style),
        ];
        if state.mode == Mode::Insert
            && state.menu.is_none()
            && state.input_cursor == state.input.len()
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
        let widget = Paragraph::new(Line::from(spans))
            .style(Style::default().bg(paint_bg()).fg(th.fg))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(prompt_color).bg(paint_bg()))
                    .style(Style::default().bg(paint_bg())),
            );
        f.render_widget(widget, chunks[3]);
        let cursor_x = inside.x + 1 + 2 + (cursor_chars - start_char) as u16;
        let cursor_y = inside.y + 1;
        f.set_cursor_position((cursor_x, cursor_y));
    } else {
        // Multi-line: wrap by inner width, prefix on row 0 only. Ghost
        // suggestions are suppressed here — the input is already long
        // enough that another sneaky completion would crowd the view.
        let inner_w = inside.width.saturating_sub(2) as usize; // borders only
        let prefix_w = 2usize;
        let visual_lines = wrap_input_lines(buf, inner_w, prefix_w);
        let mut lines: Vec<Line> = Vec::with_capacity(visual_lines.len());
        for (i, l) in visual_lines.iter().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![
                    Span::styled(prompt, Style::default().fg(prompt_color)),
                    Span::styled(l.clone(), body_style),
                ]));
            } else {
                lines.push(Line::from(Span::styled(l.clone(), body_style)));
            }
        }
        let widget = Paragraph::new(lines)
            .style(Style::default().bg(paint_bg()).fg(th.fg))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(prompt_color).bg(paint_bg()))
                    .style(Style::default().bg(paint_bg())),
            );
        f.render_widget(widget, chunks[3]);

        let (cursor_col, cursor_row) = cursor_visual_pos(buf, buf_cursor, inner_w, prefix_w);
        // Clamp inside the visible content rows so a cursor past the cap
        // pins to the last visible row.
        let max_row = input_content_rows.saturating_sub(1) as u16;
        let cursor_row = cursor_row.min(max_row);
        let cursor_x = inside.x + 1 + cursor_col;
        let cursor_y = inside.y + 1 + cursor_row;
        f.set_cursor_position((cursor_x, cursor_y));
    }

    let (mode_label, mode_color) = match state.mode {
        Mode::Insert => ("INS", th.cyan),
        Mode::Normal => ("NOR", th.green),
        Mode::Visual => match state.selection.map(|s| s.kind).unwrap_or_default() {
            SelectionKind::Char => ("VIS", th.purple),
            SelectionKind::Line => ("V-L", th.purple),
            SelectionKind::Block => ("V-B", th.purple),
        },
        Mode::Command => ("CMD", th.yellow),
        Mode::Replace => ("REP", th.red),
        Mode::Search => ("SCH", th.blue),
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
    // Permission-mode chip. Always shown so the user can't lose track:
    // PLAN (blue) is read-only, BUILD (green) prompts per-call, AUTO
    // (red) skips every prompt — colour-coded by risk.
    let (chip, chip_bg) = match state.permission_mode {
        PermissionMode::Plan => (" PLAN ", th.blue),
        PermissionMode::Build => (" BUILD ", th.green),
        PermissionMode::Auto => (" AUTO ", th.red),
    };
    status_spans.push(Span::styled(" · ", Style::default().fg(th.dim)));
    status_spans.push(Span::styled(
        chip,
        Style::default()
            .fg(th.bg)
            .bg(chip_bg)
            .add_modifier(Modifier::BOLD),
    ));
    // Animated rainbow badge for the top effort tier. Only `leetcode`
    // gets the flourish; the frame counter makes the hues flow.
    if state.reasoning_effort.as_deref() == Some("leetcode") {
        status_spans.push(Span::styled(" · ", Style::default().fg(th.dim)));
        for (ch, color) in leetcode_badge(&th, state.frame) {
            status_spans.push(Span::styled(
                ch.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        }
    }
    // Sticky auto-prompting (shown as `entropy`) is a persisted mode that
    // changes behaviour across sessions, so surface it — an animated badge
    // whose colours scatter chaotically, like the leetcode flourish but
    // disordered rather than a flowing rainbow.
    if state.autoprompt {
        status_spans.push(Span::styled(" · ", Style::default().fg(th.dim)));
        for (ch, color) in entropy_badge(&th, state.frame) {
            status_spans.push(Span::styled(
                ch.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        }
    }
    // Reasoning hidden (`/thinking off` or ^t) — surface it so the missing
    // thinking stream reads as intentional, not a glitch. Shown only when
    // off; on is the default and needs no chrome.
    if !show_thinking() {
        status_spans.push(Span::styled(" · ", Style::default().fg(th.dim)));
        status_spans.push(Span::styled(
            "think off",
            Style::default().fg(th.dim).add_modifier(Modifier::ITALIC),
        ));
    }
    status_spans.push(Span::raw("   "));
    // Replace the right-side mode hints while a turn is in flight: the
    // normal mode keys are dropped mid-stream anyway, and surfacing the
    // interrupt keys here is the only place a first-time user finds out
    // they can stop the model.
    let hint = if state.pending_key_entry.is_some() {
        "type key · enter set · esc cancel"
    } else if state.pending_approval.is_some() {
        "y/n/a · esc cancel"
    } else if state.working {
        "esc / ^c interrupt"
    } else {
        mode_hints(state.mode)
    };
    status_spans.push(Span::styled(hint, Style::default().fg(th.dim)));
    f.render_widget(
        Paragraph::new(Line::from(status_spans)).style(Style::default().bg(paint_bg()).fg(th.fg)),
        chunks[5],
    );

    // Drag-selection highlight overlay. Applied last so it paints on top of
    // every widget; clamped to the log area's inner rect so it never bleeds
    // onto the border, title, input, or status bar.
    if let Some(sel) = state.selection {
        let inner = selection_inner_area(state.log_area);
        let buf = f.buffer_mut();
        for (col, row) in selection_cells(sel, inner) {
            if let Some(cell) = buf.cell_mut((col, row)) {
                cell.set_bg(th.bg_hl);
            }
        }
        // In Visual mode the hardware cursor follows the selection head
        // instead of sitting in the input box, so the user can see where
        // hjkl is taking them. Overrides the set_cursor call that the
        // input renderer issued earlier this frame.
        if state.mode == Mode::Visual && rect_contains(inner, sel.cursor.0, sel.cursor.1) {
            f.set_cursor_position(sel.cursor);
        }
    }
}

/// The inside of the chat-area block (i.e. inside its borders) — drag
/// selection is clamped to this rect so it never crosses the border,
/// title, input, or status bar.
fn selection_inner_area(area: Rect) -> Rect {
    if area.width < 2 || area.height < 2 {
        return Rect::default();
    }
    Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width - 2,
        height: area.height - 2,
    }
}

/// Iterator of `(col, row)` cells covered by a selection. The shape
/// depends on `sel.kind`:
///
/// * `Char` — first row from anchor to end-of-row, middle rows
///   full-width, last row from start-of-row to cursor. Matches vim `v`
///   and mouse drag-select.
/// * `Line` — every touched row taken full-width regardless of column.
///   Matches vim `V`. A `Line` selection is never a point, even when
///   anchor and cursor share a cell, so it always covers at least one
///   full row.
/// * `Block` — the rectangle bounded by the min/max col and min/max row
///   of anchor and cursor. Matches vim `Ctrl-v`. A `Block` selection
///   with a single cell is also never treated as a point so the user
///   can paint a 1×1 block deliberately.
///
/// All coordinates are clamped to `area`.
fn selection_cells(sel: Selection, area: Rect) -> Vec<(u16, u16)> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    if sel.kind == SelectionKind::Char && sel.is_point() {
        return Vec::new();
    }
    let left = area.x;
    let right = area.x + area.width - 1;
    let top = area.y;
    let bottom = area.y + area.height - 1;
    let clamp_col = |c: u16| c.clamp(left, right);
    let clamp_row = |r: u16| r.clamp(top, bottom);

    match sel.kind {
        SelectionKind::Char => {
            let (start, end) = sel.ordered();
            let s_row = clamp_row(start.1);
            let e_row = clamp_row(end.1);
            let mut out = Vec::new();
            if s_row == e_row {
                let s_col = clamp_col(start.0);
                let e_col = clamp_col(end.0);
                for c in s_col..=e_col {
                    out.push((c, s_row));
                }
                return out;
            }
            let s_col = clamp_col(start.0);
            for c in s_col..=right {
                out.push((c, s_row));
            }
            for r in (s_row + 1)..e_row {
                for c in left..=right {
                    out.push((c, r));
                }
            }
            let e_col = clamp_col(end.0);
            for c in left..=e_col {
                out.push((c, e_row));
            }
            out
        }
        SelectionKind::Line => {
            let s_row = clamp_row(sel.anchor.1.min(sel.cursor.1));
            let e_row = clamp_row(sel.anchor.1.max(sel.cursor.1));
            let mut out = Vec::new();
            for r in s_row..=e_row {
                for c in left..=right {
                    out.push((c, r));
                }
            }
            out
        }
        SelectionKind::Block => {
            let s_col = clamp_col(sel.anchor.0.min(sel.cursor.0));
            let e_col = clamp_col(sel.anchor.0.max(sel.cursor.0));
            let s_row = clamp_row(sel.anchor.1.min(sel.cursor.1));
            let e_row = clamp_row(sel.anchor.1.max(sel.cursor.1));
            let mut out = Vec::new();
            for r in s_row..=e_row {
                for c in s_col..=e_col {
                    out.push((c, r));
                }
            }
            out
        }
    }
}

fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

/// The full word that `cursor` sits inside, or `None` when the cursor
/// is on whitespace / past end-of-buffer. Used by `*` and `#` to seed
/// a search from whatever the user is hovering. "Word" = run of
/// non-whitespace chars (same definition as `w`/`b`/`e`); we don't
/// split on punctuation the way vim's strict iskeyword does, since
/// the chat tends to surface compound tokens (paths, snake_case,
/// kebab-case) that the user wants searched as one unit.
fn word_under_cursor(s: &str, cursor: usize) -> Option<String> {
    let suffix = &s[cursor..];
    let first = suffix.chars().next()?;
    if first.is_whitespace() {
        return None;
    }
    let mut start = cursor;
    for (i, c) in s[..cursor].char_indices().rev() {
        if c.is_whitespace() {
            break;
        }
        start = i;
    }
    let mut end = cursor;
    for (i, c) in s[cursor..].char_indices() {
        if c.is_whitespace() {
            break;
        }
        end = cursor + i + c.len_utf8();
    }
    Some(s[start..end].to_string())
}

/// Sum of unwrapped Line rows produced by every history entry strictly
/// before `entry_idx`, including the inter-entry spacers
/// [`needs_spacer`] would insert. This is what `n` / `N` use to
/// approximate the wrapped-line offset of the matched entry without
/// re-running `Paragraph::line_count` over a partial history.
///
/// Wrap is ignored — short prose lines wrap to 1 visual row anyway,
/// and a slightly-too-low scroll value just leaves the match a few
/// rows below the top of the viewport. Cheaper than the exact count;
/// the user can fine-tune with `j` / `k`.
fn unwrapped_lines_before(
    history: &[Entry],
    entry_idx: usize,
    frame: usize,
    username: &str,
) -> usize {
    let mut count = 0usize;
    let mut prev: Option<&Entry> = None;
    for (i, entry) in history.iter().enumerate() {
        if i == entry_idx {
            break;
        }
        if let Some(p) = prev {
            if needs_spacer(p, entry) {
                count += 1;
            }
        }
        count += render_entry(entry, frame, username).len();
        prev = Some(entry);
    }
    count
}

/// Position scroll so the entry holding the current `search_idx` match
/// is near the top of the visible chat window. Approximation only —
/// uses unwrapped line counts and the previous frame's total — but
/// gets the user close enough that the highlighted match is on screen.
fn scroll_to_current_match(state: &mut State) {
    let Some(idx) = state.search_idx else { return };
    let Some(hit) = state.search_matches.get(idx) else {
        return;
    };
    let entry_idx = hit.entry_idx;
    let lines_before =
        unwrapped_lines_before(&state.history, entry_idx, state.frame, &state.username);
    let total = state.last_total_lines;
    // `state.scroll` is "rows away from the bottom"; setting it to
    // (total - lines_before) puts lines_before at the top of the
    // viewport. saturating_sub guards the entry-at-bottom case where
    // lines_before > total (shouldn't happen, but cheap to defend).
    let target = total.saturating_sub(lines_before);
    state.scroll = u16::try_from(target).unwrap_or(u16::MAX);
    state.follow_bottom = false;
    state.status = format!("match {} of {}", idx + 1, state.search_matches.len());
}

/// Vim `n`: advance to the next search match, wrapping past the end.
/// No-op on an empty match list (with a status note).
fn search_next(state: &mut State) {
    if state.search_matches.is_empty() {
        state.status = "no search results".into();
        return;
    }
    let len = state.search_matches.len();
    state.search_idx = Some(match state.search_idx {
        Some(i) => (i + 1) % len,
        None => 0,
    });
    scroll_to_current_match(state);
}

/// Vim `N`: back to the previous search match, wrapping past the start.
fn search_prev(state: &mut State) {
    if state.search_matches.is_empty() {
        state.status = "no search results".into();
        return;
    }
    let len = state.search_matches.len();
    state.search_idx = Some(match state.search_idx {
        Some(i) => (i + len - 1) % len,
        None => len - 1,
    });
    scroll_to_current_match(state);
}

/// Scan `state.history` for every byte-offset occurrence of `pattern`,
/// update `state.search_matches` + `state.search_pattern`, point
/// `state.search_idx` at the first hit (or `None` when nothing
/// matched), and write a status message. Substring matching only — no
/// regex, no case folding. An empty `pattern` clears the search.
fn run_search(state: &mut State, pattern: String) {
    if pattern.is_empty() {
        state.search_pattern = None;
        state.search_matches.clear();
        state.search_idx = None;
        state.status = "search cleared".into();
        return;
    }
    let mut hits = Vec::new();
    for (entry_idx, entry) in state.history.iter().enumerate() {
        let text = entry.searchable_text();
        let plen = pattern.len();
        let mut start = 0usize;
        while let Some(rel) = text[start..].find(&pattern) {
            hits.push(SearchHit {
                entry_idx,
                byte_offset: start + rel,
                len: plen,
            });
            // Advance past this match to find the next one. Step by
            // the pattern's full length so overlapping occurrences
            // aren't double-counted (e.g. "aaa" with pattern "aa"
            // yields one hit at offset 0, not two at 0 and 1).
            start += rel + plen;
            if start >= text.len() {
                break;
            }
        }
    }
    let count = hits.len();
    state.search_matches = hits;
    state.search_pattern = Some(pattern.clone());
    state.search_idx = if count == 0 { None } else { Some(0) };
    state.status = if count == 0 {
        format!("no matches for '{pattern}'")
    } else {
        format!("{count} match(es) for '{pattern}' · n/N to cycle")
    };
    if count > 0 {
        scroll_to_current_match(state);
    }
}

/// Pop the most recent snapshot off `input_undo`, save the current
/// input on `input_redo`, and restore the popped snapshot. No-op when
/// the undo stack is empty. `skip_input_undo_record` is set so the
/// outer event loop's diff-based recorder doesn't capture this swap
/// as a new edit.
fn undo_input(state: &mut State) {
    let Some(prev) = state.input_undo.pop() else {
        state.status = "already at oldest input".into();
        return;
    };
    state
        .input_redo
        .push((std::mem::take(&mut state.input), state.input_cursor));
    if state.input_redo.len() > INPUT_UNDO_CAP {
        state.input_redo.remove(0);
    }
    state.input = prev.0;
    state.input_cursor = prev.1;
    state.skip_input_undo_record = true;
    state.recall_idx = None;
}

/// Pop the most recent snapshot off `input_redo`, save the current
/// input on `input_undo`, and restore the popped snapshot. Mirror of
/// [`undo_input`]; runs on Ctrl-r in Normal.
fn redo_input(state: &mut State) {
    let Some(next) = state.input_redo.pop() else {
        state.status = "already at newest input".into();
        return;
    };
    state
        .input_undo
        .push((std::mem::take(&mut state.input), state.input_cursor));
    if state.input_undo.len() > INPUT_UNDO_CAP {
        state.input_undo.remove(0);
    }
    state.input = next.0;
    state.input_cursor = next.1;
    state.skip_input_undo_record = true;
    state.recall_idx = None;
}

/// Replace the input char at `state.input_cursor` with `c`. No-op when
/// the cursor is at end-of-buffer or the input is empty (vim's `r`
/// behaves the same way — there's no char to replace). Preserves
/// multi-byte boundaries by deleting the next char and inserting `c`
/// at the same cursor position rather than splicing bytes directly.
fn replace_char_at_cursor(state: &mut State, c: char) {
    let Some(existing) = state.input[state.input_cursor..].chars().next() else {
        return;
    };
    let end = state.input_cursor + existing.len_utf8();
    state
        .input
        .replace_range(state.input_cursor..end, &c.to_string());
}

/// Overwrite the input char at `state.input_cursor` with `c` and advance
/// the cursor past it. At end-of-buffer the char is appended (matches
/// vim's `R`, which behaves like Insert once you walk off the end of the
/// original buffer). Multi-byte safe.
fn overwrite_char_at_cursor(state: &mut State, c: char) {
    match state.input[state.input_cursor..].chars().next() {
        Some(existing) => {
            let end = state.input_cursor + existing.len_utf8();
            state
                .input
                .replace_range(state.input_cursor..end, &c.to_string());
            state.input_cursor += c.len_utf8();
        }
        None => {
            state.input.insert(state.input_cursor, c);
            state.input_cursor += c.len_utf8();
        }
    }
}

/// Enter Visual mode with the given selection kind: drop a fresh
/// selection at the top-left of the inner chat area and switch mode. If
/// the chat area hasn't been drawn yet (no log_area set), bail and stay
/// in Normal — there's nowhere meaningful to place the cursor. Char is
/// vim's `v`, Line is vim's `V`, Block is vim's `Ctrl-v`.
fn enter_visual(state: &mut State, kind: SelectionKind) {
    let inner = selection_inner_area(state.log_area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    state.selection = Some(Selection::with_kind(kind, inner.x, inner.y));
    state.mode = Mode::Visual;
}

/// Leave Visual mode for Normal. If `keep_selection` is true the
/// highlight stays visible (matches mouse-up behavior after a successful
/// yank); otherwise the selection is cleared.
fn exit_visual(state: &mut State, keep_selection: bool) {
    if !keep_selection {
        state.selection = None;
    }
    state.mode = Mode::Normal;
}

/// Move the Visual-mode cursor by (dx, dy) cells, clamped to the inner
/// chat area. The anchor stays put — only `selection.cursor` moves, so
/// the highlight grows/shrinks like vim's `v`.
fn move_visual_cursor(state: &mut State, dx: i32, dy: i32) {
    let inner = selection_inner_area(state.log_area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if let Some(sel) = state.selection.as_mut() {
        let max_col = inner.x + inner.width - 1;
        let max_row = inner.y + inner.height - 1;
        let new_col = i32::from(sel.cursor.0)
            .saturating_add(dx)
            .clamp(i32::from(inner.x), i32::from(max_col));
        let new_row = i32::from(sel.cursor.1)
            .saturating_add(dy)
            .clamp(i32::from(inner.y), i32::from(max_row));
        sel.cursor = (new_col as u16, new_row as u16);
    }
}

/// Snap the Visual cursor to the start (`0`) or end (`$`) of its current
/// row — same row, leftmost or rightmost column of the inner chat area.
fn snap_visual_cursor_x(state: &mut State, to_start: bool) {
    let inner = selection_inner_area(state.log_area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if let Some(sel) = state.selection.as_mut() {
        sel.cursor.0 = if to_start {
            inner.x
        } else {
            inner.x + inner.width - 1
        };
    }
}

/// Apply a single readline-style edit to `state.input` / `input_cursor`.
/// Subset of the Insert-mode handler: just the keys that mutate the
/// input buffer — no menu, no recall, no scroll, no submit. Used inside
/// `run_turn` so the user can keep typing while a turn streams.
/// Insert text from a bracketed-paste event into whichever input
/// buffer is currently focused: the masked API-key buffer takes
/// precedence (matches the focus order in the outer event loop),
/// then Insert mode targets `state.input`, Command mode targets
/// `state.command_buf`, and Normal mode swallows the paste.
fn insert_paste(state: &mut State, text: &str) {
    if let Some(entry) = state.pending_key_entry.as_mut() {
        entry.buf.push_str(text);
        return;
    }
    match state.mode {
        Mode::Insert | Mode::Replace => {
            // Bracketed paste in Replace overwrites starting at the
            // cursor: walk the pasted text char-by-char and apply
            // the same logic as a series of Replace keystrokes.
            // Insert is unchanged — splice the whole string in.
            if state.mode == Mode::Replace {
                for c in text.chars() {
                    let existing = state.input[state.input_cursor..].chars().next();
                    match existing {
                        Some(ex) => {
                            let end = state.input_cursor + ex.len_utf8();
                            state
                                .input
                                .replace_range(state.input_cursor..end, &c.to_string());
                            state.input_cursor += c.len_utf8();
                        }
                        None => {
                            state.input.insert(state.input_cursor, c);
                            state.input_cursor += c.len_utf8();
                        }
                    }
                }
            } else {
                state.input.insert_str(state.input_cursor, text);
                state.input_cursor += text.len();
            }
            state.recall_idx = None;
        }
        Mode::Command => {
            state.command_buf.insert_str(state.command_cursor, text);
            state.command_cursor += text.len();
        }
        Mode::Search => {
            state.search_buf.insert_str(state.search_cursor, text);
            state.search_cursor += text.len();
        }
        Mode::Normal | Mode::Visual => {}
    }
}

fn handle_input_edit(state: &mut State, key: KeyEvent) {
    match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => match c {
            'a' => state.input_cursor = 0,
            'e' => state.input_cursor = state.input.len(),
            'u' => {
                state.input.clear();
                state.input_cursor = 0;
            }
            'w' => delete_word_before_cursor(state),
            _ => {}
        },
        KeyCode::Char(c) => {
            state.input.insert(state.input_cursor, c);
            state.input_cursor += c.len_utf8();
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
        KeyCode::Left => {
            if let Some((i, _)) = state.input[..state.input_cursor].char_indices().next_back() {
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
        _ => {}
    }
}

/// Mouse-event dispatch shared by the outer event loop and the in-turn
/// event drain. Scroll wheel adjusts `state.scroll`; left-button
/// down/drag/up drives drag-select + clipboard copy. Returns `io::Result`
/// because the drag-release path re-issues `terminal.draw()` to capture
/// the just-rendered buffer (see the Up(Left) arm).
fn handle_mouse<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut State,
    m: MouseEvent,
) -> std::io::Result<()> {
    match m.kind {
        MouseEventKind::ScrollUp => scroll_up(state, 3),
        MouseEventKind::ScrollDown => scroll_down(state, 3),
        MouseEventKind::Down(MouseButton::Left) => {
            // Any new click clears the previous selection's highlight; a
            // click inside the chat area also begins a fresh selection.
            let inner = selection_inner_area(state.log_area);
            state.selection = if rect_contains(inner, m.column, m.row) {
                Some(Selection::new(m.column, m.row))
            } else {
                None
            };
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(sel) = state.selection.as_mut() {
                sel.cursor = (m.column, m.row);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(sel) = state.selection {
                if !sel.is_point() {
                    // `terminal.current_buffer_mut()` returns the back
                    // buffer that `swap_buffers()` just reset at the end
                    // of the previous `draw()` — empty cells. The
                    // just-rendered buffer is only reachable via the
                    // `CompletedFrame` returned by `terminal.draw()`.
                    // Re-issuing a draw here and reading from
                    // `cf.buffer` is the same trick the Visual-mode `y`
                    // arm uses; without it `extract_selection_text`
                    // walks cleared cells, the `!text.is_empty()` guard
                    // skips the copy, and drag-select silently does
                    // nothing.
                    let cf = terminal.draw(|f| draw(f, state))?;
                    let inner = selection_inner_area(state.log_area);
                    let text = extract_selection_text(cf.buffer, sel, inner);
                    if !text.is_empty() {
                        copy_to_clipboard(&text, state);
                    }
                }
            }
        }
        // Middle-click pastes from the system clipboard — completes the
        // drag-select-to-copy pair the terminal's own middle-click handler
        // would have done if mouse capture weren't on.
        MouseEventKind::Down(MouseButton::Middle) => {
            paste_from_clipboard(state);
        }
        _ => {}
    }
    Ok(())
}

/// Read the system clipboard via arboard and route the text through
/// [`insert_paste`] so it lands in whichever buffer matches the current
/// mode (input / command / pending key entry). Sets the status line on
/// success or failure so the user sees what happened.
fn paste_from_clipboard(state: &mut State) {
    match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
        Ok(text) if !text.is_empty() => {
            let chars = text.chars().count();
            insert_paste(state, &text);
            state.status = format!("pasted {chars} chars from clipboard");
        }
        Ok(_) => {
            state.status = "clipboard is empty".to_string();
        }
        Err(_) => {
            state.status = "clipboard read failed (no display server?)".to_string();
        }
    }
}

/// Push selected text to the system clipboard. Tries one channel at a
/// time and stops at the first one that succeeds — running every channel
/// in parallel makes them race to set the wlr-data-control selection (or
/// kitty/foot/wezterm's internal clipboard via OSC 52), and the loser's
/// stale or half-decoded write silently clobbers the winner's good one.
///
/// Order of preference:
///
/// 1. **`wl-copy`** (only when `$WAYLAND_DISPLAY` is set) — shells out to
///    the canonical Wayland clipboard tool, which forks a daemon that
///    keeps the selection alive after we return. arboard on Wayland uses
///    `wlr-data-control` and destroys its data source the moment the
///    `Clipboard` struct drops at end of this expression, so the
///    compositor clears the selection and pasting elsewhere comes up
///    blank even though `set_text` returned `Ok` — hence the bypass.
/// 2. **arboard** (non-Wayland only) — direct system-clipboard API.
///    Talks to X11 via xclip/xsel (which fork their own daemons) or the
///    OS clipboard on macOS / Windows.
/// 3. **`tmux load-buffer -w`** (only when `$TMUX` is set) — populates
///    tmux's internal paste buffer AND asks tmux to forward the
///    selection to the outer terminal via OSC 52. Used when neither
///    wl-copy nor arboard can reach a display server (typical
///    SSH-into-tmux case).
/// 4. **Direct OSC 52** (`ESC ] 52 ; c ; <base64> BEL`, plus a tmux
///    DCS-passthrough copy when in tmux) — terminal-escape route that
///    rides the TTY across SSH when none of the above are available.
///    Honored by foot, kitty, alacritty (with `clipboard.write.enabled`),
///    wezterm, iTerm2, Windows Terminal, modern xterm. Capped at
///    [`OSC52_MAX_RAW_BYTES`] because xterm silently truncates oversize
///    sequences — a selection past that limit reports a clear error
///    instead of a misleading "copied" status.
///
/// The status line names the channel that won, so future "paste is
/// blank" reports point at the right layer.
fn copy_to_clipboard(text: &str, state: &mut State) {
    let chars = text.chars().count();
    let in_tmux = std::env::var_os("TMUX").is_some();
    let in_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();

    if in_wayland {
        if wl_copy(text).is_ok() {
            state.status = format!("copied {chars} chars to clipboard (wl-copy)");
            return;
        }
        // Deliberately skip arboard here: on Wayland it reports Ok but
        // the selection evaporates with the Clipboard struct — falling
        // through to it would just lie to the user.
    } else {
        if state.clipboard.is_none() {
            state.clipboard = arboard::Clipboard::new().ok();
        }
        let arboard_ok = state
            .clipboard
            .as_mut()
            .map(|cb| cb.set_text(text.to_string()).is_ok())
            .unwrap_or(false);
        if arboard_ok {
            state.status = format!("copied {chars} chars to clipboard (arboard)");
            return;
        }
    }
    if in_tmux && tmux_load_buffer(text).is_ok() {
        state.status = format!("copied {chars} chars to clipboard (tmux)");
        return;
    }
    // OSC 52 ride: most terminals cap the entire OSC sequence at 64KB
    // (xterm's default `controlSeqMaxLen`). base64 inflates input by 4/3,
    // so a selection larger than ~48KB raw would be silently truncated
    // downstream — the user sees "copied" in the status line and a
    // partial / blank paste at the other end. Refuse with an explicit
    // status instead of pretending it worked.
    if text.len() > OSC52_MAX_RAW_BYTES {
        state.status = format!(
            "clipboard: {chars} chars ({} bytes) exceeds OSC 52 safe size (~{OSC52_MAX_RAW_BYTES} bytes); copy a smaller selection",
            text.len()
        );
        return;
    }
    if emit_osc52(text).is_ok() {
        state.status = format!("copied {chars} chars to clipboard (osc52)");
        return;
    }
    state.status = "clipboard error: no channel succeeded".to_string();
}

/// Largest raw selection (in bytes) we'll route through OSC 52. xterm's
/// default `controlSeqMaxLen` is 65536, and base64 inflates input by 4/3,
/// so 48000 bytes encodes to ~64000 base64 chars plus ~10 bytes of
/// `ESC ] 52 ; c ; … BEL` wrapper — well under the ceiling. Other
/// terminals are typically more generous (foot 256KB, kitty / iTerm2 /
/// WezTerm 1MB+); the xterm default is the floor we pin to.
const OSC52_MAX_RAW_BYTES: usize = 48_000;

/// Copy the most recent non-empty assistant response to the clipboard.
/// Shared by the `:copy` / `:yank` ex-commands and the `y` Normal-mode
/// keybind so both routes report the same status and the same
/// "nothing to copy yet" error when scrollback has no assistant turn.
fn yank_last_assistant(state: &mut State) {
    let last = state.history.iter().rev().find_map(|e| match e {
        Entry::Assistant { text, .. } if !text.is_empty() => Some(text.clone()),
        _ => None,
    });
    match last {
        Some(text) => {
            let len = text.chars().count();
            copy_to_clipboard(&text, state);
            state.push(Entry::Info(format!("copied {len} chars to clipboard")));
        }
        None => state.push(Entry::Error("nothing to copy yet".into())),
    }
}

/// Run `wl-copy` with the selection piped on stdin. wl-copy reads stdin
/// to EOF, forks a background daemon that holds the wlr-data-control
/// selection alive after the foreground process exits, then returns
/// success — so the selection persists across our `wait()` and after
/// teleia exits, unlike arboard's in-process Clipboard which dies the
/// moment its struct drops. Any spawn or non-zero exit bubbles up so
/// [`copy_to_clipboard`] can fall back to other channels.
fn wl_copy(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("wl-copy: no stdin"))?;
        stdin.write_all(text.as_bytes())?;
        // stdin drops here, closing the pipe so wl-copy sees EOF and forks.
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("wl-copy exited {status}")))
    }
}

/// Run `tmux load-buffer -w -` with the selection piped on stdin. The
/// `-w` flag (tmux 3.2+) tells tmux to also forward the buffer to the
/// outer terminal via its own OSC 52 emission. Any spawn or non-zero
/// exit bubbles up so [`copy_to_clipboard`] can fall back to other
/// channels.
fn tmux_load_buffer(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("tmux")
        .args(["load-buffer", "-w", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("tmux load-buffer: no stdin"))?;
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "tmux load-buffer exited {status}"
        )))
    }
}

/// Write an OSC 52 clipboard-set sequence directly to stdout. Inside
/// crossterm raw mode the TUI redraws every frame, so an emulator that
/// chooses to display the OSC payload would have it overwritten on the
/// next paint — but modern terminals consume it silently. When `$TMUX`
/// is set, also emit the DCS-passthrough wrapped form so the outer
/// terminal receives the OSC even if the user hasn't set
/// `set -g set-clipboard on` in their tmux config.
fn emit_osc52(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let bytes = osc52_payload(text, std::env::var_os("TMUX").is_some());
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(bytes.as_bytes())?;
    stdout.flush()
}

/// Build the OSC 52 clipboard-set escape, optionally followed by a
/// tmux-passthrough-wrapped copy of the same bytes. Split out from
/// [`emit_osc52`] so the wrapping logic is testable without owning
/// stdout.
///
/// Tmux passthrough is `ESC P tmux ; <payload with every ESC doubled>
/// ESC \\` — tmux strips the wrapper and forwards the inner bytes to
/// the host terminal. Requires `allow-passthrough on` (default in
/// tmux 3.3+).
fn osc52_payload(text: &str, in_tmux: bool) -> String {
    let encoded = base64_encode(text.as_bytes());
    let bare = format!("\x1b]52;c;{encoded}\x07");
    if in_tmux {
        let inner = bare.replace('\x1b', "\x1b\x1b");
        format!("{bare}\x1bPtmux;{inner}\x1b\\")
    } else {
        bare
    }
}

/// Standard base64 (RFC 4648 §4) without padding-stripping, alphabet
/// `A-Za-z0-9+/`. Hand-rolled to match the no-base64-crate posture of
/// `sha256` in teleia-tools — OSC 52 is the only consumer.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let (chunks, rem) = input.as_chunks::<3>();
    for chunk in chunks {
        let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | (chunk[2] as u32);
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        out.push(ALPHABET[(n & 0x3f) as usize] as char);
    }
    match rem.len() {
        1 => {
            let n = (rem[0] as u32) << 16;
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

/// Pull the symbol from each cell in the selection range into a string,
/// inserting a newline between rows. Trailing spaces on every line are
/// trimmed so block highlights of short text don't drag a slab of padding
/// onto the user's clipboard, and the chat block's top-padding row plus
/// any empty rows below the last entry get peeled off the top and bottom
/// — a drag that overshoots into the empty chat area otherwise pastes as
/// `<text>\n\n\n\n\n`.
fn extract_selection_text(buf: &ratatui::buffer::Buffer, sel: Selection, area: Rect) -> String {
    let cells = selection_cells(sel, area);
    if cells.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut current_row = cells[0].1;
    let mut line_buf = String::new();
    for (col, row) in cells {
        if row != current_row {
            out.push_str(line_buf.trim_end());
            out.push('\n');
            line_buf.clear();
            current_row = row;
        }
        if let Some(cell) = buf.cell((col, row)) {
            line_buf.push_str(cell.symbol());
        }
    }
    out.push_str(line_buf.trim_end());
    out.trim_matches('\n').to_string()
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

/// Strip JSON noise from tool args so the permission prompt shows
/// something readable. For bash we surface the `command` field
/// directly; otherwise we pretty-print the whole JSON object. Output is
/// truncated to `max` chars with a trailing `…` so it can't blow the
/// approval row off the screen.
fn compact_tool_args(raw: &str, max: usize) -> String {
    let trimmed = if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
        if let Some(cmd) = v.get("command").and_then(|c| c.as_str()) {
            cmd.to_string()
        } else {
            // Compact JSON: no whitespace.
            v.to_string()
        }
    } else {
        raw.to_string()
    };
    // Collapse any embedded newlines so the prompt stays single-line.
    let single = trimmed.replace('\n', " ");
    if single.chars().count() > max && max > 1 {
        let head: String = single.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    } else {
        single
    }
}

/// How many visual rows a prompt buffer needs once wrapped at the input
/// box's inner width. Row 0 starts with the prefix offset (`"> "` or
/// `": "`); rows 1+ start at column 0. Explicit `'\n'` breaks the line
/// regardless of column. Always returns at least 1.
fn input_visual_rows(buf: &str, line_width: usize, prefix_width: usize) -> usize {
    if line_width <= prefix_width {
        return 1;
    }
    let mut rows = 1usize;
    let mut col = prefix_width;
    for c in buf.chars() {
        if c == '\n' {
            rows += 1;
            col = 0;
            continue;
        }
        if col >= line_width {
            rows += 1;
            col = 0;
        }
        col += 1;
    }
    rows
}

/// Split a prompt buffer into the strings that go on each rendered row.
/// Mirrors the geometry of [`input_visual_rows`] so the cursor position
/// computed by [`cursor_visual_pos`] lines up with what's drawn. Row 0
/// gets one prefix-width's worth of leading slack (because the prompt
/// itself eats those cells); subsequent rows start at column 0.
fn wrap_input_lines(buf: &str, line_width: usize, prefix_width: usize) -> Vec<String> {
    if line_width <= prefix_width {
        return vec![buf.to_string()];
    }
    let mut out: Vec<String> = vec![String::new()];
    let mut col = prefix_width;
    for c in buf.chars() {
        if c == '\n' {
            out.push(String::new());
            col = 0;
            continue;
        }
        if col >= line_width {
            out.push(String::new());
            col = 0;
        }
        out.last_mut().unwrap().push(c);
        col += 1;
    }
    out
}

/// Where the cursor lands inside the input box after rendering the
/// portion of `buf` before `cursor_byte`. Returns `(col, row)` in the
/// input-box's *inner* coordinate space (excluding borders). Matches the
/// wrap geometry of [`wrap_input_lines`].
fn cursor_visual_pos(
    buf: &str,
    cursor_byte: usize,
    line_width: usize,
    prefix_width: usize,
) -> (u16, u16) {
    if line_width <= prefix_width {
        return (prefix_width as u16, 0);
    }
    let mut row: u16 = 0;
    let mut col: u16 = prefix_width as u16;
    for (b, c) in buf.char_indices() {
        if b >= cursor_byte {
            break;
        }
        if c == '\n' {
            row += 1;
            col = 0;
            continue;
        }
        if col >= line_width as u16 {
            row += 1;
            col = 0;
        }
        col += 1;
    }
    // If we landed exactly at the wrap point, push the cursor onto the
    // next row's column 0 — most terminals render that cleaner than
    // sitting one past the right border.
    if col >= line_width as u16 {
        row += 1;
        col = 0;
    }
    (col, row)
}

/// Compact "hf.co/FoolDev/Thanatos-27B-HERETIC:Q4_K_M" → "Thanatos-27B-HERETIC" for status-
/// bar use: strip any leading `provider:` selector, then take the segment
/// after the last '/', then drop the Ollama quant tag from the first ':'.
/// Leaves short names like "llama3" alone.
fn short_model(model: &str) -> &str {
    let resolved = teleia_llm::resolve_model_name(model);
    let tail = resolved.rsplit('/').next().unwrap_or(resolved);
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
/// Probe `/etc/os-release` (Linux) and the build target (macOS / BSD /
/// Windows) to pick a distro identity for the welcome banner. Returns
/// one of the keys understood by [`distro_art`]; falls back to
/// `"linux"` or `"unknown"` on detection failure.
fn detect_distro() -> &'static str {
    if cfg!(target_os = "macos") {
        return "macos";
    }
    if cfg!(target_os = "windows") {
        return "windows";
    }
    if cfg!(target_os = "freebsd") {
        return "freebsd";
    }
    if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
        for line in content.lines() {
            if let Some(id) = line.strip_prefix("ID=") {
                let id = id.trim().trim_matches('"').to_lowercase();
                return match id.as_str() {
                    "arch" | "manjaro" | "endeavouros" | "garuda" | "artix" => "arch",
                    "ubuntu" | "linuxmint" | "pop" | "elementary" | "zorin" => "ubuntu",
                    "debian" | "raspbian" => "debian",
                    "fedora" | "rhel" | "centos" | "rocky" | "almalinux" => "fedora",
                    "alpine" | "postmarketos" => "alpine",
                    "nixos" => "nixos",
                    "gentoo" => "gentoo",
                    "void" => "void",
                    "opensuse" | "opensuse-tumbleweed" | "opensuse-leap" | "suse" => "suse",
                    _ => "linux",
                };
            }
        }
        return "linux";
    }
    "unknown"
}

/// Pretty display name for the banner — capitalised, branded forms
/// (NixOS, openSUSE) where appropriate.
fn distro_display_name(id: &str) -> &'static str {
    match id {
        "arch" => "Arch Linux",
        "ubuntu" => "Ubuntu",
        "debian" => "Debian",
        "fedora" => "Fedora",
        "alpine" => "Alpine Linux",
        "nixos" => "NixOS",
        "gentoo" => "Gentoo",
        "void" => "Void Linux",
        "suse" => "openSUSE",
        "linux" => "Linux",
        "macos" => "macOS",
        "windows" => "Windows",
        "freebsd" => "FreeBSD",
        _ => "Terminal",
    }
}

/// Block-character pixel art for each known distro. Width-normalised
/// (all rows the same `chars().count()`) so the centring math stays
/// simple. Each panel is sized to fit comfortably in a 120-col TUI.
fn distro_art(id: &str) -> &'static [&'static str] {
    match id {
        "arch" => &[
            "        ▟▙        ",
            "       ▟██▙       ",
            "      ▟████▙      ",
            "     ▟██▟▙██▙     ",
            "    ▟███  ███▙    ",
            "   ▟████  ████▙   ",
            "  ▟█████  █████▙  ",
            " ▟██████  ██████▙ ",
            "▟████████████████▙",
        ],
        "ubuntu" => &[
            "                  ",
            "       ▟██▙       ",
            "      ███████     ",
            "     ██     ●●    ",
            "    ●●  ██████    ",
            "     ██     ●●    ",
            "      ███████     ",
            "       ▜██▛       ",
            "                  ",
        ],
        "debian" => &[
            "                  ",
            "     ▄▄▄▄▄▄▄      ",
            "   ▄█████████▄    ",
            "  ███▀▀   ▀▀███   ",
            "  ██▀         █   ",
            "  ██   ▄▄▄▄▄  █   ",
            "   ██  ▀▀▀▀█▄     ",
            "    ▀▀▀▀▀▀▀       ",
            "                  ",
        ],
        "fedora" => &[
            "      ▄▄▄▄▄▄      ",
            "    ▄██████████   ",
            "   ██▀▀▀▀▀▀▀██▀   ",
            "  ██   ██████     ",
            "  ██   ██         ",
            "  ██   ██████     ",
            "  ██   ██         ",
            "   ██▄▄▄▄▄▄▄▄     ",
            "    ▀▀▀▀▀▀▀▀      ",
        ],
        "alpine" => &[
            "                  ",
            "        ▲         ",
            "       ▲▲▲        ",
            "      ▲   ▲       ",
            "     ▲ ▲▲▲ ▲      ",
            "    ▲ ▲   ▲ ▲     ",
            "   ▲           ▲  ",
            "  ▲▲▲▲▲▲▲▲▲▲▲▲▲▲  ",
            "                  ",
        ],
        "nixos" => &[
            "    ▟▙        ▟▙  ",
            "    ██▙      ▟██  ",
            "     ██▙    ▟██   ",
            "  ▟▙▟███▙  ▟███▙▟▙",
            " ███████████████  ",
            "  ▟▙▜███▛  ▜███▛▟▙",
            "     ██▛    ▜██   ",
            "    ██▛      ▜██  ",
            "    ▜▛        ▜▛  ",
        ],
        "gentoo" => &[
            "    ▄▄▄▄▄         ",
            "   ████████▄      ",
            "    ████████▄     ",
            "       ▀█████▄    ",
            "      ▄█████▀     ",
            "     ████▀        ",
            "    ████          ",
            "    ▀▀▀           ",
            "                  ",
        ],
        "void" => &[
            "      ▄▄▄▄▄       ",
            "    ▄███████▄  ▄  ",
            "   ██▀  ▄  ▀██ █  ",
            "  ██   ███   ██   ",
            "  ██   ▀▀▀   ██   ",
            "   ██▄  ▀  ▄██    ",
            "  ▄ ▀███████▀     ",
            "  ▀   ▀▀▀▀▀       ",
            "                  ",
        ],
        "suse" => &[
            "                  ",
            "     ▄▄▄▄▄▄▄▄     ",
            "    █  ▄▄▄▄  █    ",
            "    █  █  █  █    ",
            "    █  █  █  █    ",
            "    █  ▀▀▀▀  █    ",
            "     ▀▀▀▀▀▀▀▀     ",
            "         ▀▀       ",
            "                  ",
        ],
        "macos" => &[
            "         ▟▙       ",
            "         ▜▛       ",
            "      ▄▄▄▄▄▄▄     ",
            "    ▄██████████   ",
            "   ███████████▀   ",
            "   ██████████     ",
            "    ████████      ",
            "     ▀████▀       ",
            "      ▀▀▀▀        ",
        ],
        "windows" => &[
            "                  ",
            "   ████▌ ▐████    ",
            "   ████▌ ▐████    ",
            "   ████▌ ▐████    ",
            "    ▀▀▀▀ ▀▀▀▀     ",
            "   ████▌ ▐████    ",
            "   ████▌ ▐████    ",
            "   ████▌ ▐████    ",
            "                  ",
        ],
        "freebsd" => &[
            "      ▄▄▄▄▄▄      ",
            "    ▄████████▄    ",
            "   ██▀  ▄▄  ▀██   ",
            "   ██  ▐██▌  ██   ",
            "   ██   ▀▀   ██   ",
            "    ▀█▄▄▄▄▄▄█▀    ",
            "      ▀████▀      ",
            "       ▀▀▀▀       ",
            "                  ",
        ],
        // "linux" and unknown fall back to a generic prompt mark.
        _ => &[
            "                  ",
            "                  ",
            "    ▟▙            ",
            "    ▜▛  ████      ",
            "    ▟▙  ▀▀▀▀      ",
            "    ▜▛            ",
            "                  ",
            "                  ",
            "                  ",
        ],
    }
}

/// Narrow-terminal fallback for the welcome banner: no pixel art,
/// no λ block, just a compact text-only stack that fits inside
/// whatever width the chat block has.
fn compact_banner(distro: &str, th: &Theme, width: u16) -> Vec<Line<'static>> {
    let center = |s: &str, style: Style| -> Line<'static> {
        let pad = (width as usize).saturating_sub(s.chars().count()) / 2;
        Line::from(Span::styled(format!("{}{s}", " ".repeat(pad)), style))
    };
    let mut out = Vec::new();
    out.push(Line::from(""));
    out.push(center(
        &format!("λ τέλεια · {}", distro_display_name(distro)),
        Style::default().fg(th.purple).add_modifier(Modifier::BOLD),
    ));
    out.push(center(
        "a minimal TUI coding agent",
        Style::default().fg(th.cyan).add_modifier(Modifier::ITALIC),
    ));
    out.push(center(
        "powered by λ τέλεια",
        Style::default().fg(th.dim).add_modifier(Modifier::ITALIC),
    ));
    out.push(Line::from(""));
    out.push(center(
        "Type to start · /help · esc normal",
        Style::default().fg(th.dim),
    ));
    out
}

fn welcome_banner(width: u16, frame: usize) -> Vec<Line<'static>> {
    let th = theme();
    let distro = detect_distro();
    let art = distro_art(distro);
    let art_width = art[0].chars().count();

    // Below this threshold the pixel-art panels (18 cols + indent)
    // can't fit — drop the art entirely and emit a compact text-only
    // banner.
    if (width as usize) < art_width + 4 {
        return compact_banner(distro, &th, width);
    }

    let art_pad = (width as usize).saturating_sub(art_width) / 2;
    let art_indent = " ".repeat(art_pad);

    let mut out = Vec::new();
    out.push(Line::from(""));

    // Per-row gradient: a slow vertical sweep purple→blue→cyan
    // (top→bottom) plus a horizontal phase nudge that slides one cell
    // every few frames so the gradient reads as a slow shimmer.
    for (row_idx, row) in art.iter().enumerate() {
        let mut spans: Vec<Span<'static>> = vec![Span::raw(art_indent.clone())];
        for (col_idx, ch) in row.chars().enumerate() {
            if ch == ' ' {
                spans.push(Span::raw(" "));
                continue;
            }
            let phase = (col_idx + row_idx * 2 + frame / 4) % 12;
            let color = match phase {
                0..=3 => th.purple,
                4..=7 => th.blue,
                _ => th.cyan,
            };
            spans.push(Span::styled(
                ch.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        }
        out.push(Line::from(spans));
    }

    // Red "full stop" cursor block sitting just below the mountain
    // (matches the SVG logo). Blinks at ~1 Hz.
    let dot_visible = (frame / 10).is_multiple_of(2);
    let dot_pad = art_pad + art_width / 2;
    if dot_visible {
        out.push(Line::from(vec![
            Span::raw(" ".repeat(dot_pad)),
            Span::styled(
                "█",
                Style::default().fg(th.red).add_modifier(Modifier::BOLD),
            ),
        ]));
    } else {
        out.push(Line::from(""));
    }

    // Distro name beneath the art.
    let distro_name = distro_display_name(distro);
    let name_pad = (width as usize).saturating_sub(distro_name.chars().count()) / 2;
    out.push(Line::from(vec![
        Span::raw(" ".repeat(name_pad)),
        Span::styled(
            distro_name.to_string(),
            Style::default().fg(th.purple).add_modifier(Modifier::BOLD),
        ),
    ]));

    // Breathing row between the distro mark/name and the λ brand
    // glyph below — without it the two pixel-art blocks read as one
    // stacked thing.
    out.push(Line::from(""));

    // Pixel-art λ — the teleia brand mark, painted in the Tokyo Night
    // gradient (cyan → blue → purple top-down). Sits between the
    // distro art and the wordmark to tie the two together.
    const LAMBDA: &[&str] = &[
        "  ▟▙    ",
        "  ▜█▙   ",
        "   ▜█▙  ",
        "  ▟████▙",
        " ▟██▛▜██",
        "▟██▛  ▜█",
    ];
    let lambda_w = LAMBDA[0].chars().count();
    let lambda_pad = (width as usize).saturating_sub(lambda_w) / 2;
    let lambda_indent = " ".repeat(lambda_pad);
    let lambda_colors = [th.cyan, th.cyan, th.blue, th.blue, th.purple, th.purple];
    for (i, row) in LAMBDA.iter().enumerate() {
        let color = lambda_colors[i.min(lambda_colors.len() - 1)];
        out.push(Line::from(vec![
            Span::raw(lambda_indent.clone()),
            Span::styled(
                (*row).to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    out.push(Line::from(""));

    let center = |s: &str, style: Style| -> Line<'static> {
        let pad = (width as usize).saturating_sub(s.chars().count()) / 2;
        Line::from(Span::styled(format!("{}{s}", " ".repeat(pad)), style))
    };
    out.push(center(
        "a minimal TUI coding agent",
        Style::default().fg(th.cyan).add_modifier(Modifier::ITALIC),
    ));
    // Watermark.
    out.push(center(
        "powered by λ τέλεια",
        Style::default().fg(th.dim).add_modifier(Modifier::ITALIC),
    ));
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
/// Build the syntax-highlighting palette from the active TUI theme so
/// code blocks read with the same colour vocabulary as the surrounding
/// chat. `/theme dracula` automatically reshuffles all highlighted
/// content.
fn highlight_palette() -> highlight::Palette {
    let th = theme();
    highlight::Palette {
        keyword: th.purple,
        string: th.green,
        number: th.yellow,
        function: th.blue,
        type_: th.cyan,
        comment: th.dim,
        punctuation: th.dim,
        fg: th.fg,
    }
}

fn render_assistant_lines(text: &str) -> Vec<Line<'static>> {
    let th = theme();
    let mut out = Vec::new();
    let mut in_code = false;
    let mut code_lang = String::new();
    let mut code_buf = String::new();

    // Indent applied to every highlighted code line so blocks visually
    // float off the left margin of the surrounding prose.
    const CODE_INDENT: &str = "  ";

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("```") {
            if in_code {
                // Closing fence: flush the buffer with a leading dim
                // language label (when the open fence supplied one)
                // and a trailing blank row so prose after the block
                // gets a breath of space.
                if !code_lang.is_empty() {
                    out.push(Line::from(Span::styled(
                        format!("{CODE_INDENT} {code_lang}"),
                        Style::default().fg(th.dim).add_modifier(Modifier::ITALIC),
                    )));
                }
                for hl in highlight::highlight(
                    code_buf.trim_end_matches('\n'),
                    &code_lang,
                    CODE_INDENT,
                    highlight_palette(),
                ) {
                    out.push(hl);
                }
                out.push(Line::from(""));
                code_buf.clear();
                code_lang.clear();
                in_code = false;
            } else {
                // Opening fence: blank row before the block so it
                // doesn't crash into the preceding prose line.
                out.push(Line::from(""));
                code_lang = rest.trim().to_string();
                in_code = true;
            }
        } else if in_code {
            code_buf.push_str(line);
            code_buf.push('\n');
        } else {
            out.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(th.fg),
            )));
        }
    }

    // If the message ended mid-fence (mid-stream), flush what we have
    // so the user sees the partial code rather than nothing. No
    // trailing blank since more content is coming.
    if in_code && !code_buf.is_empty() {
        if !code_lang.is_empty() {
            out.push(Line::from(Span::styled(
                format!("{CODE_INDENT} {code_lang}"),
                Style::default().fg(th.dim).add_modifier(Modifier::ITALIC),
            )));
        }
        for hl in highlight::highlight(
            code_buf.trim_end_matches('\n'),
            &code_lang,
            CODE_INDENT,
            highlight_palette(),
        ) {
            out.push(hl);
        }
    }

    out
}

/// Render the full chat history with automatic inter-entry spacing.
/// Consecutive agent-side entries (Assistant + Tool, or Tool + Tool)
/// stay tight because they belong to the same logical turn; a blank
/// line separates one conceptual turn from the next, and Info / Error
/// rows always get a trailing breath of space.
fn render_history(history: &[Entry], frame: usize, username: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut prev: Option<&Entry> = None;
    for entry in history {
        if let Some(p) = prev {
            if needs_spacer(p, entry) {
                out.push(Line::from(""));
            }
        }
        out.extend(render_entry(entry, frame, username));
        prev = Some(entry);
    }
    out
}

/// Should a blank line sit between `prev` and `next`? `true` separates
/// conceptual turns; `false` lets a tightly-related block (assistant
/// reply + its tool calls, consecutive tools, an info burst followed by
/// the next message) read as one unit.
fn needs_spacer(prev: &Entry, next: &Entry) -> bool {
    use Entry::*;
    // A hidden reasoning entry renders no lines, so it must not pull a blank
    // spacer with it (that would leave a stray gap where the thinking was).
    if !show_thinking() && matches!((prev, next), (Reasoning { .. }, _) | (_, Reasoning { .. })) {
        return false;
    }
    match (prev, next) {
        // Inside an agent turn: assistant prose, then its tools, then
        // possibly more assistant prose. No gap.
        (Assistant { .. } | Tool { .. }, Assistant { .. } | Tool { .. }) => false,
        // A user message right after their own previous one (rare) or
        // right after an agent block always opens a fresh turn.
        (_, User(_)) => true,
        // Info / Error always get breathing room before the next entry.
        (Info(_) | Error(_), _) => true,
        // Agent reply right after a user message — no extra gap; the
        // user header already has its own one-line separator from the
        // text body inside `render_entry`.
        (User(_), _) => false,
        _ => true,
    }
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
                out.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(th.fg),
                )));
            }
        }
        Entry::Reasoning { text, complete } => {
            // `/thinking off` hides the reasoning stream. Returning no lines
            // (rather than filtering upstream) keeps every caller's line-count
            // math — scroll offsets, spacer counts — consistent.
            if !show_thinking() {
                return Vec::new();
            }
            let header = if *complete {
                "τέλεια · thinking"
            } else if (frame / 10).is_multiple_of(2) {
                "τέλεια · thinking ▌"
            } else {
                "τέλεια · thinking  "
            };
            out.push(Line::from(Span::styled(
                header,
                Style::default()
                    .fg(th.dim)
                    .add_modifier(Modifier::ITALIC | Modifier::BOLD),
            )));
            for line in text.lines() {
                out.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(th.dim).add_modifier(Modifier::ITALIC),
                )));
            }
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
                    .map(|ext| highlight::highlight(output, &ext, "  ", highlight_palette()))
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
        }
        Entry::Error(text) => {
            out.push(Line::from(Span::styled(
                format!("error: {text}"),
                Style::default().fg(th.red),
            )));
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
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encode_empty_is_empty() {
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn base64_encode_one_byte_double_pads() {
        // RFC 4648 §10: "f" → "Zg==".
        assert_eq!(base64_encode(b"f"), "Zg==");
    }

    #[test]
    fn base64_encode_two_bytes_single_pads() {
        // RFC 4648 §10: "fo" → "Zm8=".
        assert_eq!(base64_encode(b"fo"), "Zm8=");
    }

    #[test]
    fn base64_encode_three_bytes_no_pad() {
        // RFC 4648 §10: "foo" → "Zm9v".
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }

    #[test]
    fn base64_encode_rfc_test_vectors() {
        // Full RFC 4648 §10 vector table.
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_payload_bare_when_not_in_tmux() {
        let p = osc52_payload("hi", false);
        // OSC 52, clipboard selection `c`, base64 of "hi" = "aGk=", BEL terminator.
        assert_eq!(p, "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn osc52_max_raw_bytes_fits_under_xterm_default_buffer() {
        // xterm's default controlSeqMaxLen is 65536 bytes for the entire
        // OSC sequence. Base64 inflates raw bytes by 4/3 (rounded up to a
        // multiple of 4), plus ~10 bytes of `ESC ] 52 ; c ; ... BEL`
        // wrapper. Verify the cap leaves real headroom under the ceiling.
        let max_base64 = super::OSC52_MAX_RAW_BYTES.div_ceil(3) * 4;
        let max_seq_size = max_base64 + 10;
        assert!(
            max_seq_size < 65_536,
            "OSC52_MAX_RAW_BYTES={} encodes to {} OSC bytes — over xterm default",
            super::OSC52_MAX_RAW_BYTES,
            max_seq_size,
        );
    }

    #[test]
    fn insert_paste_appends_to_input_in_insert_mode() {
        let mut s = State::new("dummy", "dummy");
        s.mode = Mode::Insert;
        s.input = "hello ".to_string();
        s.input_cursor = s.input.len();
        insert_paste(&mut s, "world");
        assert_eq!(s.input, "hello world");
        assert_eq!(s.input_cursor, s.input.len());
    }

    #[test]
    fn insert_paste_targets_command_buffer_in_command_mode() {
        let mut s = State::new("dummy", "dummy");
        s.mode = Mode::Command;
        s.command_buf = "save ".to_string();
        s.command_cursor = s.command_buf.len();
        insert_paste(&mut s, "alias");
        assert_eq!(s.command_buf, "save alias");
        assert_eq!(s.command_cursor, s.command_buf.len());
        assert_eq!(s.input, ""); // input untouched
    }

    #[test]
    fn insert_paste_is_no_op_in_normal_mode() {
        let mut s = State::new("dummy", "dummy");
        s.mode = Mode::Normal;
        s.input = "untouched".to_string();
        insert_paste(&mut s, "nope");
        assert_eq!(s.input, "untouched");
    }

    #[test]
    fn insert_paste_into_pending_key_entry_overrides_mode() {
        let mut s = State::new("dummy", "dummy");
        // Even though mode is Insert, pending_key_entry takes the paste.
        s.mode = Mode::Insert;
        s.input = "should-stay".to_string();
        s.input_cursor = s.input.len();
        s.pending_key_entry = Some(KeyEntry {
            provider: "anthropic".into(),
            env_var: "ANTHROPIC_API_KEY".into(),
            buf: "sk-".into(),
            existing: false,
        });
        insert_paste(&mut s, "ant-abcdef");
        assert_eq!(s.pending_key_entry.as_ref().unwrap().buf, "sk-ant-abcdef");
        assert_eq!(s.input, "should-stay"); // input box untouched
    }

    #[test]
    fn extract_strips_top_padding_and_empty_trailing_rows() {
        use ratatui::widgets::Widget;
        // Mirror the real chat: bordered Paragraph with 1-cell padding
        // and Wrap, rendered into a buffer that's taller than the
        // content so empty rows sit below the last line.
        let area = Rect::new(0, 0, 30, 10);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        Paragraph::new(vec![Line::from("hello world")])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .padding(Padding::new(1, 1, 1, 0)),
            )
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);

        // Selection across the whole inner area — drags from top-left to
        // bottom-right, the way a user grabbing "everything in chat"
        // would. Pre-fix this returned `"\n hello world\n\n\n\n\n\n"`;
        // we want just the visible line.
        let inner = selection_inner_area(area);
        let sel = Selection {
            anchor: (inner.x, inner.y),
            cursor: (inner.x + inner.width - 1, inner.y + inner.height - 1),
            kind: SelectionKind::Char,
        };
        let text = extract_selection_text(&buf, sel, inner);
        assert_eq!(text, " hello world");
    }

    #[test]
    fn osc52_payload_includes_tmux_passthrough_when_in_tmux() {
        let p = osc52_payload("hi", true);
        // Bare OSC 52 (so tmux's own set-clipboard handler still sees
        // it), followed by the DCS-passthrough form (so the outer term
        // gets the OSC regardless of tmux config). Inside the wrapper,
        // every ESC in the payload is doubled.
        let bare = "\x1b]52;c;aGk=\x07";
        let inner_escaped = "\x1b\x1b]52;c;aGk=\x07";
        let expected = format!("{bare}\x1bPtmux;{inner_escaped}\x1b\\");
        assert_eq!(p, expected);
    }

    #[test]
    fn base64_encode_handles_non_ascii() {
        // Greek τέλεια — three multi-byte codepoints, exercises the
        // full byte-range of the alphabet table.
        let encoded = base64_encode("τέλεια".as_bytes());
        // Round-trip via a known-good reference (computed once,
        // documented inline so the test is self-contained).
        assert_eq!(encoded, "z4TOrc67zrXOuc6x");
    }

    #[test]
    fn plain_prose_passes_through_unchanged() {
        let lines = render_assistant_lines("hello\nworld");
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn fence_markers_are_stripped() {
        // ```rust opens; the fence markers themselves don't survive.
        // What we get: blank before · lang label · 1 code line · blank after.
        let lines = render_assistant_lines("```rust\nfn x() {}\n```");
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn prose_and_code_alternate() {
        // prose · blank-before · label · code · blank-after · prose.
        let lines = render_assistant_lines("before\n```rust\ncode\n```\nafter");
        assert_eq!(lines.len(), 6);
    }

    #[test]
    fn unclosed_fence_still_flushes() {
        // Mid-stream: the assistant hasn't emitted the closing ```
        // yet. We open the block (blank-before · label) and flush
        // what we have — no trailing blank since more is coming.
        let lines = render_assistant_lines("```rust\nfn x() {}\n");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn empty_text_renders_nothing() {
        assert_eq!(render_assistant_lines("").len(), 0);
    }

    #[test]
    fn unlabeled_fence_falls_back_to_plain() {
        // ``` without a language token: blank-before · code · blank-after
        // (no label row because the language is empty).
        let lines = render_assistant_lines("```\nsome code\n```");
        assert_eq!(lines.len(), 3);
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
            short_model("hf.co/FoolDev/Thanatos-27B-HERETIC:Q4_K_M"),
            "Thanatos-27B-HERETIC"
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
    fn input_visual_rows_counts_wrap_and_newlines() {
        // line_width=10, prefix=2 → first row holds 8 input chars,
        // subsequent rows hold 10. Empty buffer is 1 row.
        assert_eq!(input_visual_rows("", 10, 2), 1);
        assert_eq!(input_visual_rows("hi", 10, 2), 1);
        // 8 chars fill row 0; 1 more wraps to row 1.
        assert_eq!(input_visual_rows("12345678", 10, 2), 1);
        assert_eq!(input_visual_rows("123456789", 10, 2), 2);
        // Explicit newlines bump rows regardless of column.
        assert_eq!(input_visual_rows("a\nb\nc", 10, 2), 3);
        // 8 chars + 10 chars + 10 chars = 28 chars → 3 rows.
        assert_eq!(input_visual_rows(&"x".repeat(28), 10, 2), 3);
    }

    #[test]
    fn cursor_visual_pos_matches_wrap_geometry() {
        // Cursor at start: just past the prefix on row 0.
        assert_eq!(cursor_visual_pos("hello", 0, 10, 2), (2, 0));
        // Cursor after one char: col 3.
        assert_eq!(cursor_visual_pos("hello", 1, 10, 2), (3, 0));
        // Cursor at the wrap point of row 0 (after 8 chars): jumps to (0, 1).
        assert_eq!(cursor_visual_pos("12345678", 8, 10, 2), (0, 1));
        // After a newline, cursor sits at col 0 of the next row.
        assert_eq!(cursor_visual_pos("a\n", 2, 10, 2), (0, 1));
    }

    #[test]
    fn wrap_input_lines_splits_at_width() {
        let lines = wrap_input_lines("12345678abc", 10, 2);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "12345678"); // first row: 10 - 2 prefix = 8
        assert_eq!(lines[1], "abc");
        // Explicit newlines force a break.
        let lines = wrap_input_lines("hi\nthere", 20, 2);
        assert_eq!(lines, vec!["hi".to_string(), "there".to_string()]);
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
    fn ex_translates_effort_and_loop() {
        assert_eq!(translate_ex("effort high").unwrap(), "effort high");
        assert_eq!(
            translate_ex("loop 5 fix the tests").unwrap(),
            "loop 5 fix the tests"
        );
    }

    #[test]
    fn parse_loop_accepts_count_and_prompt() {
        assert_eq!(
            parse_loop("3 fix the tests"),
            Ok((3, "fix the tests".to_string()))
        );
        assert_eq!(parse_loop("1 go"), Ok((1, "go".to_string())));
    }

    #[test]
    fn parse_loop_clamps_to_max() {
        let (count, _) = parse_loop("100000 spin").unwrap();
        assert_eq!(count, LOOP_MAX);
    }

    #[test]
    fn parse_loop_rejects_bad_input() {
        assert!(parse_loop("").is_err()); // nothing at all
        assert!(parse_loop("5").is_err()); // count but no prompt
        assert!(parse_loop("0 go").is_err()); // zero iterations
        assert!(parse_loop("foo bar").is_err()); // non-numeric count
    }

    #[test]
    fn auto_continue_fires_only_while_armed_working_and_under_cap() {
        use TurnOutcome::*;
        // Armed, last turn completed and used a tool, under the cap → continue.
        assert!(should_auto_continue(true, Completed, true, 0));
        assert!(should_auto_continue(true, Completed, true, LOOP_MAX - 1));
        // Disarmed → never.
        assert!(!should_auto_continue(false, Completed, true, 0));
        // Turn made no tool call → the agent is done working, stop.
        assert!(!should_auto_continue(true, Completed, false, 0));
        // Interrupted / errored turn → stop.
        assert!(!should_auto_continue(true, Stopped, true, 0));
        // Safety cap reached → stop.
        assert!(!should_auto_continue(true, Completed, true, LOOP_MAX));
    }

    #[test]
    fn compact_is_a_listed_command_in_both_surfaces() {
        assert!(SLASH_COMMANDS.contains(&"compact"));
        assert!(EX_COMMANDS.contains(&"compact"));
        assert_eq!(translate_ex("compact").unwrap(), "compact");
    }

    #[test]
    fn entropy_is_a_listed_command_with_on_off_completion() {
        assert!(SLASH_COMMANDS.contains(&"entropy"));
        assert!(EX_COMMANDS.contains(&"entropy"));
        assert_eq!(arg_placeholder("entropy"), Some(" [on|off]"));
        assert_eq!(translate_ex("entropy on").unwrap(), "entropy on");
    }

    #[test]
    fn autocompact_is_a_listed_toggle_with_on_off_completion() {
        assert!(SLASH_COMMANDS.contains(&"autocompact"));
        assert!(EX_COMMANDS.contains(&"autocompact"));
        assert_eq!(arg_placeholder("autocompact"), Some(" [on|off]"));
        assert_eq!(ex_arg_placeholder("autocompact"), Some(" [on|off]"));
        assert_eq!(translate_ex("autocompact off").unwrap(), "autocompact off");
        // ghost-text autocomplete: "/autocompact o" → "on", "of" → "off"
        assert_eq!(
            arg_suggestion("autocompact", "o", &[]).map(|s| s.completion),
            Some("n".to_string())
        );
        assert_eq!(
            arg_suggestion("autocompact", "of", &[]).map(|s| s.completion),
            Some("f".to_string())
        );
    }

    #[test]
    fn thinking_is_a_listed_command_with_on_off_completion_and_dropdown() {
        assert!(SLASH_COMMANDS.contains(&"thinking"));
        assert!(EX_COMMANDS.contains(&"thinking"));
        assert_eq!(arg_placeholder("thinking"), Some(" [on|off]"));
        assert_eq!(ex_arg_placeholder("thinking"), Some(" [on|off]"));
        assert_eq!(translate_ex("thinking off").unwrap(), "thinking off");
        // ghost-text autocomplete: "/thinking o" → "on", "/thinking of" → "off"
        assert_eq!(
            arg_suggestion("thinking", "o", &[]).map(|s| s.completion),
            Some("n".to_string())
        );
        assert_eq!(
            arg_suggestion("thinking", "of", &[]).map(|s| s.completion),
            Some("f".to_string())
        );
        // dropdown offers both values, filtered by prefix
        assert_eq!(
            compute_menu("/thinking ", &[], &[], &[]).map(|m| m.items),
            Some(vec!["on".to_string(), "off".to_string()])
        );
        assert_eq!(
            compute_menu("/thinking of", &[], &[], &[]).map(|m| m.items),
            Some(vec!["off".to_string()])
        );
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
    fn ex_strips_trailing_bang_force_suffix() {
        assert_eq!(translate_ex("q!").unwrap(), "quit");
        assert_eq!(translate_ex("qa!").unwrap(), "quit");
        assert_eq!(translate_ex("qall!").unwrap(), "quit");
        assert_eq!(translate_ex("x!").unwrap(), "quit");
        assert_eq!(translate_ex("exit!").unwrap(), "quit");
        assert_eq!(translate_ex("w! foo").unwrap(), "save foo");
        assert_eq!(translate_ex("wq! foo").unwrap(), "save foo");
        assert_eq!(translate_ex("wa!").unwrap(), "save ");
    }

    #[test]
    fn ex_translates_saveas_aliases() {
        assert_eq!(translate_ex("saveas foo").unwrap(), "save foo");
        assert_eq!(translate_ex("sav foo").unwrap(), "save foo");
    }

    #[test]
    fn ex_translates_tab_and_buffer_lifecycle() {
        assert_eq!(translate_ex("tabnew").unwrap(), "reset");
        assert_eq!(translate_ex("tabe").unwrap(), "reset");
        assert_eq!(translate_ex("tabedit").unwrap(), "reset");
        assert_eq!(translate_ex("vnew").unwrap(), "reset");
        assert_eq!(translate_ex("vne").unwrap(), "reset");
        assert_eq!(translate_ex("tabc foo").unwrap(), "delete foo");
        assert_eq!(translate_ex("tabclose foo").unwrap(), "delete foo");
        assert_eq!(translate_ex("bw foo").unwrap(), "delete foo");
        assert_eq!(translate_ex("bwipe foo").unwrap(), "delete foo");
        assert_eq!(translate_ex("bwipeout foo").unwrap(), "delete foo");
    }

    #[test]
    fn ex_translates_close_and_messages_aliases() {
        assert_eq!(translate_ex("clo").unwrap(), "quit");
        assert_eq!(translate_ex("close").unwrap(), "quit");
        assert_eq!(translate_ex("mes").unwrap(), "show");
        assert_eq!(translate_ex("messages").unwrap(), "show");
    }

    #[test]
    fn ex_translates_cd_window_local_variants() {
        assert_eq!(translate_ex("lcd /tmp").unwrap(), "cd /tmp");
        assert_eq!(translate_ex("tcd /tmp").unwrap(), "cd /tmp");
    }

    #[test]
    fn ex_translates_show_and_info() {
        assert_eq!(translate_ex("show").unwrap(), "show");
        assert_eq!(translate_ex("info").unwrap(), "show");
    }

    #[test]
    fn menu_command_list_filters_by_prefix() {
        let m = compute_menu("/sa", &[], &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Command);
        assert_eq!(m.items, vec!["save"]);
    }

    #[test]
    fn menu_command_list_returns_all_for_lone_slash() {
        let m = compute_menu("/", &[], &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Command);
        assert_eq!(m.items.len(), SLASH_COMMANDS.len());
    }

    #[test]
    fn menu_command_list_none_for_unknown_prefix() {
        assert!(compute_menu("/zzz", &[], &[], &[]).is_none());
    }

    #[test]
    fn menu_alias_filters_by_prefix() {
        let aliases = vec![
            "audit-pass-1".to_string(),
            "audit-pass-2".to_string(),
            "draft".to_string(),
        ];
        let m = compute_menu("/load aud", &aliases, &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Alias);
        assert_eq!(m.items, vec!["audit-pass-1", "audit-pass-2"]);
    }

    #[test]
    fn menu_alias_shows_all_on_empty_arg() {
        let aliases = vec!["foo".to_string(), "bar".to_string()];
        let m = compute_menu("/load ", &aliases, &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Alias);
        assert_eq!(m.items.len(), 2);
    }

    #[test]
    fn menu_alias_none_when_no_aliases_match() {
        let aliases = vec!["foo".to_string()];
        assert!(compute_menu("/load zzz", &aliases, &[], &[]).is_none());
    }

    #[test]
    fn menu_none_for_non_alias_commands_with_space() {
        // /help takes no arg; once a space is typed, no menu.
        assert!(compute_menu("/help ", &[], &[], &[]).is_none());
    }

    #[test]
    fn menu_none_for_empty_or_no_slash() {
        assert!(compute_menu("", &[], &[], &[]).is_none());
        assert!(compute_menu("hello", &[], &[], &[]).is_none());
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
        let m = compute_menu("/theme dra", &[], &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Theme);
        assert_eq!(m.items, vec!["dracula"]);
    }

    #[test]
    fn menu_theme_lists_all_when_arg_empty() {
        let m = compute_menu("/theme ", &[], &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Theme);
        // Three themes today: tokyo-night, catppuccin, dracula
        assert_eq!(m.items.len(), 3);
    }

    #[test]
    fn ex_menu_filters_by_prefix() {
        let m = compute_ex_menu("the").unwrap();
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
            "hf.co/FoolDev/Thanatos-27B-HERETIC:Q4_K_M".to_string(),
            "hf.co/FoolDev/Janus-35B-HERETIC:Q4_K_M".to_string(),
        ];
        let m = compute_menu("/model hf.co", &[], &models, &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Model);
        assert_eq!(m.items.len(), 2);
    }

    #[test]
    fn menu_model_shows_all_on_empty_arg() {
        let models = vec!["a".to_string(), "b".to_string()];
        let m = compute_menu("/model ", &[], &models, &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Model);
        assert_eq!(m.items.len(), 2);
    }

    #[test]
    fn menu_model_none_when_no_models_cached() {
        // Empty model list (Ollama unreachable at startup) → no menu.
        assert!(compute_menu("/model anything", &[], &[], &[]).is_none());
    }

    #[test]
    fn menu_key_lists_providers() {
        let m = compute_menu("/key ", &[], &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Theme);
        assert_eq!(m.items.len(), teleia_llm::PROVIDERS.len());
    }

    #[test]
    fn menu_key_filters_by_prefix_case_insensitive() {
        // Provider names are TitleCase but the filter is case-insensitive,
        // so lowercase prefixes still hit them.
        let m = compute_menu("/key open", &[], &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Theme);
        assert!(!m.items.is_empty());
        assert!(m
            .items
            .iter()
            .all(|n| n.to_ascii_lowercase().starts_with("open")));
    }

    #[test]
    fn menu_notify_offers_on_off() {
        let m = compute_menu("/notify ", &[], &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Theme);
        assert_eq!(m.items, vec!["on", "off"]);
    }

    #[test]
    fn menu_transparent_filters_to_off() {
        let m = compute_menu("/transparent of", &[], &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Theme);
        assert_eq!(m.items, vec!["off"]);
    }

    #[test]
    fn menu_effort_offers_all_tiers() {
        let m = compute_menu("/effort ", &[], &[], &[]).unwrap();
        assert_eq!(m.kind, MenuKind::Theme);
        assert_eq!(
            m.items,
            vec!["off", "low", "medium", "high", "xhigh", "max", "leetcode"]
        );
    }

    #[test]
    fn menu_effort_filters_by_prefix() {
        // Both "low" and "leetcode" start with "l".
        let m = compute_menu("/effort l", &[], &[], &[]).unwrap();
        assert_eq!(m.items, vec!["low", "leetcode"]);
    }

    #[test]
    fn suggestion_effort_completes_a_tier() {
        let s = compute_suggestion("/effort le", &[]).unwrap();
        assert_eq!(s.completion, "etcode");
        assert!(compute_suggestion("/effort zzz", &[]).is_none());
    }

    #[test]
    fn leetcode_badge_spells_the_word_and_animates() {
        let f0 = leetcode_badge(&TOKYO_NIGHT, 0);
        let word: String = f0.iter().map(|(c, _)| c).collect();
        assert_eq!(word, "leetcode");
        // Colours flow: two frames on, the palette has advanced, so at
        // least one character's colour differs.
        let f2 = leetcode_badge(&TOKYO_NIGHT, 2);
        assert!(f0.iter().zip(&f2).any(|(a, b)| a.1 != b.1));
    }

    #[test]
    fn entropy_badge_spells_the_word_and_animates() {
        let f0 = entropy_badge(&TOKYO_NIGHT, 0);
        let word: String = f0.iter().map(|(c, _)| c).collect();
        assert_eq!(word, "⟳ entropy");
        // The scatter shifts across frames — some colour differs a few
        // frames on.
        let f6 = entropy_badge(&TOKYO_NIGHT, 6);
        assert!(
            f0.iter().zip(&f6).any(|(a, b)| a.1 != b.1),
            "entropy badge should animate between frames"
        );
    }

    #[test]
    fn menu_mcps_offers_enable_disable_after_space() {
        let m = compute_menu("/mcps ", &[], &[], &[]).unwrap();
        // First sub-token dropdown reuses the single-arg replacement
        // kind — same as the `/notify on/off` menu.
        assert_eq!(m.kind, MenuKind::Theme);
        assert_eq!(m.items, vec!["enable", "disable"]);
    }

    #[test]
    fn menu_mcps_filters_action_by_prefix() {
        let m = compute_menu("/mcps en", &[], &[], &[]).unwrap();
        assert_eq!(m.items, vec!["enable"]);
    }

    #[test]
    fn menu_mcps_lists_servers_after_action() {
        let servers = vec![
            "filesystem".to_string(),
            "github".to_string(),
            "git".to_string(),
        ];
        let m = compute_menu("/mcps enable ", &[], &[], &servers).unwrap();
        assert_eq!(m.kind, MenuKind::McpServer);
        assert_eq!(m.items.len(), 3);
    }

    #[test]
    fn menu_mcps_filters_server_names_by_prefix() {
        let servers = vec!["filesystem".to_string(), "github".to_string()];
        let m = compute_menu("/mcps disable gi", &[], &[], &servers).unwrap();
        assert_eq!(m.kind, MenuKind::McpServer);
        assert_eq!(m.items, vec!["github"]);
    }

    #[test]
    fn menu_mcps_none_for_unknown_action() {
        let servers = vec!["filesystem".to_string()];
        assert!(compute_menu("/mcps reload ", &[], &[], &servers).is_none());
    }

    #[test]
    fn accept_menu_mcp_server_replaces_only_trailing_token() {
        let mut state = State::new("dummy-session-id", "dummy-model");
        state.input = "/mcps enable gi".to_string();
        state.input_cursor = state.input.len();
        state.menu = Some(Menu {
            items: vec!["github".to_string()],
            selected: 0,
            kind: MenuKind::McpServer,
        });
        assert!(accept_menu(&mut state));
        assert_eq!(state.input, "/mcps enable github");
    }

    #[test]
    fn word_forward_jumps_over_word_then_whitespace() {
        // cursor on 'h' of "hello", `w` → start of "world"
        assert_eq!(word_forward("hello world", 0), 6);
        // mid-word still jumps to start of next word
        assert_eq!(word_forward("hello world", 2), 6);
        // already on whitespace: skip to next word start
        assert_eq!(word_forward("hello  world", 5), 7);
        // last word: lands at end of input
        assert_eq!(word_forward("hello", 0), 5);
        // empty: stays at 0
        assert_eq!(word_forward("", 0), 0);
    }

    #[test]
    fn word_back_returns_to_word_start() {
        // end of input → start of last word
        assert_eq!(word_back("hello world", 11), 6);
        // on start of word → start of previous word
        assert_eq!(word_back("hello world", 6), 0);
        // on whitespace → start of previous word
        assert_eq!(word_back("hello world", 5), 0);
        // already at 0
        assert_eq!(word_back("hello", 0), 0);
        // empty input
        assert_eq!(word_back("", 0), 0);
    }

    #[test]
    fn word_end_advances_to_word_end() {
        // start of word → end of same word
        assert_eq!(word_end("hello world", 0), 4);
        // already at end of word → end of next word
        assert_eq!(word_end("hello world", 4), 10);
        // on whitespace → end of next word
        assert_eq!(word_end("hello world", 5), 10);
        // at end of input: stays
        assert_eq!(word_end("hello", 5), 5);
    }

    #[test]
    fn word_motion_handles_multibyte_chars() {
        // "héllo wörld" — each accented char is 2 bytes in UTF-8.
        let s = "héllo wörld";
        // word_forward from byte 0 should land at "wörld" start (byte 7).
        assert_eq!(word_forward(s, 0), 7);
        // word_back from end should return to start of "wörld".
        assert_eq!(word_back(s, s.len()), 7);
    }

    #[test]
    fn enter_visual_anchors_at_inner_top_left_and_switches_mode() {
        let mut s = State::new("dummy", "dummy");
        s.log_area = Rect::new(0, 0, 30, 10);
        enter_visual(&mut s, SelectionKind::Char);
        assert_eq!(s.mode, Mode::Visual);
        let inner = selection_inner_area(s.log_area);
        let sel = s.selection.expect("selection set on enter_visual");
        assert_eq!(sel.anchor, (inner.x, inner.y));
        assert_eq!(sel.cursor, (inner.x, inner.y));
    }

    #[test]
    fn enter_visual_is_noop_when_log_area_is_zero() {
        let mut s = State::new("dummy", "dummy");
        s.mode = Mode::Normal;
        s.log_area = Rect::default();
        enter_visual(&mut s, SelectionKind::Char);
        assert_eq!(s.mode, Mode::Normal);
        assert!(s.selection.is_none());
    }

    #[test]
    fn move_visual_cursor_clamps_to_inner_rect() {
        let mut s = State::new("dummy", "dummy");
        s.log_area = Rect::new(0, 0, 6, 4);
        enter_visual(&mut s, SelectionKind::Char);
        let inner = selection_inner_area(s.log_area);
        // Drift past both edges with a saturating delta; both axes pin.
        move_visual_cursor(&mut s, i32::MAX, i32::MAX);
        let sel = s.selection.unwrap();
        assert_eq!(
            sel.cursor,
            (inner.x + inner.width - 1, inner.y + inner.height - 1)
        );
        // Anchor never moves.
        assert_eq!(sel.anchor, (inner.x, inner.y));
        // Walk back to the top-left, also clamped.
        move_visual_cursor(&mut s, i32::MIN, i32::MIN);
        assert_eq!(s.selection.unwrap().cursor, (inner.x, inner.y));
    }

    #[test]
    fn visual_motion_extracts_rendered_text() {
        use ratatui::widgets::Widget;
        // Mirror the real chat block: bordered Paragraph with 1-cell
        // padding and Wrap.
        let area = Rect::new(0, 0, 30, 6);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        Paragraph::new(vec![Line::from("hello world")])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .padding(Padding::new(1, 1, 1, 0)),
            )
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);

        let mut s = State::new("dummy", "dummy");
        s.log_area = area;
        enter_visual(&mut s, SelectionKind::Char);
        // Walk to the right edge of the inner area — the cursor stays
        // pinned inside the rect even with a saturating delta.
        move_visual_cursor(&mut s, i32::MAX, 0);
        let inner = selection_inner_area(s.log_area);
        let sel = s.selection.unwrap();
        assert_eq!(sel.cursor, (inner.x + inner.width - 1, inner.y));
        // The first inner row is padding, so the extract is whitespace.
        // What this test pins is that motion → extract round-trips
        // without panicking and yields a sensible (non-overflowing) cell
        // range.
        let text = extract_selection_text(&buf, sel, inner);
        assert!(text.trim().is_empty(), "row-0 padding extract: {text:?}");
    }

    #[test]
    fn exit_visual_clears_or_keeps_selection() {
        let mut s = State::new("dummy", "dummy");
        s.log_area = Rect::new(0, 0, 10, 6);
        enter_visual(&mut s, SelectionKind::Char);
        exit_visual(&mut s, /* keep_selection */ true);
        assert_eq!(s.mode, Mode::Normal);
        assert!(s.selection.is_some(), "keep_selection=true preserves it");

        enter_visual(&mut s, SelectionKind::Char);
        exit_visual(&mut s, /* keep_selection */ false);
        assert_eq!(s.mode, Mode::Normal);
        assert!(s.selection.is_none(), "keep_selection=false clears it");
    }

    #[test]
    fn replace_char_at_cursor_swaps_existing_char_and_preserves_cursor() {
        let mut s = State::new("dummy", "dummy");
        s.input = "hello".into();
        s.input_cursor = 1;
        replace_char_at_cursor(&mut s, 'a');
        assert_eq!(s.input, "hallo");
        assert_eq!(s.input_cursor, 1, "cursor stays — matches vim `r`");
    }

    #[test]
    fn replace_char_at_cursor_is_noop_at_end_of_buffer() {
        let mut s = State::new("dummy", "dummy");
        s.input = "hi".into();
        s.input_cursor = 2;
        replace_char_at_cursor(&mut s, 'X');
        assert_eq!(s.input, "hi", "no char under cursor → no change");
        assert_eq!(s.input_cursor, 2);
    }

    #[test]
    fn replace_char_at_cursor_handles_multibyte_chars() {
        // Replace a 3-byte char with a 1-byte char, then back to a
        // multi-byte char. Cursor must stay at the same byte offset
        // since `r` doesn't move the cursor in vim.
        let mut s = State::new("dummy", "dummy");
        s.input = "héllo".into(); // é = 2 bytes
        s.input_cursor = 1; // on `é`
        replace_char_at_cursor(&mut s, 'e');
        assert_eq!(s.input, "hello");
        assert_eq!(s.input_cursor, 1);
        replace_char_at_cursor(&mut s, 'ä');
        assert_eq!(s.input, "hällo");
        assert_eq!(s.input_cursor, 1);
    }

    #[test]
    fn word_under_cursor_extracts_full_word_around_cursor() {
        // Cursor inside "world" returns the whole word.
        assert_eq!(
            word_under_cursor("hello world today", 7).as_deref(),
            Some("world")
        );
        // Cursor at the first byte of "world".
        assert_eq!(
            word_under_cursor("hello world", 6).as_deref(),
            Some("world")
        );
        // Cursor at the last byte of "hello".
        assert_eq!(
            word_under_cursor("hello world", 4).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn word_under_cursor_returns_none_on_whitespace_or_end() {
        assert!(word_under_cursor("hello world", 5).is_none(), "space");
        assert!(word_under_cursor("hello", 5).is_none(), "EOB");
        assert!(word_under_cursor("", 0).is_none(), "empty");
    }

    #[test]
    fn search_next_wraps_at_end_of_match_list() {
        let mut s = State::new("dummy", "dummy");
        s.history.push(Entry::User("a a a".into()));
        run_search(&mut s, "a".into());
        assert_eq!(s.search_idx, Some(0));
        search_next(&mut s);
        assert_eq!(s.search_idx, Some(1));
        search_next(&mut s);
        assert_eq!(s.search_idx, Some(2));
        search_next(&mut s);
        assert_eq!(s.search_idx, Some(0), "wraps past end");
    }

    #[test]
    fn search_prev_wraps_at_start_of_match_list() {
        let mut s = State::new("dummy", "dummy");
        s.history.push(Entry::User("a a a".into()));
        run_search(&mut s, "a".into());
        search_prev(&mut s);
        assert_eq!(s.search_idx, Some(2), "wraps past start");
        search_prev(&mut s);
        assert_eq!(s.search_idx, Some(1));
    }

    #[test]
    fn ctrl_c_clears_a_typed_prompt_instead_of_quitting() {
        let mut s = State::new("dummy", "dummy");
        s.input = "half a thought".into();
        s.input_cursor = s.input.len();
        assert!(
            !ctrl_c_should_quit(&mut s),
            "should not quit with text typed"
        );
        assert!(s.input.is_empty());
        assert_eq!(s.input_cursor, 0);
        // The cleared prompt is recoverable with `u`.
        assert_eq!(
            s.input_undo.last().map(|(t, _)| t.as_str()),
            Some("half a thought")
        );
    }

    #[test]
    fn ctrl_c_on_empty_prompt_quits() {
        let mut s = State::new("dummy", "dummy");
        assert!(s.input.is_empty());
        assert!(ctrl_c_should_quit(&mut s), "empty prompt should quit");
    }

    #[test]
    fn ctrl_c_cancels_command_and_search_modes_to_normal() {
        let mut s = State::new("dummy", "dummy");
        s.mode = Mode::Command;
        s.command_buf = "wq".into();
        s.command_cursor = 2;
        assert!(!ctrl_c_should_quit(&mut s));
        assert_eq!(s.mode, Mode::Normal);
        assert!(s.command_buf.is_empty());

        s.mode = Mode::Search;
        s.search_buf = "needle".into();
        s.search_cursor = 6;
        assert!(!ctrl_c_should_quit(&mut s));
        assert_eq!(s.mode, Mode::Normal);
        assert!(s.search_buf.is_empty());
    }

    #[test]
    fn reasoning_only_turn_is_not_blank() {
        // Regression: a reasoning model whose whole turn is thinking (empty
        // `content`) must not render blank — the thinking stays visible.
        let mut s = State::new("dummy", "dummy");
        s.apply(TurnEvent::AssistantStart);
        s.apply(TurnEvent::ReasoningDelta("weighing ".into()));
        s.apply(TurnEvent::ReasoningDelta("options".into()));
        s.apply(TurnEvent::AssistantEnd);
        assert!(matches!(
            s.history.last(),
            Some(Entry::Reasoning { text, complete: true }) if text == "weighing options"
        ));
    }

    #[test]
    fn reasoning_then_answer_keeps_thinking_above_the_reply() {
        let mut s = State::new("dummy", "dummy");
        s.apply(TurnEvent::AssistantStart);
        s.apply(TurnEvent::ReasoningDelta("hmm".into()));
        s.apply(TurnEvent::AssistantDelta("42".into()));
        s.apply(TurnEvent::AssistantEnd);
        assert_eq!(s.history.len(), 2);
        assert!(
            matches!(&s.history[0], Entry::Reasoning { text, complete: true } if text == "hmm")
        );
        assert!(matches!(&s.history[1], Entry::Assistant { text, complete: true } if text == "42"));
    }

    #[test]
    fn notice_event_surfaces_as_a_warning_info_entry() {
        let mut s = State::new("dummy", "dummy");
        s.apply(TurnEvent::Notice("cut off at context limit".into()));
        assert!(matches!(
            s.history.last(),
            Some(Entry::Info(t)) if t.contains("cut off at context limit") && t.starts_with('⚠')
        ));
    }

    #[test]
    fn finalize_completes_an_interrupted_reasoning_and_answer() {
        // Interrupted mid-stream: reasoning + a partial answer, no
        // AssistantEnd. Both must be finalized so their headers stop
        // animating.
        let mut s = State::new("dummy", "dummy");
        s.apply(TurnEvent::AssistantStart);
        s.apply(TurnEvent::ReasoningDelta("weighing".into()));
        s.apply(TurnEvent::AssistantDelta("partial".into()));
        assert!(matches!(
            &s.history[0],
            Entry::Reasoning {
                complete: false,
                ..
            }
        ));
        assert!(matches!(
            &s.history[1],
            Entry::Assistant {
                complete: false,
                ..
            }
        ));
        s.finalize_trailing_stream();
        assert!(matches!(
            &s.history[0],
            Entry::Reasoning { complete: true, .. }
        ));
        assert!(matches!(
            &s.history[1],
            Entry::Assistant { complete: true, .. }
        ));
    }

    #[test]
    fn finalize_completes_an_interrupted_tool_spinner() {
        let mut s = State::new("dummy", "dummy");
        s.apply(TurnEvent::ToolStart {
            name: "read".into(),
            arguments: "{}".into(),
        });
        assert!(matches!(
            s.history.last(),
            Some(Entry::Tool {
                complete: false,
                ..
            })
        ));
        s.finalize_trailing_stream();
        assert!(matches!(
            s.history.last(),
            Some(Entry::Tool { complete: true, .. })
        ));
    }

    #[test]
    fn finalize_pops_an_empty_answer_bubble_but_keeps_reasoning() {
        // Interrupted after reasoning + an empty content delta: the blank
        // answer bubble must be dropped so no header keeps blinking, while
        // the reasoning is kept and completed.
        let mut s = State::new("dummy", "dummy");
        s.apply(TurnEvent::AssistantStart);
        s.apply(TurnEvent::ReasoningDelta("weighing".into()));
        s.apply(TurnEvent::AssistantDelta(String::new()));
        assert_eq!(s.history.len(), 2);
        assert!(matches!(
            s.history.last(),
            Some(Entry::Assistant { text, .. }) if text.is_empty()
        ));
        s.finalize_trailing_stream();
        assert_eq!(s.history.len(), 1);
        assert!(matches!(
            s.history.last(),
            Some(Entry::Reasoning { text, complete: true }) if text == "weighing"
        ));
    }

    #[test]
    fn finalize_stops_at_completed_prior_entries() {
        // finalize walks back only over the current round's trailing
        // incomplete entries and stops at the first completed one, so an
        // earlier finished round (a completed tool + a completed answer) is
        // left byte-for-byte intact.
        let mut s = State::new("dummy", "dummy");
        s.apply(TurnEvent::ToolStart {
            name: "read".into(),
            arguments: "{}".into(),
        });
        s.apply(TurnEvent::ToolEnd {
            name: "read".into(),
            output: "file contents".into(),
        });
        s.apply(TurnEvent::AssistantStart);
        s.apply(TurnEvent::AssistantDelta("earlier answer".into()));
        s.apply(TurnEvent::AssistantEnd);
        // A new round, interrupted mid-answer (no AssistantEnd).
        s.apply(TurnEvent::AssistantStart);
        s.apply(TurnEvent::AssistantDelta("partial".into()));
        s.finalize_trailing_stream();
        assert_eq!(s.history.len(), 3);
        assert!(matches!(
            &s.history[2],
            Entry::Assistant { text, complete: true } if text == "partial"
        ));
        assert!(matches!(
            &s.history[1],
            Entry::Assistant { text, complete: true } if text == "earlier answer"
        ));
        assert!(matches!(
            &s.history[0],
            Entry::Tool { output, complete: true, .. } if output == "file contents"
        ));
    }

    #[test]
    fn content_only_turn_has_no_reasoning_entry() {
        let mut s = State::new("dummy", "dummy");
        s.apply(TurnEvent::AssistantStart);
        s.apply(TurnEvent::AssistantDelta("hi".into()));
        s.apply(TurnEvent::AssistantEnd);
        assert_eq!(s.history.len(), 1);
        assert!(
            matches!(s.history.last(), Some(Entry::Assistant { text, complete: true }) if text == "hi")
        );
    }

    #[test]
    fn search_next_on_empty_match_list_is_noop() {
        let mut s = State::new("dummy", "dummy");
        s.history.push(Entry::User("foo".into()));
        run_search(&mut s, "bar".into());
        assert!(s.search_idx.is_none());
        search_next(&mut s);
        assert!(s.search_idx.is_none());
        search_prev(&mut s);
        assert!(s.search_idx.is_none());
    }

    #[test]
    fn run_search_finds_every_non_overlapping_occurrence() {
        let mut s = State::new("dummy", "dummy");
        s.history.push(Entry::User("hello hello".into()));
        s.history.push(Entry::Assistant {
            text: "hello there".into(),
            complete: true,
        });
        run_search(&mut s, "hello".into());
        assert_eq!(s.search_pattern.as_deref(), Some("hello"));
        assert_eq!(s.search_matches.len(), 3);
        // First entry: matches at 0 and 6 (non-overlapping).
        assert_eq!(s.search_matches[0].entry_idx, 0);
        assert_eq!(s.search_matches[0].byte_offset, 0);
        assert_eq!(s.search_matches[1].entry_idx, 0);
        assert_eq!(s.search_matches[1].byte_offset, 6);
        // Second entry: one match at 0.
        assert_eq!(s.search_matches[2].entry_idx, 1);
        assert_eq!(s.search_matches[2].byte_offset, 0);
        assert_eq!(s.search_idx, Some(0));
    }

    #[test]
    fn run_search_skips_overlapping_occurrences() {
        let mut s = State::new("dummy", "dummy");
        s.history.push(Entry::Info("aaaa".into()));
        run_search(&mut s, "aa".into());
        // "aa" in "aaaa" lands at offsets 0 and 2 — not 0, 1, 2.
        assert_eq!(s.search_matches.len(), 2);
        assert_eq!(s.search_matches[0].byte_offset, 0);
        assert_eq!(s.search_matches[1].byte_offset, 2);
    }

    #[test]
    fn run_search_empty_pattern_clears_state() {
        let mut s = State::new("dummy", "dummy");
        s.search_pattern = Some("stale".into());
        s.search_matches.push(SearchHit {
            entry_idx: 0,
            byte_offset: 0,
            len: 5,
        });
        s.search_idx = Some(0);
        run_search(&mut s, String::new());
        assert!(s.search_pattern.is_none());
        assert!(s.search_matches.is_empty());
        assert!(s.search_idx.is_none());
    }

    #[test]
    fn run_search_zero_matches_yields_none_idx() {
        let mut s = State::new("dummy", "dummy");
        s.history.push(Entry::User("nothing here".into()));
        run_search(&mut s, "absent".into());
        assert_eq!(s.search_pattern.as_deref(), Some("absent"));
        assert!(s.search_matches.is_empty());
        assert!(s.search_idx.is_none());
    }

    #[test]
    fn undo_pops_most_recent_snapshot_and_pushes_current_onto_redo() {
        let mut s = State::new("dummy", "dummy");
        s.input_undo.push(("hello".into(), 5));
        s.input = "hello world".into();
        s.input_cursor = 11;
        undo_input(&mut s);
        assert_eq!(s.input, "hello");
        assert_eq!(s.input_cursor, 5);
        assert_eq!(s.input_redo.len(), 1, "current state moved to redo");
        assert_eq!(s.input_redo[0], ("hello world".into(), 11));
        assert!(s.skip_input_undo_record, "swap shouldn't re-record");
    }

    #[test]
    fn undo_on_empty_stack_is_noop() {
        let mut s = State::new("dummy", "dummy");
        s.input = "untouched".into();
        s.input_cursor = 3;
        undo_input(&mut s);
        assert_eq!(s.input, "untouched");
        assert_eq!(s.input_cursor, 3);
        assert!(s.input_redo.is_empty(), "nothing to redo from a no-op");
    }

    #[test]
    fn redo_reverses_undo_round_trip() {
        let mut s = State::new("dummy", "dummy");
        s.input = "abc".into();
        s.input_cursor = 3;
        s.input_undo.push(("ab".into(), 2));
        undo_input(&mut s);
        // After undo: input = "ab", cursor = 2.
        assert_eq!(s.input, "ab");
        assert_eq!(s.input_cursor, 2);
        s.skip_input_undo_record = false;
        redo_input(&mut s);
        assert_eq!(s.input, "abc");
        assert_eq!(s.input_cursor, 3);
        assert!(s.input_redo.is_empty());
        assert_eq!(s.input_undo.len(), 1, "round-tripped onto undo");
    }

    #[test]
    fn overwrite_char_at_cursor_replaces_and_advances() {
        let mut s = State::new("dummy", "dummy");
        s.input = "abc".into();
        s.input_cursor = 0;
        overwrite_char_at_cursor(&mut s, 'X');
        assert_eq!(s.input, "Xbc");
        assert_eq!(s.input_cursor, 1, "cursor advances past the new char");
    }

    #[test]
    fn overwrite_char_at_cursor_appends_at_end_of_buffer() {
        let mut s = State::new("dummy", "dummy");
        s.input = "ab".into();
        s.input_cursor = 2;
        overwrite_char_at_cursor(&mut s, 'c');
        assert_eq!(s.input, "abc", "EOB → append (vim R behavior)");
        assert_eq!(s.input_cursor, 3);
    }

    #[test]
    fn overwrite_char_at_cursor_handles_multibyte() {
        let mut s = State::new("dummy", "dummy");
        s.input = "café".into(); // é = 2 bytes
        s.input_cursor = 3; // on `é`
        overwrite_char_at_cursor(&mut s, 'e');
        assert_eq!(s.input, "cafe");
        assert_eq!(s.input_cursor, 4, "1-byte replacement at byte 3 → 4");
        // And the reverse: append a multi-byte char past EOB.
        overwrite_char_at_cursor(&mut s, 'ñ');
        assert_eq!(s.input, "cafeñ");
        assert_eq!(s.input_cursor, 6, "2-byte append moves cursor by 2");
    }

    #[test]
    fn enter_visual_sets_selection_kind() {
        let mut s = State::new("dummy", "dummy");
        s.log_area = Rect::new(0, 0, 30, 10);
        for kind in [
            SelectionKind::Char,
            SelectionKind::Line,
            SelectionKind::Block,
        ] {
            s.mode = Mode::Normal;
            s.selection = None;
            enter_visual(&mut s, kind);
            assert_eq!(s.mode, Mode::Visual);
            assert_eq!(s.selection.unwrap().kind, kind);
        }
    }

    #[test]
    fn selection_cells_line_covers_full_rows_regardless_of_columns() {
        // 10x5 inner rect, line selection on rows 1..=2 anchored at col 0,
        // cursor at col 0. Char would produce ~2 cells; Line should cover
        // every column on both rows.
        let area = Rect::new(0, 0, 10, 5);
        let sel = Selection {
            anchor: (0, 1),
            cursor: (0, 2),
            kind: SelectionKind::Line,
        };
        let cells = selection_cells(sel, area);
        assert_eq!(cells.len(), 20, "2 rows × 10 cols = 20 cells");
        // Every cell in rows 1 and 2 is present.
        for row in 1..=2 {
            for col in 0..10 {
                assert!(cells.contains(&(col, row)), "missing ({col},{row})");
            }
        }
    }

    #[test]
    fn selection_cells_block_is_a_rectangle() {
        // 10x5 area, block from (2,1) to (5,3). Expect a 4-col × 3-row
        // rectangle, regardless of which corner is anchor vs cursor.
        let area = Rect::new(0, 0, 10, 5);
        let sel = Selection {
            anchor: (5, 3),
            cursor: (2, 1),
            kind: SelectionKind::Block,
        };
        let cells = selection_cells(sel, area);
        assert_eq!(cells.len(), 12, "4 cols × 3 rows = 12 cells");
        for row in 1..=3 {
            for col in 2..=5 {
                assert!(cells.contains(&(col, row)), "missing ({col},{row})");
            }
        }
        // No cells outside the rectangle.
        assert!(!cells.contains(&(1, 1)));
        assert!(!cells.contains(&(6, 1)));
        assert!(!cells.contains(&(2, 0)));
        assert!(!cells.contains(&(2, 4)));
    }

    #[test]
    fn selection_cells_line_and_block_emit_for_single_cell() {
        // Char selections at a single point return empty (no drag).
        // Line and Block still emit something — the user pressed `V`
        // or `Ctrl-v` deliberately on this row/cell.
        let area = Rect::new(0, 0, 10, 5);
        let line = Selection {
            anchor: (3, 2),
            cursor: (3, 2),
            kind: SelectionKind::Line,
        };
        assert_eq!(selection_cells(line, area).len(), 10, "Line: full row");
        let block = Selection {
            anchor: (3, 2),
            cursor: (3, 2),
            kind: SelectionKind::Block,
        };
        assert_eq!(selection_cells(block, area).len(), 1, "Block: 1×1");
        let chr = Selection {
            anchor: (3, 2),
            cursor: (3, 2),
            kind: SelectionKind::Char,
        };
        assert!(selection_cells(chr, area).is_empty(), "Char point: empty");
    }

    #[test]
    fn backend_crash_heuristic_flags_server_failures_not_client_errors() {
        // Local-backend crash/timeout shapes → hint.
        assert!(looks_like_backend_crash(
            "backend returned 500: internal error"
        ));
        assert!(looks_like_backend_crash(
            "POST .../chat: llama runner process has terminated: signal: killed"
        ));
        assert!(looks_like_backend_crash("request timed out after 120s"));
        // Clean client errors → no hint (compacting wouldn't help).
        assert!(!looks_like_backend_crash(
            "backend returned 401: invalid api key"
        ));
        assert!(!looks_like_backend_crash(
            "backend returned 400: bad request"
        ));
    }
}
