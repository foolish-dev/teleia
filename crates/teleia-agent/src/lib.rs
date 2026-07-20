use anyhow::Result;
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

// Filename is capital-K (matches `Karpathy.md` at the workspace root),
// but the module identifier stays snake_case so callers keep using
// `karpathy::GUIDELINES`.
#[path = "Karpathy.rs"]
mod karpathy;

#[derive(Debug, Clone, Copy, Default)]
pub struct TokenCounts {
    pub prompt: u64,
    pub completion: u64,
}

const SYSTEM_PROMPT_BASE: &str = "You are τέλεια, a terse coding assistant running in a terminal. \
Use the provided tools to do real work: read, write, edit, bash, list, glob, grep, head, tail, \
tree, stat, diff, which, fetch, mkdir, mv, cp, apply_patch, wc, touch, sha256, date, lint, \
format, typecheck (plus any MCP tools the user has configured). After any code change, run \
`lint`/`typecheck` to confirm the edit before claiming done. Default to brief replies. When \
you finish a turn, stop — do not narrate.";

/// Base prompt + the karpathy-derived guidelines, joined once at startup.
fn system_prompt() -> String {
    format!("{SYSTEM_PROMPT_BASE}\n\n{}", karpathy::GUIDELINES)
}

/// Events emitted by `turn()`. The TUI consumes these to render
/// incrementally. Not `Clone` because `ToolApprovalRequest` carries a
/// `oneshot::Sender` that owns its single reply slot.
#[derive(Debug)]
pub enum TurnEvent {
    AssistantStart,
    AssistantDelta(String),
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
    },
    TurnEnd,
}

/// User's response to a [`TurnEvent::ToolApprovalRequest`]. `AllowAll`
/// permits the call and also flips the agent into auto mode for the
/// rest of the session; `Deny` injects a `"denied by user"` tool result
/// so the model can react.
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
    /// Read-only investigation: `read` / `list` / `glob` / `grep` run
    /// without prompting; `write` / `edit` / `bash` short-circuit with
    /// a synthetic "blocked: plan mode" tool result so the model is
    /// pushed toward describing what it would do.
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

/// Tools that just *look* at the filesystem (or fetch read-only data)
/// and don't change anything. Anything else is gated by
/// [`PermissionMode::Plan`].
fn is_readonly_tool(name: &str) -> bool {
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
            | "fetch"
            | "wc"
            | "sha256"
            | "date"
            | "lint"
            | "typecheck"
    )
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
}

impl Agent {
    pub fn new(llm: LlmClient, store: Store) -> Result<Self> {
        let session_id = store.create_session(llm.model())?;
        // Auto-bookmark every new session as `last` so the next launch
        // can `--resume` without the user needing to type `/save`.
        let _ = store.save_alias("last", &session_id);
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
                let messages = store.load(&id)?;
                let seq = messages.len();
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
                })
            }
            None => Self::new(llm, store),
        }
    }

    /// Plug an external tool source into the agent. Its definitions
    /// are appended to the built-in tool list immediately; subsequent
    /// dispatches check the router before falling back to
    /// `teleia_tools`.
    pub fn set_tool_router(&mut self, router: Box<dyn ToolRouter>) {
        for def in router.definitions() {
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
        let drop: std::collections::HashSet<&str> =
            defs.iter().map(|d| d.function.name.as_str()).collect();
        self.tools
            .retain(|t| !drop.contains(t.function.name.as_str()));
    }

    fn show_mcp_tools(&mut self, name: &str) {
        let Some(defs) = self.mcp_servers.get(name).cloned() else {
            return;
        };
        for def in defs {
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
        self.set_pref("mcp_disabled", &joined.join(","));
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

    // ---- preference + history pass-through ----
    // Centralised so the TUI doesn't need to own its own Store handle.

    pub fn get_pref(&self, key: &str) -> Option<String> {
        self.store.get_pref(key).ok().flatten()
    }

    pub fn set_pref(&self, key: &str, value: &str) {
        let _ = self.store.set_pref(key, value);
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
        self.session_id = session_id;
        self.messages.clear();
        self.seq = 0;
        self.tokens = TokenCounts::default();
        self.push(Message::System {
            content: system_prompt(),
        })?;
        Ok(())
    }

    pub fn load_alias(&mut self, name: &str) -> Result<String> {
        let session_id = self.store.resolve_alias(name)?;
        let messages = self.store.load(&session_id)?;
        self.session_id = session_id.clone();
        self.messages = messages;
        self.seq = self.messages.len();
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

                {
                    let stream = self.llm.stream(&self.messages, Some(&self.tools));
                    pin_mut!(stream);
                    while let Some(event) = stream.next().await {
                        match event? {
                            ChatEvent::ContentDelta(text) => {
                                content_buf.push_str(&text);
                                yield TurnEvent::AssistantDelta(text);
                            }
                            ChatEvent::Done { tool_calls: tcs, usage } => {
                                tool_calls = tcs;
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
                    // Permission gate. Three modes, each with its own
                    // policy: Auto runs everything; Plan auto-allows
                    // read-only tools and synthesizes a "blocked" result
                    // for write/edit/bash so the model knows to describe
                    // rather than execute; Build prompts per-call.
                    match self.permission_mode {
                        PermissionMode::Auto => {}
                        PermissionMode::Plan if is_readonly_tool(&call.function.name) => {}
                        PermissionMode::Plan => {
                            let output = format!(
                                "blocked: plan mode does not permit `{}`. Describe what you would do; the user can switch to build mode (Shift+Tab or /build) to execute.",
                                call.function.name
                            );
                            yield TurnEvent::ToolStart {
                                name: call.function.name.clone(),
                                arguments: call.function.arguments.clone(),
                            };
                            yield TurnEvent::ToolEnd {
                                name: call.function.name.clone(),
                                output: output.clone(),
                            };
                            self.push(Message::Tool {
                                tool_call_id: call.id.clone(),
                                content: output,
                            })?;
                            continue;
                        }
                        PermissionMode::Build => {
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
                                    self.permission_mode = PermissionMode::Auto;
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
                    let routed = self
                        .router
                        .as_ref()
                        .map(|r| r.handles(&call.function.name))
                        .unwrap_or(false);
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
                    yield TurnEvent::ToolEnd {
                        name: call.function.name.clone(),
                        output: output.clone(),
                    };
                    self.push(Message::Tool {
                        tool_call_id: call.id.clone(),
                        content: output,
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
}
