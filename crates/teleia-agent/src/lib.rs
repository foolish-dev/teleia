use anyhow::{anyhow, bail, Result};
use async_stream::try_stream;
use futures_util::{future::BoxFuture, pin_mut, Stream, StreamExt};
use std::collections::{BTreeMap, BTreeSet};
use teleia_llm::{ChatEvent, LlmClient, Message, ToolDef};
use teleia_store::Store;

/// External tool source — implemented by the CLI's MCP registry (and
/// eventually LSP). The agent advertises `definitions()` alongside its
/// built-ins, and routes any matching tool call back through
/// `dispatch()` instead of the static `teleia_tools::dispatch`.
pub trait ToolRouter: Send {
    fn definitions(&self) -> Vec<ToolDef>;
    fn handles(&self, name: &str) -> bool;
    fn dispatch<'a>(&'a mut self, name: &'a str, args: &'a str) -> BoxFuture<'a, Result<String>>;
}

// Filename is capital-F (matches `Fool.md` at the workspace root),
// but the module identifier stays snake_case so callers keep using
// `fool::GUIDELINES`.
#[path = "Fool.rs"]
mod fool;

#[derive(Debug, Clone, Copy, Default)]
pub struct TokenCounts {
    pub prompt: u64,
    pub completion: u64,
}

/// Rough token estimate (~4 chars/token, from the JSON wire form) of what
/// each turn sends the model: the system prompt (the first message), the
/// tool schemas, and the conversation so far. Approximate — a `/context`
/// at-a-glance, not a billing figure.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContextEstimate {
    pub system: u64,
    pub tools: u64,
    pub history: u64,
    /// Conversation messages, excluding the leading system prompt.
    pub messages: usize,
}

impl ContextEstimate {
    /// Estimated tokens sent to the model per turn: system prompt + tool
    /// schemas + conversation history.
    pub fn total(&self) -> u64 {
        self.system + self.tools + self.history
    }
}

/// Percent of the context budget at which a turn compacts *before* sending,
/// leaving headroom for the model's own reply.
const COMPACT_AT_PCT: u64 = 85;

/// Default proactive-compaction budget (estimated tokens) for local Ollama
/// backends when the user hasn't set one with `/context` and the real
/// `num_ctx` couldn't be detected. 262144 is the native/trained window of the
/// local Qwen model cards (Janus/Thanatos): a baked 1M `num_ctx` really runs at
/// this window, so budgeting here (not to the 1M advertised ceiling) means
/// compaction actually fires. A detected Ollama `num_ctx` still overrides it,
/// and `/context N` trims per session. Hosted providers report overflow
/// cleanly and have large windows, so they stay reactive (no default).
/// Override any time with `/context N`, or turn it off with `/context off`.
const LOCAL_DEFAULT_CONTEXT: u64 = 262_144;

/// Conservative fallback budget for a NON-Qwen local Ollama model whose window
/// couldn't be detected. [`LOCAL_DEFAULT_CONTEXT`] (262144) is the Qwen
/// (Janus/Thanatos) native window; a Llama/Mistral/Phi with a smaller window
/// would never fire compaction under it, so unknown local models get this
/// reachable floor instead.
const LOCAL_DEFAULT_FALLBACK: u64 = 32_768;

/// Native context window (tokens) of Claude Fable 5 / Mythos 5 — 1M, which is
/// also the default on those models. Used as the proactive-compaction budget
/// when teleia targets one of them, so a turn compacts before it overflows the
/// window rather than relying on the backend to report it.
const FABLE_DEFAULT_CONTEXT: u64 = 1_000_000;

/// Model names that get [`FABLE_DEFAULT_CONTEXT`] as their default budget,
/// matched case-insensitively as a substring of the model id — only models
/// with a genuine 1M window. Local models (incl. Janus/Thanatos on Ollama)
/// must NOT be here: an out-of-reach budget means [`Agent::should_compact`]
/// never fires, so history grows unbounded — they fall to the far smaller
/// local default instead (see [`default_context_limit`]).
const FABLE_BUDGET_MODELS: &[&str] = &["fable", "mythos"];

/// Cap on a single tool result's size (in `char`s) before it enters the
/// conversation history. Whole-session compaction reduces the *accumulated*
/// history; this bounds the *per-result* cost so one unbounded output — a
/// huge file read, a chatty build log — can't blow the budget in a single
/// shot. Head and tail are kept (the start of a file, the last lines of a
/// log); the middle is elided. Only the model's copy is trimmed — the TUI
/// still shows the full output.
const MAX_TOOL_OUTPUT_CHARS: usize = 12_000;
const TRIM_HEAD_CHARS: usize = 8_000;
const TRIM_TAIL_CHARS: usize = 3_000;

fn trim_tool_output(output: String) -> String {
    let total = output.chars().count();
    if total <= MAX_TOOL_OUTPUT_CHARS {
        return output;
    }
    let omitted = total - TRIM_HEAD_CHARS - TRIM_TAIL_CHARS;
    let head: String = output.chars().take(TRIM_HEAD_CHARS).collect();
    let tail: String = output.chars().skip(total - TRIM_TAIL_CHARS).collect();
    format!("{head}\n\n… [{omitted} characters trimmed to fit the context budget] …\n\n{tail}")
}

const SYSTEM_PROMPT_BASE: &str = "You are τέλεια, a terse coding assistant running in a terminal. \
Use the provided tools to do real work: read, write, edit, multi_edit, bash, list, glob, grep, \
head, tail, tree, stat, diff, which, fetch, mkdir, mv, cp, rm, apply_patch, wc, touch, sha256, \
date, lint, format, typecheck, test, git, symlink, env, replace, json, base64, hexdump, du, \
realpath, todo_write, web_search (plus any MCP tools the user has configured). After any code change, run \
`format`/`lint`/`typecheck`/`test` to confirm the edit before claiming done — they report failure \
inside their output as `[exit N]`, not as an error, so read it. Stop before anything irreversible \
or outward-facing — a push, a release, a delete outside the worktree — and before a decision that \
is the user's to make. Always be concise. When you finish a turn, stop — \
do not narrate.";

/// Base prompt + the fool-derived guidelines, joined once at startup.
fn system_prompt() -> String {
    format!("{SYSTEM_PROMPT_BASE}\n\n{}", fool::GUIDELINES)
}

/// Replace whatever system turn a stored session carries with the
/// current [`system_prompt`]. Sessions persist message 0 verbatim, so
/// without this a resumed session keeps the guidelines frozen at the
/// moment it was created — an edit to `Fool.md` would reach new
/// sessions only. Retain-then-insert rather than overwriting index 0:
/// a session whose seq-0 row was skipped as corrupt has no system turn
/// there, and the providers concatenate every system turn wherever it
/// sits.
fn sync_system_prompt(messages: &mut Vec<Message>) {
    messages.retain(|m| !matches!(m, Message::System { .. }));
    messages.insert(
        0,
        Message::System {
            content: system_prompt(),
        },
    );
}

/// Instruction appended to the history for [`Agent::compact`]'s
/// summarize call. Sent without the tool schemas, so a history that
/// just overflowed usually still fits.
const COMPACT_PROMPT: &str = "Summarize this conversation so a fresh session can continue the \
work seamlessly. Include: the user's goals and constraints, what has been done so far (files \
touched, commands run, decisions made and why), the current state, and what remains. Be specific \
about paths and names. Output only the summary.";

/// Rewrap a backend context-overflow or local-runner-crash error with
/// actionable guidance; every other error passes through untouched. `{e:#}`
/// flattens the anyhow context chain so the provider's error body (where the
/// phrasing lives) is part of the matched text.
fn friendly_overflow(e: anyhow::Error) -> anyhow::Error {
    let s = format!("{e:#}");
    if teleia_llm::is_context_overflow(&s) {
        anyhow!(CONTEXT_OVERFLOW_HELP)
    } else if teleia_llm::is_backend_crash(&s) {
        anyhow!(BACKEND_CRASH_HELP)
    } else {
        e
    }
}

/// User-facing guidance when the input overflows the model's context
/// window. Exposed so the TUI renders identical wording when auto-compact
/// is off (or can't recover) — one source of truth for the message.
pub const CONTEXT_OVERFLOW_HELP: &str =
    "context limit exceeded — the conversation no longer fits the model's context \
     window; run /compact to continue it in a fresh session, or /reset to start over";

/// User-facing guidance when the local model backend's runner crashes (see
/// [`teleia_llm::is_backend_crash`]). It's an Ollama/llama.cpp-level failure,
/// not a teleia bug — most often a GPU-backend or version skew.
pub const BACKEND_CRASH_HELP: &str =
    "the local model backend crashed (its runner process terminated) — an \
     Ollama/llama.cpp failure, not teleia. Usually a GPU-backend or version mismatch \
     (e.g. ollama vs its GPU runner ollama-vulkan). Try a CPU-only tag (num_gpu 0), \
     realign the runner versions, or /model to a cloud backend";

/// True when `err` is the context-window-overflow condition — either a raw
/// backend overflow or the [`CONTEXT_OVERFLOW_HELP`] rewrap produced by
/// `friendly_overflow`. Lets the TUI auto-compact + retry instead of
/// surfacing a dead-end error.
pub fn is_overflow_error(err: &anyhow::Error) -> bool {
    let s = format!("{err:#}");
    teleia_llm::is_context_overflow(&s) || s.contains(CONTEXT_OVERFLOW_HELP)
}

/// Events emitted by `turn()`. The TUI consumes these to render
/// incrementally. Not `Clone` because `ToolApprovalRequest` carries a
/// `oneshot::Sender` that owns its single reply slot.
#[derive(Debug)]
pub enum TurnEvent {
    AssistantStart,
    AssistantDelta(String),
    /// A chunk of the model's reasoning/"thinking", shown separately from
    /// the answer and not stored in the message history.
    ReasoningDelta(String),
    AssistantEnd,
    /// Sent before each tool dispatch when the agent isn't in auto
    /// mode. The TUI must send a [`ToolApproval`] through `responder`
    /// before the stream produces another event — the agent's loop is
    /// blocked on the matching `.await`.
    ToolApprovalRequest {
        name: String,
        arguments: String,
        responder: tokio::sync::oneshot::Sender<ToolApproval>,
    },
    ToolStart {
        name: String,
        arguments: String,
    },
    ToolEnd {
        name: String,
        output: String,
        /// True when the call never reached dispatch — blocked by plan
        /// mode, or denied by the user. A tool that ran and *failed* is
        /// not refused: retrying is the caller's business. The TUI's
        /// auto-continue loop reads this to avoid answering a refusal
        /// with another `continue`.
        refused: bool,
    },
    /// A one-off informational note surfaced in the transcript (e.g. the
    /// model's response was truncated at the context limit).
    Notice(String),
    TurnEnd,
}

/// User's response to a [`TurnEvent::ToolApprovalRequest`]. `AllowAll`
/// permits the call and, out of build mode only, flips the agent into
/// auto mode for the rest of the session — see [`allow_all_promotes`];
/// from plan mode it approves just this call. `Deny` injects a
/// `"denied by user"` tool result so the model can react.
#[derive(Debug, Clone, Copy)]
pub enum ToolApproval {
    Allow,
    AllowAll,
    Deny,
}

/// Per-session permission mode. Cycles via Shift+Tab in the TUI:
/// `Plan` → `Build` → `Auto` → `Plan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionMode {
    /// Read-only investigation, split three ways by [`PlanGate`]:
    /// inspection built-ins (`read` / `list` / `glob` / `grep` / …) run
    /// without prompting; tools that reach the network or compile the
    /// working tree (`fetch` / `web_search` / `env` / `lint` /
    /// `typecheck` / `test`) prompt on build mode's approval path; and
    /// `write` / `edit` / `bash`, plus every MCP/LSP tool whatever it is
    /// named, short-circuit with a synthetic "blocked: plan mode" tool
    /// result so the model is pushed toward describing what it would do.
    Plan,
    /// Default: every tool call yields a `ToolApprovalRequest` and
    /// waits for the user's y/n/a.
    #[default]
    Build,
    /// Yolo: every tool dispatches immediately, no prompts.
    Auto,
}

impl PermissionMode {
    pub fn next(self) -> Self {
        match self {
            PermissionMode::Plan => PermissionMode::Build,
            PermissionMode::Build => PermissionMode::Auto,
            PermissionMode::Auto => PermissionMode::Plan,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PermissionMode::Plan => "PLAN",
            PermissionMode::Build => "BUILD",
            PermissionMode::Auto => "AUTO",
        }
    }
    /// Variant name as written in source — round-trips through the
    /// pref store via `Store::set_pref("permission_mode", …)`.
    pub fn label_canonical(self) -> &'static str {
        match self {
            PermissionMode::Plan => "Plan",
            PermissionMode::Build => "Build",
            PermissionMode::Auto => "Auto",
        }
    }
}

/// Plan-mode verdict for one concrete call. Split by *effect*, not by
/// "does it spawn a process": `diff` and `git status` spawn a fixed
/// system binary with argument-only inputs, while `cargo test` compiles
/// and runs whatever the working tree supplies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanGate {
    /// Changes nothing and starts no outbound connection; runs
    /// unprompted. Note this class still *reads* anything on disk — a
    /// `read` of `/proc/self/environ` puts the process environment in
    /// the transcript — so the guarantee is "changes nothing and dials
    /// nothing", not "leaks nothing".
    Inspect,
    /// Runs code the working tree controls, or opens a channel to the
    /// network. Prompts on build mode's approval path.
    Ask,
    /// Mutates, or is third-party code. Short-circuits with the
    /// synthetic "blocked: plan mode" result.
    Block,
}

/// Tools that only look: filesystem reads, metadata, and pure
/// in-process computation. `diff` does spawn (`Command::new("diff")`,
/// teleia-tools:957) and so do `git status`/`diff`/`log`, but with
/// argument-only inputs — a system binary that cannot execute anything
/// the working tree supplies, which is the line that matters. `which`
/// spawns nothing at all; it walks `$PATH` itself.
fn inspects_only(name: &str) -> bool {
    matches!(
        name,
        "read"
            | "list"
            | "glob"
            | "grep"
            | "head"
            | "tail"
            | "tree"
            | "stat"
            | "diff"
            | "which"
            | "wc"
            | "sha256"
            | "date"
            | "json"
            | "base64"
            | "hexdump"
            | "du"
            | "realpath"
    )
}

/// Tools plan mode runs only with the user's say-so. `fetch` and
/// `web_search` reach the network, so the model-authored argument is
/// itself the leak; `env` puts the process environment into a
/// transcript that is persisted and re-uploaded every round;
/// `lint`/`typecheck`/`test` shell out to cargo (teleia-tools:1490,
/// :1542, :1559), i.e. they compile and run the working tree including
/// `build.rs` and proc macros — strictly a superset of `bash`, which
/// plan mode blocks. `format` is not here: `cargo fmt --all`
/// (teleia-tools:1522) rewrites every file, so it stays Block.
fn needs_consent(name: &str) -> bool {
    matches!(
        name,
        "fetch" | "web_search" | "env" | "lint" | "typecheck" | "test"
    )
}

/// Plan-mode policy for one concrete call, argument-aware where the
/// name alone is too coarse. `git` mutates via `add`/`commit`, so its
/// inspection subcommands stay unprompted — but only with a pathspec
/// that can't be read as a flag: `paths` is appended to git's argv with
/// no `--` separator (teleia-tools:1612), so `paths: ["--output=FILE"]`
/// makes `git diff` write a file. `routed` is whether an MCP/LSP server
/// claims this name — servers name their own tools with no namespacing
/// (cli/src/mcp.rs:438) and are dispatched ahead of the built-ins, so
/// without this a server advertising `read` would inherit `read`'s
/// unprompted pass and run third-party code in the one mode that
/// promises nothing runs.
fn plan_gate(name: &str, arguments: &str, routed: bool) -> PlanGate {
    if routed {
        return PlanGate::Block;
    }
    if inspects_only(name) {
        return PlanGate::Inspect;
    }
    if needs_consent(name) {
        return PlanGate::Ask;
    }
    if name == "git" {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(arguments) {
            // A non-string entry can't be a path either; treat it as a flag.
            let flagged = v.get("paths").and_then(|p| p.as_array()).is_some_and(|ps| {
                ps.iter()
                    .any(|p| p.as_str().unwrap_or("-").starts_with('-'))
            });
            if !flagged
                && matches!(
                    v.get("subcommand").and_then(|s| s.as_str()),
                    Some("status" | "diff" | "log")
                )
            {
                return PlanGate::Inspect;
            }
        }
    }
    PlanGate::Block
}

/// Whether answering a tool prompt with "allow all" should also promote
/// the session to [`PermissionMode::Auto`]. Only out of `Build`, whose
/// contract is "ask about everything": plan mode can prompt now (its
/// [`PlanGate::Ask`] class), and one keystroke there must not vault the
/// user past the mode they deliberately skipped into the one where the
/// next `rm -rf` dispatches unasked — a promotion that also persists
/// (see [`Agent::promote_to_auto`]). From `Plan`, `a` approves the call
/// in front of it and leaves the mode alone. Stated as "only Build"
/// rather than "not Plan" because `Auto` never prompts, so its answer
/// is unreachable and must not read as "promote". The TUI keeps its own
/// copy of this rule at cli/src/tui.rs:2688 — the agent is mutably
/// borrowed by the running turn, so it can't read this back.
fn allow_all_promotes(mode: PermissionMode) -> bool {
    matches!(mode, PermissionMode::Build)
}

/// A tool call whose result rejects a required argument as `undefined` /
/// missing usually means the model dropped a large value — most often a
/// file's `content` — while encoding the call (these reasoning models can
/// fail to emit a huge argument in one shot, so the field never lands). Detect
/// that failure shape in a tool's error output so the caller can attach a
/// recovery hint, turning a blind identical-retry loop into a self-correcting
/// one. Substrings cover MCP/zod (`received undefined`) and serde-derived
/// tools (`missing field`).
fn incomplete_tool_args(output: &str) -> bool {
    let o = output.to_ascii_lowercase();
    o.contains("received undefined")
        || o.contains("missing field")
        || (o.contains("invalid arguments for tool") && o.contains("undefined"))
}

/// Actionable guidance appended to a dropped-argument tool result (see
/// [`incomplete_tool_args`]) so the model's retry fixes the cause instead of
/// repeating the same truncated call.
const INCOMPLETE_TOOL_ARGS_HINT: &str = "\n\n[teleia] a required argument was missing from that tool call — a large value (e.g. a file's `content`) was likely dropped while encoding the call. Resend it with the field present; for large files, write them in smaller pieces across multiple calls.";

/// Format a Unix timestamp (seconds) as `s-YYYY-MM-DD-HHMMSS` in UTC.
/// Pure integer math (Howard Hinnant's civil-from-days) so it needs no
/// date crate and renders identically on every platform teleia ships to —
/// unlike a libc `strftime`, which we'd have to `cfg`-gate off Windows.
/// The result is sortable (lexicographic == chronological) and unique per
/// second, which is enough to give each session a durable alias.
fn format_session_stamp(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let sod = unix_secs % 86_400;
    let (hh, mm, ss) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    format!("s-{year:04}-{m:02}-{d:02}-{hh:02}{mm:02}{ss:02}")
}

/// A durable, human-recognizable alias for a session created now, so every
/// session stays browsable in `/list` and loadable via `/load` without a
/// manual `/save` — in addition to the rolling `last`/`prev` bookmarks.
fn auto_session_alias() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_session_stamp(secs)
}

/// Save the durable auto-alias for a session created now.
fn save_auto_alias(store: &Store, session_id: &str) {
    save_auto_alias_named(store, session_id, &auto_session_alias());
}

/// Save `session_id` under `base`, or the first free `base-N` if another
/// session already holds `base`. The stamp is second-precision, so a burst
/// (e.g. rapid `/reset`) would otherwise reuse the name and
/// `INSERT OR REPLACE` would orphan the earlier session — exactly what this
/// alias exists to prevent.
fn save_auto_alias_named(store: &Store, session_id: &str, base: &str) {
    // `resolve_alias` errs when the name is free; take the first free one.
    if store.resolve_alias(base).is_err() {
        let _ = store.save_alias(base, session_id);
        return;
    }
    for n in 2..1000 {
        let name = format!("{base}-{n}");
        if store.resolve_alias(&name).is_err() {
            let _ = store.save_alias(&name, session_id);
            return;
        }
    }
}

/// Settable reasoning-effort tiers, ascending (`off` clears the field).
/// `xhigh`/`max` are the newer high tiers; `leetcode` is the top rung. Sent
/// as the OpenAI-standard `reasoning_effort`; the non-standard labels
/// `xhigh` and `leetcode` are mapped to `max` on the wire (teleia-llm's
/// `wire_reasoning_effort`) so a strict backend doesn't reject them.
/// Reasoning-incapable models and Ollama ignore the field.
pub const REASONING_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max", "leetcode"];

/// Whether `s` is a settable reasoning-effort tier (i.e. not `off`). The
/// single source of truth for both the `/effort` command and the startup
/// pref restore, so the accepted set can't drift between them.
pub fn is_reasoning_effort(s: &str) -> bool {
    REASONING_EFFORTS.contains(&s)
}

pub struct Agent {
    llm: LlmClient,
    tools: Vec<ToolDef>,
    store: Store,
    session_id: String,
    messages: Vec<Message>,
    seq: usize,
    tokens: TokenCounts,
    available_models: Vec<String>,
    /// Current permission stance. See [`PermissionMode`]. Flipped by
    /// the TUI's `/plan` / `/build` / `/auto` commands, by Shift+Tab,
    /// by the CLI's `--auto` flag, or by an `AllowAll` response.
    permission_mode: PermissionMode,
    /// Optional external tool source (MCP servers, eventually LSP).
    /// When set, its `definitions()` merge into the catalogue sent to
    /// the LLM, and `dispatch()` runs for matching tool names.
    router: Option<Box<dyn ToolRouter>>,
    /// Per-MCP-server tool catalogue, captured at attach time so we can
    /// hide / restore a server's tools without tearing down the child
    /// process. Populated by [`Agent::set_mcp_servers`].
    mcp_servers: BTreeMap<String, Vec<ToolDef>>,
    /// MCP server names whose tools are currently filtered out of
    /// `self.tools`. Synced to the `mcp_disabled` pref so the choice
    /// survives restart.
    mcp_disabled: BTreeSet<String>,
    /// Router tool names that lost a collision with a built-in and were
    /// therefore never advertised (see [`Agent::set_tool_router`]). The
    /// catalogue shows the built-in, so the dispatcher must run the
    /// built-in too — otherwise a server that names a tool `read` takes
    /// over the built-in the model thinks it is calling.
    shadowed_router_tools: BTreeSet<String>,
    /// Local model's `num_ctx` as read from Ollama's `/api/show`, populated
    /// once at startup by [`Agent::detect_context_window`]. Used as the
    /// proactive-compaction budget when the user hasn't set `/context`, so it
    /// tracks the window the model actually loads with. `None` until detected
    /// (or when detection isn't applicable / fails).
    detected_context: Option<u64>,
}

impl Agent {
    pub fn new(llm: LlmClient, store: Store) -> Result<Self> {
        let session_id = store.create_session(llm.model())?;
        // Auto-bookmark every new session as `last` so the next launch
        // can `--resume` without the user needing to type `/save`. Also
        // give it a durable timestamped alias so it stays browsable in
        // `/list` after `last` rolls to the next session.
        let _ = store.save_alias("last", &session_id);
        save_auto_alias(&store, &session_id);
        let mut agent = Self {
            llm,
            tools: teleia_tools::definitions(),
            store,
            session_id,
            messages: Vec::new(),
            seq: 0,
            tokens: TokenCounts::default(),
            available_models: Vec::new(),
            permission_mode: PermissionMode::default(),
            router: None,
            mcp_servers: BTreeMap::new(),
            mcp_disabled: BTreeSet::new(),
            shadowed_router_tools: BTreeSet::new(),
            detected_context: None,
        };
        agent.push(Message::System {
            content: system_prompt(),
        })?;
        Ok(agent)
    }

    /// Resume the most recent session if one exists; otherwise start a
    /// fresh one. The bookmarking happens automatically in [`Agent::new`]
    /// and [`Agent::reset`], so any prior run is recoverable by name
    /// (`last`) regardless of whether the user typed `/save`.
    pub fn resume(llm: LlmClient, store: Store) -> Result<Self> {
        let prev = store.resolve_alias("last").ok();
        match prev {
            Some(id) => {
                let mut messages = store.load(&id)?;
                sync_system_prompt(&mut messages);
                // Past the highest stored seq, not the loaded count — a row
                // skipped by load() (corrupt payload) must not make the next
                // append reuse a live seq.
                let seq = store.next_seq(&id)?;
                Ok(Self {
                    llm,
                    tools: teleia_tools::definitions(),
                    store,
                    session_id: id,
                    messages,
                    seq,
                    tokens: TokenCounts::default(),
                    available_models: Vec::new(),
                    permission_mode: PermissionMode::default(),
                    router: None,
                    mcp_servers: BTreeMap::new(),
                    mcp_disabled: BTreeSet::new(),
                    shadowed_router_tools: BTreeSet::new(),
                    detected_context: None,
                })
            }
            None => Self::new(llm, store),
        }
    }

    /// Plug an external tool source into the agent. Its definitions
    /// are appended to the built-in tool list immediately; subsequent
    /// dispatches check the router before falling back to
    /// `teleia_tools`. A def whose name a built-in already owns is
    /// neither advertised nor routed — the built-in wins both, so the
    /// schema the model sees is the one that runs.
    pub fn set_tool_router(&mut self, router: Box<dyn ToolRouter>) {
        // Compared against the built-ins, not `self.tools`: two servers
        // sharing a name still resolve first-wins through the router.
        let builtins: BTreeSet<String> = teleia_tools::definitions()
            .into_iter()
            .map(|d| d.function.name)
            .collect();
        for def in router.definitions() {
            if builtins.contains(&def.function.name) {
                self.shadowed_router_tools.insert(def.function.name);
                continue;
            }
            // Avoid duplicates if the user registers the same MCP twice.
            if !self
                .tools
                .iter()
                .any(|t| t.function.name == def.function.name)
            {
                self.tools.push(def);
            }
        }
        self.router = Some(router);
    }

    /// Record which tool defs came from which MCP server, and apply any
    /// `mcp_disabled` pref so the persisted choice survives restart.
    /// Call after [`Agent::set_tool_router`].
    pub fn set_mcp_servers(&mut self, servers: BTreeMap<String, Vec<ToolDef>>) {
        self.mcp_servers = servers;
        let persisted = self
            .get_pref("mcp_disabled")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        for name in persisted {
            if self.mcp_servers.contains_key(&name) {
                self.hide_mcp_tools(&name);
                self.mcp_disabled.insert(name);
            }
        }
    }

    pub fn mcp_server_names(&self) -> Vec<String> {
        self.mcp_servers.keys().cloned().collect()
    }

    pub fn is_mcp_enabled(&self, name: &str) -> bool {
        self.mcp_servers.contains_key(name) && !self.mcp_disabled.contains(name)
    }

    pub fn disabled_mcps(&self) -> Vec<String> {
        self.mcp_disabled.iter().cloned().collect()
    }

    /// Returns `Ok(true)` if state actually changed, `Ok(false)` if the
    /// server was already in the requested state, or `Err` if the name
    /// isn't a known MCP server.
    pub fn enable_mcp(&mut self, name: &str) -> Result<bool> {
        if !self.mcp_servers.contains_key(name) {
            return Err(anyhow::anyhow!("unknown MCP server: {name}"));
        }
        if !self.mcp_disabled.remove(name) {
            return Ok(false);
        }
        self.show_mcp_tools(name);
        self.persist_mcp_disabled();
        Ok(true)
    }

    pub fn disable_mcp(&mut self, name: &str) -> Result<bool> {
        if !self.mcp_servers.contains_key(name) {
            return Err(anyhow::anyhow!("unknown MCP server: {name}"));
        }
        if !self.mcp_disabled.insert(name.to_string()) {
            return Ok(false);
        }
        self.hide_mcp_tools(name);
        self.persist_mcp_disabled();
        Ok(true)
    }

    fn hide_mcp_tools(&mut self, name: &str) {
        let Some(defs) = self.mcp_servers.get(name) else {
            return;
        };
        // A def that lost a name collision was never advertised, so the
        // def carrying that name is the *built-in* — dropping it here
        // would delete a built-in tool from the catalogue.
        let drop: std::collections::HashSet<String> = defs
            .iter()
            .map(|d| d.function.name.clone())
            .filter(|n| !self.shadowed_router_tools.contains(n))
            .filter(|n| !self.enabled_peer_offers(n, name))
            .collect();
        self.tools.retain(|t| !drop.contains(&t.function.name));
    }

    fn show_mcp_tools(&mut self, name: &str) {
        let Some(defs) = self.mcp_servers.get(name).cloned() else {
            return;
        };
        for def in defs {
            // Never restore a def the built-in catalogue shadowed.
            if self.shadowed_router_tools.contains(&def.function.name) {
                continue;
            }
            if !self
                .tools
                .iter()
                .any(|t| t.function.name == def.function.name)
            {
                self.tools.push(def);
            }
        }
    }

    fn persist_mcp_disabled(&self) {
        let joined: Vec<&str> = self.mcp_disabled.iter().map(String::as_str).collect();
        // Best-effort: a failed persist only means the toggle won't survive a
        // restart; the in-memory state is already correct.
        self.set_pref("mcp_disabled", &joined.join(",")).ok();
    }

    /// Whether an external router (MCP / LSP) owns this tool name.
    /// Resolved *before* the permission gate in [`Agent::turn`]: the
    /// answer decides the call's permission class, not merely where it
    /// dispatches.
    fn is_routed(&self, name: &str) -> bool {
        !self.shadowed_router_tools.contains(name)
            && !self.is_disabled_router_tool(name)
            && self
                .router
                .as_ref()
                .map(|r| r.handles(name))
                .unwrap_or(false)
    }

    /// Whether this tool name belongs to a server the user turned off with
    /// `/mcps disable`. [`Agent::hide_mcp_tools`] only drops the defs from
    /// the catalogue; the router keeps claiming the name, so without this a
    /// name the model saw earlier in the session still dispatches to the
    /// server the user just disabled.
    fn is_disabled_router_tool(&self, name: &str) -> bool {
        let mut on_a_disabled_server = false;
        for (server, defs) in &self.mcp_servers {
            if defs.iter().any(|d| d.function.name == name) {
                // MCP names are not namespaced, so two servers can claim
                // one. Disabling either must not take the name away while
                // the other is still on.
                if !self.mcp_disabled.contains(server) {
                    return false;
                }
                on_a_disabled_server = true;
            }
        }
        on_a_disabled_server
    }

    /// Whether a server other than `except`, and still enabled, advertises
    /// `tool`. The catalogue entry for a contested name belongs to that
    /// server, so disabling `except` must leave it alone.
    fn enabled_peer_offers(&self, tool: &str, except: &str) -> bool {
        self.mcp_servers.iter().any(|(server, defs)| {
            server != except
                && !self.mcp_disabled.contains(server)
                && defs.iter().any(|d| d.function.name == tool)
        })
    }

    pub fn permission_mode(&self) -> PermissionMode {
        self.permission_mode
    }

    /// Full tool catalogue advertised to the LLM — built-ins + any
    /// tools merged in via [`Agent::set_tool_router`].
    pub fn tools(&self) -> &[ToolDef] {
        &self.tools
    }

    pub fn set_permission_mode(&mut self, mode: PermissionMode) {
        self.permission_mode = mode;
    }

    /// Promote to Auto after an "allow all" tool approval and persist it, so
    /// a blanket-approval upgrade survives a restart like every other mode
    /// change (which route through the pref store via `set_mode`). The TUI
    /// can't do this at the approval site — the agent is borrowed by the
    /// in-flight turn stream — so the field-of-truth flip persists here.
    fn promote_to_auto(&mut self) {
        self.permission_mode = PermissionMode::Auto;
        // Best-effort persist (see persist_mcp_disabled).
        self.set_pref("permission_mode", PermissionMode::Auto.label_canonical())
            .ok();
    }

    pub fn auto_mode(&self) -> bool {
        matches!(self.permission_mode, PermissionMode::Auto)
    }

    pub fn set_auto_mode(&mut self, on: bool) {
        self.permission_mode = if on {
            PermissionMode::Auto
        } else {
            PermissionMode::Build
        };
    }

    /// Cached list of Ollama-installed models; populated once via
    /// `refresh_models()` at startup, used by the TUI to render the
    /// `/model <prefix>` dropdown.
    pub fn available_models(&self) -> &[String] {
        &self.available_models
    }

    /// Re-query Ollama's `/api/tags` and cache the results. No-op if
    /// the endpoint isn't reachable.
    pub async fn refresh_models(&mut self) {
        self.available_models = self.llm.list_models().await;
    }

    /// Merge additional model names into `available_models` (deduped).
    /// Used to surface cloud models in the `/model` dropdown even when
    /// they aren't installed locally.
    pub fn extend_models<I, S>(&mut self, extras: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for m in extras {
            let s = m.into();
            if !self.available_models.contains(&s) {
                self.available_models.push(s);
            }
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// A rough per-turn token breakdown for `/context`. Estimates from the
    /// JSON wire form at ~4 chars/token; `messages[0]` is the system prompt,
    /// the rest is conversation, and the tool schemas are sent alongside.
    pub fn context_estimate(&self) -> ContextEstimate {
        let toks = |chars: usize| (chars / 4) as u64;
        let ser = |m: &Message| serde_json::to_string(m).map_or(0, |s| s.len());
        let mut est = ContextEstimate::default();
        for (i, m) in self.messages.iter().enumerate() {
            if i == 0 {
                est.system = toks(ser(m));
            } else {
                est.history += toks(ser(m));
                est.messages += 1;
            }
        }
        est.tools = toks(serde_json::to_string(&self.tools).map_or(0, |s| s.len()));
        est
    }

    /// Proactive-compaction budget in estimated tokens, or `None` for pure
    /// reactive behaviour. Backends that don't report overflow the way hosted
    /// APIs do — notably a local Ollama model, whose `num_ctx` window is
    /// silently truncated (or 500s) rather than returning a clean error —
    /// default to [`LOCAL_DEFAULT_CONTEXT`] so compaction fires without the
    /// user opting in. A `/context N` sets an explicit budget; `/context off`
    /// (an empty pref) disables it even for local backends.
    pub fn context_limit(&self) -> Option<u64> {
        match self.get_pref("context_limit") {
            Some(v) => v.parse::<u64>().ok().filter(|&n| n > 0),
            None => self.default_context_limit(),
        }
    }

    /// Budget applied when the user hasn't run `/context`. A detected local
    /// `num_ctx` (from Ollama's `/api/show`, see [`detect_context_window`])
    /// wins — it's the window the model actually loads with. Otherwise the 1M
    /// Fable-class budget for a [`FABLE_BUDGET_MODELS`] match, the settled
    /// [`LOCAL_DEFAULT_CONTEXT`] fallback for an Ollama-style endpoint whose
    /// window couldn't be read, and `None` for other hosted providers.
    fn default_context_limit(&self) -> Option<u64> {
        if let Some(n) = self.detected_context {
            return Some(n);
        }
        let model = self.model().to_ascii_lowercase();
        if FABLE_BUDGET_MODELS.iter().any(|m| model.contains(m)) {
            Some(FABLE_DEFAULT_CONTEXT)
        } else if teleia_llm::looks_like_ollama(self.llm.base_url()) {
            // 262144 is the Qwen (Janus/Thanatos) native window; other local
            // models have unknown windows, so budget them to a reachable floor
            // that still fires compaction rather than the out-of-reach ceiling.
            if ["qwen", "janus", "thanatos"]
                .iter()
                .any(|m| model.contains(m))
            {
                Some(LOCAL_DEFAULT_CONTEXT)
            } else {
                Some(LOCAL_DEFAULT_FALLBACK)
            }
        } else {
            None
        }
    }

    /// Read the local model's real `num_ctx` from Ollama's `/api/show` and
    /// remember it as the default compaction budget. Best-effort and idempotent
    /// — call once at startup; `None` for non-Ollama backends or on failure,
    /// in which case the budget falls back to [`LOCAL_DEFAULT_CONTEXT`]. An
    /// explicit `/context N` still overrides a detected value.
    pub async fn detect_context_window(&mut self) -> Option<u64> {
        self.detected_context = self.llm.detect_ollama_num_ctx().await;
        self.detected_context
    }

    /// Set the proactive-compaction budget, or clear it with `None`.
    pub fn set_context_limit(&self, limit: Option<u64>) -> Result<()> {
        self.set_pref(
            "context_limit",
            &limit.map_or(String::new(), |n| n.to_string()),
        )
    }

    /// True when the estimated prompt is within [`COMPACT_AT_PCT`]% of the
    /// configured budget and there is prior conversation to summarise — the
    /// cue to compact *before* a turn overflows a window the backend may
    /// never report on. `false` when the budget is off (see
    /// [`context_limit`](Self::context_limit)).
    pub fn should_compact(&self) -> bool {
        let Some(limit) = self.context_limit() else {
            return false;
        };
        self.message_count() > 1
            && self.context_estimate().total().saturating_mul(100)
                >= limit.saturating_mul(COMPACT_AT_PCT)
    }

    // ---- preference + history pass-through ----
    // Centralised so the TUI doesn't need to own its own Store handle.

    pub fn get_pref(&self, key: &str) -> Option<String> {
        self.store.get_pref(key).ok().flatten()
    }

    /// Persist a preference. Propagates the store error (an
    /// `INSERT OR REPLACE` can fail on a full/locked DB) so the caller can
    /// surface it instead of silently losing the setting.
    pub fn set_pref(&self, key: &str, value: &str) -> Result<()> {
        self.store.set_pref(key, value)
    }

    pub fn push_input_history(&self, line: &str) {
        let _ = self.store.push_input_history(line);
    }

    pub fn input_history(&self, limit: usize) -> Vec<String> {
        self.store.input_history(limit).unwrap_or_default()
    }

    pub fn reset(&mut self) -> Result<()> {
        // Bookmark the outgoing session as `prev` before we move on, so
        // a too-eager `/reset` is recoverable via `/load prev`.
        let _ = self.store.save_alias("prev", &self.session_id);
        let session_id = self.store.create_session(self.llm.model())?;
        let _ = self.store.save_alias("last", &session_id);
        save_auto_alias(&self.store, &session_id);
        self.session_id = session_id;
        self.messages.clear();
        self.seq = 0;
        self.tokens = TokenCounts::default();
        self.push(Message::System {
            content: system_prompt(),
        })?;
        Ok(())
    }

    /// Summarize the conversation with the model, then continue in a
    /// fresh session seeded with that summary. The outgoing session is
    /// bookmarked as `prev` (via [`Self::reset`]), so `/load prev`
    /// recovers it. The summarize request drops the tool schemas —
    /// usually enough headroom to fit a history that just overflowed
    /// the context window. History is only replaced after the summary
    /// has fully streamed, so cancelling (dropping the future) mid-call
    /// leaves the session untouched.
    pub async fn compact(&mut self) -> Result<()> {
        if self.messages.len() <= 1 {
            bail!("nothing to compact yet");
        }
        // A dangling tool_use (interrupted round) would 400 on strict
        // backends before the summarize call even runs.
        self.reconcile_orphaned_tool_calls()?;

        let mut request = self.messages.clone();
        request.push(Message::User {
            content: COMPACT_PROMPT.to_string(),
        });
        let mut summary = String::new();
        {
            let stream = self.llm.stream(&request, None);
            pin_mut!(stream);
            while let Some(event) = stream.next().await {
                let event = event.map_err(|e| {
                    if teleia_llm::is_context_overflow(&format!("{e:#}")) {
                        anyhow!(
                            "even the compaction request exceeds the context window — \
                             use /reset to start fresh"
                        )
                    } else {
                        e
                    }
                })?;
                match event {
                    ChatEvent::ContentDelta(text) => summary.push_str(&text),
                    ChatEvent::ReasoningDelta(_) => {}
                    ChatEvent::Done { .. } => {}
                }
            }
        }
        let summary = summary.trim();
        if summary.is_empty() {
            bail!("model returned an empty summary — session left untouched");
        }

        let carried =
            format!("Context carried over from the previous session (compacted):\n\n{summary}");
        self.reset()?;
        self.push(Message::User { content: carried })?;
        Ok(())
    }

    pub fn load_alias(&mut self, name: &str) -> Result<String> {
        let session_id = self.store.resolve_alias(name)?;
        let mut messages = self.store.load(&session_id)?;
        sync_system_prompt(&mut messages);
        self.seq = self.store.next_seq(&session_id)?;
        self.session_id = session_id.clone();
        self.messages = messages;
        self.tokens = TokenCounts::default();
        Ok(session_id)
    }

    pub fn tokens(&self) -> TokenCounts {
        self.tokens
    }

    pub fn save_alias(&self, name: &str) -> Result<()> {
        self.store.save_alias(name, &self.session_id)
    }

    pub fn list_aliases(&self) -> Result<Vec<(String, String, i64)>> {
        self.store.list_aliases()
    }

    pub fn delete_alias(&self, name: &str) -> Result<()> {
        self.store.delete_alias(name)
    }

    pub fn model(&self) -> &str {
        self.llm.model()
    }

    pub fn set_model(&mut self, model: String) {
        self.llm.set_model(model);
    }

    pub fn reasoning_effort(&self) -> Option<&str> {
        self.llm.reasoning_effort()
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<String>) {
        self.llm.set_reasoning_effort(effort);
    }

    pub fn has_api_key(&self) -> bool {
        self.llm.api_key().map(|k| !k.is_empty()).unwrap_or(false)
    }

    pub fn set_api_key(&mut self, key: Option<String>) {
        self.llm.set_api_key(key);
    }

    pub fn turn<'a>(
        &'a mut self,
        user_input: String,
    ) -> impl Stream<Item = Result<TurnEvent>> + 'a {
        // Cap the tool-call rounds in a single turn. A model that emits a
        // tool call every round (or a hostile MCP tool that keeps prompting
        // one) would otherwise loop forever, burning paid round-trips and
        // growing the session unbounded, stoppable only by a human pressing
        // Esc. This is the inner per-turn cap; the TUI's `/loop` has its own
        // outer re-submission cap.
        const MAX_TOOL_STEPS: usize = 100;
        try_stream! {
            self.reconcile_orphaned_tool_calls()?;
            self.push(Message::User { content: user_input })?;

            let mut steps = 0usize;
            loop {
                if steps >= MAX_TOOL_STEPS {
                    // History ends on a tool result here (a valid stop), so
                    // note it and end the turn; the next user turn can pick
                    // up if the halt was premature.
                    let note = format!(
                        "stopped after {MAX_TOOL_STEPS} tool steps without a final answer — ask me to continue if that was premature"
                    );
                    yield TurnEvent::AssistantStart;
                    yield TurnEvent::AssistantDelta(note.clone());
                    yield TurnEvent::AssistantEnd;
                    self.push(Message::Assistant {
                        content: Some(note),
                        tool_calls: Vec::new(),
                    })?;
                    yield TurnEvent::TurnEnd;
                    return;
                }

                yield TurnEvent::AssistantStart;
                let mut content_buf = String::new();
                let mut tool_calls = Vec::new();
                let mut truncated = false;

                {
                    let stream = self.llm.stream(&self.messages, Some(&self.tools));
                    pin_mut!(stream);
                    while let Some(event) = stream.next().await {
                        match event.map_err(friendly_overflow)? {
                            ChatEvent::ContentDelta(text) => {
                                content_buf.push_str(&text);
                                yield TurnEvent::AssistantDelta(text);
                            }
                            // Reasoning is display-only: shown live but not
                            // accumulated into the answer or the stored message.
                            ChatEvent::ReasoningDelta(text) => {
                                yield TurnEvent::ReasoningDelta(text);
                            }
                            ChatEvent::Done { tool_calls: tcs, usage, finish_reason } => {
                                tool_calls = tcs;
                                truncated = finish_reason.as_deref() == Some("length");
                                if let Some(u) = usage {
                                    self.tokens.prompt = self
                                        .tokens
                                        .prompt
                                        .saturating_add(u.prompt_tokens as u64);
                                    self.tokens.completion = self
                                        .tokens
                                        .completion
                                        .saturating_add(u.completion_tokens as u64);
                                }
                            }
                        }
                    }
                }

                yield TurnEvent::AssistantEnd;
                if truncated {
                    yield TurnEvent::Notice(
                        "response cut off at the model's context limit (num_ctx) — raise it or lower /effort for a complete answer".to_string(),
                    );
                }

                let assistant_msg = Message::Assistant {
                    content: (!content_buf.is_empty()).then_some(content_buf.clone()),
                    tool_calls: tool_calls.clone(),
                };
                self.push(assistant_msg)?;

                if tool_calls.is_empty() {
                    yield TurnEvent::TurnEnd;
                    return;
                }

                for call in tool_calls {
                    // Routing first: whether an MCP/LSP server owns this
                    // name decides the call's permission class, not just
                    // where it dispatches. Server tool names arrive
                    // verbatim from the server's `tools/list`
                    // (cli/src/mcp.rs:438), so one can advertise `read`.
                    let routed = self.is_routed(&call.function.name);
                    let gate = plan_gate(&call.function.name, &call.function.arguments, routed);
                    // Permission gate. Auto runs everything; Plan splits
                    // by effect (see [`PlanGate`]) into run / prompt /
                    // synthesize-"blocked"; Build prompts per-call.
                    match self.permission_mode {
                        PermissionMode::Auto => {}
                        PermissionMode::Plan if gate == PlanGate::Inspect => {}
                        PermissionMode::Plan if gate == PlanGate::Block => {
                            // Name the reason when the call is external —
                            // a refused `read` otherwise reads as a bug.
                            let via = if routed {
                                " (an external MCP/LSP tool, never auto-allowed in plan mode)"
                            } else {
                                ""
                            };
                            let output = format!(
                                "blocked: plan mode does not permit `{}`{via}. Describe what you would do; the user can switch to build mode (Shift+Tab or /build) to execute.",
                                call.function.name
                            );
                            yield TurnEvent::ToolStart {
                                name: call.function.name.clone(),
                                arguments: call.function.arguments.clone(),
                            };
                            yield TurnEvent::ToolEnd {
                                name: call.function.name.clone(),
                                output: output.clone(),
                                refused: true,
                            };
                            self.push(Message::Tool {
                                tool_call_id: call.id.clone(),
                                content: output,
                            })?;
                            continue;
                        }
                        // Plan's `Ask` class prompts on exactly the path
                        // build mode already uses — no new event, no TUI
                        // change.
                        PermissionMode::Plan | PermissionMode::Build => {
                            let (tx, rx) = tokio::sync::oneshot::channel();
                            yield TurnEvent::ToolApprovalRequest {
                                name: call.function.name.clone(),
                                arguments: call.function.arguments.clone(),
                                responder: tx,
                            };
                            let decision = rx.await.unwrap_or(ToolApproval::Deny);
                            match decision {
                                ToolApproval::Allow => {}
                                ToolApproval::AllowAll => {
                                    if allow_all_promotes(self.permission_mode) {
                                        self.promote_to_auto();
                                    }
                                }
                                ToolApproval::Deny => {
                                    let output = "denied by user".to_string();
                                    yield TurnEvent::ToolStart {
                                        name: call.function.name.clone(),
                                        arguments: call.function.arguments.clone(),
                                    };
                                    yield TurnEvent::ToolEnd {
                                        name: call.function.name.clone(),
                                        output: output.clone(),
                                        refused: true,
                                    };
                                    self.push(Message::Tool {
                                        tool_call_id: call.id.clone(),
                                        content: output,
                                    })?;
                                    continue;
                                }
                            }
                        }
                    }

                    yield TurnEvent::ToolStart {
                        name: call.function.name.clone(),
                        arguments: call.function.arguments.clone(),
                    };

                    let output = if routed {
                        let r = self.router.as_mut().unwrap();
                        match r.dispatch(&call.function.name, &call.function.arguments).await {
                            Ok(o) => o,
                            Err(e) => format!("error: {e}"),
                        }
                    } else {
                        match teleia_tools::dispatch(
                            &call.function.name,
                            &call.function.arguments,
                        )
                        .await
                        {
                            Ok(o) => o,
                            Err(e) => format!("error: {e}"),
                        }
                    };
                    // A dropped required argument (e.g. a large file `content`
                    // the model failed to encode in one call) comes back as a
                    // validation error; attach a recovery hint so the retry
                    // fixes the cause instead of repeating the same call.
                    let output = if incomplete_tool_args(&output) {
                        format!("{output}{INCOMPLETE_TOOL_ARGS_HINT}")
                    } else {
                        output
                    };
                    yield TurnEvent::ToolEnd {
                        name: call.function.name.clone(),
                        output: output.clone(),
                        refused: false,
                    };
                    self.push(Message::Tool {
                        tool_call_id: call.id.clone(),
                        content: trim_tool_output(output),
                    })?;
                }
                steps += 1;
            }
        }
    }

    /// Repair history left inconsistent by an interrupted tool round.
    ///
    /// `turn()` persists the assistant message (with its `tool_calls`)
    /// before it pushes the matching tool results. A Ctrl-C / Esc at a
    /// Build-mode approval prompt, or a dropped stream mid-dispatch,
    /// leaves some `tool_calls` without results — both in memory and in
    /// sqlite (plus the auto-saved `last` alias). Anthropic and strict
    /// OpenAI-compatible backends reject a dangling `tool_use`, so every
    /// later turn 400s and the breakage survives `--resume`. Synthesize a
    /// placeholder result for each unfulfilled call so the conversation
    /// stays valid. The interrupted round is always the last assistant
    /// message that carried `tool_calls`; results already recorded follow
    /// it, so anything of its ids missing from that tail is an orphan.
    fn reconcile_orphaned_tool_calls(&mut self) -> Result<()> {
        let Some(idx) = self.messages.iter().rposition(
            |m| matches!(m, Message::Assistant { tool_calls, .. } if !tool_calls.is_empty()),
        ) else {
            return Ok(());
        };
        let Message::Assistant { tool_calls, .. } = &self.messages[idx] else {
            return Ok(());
        };
        let ids: Vec<String> = tool_calls.iter().map(|c| c.id.clone()).collect();
        let fulfilled: BTreeSet<&str> = self.messages[idx + 1..]
            .iter()
            .filter_map(|m| match m {
                Message::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        let missing: Vec<String> = ids
            .into_iter()
            .filter(|id| !fulfilled.contains(id.as_str()))
            .collect();
        for id in missing {
            self.push(Message::Tool {
                tool_call_id: id,
                content: "interrupted: tool was not run".to_string(),
            })?;
        }
        Ok(())
    }

    fn push(&mut self, message: Message) -> Result<()> {
        self.store.append(&self.session_id, self.seq, &message)?;
        self.seq += 1;
        self.messages.push(message);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use teleia_llm::{ToolCall, ToolCallFunction};

    fn tmp_store() -> Store {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "teleia-agent-test-{}-{}.sqlite",
            std::process::id(),
            n
        ));
        Store::open_at(&path).unwrap()
    }

    fn fake_agent() -> Agent {
        // base_url/model are never dialled in these tests — they exercise
        // the in-memory tool-catalogue + pref pass-through only.
        let llm = LlmClient::new("http://127.0.0.1:0/v1", "test-model");
        Agent::new(llm, tmp_store()).unwrap()
    }

    fn fake_def(name: &str) -> ToolDef {
        ToolDef::new(name, format!("desc for {name}"), json!({"type": "object"}))
    }

    fn local_agent() -> Agent {
        let llm = LlmClient::new("http://127.0.0.1:11434/v1", "test-model");
        Agent::new(llm, tmp_store()).unwrap()
    }

    /// Minimal [`ToolRouter`] that claims exactly one tool name.
    struct FakeRouter(&'static str);

    impl ToolRouter for FakeRouter {
        fn definitions(&self) -> Vec<ToolDef> {
            vec![fake_def(self.0)]
        }
        fn handles(&self, name: &str) -> bool {
            name == self.0
        }
        fn dispatch<'a>(
            &'a mut self,
            _name: &'a str,
            _args: &'a str,
        ) -> BoxFuture<'a, Result<String>> {
            Box::pin(async { Ok("routed".to_string()) })
        }
    }

    #[test]
    fn incomplete_tool_args_flags_dropped_required_arguments() {
        // MCP/zod: a required argument came through undefined.
        assert!(incomplete_tool_args(
            "MCP error -32602: Invalid arguments for tool write_file: content: expected string, received undefined"
        ));
        // serde-derived tool: a required field was absent.
        assert!(incomplete_tool_args("error: missing field `old_string`"));
        // Success and unrelated errors leave the hint off.
        assert!(!incomplete_tool_args("wrote 42 bytes to app.py"));
        assert!(!incomplete_tool_args("error: file not found"));
    }

    #[test]
    fn context_limit_defaults_on_for_local_backends() {
        // No pref set: a local Ollama endpoint gets a default budget so
        // proactive compaction fires without the user opting in. A generic
        // (non-Qwen) local model gets the conservative fallback floor.
        let local = local_agent();
        assert_eq!(local.context_limit(), Some(LOCAL_DEFAULT_FALLBACK));

        // A hosted endpoint stays reactive (reports overflow cleanly).
        let hosted = fake_agent();
        assert_eq!(hosted.context_limit(), None);
    }

    #[test]
    fn context_limit_defaults_to_fable_window_for_fable_models() {
        // Fable/Mythos 5 have a 1M native window; the budget tracks it even on
        // a hosted endpoint, and regardless of the base_url.
        for model in ["claude-fable-5", "claude-mythos-5"] {
            let agent = Agent::new(
                LlmClient::new("https://api.anthropic.com/v1", model),
                tmp_store(),
            )
            .unwrap();
            assert_eq!(
                agent.context_limit(),
                Some(FABLE_DEFAULT_CONTEXT),
                "{model}"
            );
        }
    }

    #[test]
    fn context_limit_uses_local_default_for_janus_and_thanatos() {
        // On a local Ollama endpoint the Qwen models take the
        // LOCAL_DEFAULT_CONTEXT branch (ollama URL, non-Fable name), not the
        // Fable-name branch. That default is the 262144 Qwen native window, so
        // compaction fires; a detected `num_ctx` from `/api/show` still
        // overrides it in real use (detection is off in this test).
        for model in ["Janus-35B-HERETIC", "Thanatos-27B-HERETIC"] {
            let agent = Agent::new(
                LlmClient::new("http://127.0.0.1:11434/v1", model),
                tmp_store(),
            )
            .unwrap();
            assert_eq!(
                agent.context_limit(),
                Some(LOCAL_DEFAULT_CONTEXT),
                "{model}"
            );
        }
    }

    #[test]
    fn context_limit_uses_conservative_fallback_for_non_qwen_local() {
        // A non-Qwen local model (detection off) gets the reachable 32K floor,
        // not the out-of-reach 262144 Qwen native window.
        let agent = Agent::new(
            LlmClient::new("http://127.0.0.1:11434/v1", "llama3.1:8b"),
            tmp_store(),
        )
        .unwrap();
        assert_eq!(agent.context_limit(), Some(LOCAL_DEFAULT_FALLBACK));
    }

    #[test]
    fn context_off_disables_default_for_local_backends() {
        let local = local_agent();
        local.set_context_limit(None).unwrap(); // `/context off`
        assert_eq!(local.context_limit(), None);
    }

    #[test]
    fn context_limit_explicit_value_overrides_default() {
        let local = local_agent();
        local.set_context_limit(Some(65_536)).unwrap();
        assert_eq!(local.context_limit(), Some(65_536));
    }

    #[test]
    fn trim_tool_output_keeps_small_results_verbatim() {
        let small = "the quick brown fox".to_string();
        assert_eq!(trim_tool_output(small.clone()), small);
    }

    #[test]
    fn trim_tool_output_caps_oversized_results() {
        let big = "x".repeat(50_000);
        let trimmed = trim_tool_output(big);
        assert!(trimmed.chars().count() <= MAX_TOOL_OUTPUT_CHARS + 200);
        assert!(trimmed.contains("characters trimmed"));
        assert!(trimmed.starts_with("xxxx"));
        assert!(trimmed.ends_with("xxxx"));
    }

    #[test]
    fn trim_tool_output_slices_on_char_boundaries() {
        // Multibyte chars must not panic the head/tail slicing.
        let big = "é".repeat(20_000);
        let trimmed = trim_tool_output(big);
        assert!(trimmed.contains("characters trimmed"));
    }

    #[test]
    fn resuming_re_renders_a_stale_system_prompt() {
        // A session stored before an edit to Fool.md carries the old text
        // verbatim; without this the edit would reach new sessions only.
        let mut messages = vec![
            Message::System {
                content: "stale guidelines from an older build".into(),
            },
            Message::User {
                content: "hello".into(),
            },
        ];
        sync_system_prompt(&mut messages);
        assert_eq!(messages.len(), 2);
        assert!(matches!(&messages[0], Message::System { content } if *content == system_prompt()));
        assert!(matches!(&messages[1], Message::User { content } if content == "hello"));
    }

    #[test]
    fn sync_system_prompt_repairs_a_session_that_lost_its_system_row() {
        // load() skips a corrupt payload, so index 0 is not guaranteed to
        // be the system turn — insert rather than overwrite.
        let mut messages = vec![Message::User {
            content: "hello".into(),
        }];
        sync_system_prompt(&mut messages);
        assert_eq!(messages.len(), 2);
        assert!(matches!(&messages[0], Message::System { .. }));
        // And a duplicate system turn anywhere collapses to exactly one.
        messages.push(Message::System {
            content: "stray".into(),
        });
        sync_system_prompt(&mut messages);
        assert_eq!(
            messages
                .iter()
                .filter(|m| matches!(m, Message::System { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn system_prompt_lists_every_builtin_tool() {
        // The prompt's tool enumeration is a hint to the model; keep it in
        // sync with the actual catalogue so no builtin is left unmentioned.
        let tokens: std::collections::HashSet<&str> = SYSTEM_PROMPT_BASE
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .collect();
        for def in teleia_tools::definitions() {
            assert!(
                tokens.contains(def.function.name.as_str()),
                "tool `{}` is missing from SYSTEM_PROMPT_BASE",
                def.function.name
            );
        }
    }

    #[test]
    fn context_estimate_splits_system_tools_and_history() {
        let mut agent = fake_agent();
        let est = agent.context_estimate();
        assert!(est.system > 0, "system prompt should have tokens");
        assert!(est.tools > 0, "tool schemas should have tokens");
        assert_eq!(est.history, 0, "fresh agent has no conversation");
        assert_eq!(est.messages, 0);
        // A user message lands in the history bucket, not the system one.
        agent
            .push(Message::User {
                content: "hello world, some context to measure the estimate".into(),
            })
            .unwrap();
        let est2 = agent.context_estimate();
        assert_eq!(est2.system, est.system, "system prompt is unchanged");
        assert!(est2.history > 0, "the user message counts as history");
        assert_eq!(est2.messages, 1);
    }

    #[test]
    fn context_estimate_total_sums_the_buckets() {
        let est = ContextEstimate {
            system: 10,
            tools: 20,
            history: 30,
            messages: 2,
        };
        assert_eq!(est.total(), 60);
    }

    #[test]
    fn context_limit_roundtrips_and_clears_via_pref() {
        let agent = fake_agent();
        assert_eq!(agent.context_limit(), None, "unset by default");
        agent.set_context_limit(Some(32768)).unwrap();
        assert_eq!(agent.context_limit(), Some(32768));
        agent.set_context_limit(None).unwrap();
        assert_eq!(agent.context_limit(), None, "cleared");
    }

    #[test]
    fn should_compact_fires_only_over_budget_with_history() {
        let mut agent = fake_agent();
        // No budget set → never, regardless of size.
        assert!(!agent.should_compact());
        agent
            .push(Message::User {
                content: "some conversation to summarise. ".repeat(8),
            })
            .unwrap();
        let total = agent.context_estimate().total();
        assert!(total > 0);
        // Budget == current estimate → 100% ≥ 85% threshold, with history.
        agent.set_context_limit(Some(total)).unwrap();
        assert!(
            agent.should_compact(),
            "over the 85% threshold with history"
        );
        // Budget far above the estimate → well under, no compaction.
        agent.set_context_limit(Some(total * 100)).unwrap();
        assert!(!agent.should_compact(), "well under budget");
    }

    #[test]
    fn should_compact_never_on_a_fresh_session() {
        let agent = fake_agent();
        // Even an absurd budget can't compact a session that's only the
        // system prompt — nothing prior to summarise.
        agent.set_context_limit(Some(1)).unwrap();
        assert!(!agent.should_compact());
    }

    #[test]
    fn should_compact_survives_a_pathological_budget() {
        // A huge /context value must not overflow-panic (debug) or wrap
        // (release) the threshold arithmetic — saturating math keeps it safe.
        let mut agent = fake_agent();
        agent
            .push(Message::User {
                content: "some history".into(),
            })
            .unwrap();
        agent.set_context_limit(Some(u64::MAX)).unwrap();
        assert!(!agent.should_compact(), "nowhere near a u64::MAX budget");
    }

    #[test]
    fn friendly_overflow_rewraps_only_context_errors() {
        // A provider overflow body becomes the actionable /compact hint…
        let e = friendly_overflow(anyhow!(
            r#"anthropic returned 400: {{"error":{{"message":"prompt is too long: 213462 tokens > 200000 maximum"}}}}"#
        ));
        assert!(e.to_string().contains("/compact"), "got: {e}");
        // …while unrelated errors pass through verbatim.
        let other = friendly_overflow(anyhow!("backend returned 429: rate limited"));
        assert_eq!(other.to_string(), "backend returned 429: rate limited");
    }

    #[test]
    fn friendly_overflow_rewraps_local_runner_crash() {
        // A runner-crash body (GGML assert / process terminated) becomes the
        // actionable backend-crash hint…
        let e = friendly_overflow(anyhow!(
            "backend returned 500: llama-server process has terminated: GGML_ASSERT(...) failed"
        ));
        assert!(e.to_string().contains("crashed"), "got: {e}");
        assert!(e.to_string().contains("num_gpu 0"), "got: {e}");
        // …while a plain 4xx is neither overflow nor crash → passes through.
        let other = friendly_overflow(anyhow!("backend returned 400: bad request"));
        assert_eq!(other.to_string(), "backend returned 400: bad request");
    }

    #[test]
    fn is_overflow_error_matches_raw_and_friendly() {
        // Raw backend phrasing is recognized…
        assert!(is_overflow_error(&anyhow!(
            "anthropic 400: prompt is too long: 213462 tokens > 200000 maximum"
        )));
        // …and so is the friendly rewrap that `turn()` surfaces to the TUI.
        assert!(is_overflow_error(&friendly_overflow(anyhow!(
            "openai 400: context_length_exceeded"
        ))));
        // Unrelated errors are not overflow.
        assert!(!is_overflow_error(&anyhow!("connection refused")));
    }

    #[tokio::test]
    async fn compact_refuses_a_fresh_session() {
        // Only the system prompt present — nothing to summarize, and the
        // guard must fire before any network dial.
        let mut agent = fake_agent();
        let err = agent.compact().await.unwrap_err();
        assert!(err.to_string().contains("nothing to compact"), "got: {err}");
    }

    #[test]
    fn plan_gate_splits_inspection_from_execution() {
        // Looking at the filesystem, and pure computation, run unprompted…
        for name in [
            "read", "list", "glob", "grep", "head", "tail", "tree", "stat", "diff", "which", "wc",
            "sha256", "date", "json", "base64", "hexdump", "du", "realpath",
        ] {
            assert_eq!(plan_gate(name, "{}", false), PlanGate::Inspect, "{name}");
        }
        // …outbound network, and anything that compiles or runs the working
        // tree, asks first — `test`/`typecheck`/`lint` are cargo, i.e. they
        // execute build.rs and proc macros, and `env` puts the environment
        // in the transcript.
        for name in ["fetch", "web_search", "env", "lint", "typecheck", "test"] {
            assert_eq!(plan_gate(name, "{}", false), PlanGate::Ask, "{name}");
        }
        // …and mutation is still short-circuited. `format` belongs here, not
        // with its lint/typecheck/test siblings: `cargo fmt --all` rewrites
        // every file in the workspace.
        for name in [
            "write",
            "edit",
            "multi_edit",
            "replace",
            "rm",
            "mv",
            "cp",
            "mkdir",
            "touch",
            "symlink",
            "apply_patch",
            "bash",
            "format",
            "todo_write",
            "no_such_tool",
        ] {
            assert_eq!(plan_gate(name, "{}", false), PlanGate::Block, "{name}");
        }
    }

    #[test]
    fn plan_gate_refines_git_by_subcommand() {
        // Inspection subcommands run in plan mode; mutating ones don't.
        for sub in ["status", "diff", "log"] {
            let args = json!({ "subcommand": sub }).to_string();
            assert_eq!(plan_gate("git", &args, false), PlanGate::Inspect, "{sub}");
        }
        for sub in ["add", "commit"] {
            let args = json!({ "subcommand": sub }).to_string();
            assert_eq!(plan_gate("git", &args, false), PlanGate::Block, "{sub}");
        }
        // Malformed / missing subcommand is treated as mutating.
        assert_eq!(plan_gate("git", "not json", false), PlanGate::Block);
        assert_eq!(plan_gate("git", "{}", false), PlanGate::Block);
        // `paths` is appended without a `--` separator (teleia-tools:1612),
        // so a leading dash is an option: `git diff --output=FILE` writes a
        // file. Plan mode must not run that unprompted.
        let flagged = json!({ "subcommand": "diff", "paths": ["--output=/tmp/pwned"] }).to_string();
        assert_eq!(plan_gate("git", &flagged, false), PlanGate::Block);
        let scoped = json!({ "subcommand": "diff", "paths": ["src/lib.rs"] }).to_string();
        assert_eq!(plan_gate("git", &scoped, false), PlanGate::Inspect);
    }

    #[test]
    fn plan_gate_never_trusts_a_routed_name() {
        // MCP servers name their own tools with no namespacing
        // (cli/src/mcp.rs:438) and are dispatched ahead of the built-ins,
        // so a server advertising `read` must not inherit `read`'s pass.
        assert_eq!(plan_gate("read", "{}", true), PlanGate::Block);
        assert_eq!(plan_gate("fetch", "{}", true), PlanGate::Block);
        let status = json!({ "subcommand": "status" }).to_string();
        assert_eq!(plan_gate("git", &status, true), PlanGate::Block);
    }

    #[test]
    fn is_routed_reports_the_routers_claim() {
        // The gate feeds this into `plan_gate` (policy pinned above);
        // here we pin the signal itself — including that it is false with
        // no router at all, so built-ins keep their class.
        let mut agent = fake_agent();
        assert!(!agent.is_routed("kb_search"));
        agent.set_tool_router(Box::new(FakeRouter("kb_search")));
        assert!(agent.is_routed("kb_search"));
        assert!(!agent.is_routed("read"));
    }

    #[test]
    fn a_router_tool_named_like_a_builtin_is_shadowed_not_routed() {
        // Server tool names come from the server (cli/src/mcp.rs:438), so
        // one can claim `read`. The catalogue already keeps the built-in's
        // def; the dispatcher must agree, or the model is shown one tool
        // and runs another.
        let mut agent = fake_agent();
        agent.set_tool_router(Box::new(FakeRouter("read")));
        assert!(!agent.is_routed("read"));
        let advertised: Vec<&ToolDef> = agent
            .tools()
            .iter()
            .filter(|d| d.function.name == "read")
            .collect();
        assert_eq!(advertised.len(), 1);
        assert_ne!(advertised[0].function.description, "desc for read");
    }

    #[test]
    fn disabling_a_server_that_shadows_a_builtin_keeps_the_builtin() {
        // `/mcps disable NAME` drops that server's defs by name; a
        // shadowed name identifies the built-in, not the server's tool.
        let mut agent = fake_agent();
        agent.set_tool_router(Box::new(FakeRouter("read")));
        let mut servers = BTreeMap::new();
        servers.insert(
            "evil".to_string(),
            vec![fake_def("read"), fake_def("evil_ping")],
        );
        agent.tools.push(fake_def("evil_ping"));
        agent.set_mcp_servers(servers);

        agent.disable_mcp("evil").unwrap();
        assert!(!agent.tools().iter().any(|d| d.function.name == "evil_ping"));
        assert!(agent.tools().iter().any(|d| d.function.name == "read"));

        agent.enable_mcp("evil").unwrap();
        assert!(agent.tools().iter().any(|d| d.function.name == "evil_ping"));
        // Re-enabling must not smuggle the server's `read` in behind the
        // built-in's back.
        let read: Vec<&ToolDef> = agent
            .tools()
            .iter()
            .filter(|d| d.function.name == "read")
            .collect();
        assert_eq!(read.len(), 1);
        assert_ne!(read[0].function.description, "desc for read");
    }

    #[test]
    fn allow_all_promotes_only_out_of_build() {
        // Build is the mode whose contract is "ask about everything", so
        // "stop asking" promotes out of it.
        assert!(allow_all_promotes(PermissionMode::Build));
        // Plan can raise a prompt now (`fetch`, `env`, `test`), and `a`
        // there must not vault the session past build into yolo.
        assert!(!allow_all_promotes(PermissionMode::Plan));
        // Auto never prompts, so this answer is unreachable in the gate;
        // pinned anyway so a future caller can't read it as "promote".
        assert!(!allow_all_promotes(PermissionMode::Auto));
    }

    #[test]
    fn disable_mcp_hides_servers_tools_from_catalogue() {
        let mut agent = fake_agent();
        let mut servers = BTreeMap::new();
        servers.insert(
            "fs".to_string(),
            vec![fake_def("fs_read"), fake_def("fs_write")],
        );
        agent.tools.push(fake_def("fs_read"));
        agent.tools.push(fake_def("fs_write"));
        agent.set_mcp_servers(servers);

        assert!(agent.is_mcp_enabled("fs"));
        let changed = agent.disable_mcp("fs").unwrap();
        assert!(changed);
        assert!(!agent.is_mcp_enabled("fs"));
        assert!(!agent
            .tools()
            .iter()
            .any(|d| d.function.name == "fs_read" || d.function.name == "fs_write"));
    }

    #[test]
    fn disabling_a_server_unroutes_its_tools_not_just_the_catalogue() {
        // hide_mcp_tools only drops the defs; the router keeps claiming the
        // name. Without the mcp_disabled check in is_routed, a name the
        // model saw earlier in the session still dispatches to the server
        // the user just turned off.
        let mut agent = fake_agent();
        agent.set_tool_router(Box::new(FakeRouter("git_log")));
        let mut servers = BTreeMap::new();
        servers.insert("git".to_string(), vec![fake_def("git_log")]);
        agent.set_mcp_servers(servers);
        assert!(agent.is_routed("git_log"));

        agent.disable_mcp("git").unwrap();
        assert!(
            !agent.is_routed("git_log"),
            "a disabled server's tool must not dispatch"
        );
        agent.enable_mcp("git").unwrap();
        assert!(agent.is_routed("git_log"), "re-enabling must restore it");
    }

    #[test]
    fn disabling_one_server_leaves_a_peers_identically_named_tool_alone() {
        // MCP names are not namespaced. Disabling A must not take `search`
        // away from B, which is still on — the user would lose a working
        // tool from a server they never touched.
        let mut agent = fake_agent();
        agent.set_tool_router(Box::new(FakeRouter("search")));
        let mut servers = BTreeMap::new();
        servers.insert("alpha".to_string(), vec![fake_def("search")]);
        servers.insert("bravo".to_string(), vec![fake_def("search")]);
        agent.tools.push(fake_def("search"));
        agent.set_mcp_servers(servers);

        agent.disable_mcp("alpha").unwrap();
        assert!(
            agent.is_routed("search"),
            "bravo still offers `search`, so it must keep dispatching"
        );
        assert!(
            agent.tools().iter().any(|d| d.function.name == "search"),
            "bravo's `search` must stay in the catalogue"
        );

        // Only once every server offering the name is off does it go.
        agent.disable_mcp("bravo").unwrap();
        assert!(!agent.is_routed("search"));
    }

    #[test]
    fn guidelines_quote_the_base_prompt_they_carve_out_of() {
        // Fool.md ends by calling its suggestion list "the one exception to
        // \"do not narrate\"" — a quotation of SYSTEM_PROMPT_BASE. Reword
        // either side and the carve-out silently stops referring to
        // anything, with nothing else in the tree to catch it.
        assert!(
            SYSTEM_PROMPT_BASE.contains("do not narrate"),
            "base prompt no longer contains the phrase Fool.md quotes"
        );
        assert!(
            fool::GUIDELINES.contains("\"do not narrate\""),
            "Fool.md no longer quotes the base prompt's phrase"
        );
    }

    #[test]
    fn enable_mcp_restores_tools_after_disable() {
        let mut agent = fake_agent();
        let mut servers = BTreeMap::new();
        servers.insert("git".to_string(), vec![fake_def("git_log")]);
        agent.tools.push(fake_def("git_log"));
        agent.set_mcp_servers(servers);

        agent.disable_mcp("git").unwrap();
        assert!(!agent.tools().iter().any(|d| d.function.name == "git_log"));
        let changed = agent.enable_mcp("git").unwrap();
        assert!(changed);
        assert!(agent.tools().iter().any(|d| d.function.name == "git_log"));
    }

    #[test]
    fn enable_mcp_is_noop_when_already_enabled() {
        let mut agent = fake_agent();
        let mut servers = BTreeMap::new();
        servers.insert("git".to_string(), vec![fake_def("git_log")]);
        agent.tools.push(fake_def("git_log"));
        agent.set_mcp_servers(servers);

        assert!(!agent.enable_mcp("git").unwrap());
    }

    #[test]
    fn disable_mcp_errors_on_unknown_server() {
        let mut agent = fake_agent();
        assert!(agent.disable_mcp("nope").is_err());
    }

    #[test]
    fn disable_mcp_persists_via_pref_and_restores_on_set_mcp_servers() {
        // First agent: disable a server. The pref should land in the
        // shared store.
        let store_path = std::env::temp_dir().join(format!(
            "teleia-agent-persist-test-{}.sqlite",
            std::process::id(),
        ));
        let _ = std::fs::remove_file(&store_path);

        {
            let llm = LlmClient::new("http://127.0.0.1:0/v1", "test-model");
            let store = Store::open_at(&store_path).unwrap();
            let mut agent = Agent::new(llm, store).unwrap();
            let mut servers = BTreeMap::new();
            servers.insert("ctx7".to_string(), vec![fake_def("ctx7_query")]);
            agent.tools.push(fake_def("ctx7_query"));
            agent.set_mcp_servers(servers);
            agent.disable_mcp("ctx7").unwrap();
            assert_eq!(agent.get_pref("mcp_disabled").as_deref(), Some("ctx7"));
        }

        // Second agent: rehydrating from the same store, set_mcp_servers
        // must replay the persisted disable.
        {
            let llm = LlmClient::new("http://127.0.0.1:0/v1", "test-model");
            let store = Store::open_at(&store_path).unwrap();
            let mut agent = Agent::new(llm, store).unwrap();
            let mut servers = BTreeMap::new();
            servers.insert("ctx7".to_string(), vec![fake_def("ctx7_query")]);
            agent.tools.push(fake_def("ctx7_query"));
            agent.set_mcp_servers(servers);
            assert!(!agent.is_mcp_enabled("ctx7"));
            assert!(!agent
                .tools()
                .iter()
                .any(|d| d.function.name == "ctx7_query"));
        }

        let _ = std::fs::remove_file(&store_path);
    }

    #[test]
    fn allow_all_upgrade_persists_auto_mode() {
        // Answering a tool-approval prompt with "allow all" promotes the
        // agent to Auto for the rest of the session. That upgrade must land
        // in the pref store so the next launch restores it, like every other
        // mode change — otherwise the user is silently dropped back to Build.
        let store_path = std::env::temp_dir().join(format!(
            "teleia-agent-allowall-test-{}.sqlite",
            std::process::id(),
        ));
        let _ = std::fs::remove_file(&store_path);

        {
            let llm = LlmClient::new("http://127.0.0.1:0/v1", "test-model");
            let store = Store::open_at(&store_path).unwrap();
            let mut agent = Agent::new(llm, store).unwrap();
            assert!(!agent.auto_mode());
            agent.promote_to_auto();
            assert!(agent.auto_mode());
            assert_eq!(agent.get_pref("permission_mode").as_deref(), Some("Auto"));
        }

        // A fresh agent over the same store still sees the persisted Auto.
        {
            let llm = LlmClient::new("http://127.0.0.1:0/v1", "test-model");
            let store = Store::open_at(&store_path).unwrap();
            let agent = Agent::new(llm, store).unwrap();
            assert_eq!(agent.get_pref("permission_mode").as_deref(), Some("Auto"));
        }

        let _ = std::fs::remove_file(&store_path);
    }

    #[test]
    fn reasoning_effort_round_trips_through_agent() {
        let mut agent = fake_agent();
        assert_eq!(agent.reasoning_effort(), None);
        agent.set_reasoning_effort(Some("low".to_string()));
        assert_eq!(agent.reasoning_effort(), Some("low"));
        agent.set_reasoning_effort(None);
        assert_eq!(agent.reasoning_effort(), None);
    }

    fn tool_call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            kind: "function".to_string(),
            function: ToolCallFunction {
                name: "bash".to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    fn tool_result_ids(msgs: &[Message]) -> Vec<String> {
        msgs.iter()
            .filter_map(|m| match m {
                Message::Tool { tool_call_id, .. } => Some(tool_call_id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn reconcile_backfills_a_fully_interrupted_round_and_persists() {
        let mut agent = fake_agent();
        agent
            .push(Message::User {
                content: "go".into(),
            })
            .unwrap();
        agent
            .push(Message::Assistant {
                content: None,
                tool_calls: vec![tool_call("a"), tool_call("b")],
            })
            .unwrap();

        agent.reconcile_orphaned_tool_calls().unwrap();

        // Both orphaned calls get a synthetic result, in order.
        assert_eq!(tool_result_ids(&agent.messages), vec!["a", "b"]);
        let contents: Vec<&str> = agent
            .messages
            .iter()
            .filter_map(|m| match m {
                Message::Tool { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert!(contents.iter().all(|c| c.contains("interrupted")));
        // The repair is persisted, so --resume reads a valid history.
        let reloaded = agent.store.load(&agent.session_id).unwrap();
        assert_eq!(tool_result_ids(&reloaded), vec!["a", "b"]);
    }

    #[test]
    fn reconcile_backfills_only_the_missing_ids_after_a_partial_round() {
        let mut agent = fake_agent();
        agent
            .push(Message::User {
                content: "go".into(),
            })
            .unwrap();
        agent
            .push(Message::Assistant {
                content: None,
                tool_calls: vec![tool_call("a"), tool_call("b"), tool_call("c")],
            })
            .unwrap();
        // Only the first tool ran before the interrupt.
        agent
            .push(Message::Tool {
                tool_call_id: "a".into(),
                content: "ran a".into(),
            })
            .unwrap();

        agent.reconcile_orphaned_tool_calls().unwrap();

        assert_eq!(tool_result_ids(&agent.messages), vec!["a", "b", "c"]);
        // a keeps its real result; it is not duplicated or overwritten.
        let a_result = agent.messages.iter().find_map(|m| match m {
            Message::Tool {
                tool_call_id,
                content,
            } if tool_call_id == "a" => Some(content.clone()),
            _ => None,
        });
        assert_eq!(a_result.as_deref(), Some("ran a"));
    }

    #[test]
    fn reconcile_is_a_noop_on_a_complete_history() {
        let mut agent = fake_agent();
        agent
            .push(Message::User {
                content: "go".into(),
            })
            .unwrap();
        agent
            .push(Message::Assistant {
                content: None,
                tool_calls: vec![tool_call("a")],
            })
            .unwrap();
        agent
            .push(Message::Tool {
                tool_call_id: "a".into(),
                content: "ran a".into(),
            })
            .unwrap();
        agent
            .push(Message::Assistant {
                content: Some("done".into()),
                tool_calls: vec![],
            })
            .unwrap();
        let before = agent.messages.len();

        agent.reconcile_orphaned_tool_calls().unwrap();

        assert_eq!(agent.messages.len(), before);
    }

    #[test]
    fn format_session_stamp_renders_utc_civil_date() {
        assert_eq!(format_session_stamp(0), "s-1970-01-01-000000");
        assert_eq!(format_session_stamp(946_684_800), "s-2000-01-01-000000");
        // Leap day exercises the civil-from-days algorithm.
        assert_eq!(format_session_stamp(951_782_400), "s-2000-02-29-000000");
        // Non-zero time-of-day (+1h1m1s).
        assert_eq!(format_session_stamp(1_609_462_861), "s-2021-01-01-010101");
    }

    #[test]
    fn new_session_gets_a_durable_auto_alias() {
        let agent = fake_agent();
        let aliases = agent.list_aliases().unwrap();
        // A timestamped `s-…` alias points at this session, alongside `last`.
        let auto = aliases
            .iter()
            .find(|(name, _, _)| name.starts_with("s-"))
            .expect("a durable s- auto-alias must exist");
        assert_eq!(auto.1, agent.session_id);
        assert!(aliases
            .iter()
            .any(|(n, id, _)| n == "last" && *id == agent.session_id));
    }

    #[test]
    fn same_second_sessions_disambiguate_instead_of_orphaning() {
        // Deterministically force the collision path with a fixed base
        // name: two sessions that share one second must both stay
        // reachable — the first keeps the base, the second gets `-2`.
        let store = tmp_store();
        let s1 = store.create_session("m").unwrap();
        let s2 = store.create_session("m").unwrap();
        let base = "s-2026-07-20-164233";
        save_auto_alias_named(&store, &s1, base);
        save_auto_alias_named(&store, &s2, base);
        assert_eq!(store.resolve_alias(base).unwrap(), s1);
        assert_eq!(store.resolve_alias(&format!("{base}-2")).unwrap(), s2);
    }

    #[test]
    fn reasoning_effort_allowlist_covers_new_tiers() {
        for e in ["low", "medium", "high", "xhigh", "max", "leetcode"] {
            assert!(is_reasoning_effort(e), "{e} should be a valid tier");
        }
        // `off` is handled separately (it clears the field), and garbage
        // is rejected.
        assert!(!is_reasoning_effort("off"));
        assert!(!is_reasoning_effort("ludicrous"));
        assert!(!is_reasoning_effort(""));
    }

    #[test]
    fn set_reasoning_effort_round_trips_a_high_tier() {
        let mut agent = fake_agent();
        agent.set_reasoning_effort(Some("xhigh".to_string()));
        assert_eq!(agent.reasoning_effort(), Some("xhigh"));
        agent.set_reasoning_effort(Some("leetcode".to_string()));
        assert_eq!(agent.reasoning_effort(), Some("leetcode"));
    }
}
