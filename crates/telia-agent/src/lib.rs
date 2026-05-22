use anyhow::Result;
use async_stream::try_stream;
use futures_util::{future::BoxFuture, pin_mut, Stream, StreamExt};
use telia_llm::{ChatEvent, LlmClient, Message, ToolDef};
use telia_store::Store;

/// External tool source — implemented by the CLI's MCP registry (and
/// eventually LSP). The agent advertises `definitions()` alongside its
/// built-ins, and routes any matching tool call back through
/// `dispatch()` instead of the static `telia_tools::dispatch`.
pub trait ToolRouter: Send {
    fn definitions(&self) -> Vec<ToolDef>;
    fn handles(&self, name: &str) -> bool;
    fn dispatch<'a>(&'a mut self, name: &'a str, args: &'a str) -> BoxFuture<'a, Result<String>>;
}

mod kaparthy;

#[derive(Debug, Clone, Copy, Default)]
pub struct TokenCounts {
    pub prompt: u64,
    pub completion: u64,
}

const SYSTEM_PROMPT_BASE: &str = "You are τέλεια, a terse coding assistant running in a terminal. \
Use the provided tools to do real work: read, write, edit, bash, list, glob, grep, head, tail, \
tree, stat, diff, which, fetch, mkdir, mv, cp, apply_patch, wc, touch, sha256, date (plus any \
MCP tools the user has configured). Default to brief replies. When you finish a turn, stop — \
do not narrate.";

/// Base prompt + the kaparthy-derived guidelines, joined once at startup.
fn system_prompt() -> String {
    format!("{SYSTEM_PROMPT_BASE}\n\n{}", kaparthy::GUIDELINES)
}

pub const MAX_TOOL_HOPS: usize = 16;

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
}

impl Agent {
    pub fn new(llm: LlmClient, store: Store) -> Result<Self> {
        let session_id = store.create_session(llm.model())?;
        // Auto-bookmark every new session as `last` so the next launch
        // can `--resume` without the user needing to type `/save`.
        let _ = store.save_alias("last", &session_id);
        let mut agent = Self {
            llm,
            tools: telia_tools::definitions(),
            store,
            session_id,
            messages: Vec::new(),
            seq: 0,
            tokens: TokenCounts::default(),
            available_models: Vec::new(),
            permission_mode: PermissionMode::default(),
            router: None,
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
                    tools: telia_tools::definitions(),
                    store,
                    session_id: id,
                    messages,
                    seq,
                    tokens: TokenCounts::default(),
                    available_models: Vec::new(),
                    permission_mode: PermissionMode::default(),
                    router: None,
                })
            }
            None => Self::new(llm, store),
        }
    }

    /// Plug an external tool source into the agent. Its definitions
    /// are appended to the built-in tool list immediately; subsequent
    /// dispatches check the router before falling back to
    /// `telia_tools`.
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

    pub fn permission_mode(&self) -> PermissionMode {
        self.permission_mode
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
        try_stream! {
            self.push(Message::User { content: user_input })?;

            for _ in 0..MAX_TOOL_HOPS {
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
                        match telia_tools::dispatch(
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
            }

            yield TurnEvent::AssistantStart;
            yield TurnEvent::AssistantDelta(format!(
                "[stopped: hit tool-hop limit of {MAX_TOOL_HOPS}]"
            ));
            yield TurnEvent::AssistantEnd;
            yield TurnEvent::TurnEnd;
        }
    }

    fn push(&mut self, message: Message) -> Result<()> {
        self.store.append(&self.session_id, self.seq, &message)?;
        self.seq += 1;
        self.messages.push(message);
        Ok(())
    }
}
