use anyhow::{anyhow, Context, Result};
use async_stream::try_stream;
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        #[serde(default, serialize_with = "serialize_content_or_empty")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        #[serde(default)]
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_tool_type")]
    pub kind: String,
    pub function: ToolCallFunction,
}

fn default_tool_type() -> String {
    "function".to_string()
}

/// Serialize an assistant message's `content` as a plain string, writing
/// `""` for `None` rather than omitting the field or emitting `null`.
/// OpenAI-compatible backends written in Go — notably Ollama (reproduced
/// on 0.30.8) — type-switch on the JSON `content` value and reject a
/// missing/`null` content with `400 invalid message content type: <nil>`.
/// That permanently wedges any session whose history contains an empty
/// assistant turn (no text, no tool calls). An empty string is accepted by
/// every provider we route to.
fn serialize_content_or_empty<S>(content: &Option<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(content.as_deref().unwrap_or(""))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: ToolDefFunction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolDef {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            kind: "function",
            function: ToolDefFunction {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

/// Token usage reported by the OpenAI-compatible /chat/completions endpoint.
/// Ollama emits these in the final SSE chunk when `stream_options.include_usage`
/// is set on the request.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
}

/// One progress update from Ollama's streaming `/api/pull` endpoint.
/// `total`/`completed` are only present while a specific layer is
/// downloading; status-only lines (`"pulling manifest"`, `"success"`)
/// leave them as `None`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PullProgress {
    pub status: String,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub total: Option<u64>,
    #[serde(default)]
    pub completed: Option<u64>,
}

/// Streaming events from the chat endpoint.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    ContentDelta(String),
    /// A chunk of the model's reasoning/"thinking" (see [`ChunkDelta`]).
    ReasoningDelta(String),
    Done {
        tool_calls: Vec<ToolCall>,
        usage: Option<Usage>,
        /// The final `finish_reason`. `Some("length")` means the response
        /// was truncated at the token/context limit.
        finish_reason: Option<String>,
    },
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ToolDef]>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
}

/// Map a teleia effort label to the wire value sent as `reasoning_effort`.
/// teleia exposes finer labels than backends accept: `xhigh` and the
/// top-tier `leetcode` have no standard wire value, so they request the
/// highest real tier, `max`, rather than 400ing (e.g. a backend that only
/// accepts none/low/medium/high/max). Standard tiers pass through
/// unchanged; reasoning-incapable backends and Ollama ignore the field.
fn wire_reasoning_effort(label: &str) -> &str {
    match label {
        "xhigh" | "leetcode" => "max",
        other => other,
    }
}

#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: ChunkDelta,
    /// Why generation stopped, on the final chunk: `"stop"` (natural end),
    /// `"length"` (hit the token/context limit — truncated), etc.
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    /// Reasoning/"thinking" channel. Reasoning models stream their
    /// chain-of-thought here with `content` empty until they finish
    /// thinking — Ollama uses `reasoning`, DeepSeek-style backends use
    /// `reasoning_content`. Without surfacing it, a turn that is all
    /// reasoning (or gets cut off mid-think) renders blank.
    #[serde(default, alias = "reasoning_content")]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
}

#[derive(Debug, Deserialize)]
struct ToolCallDelta {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    function: Option<ToolCallFunctionDelta>,
}

#[derive(Debug, Default, Deserialize)]
struct ToolCallFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

pub struct LlmClient {
    base_url: String,
    model: String,
    api_key: Option<String>,
    /// Reasoning effort sent as the OpenAI-standard `reasoning_effort`
    /// request field (`low` / `medium` / `high`). `None` omits it, so
    /// non-reasoning models and Ollama are unaffected unless the user
    /// opts in via `/effort`.
    reasoning_effort: Option<String>,
    http: reqwest::Client,
}

/// Static descriptor for a cloud chat-completions provider. The table
/// below is the single source of truth for routing model names → base
/// URL + API-key env var.
#[derive(Debug, Clone, Copy)]
pub struct Provider {
    /// Human-readable name; doubles as the explicit `provider:` selector
    /// (matched case-insensitively) for ambiguous model names.
    pub name: &'static str,
    pub base_url: &'static str,
    pub env_var: &'static str,
    /// Model-name prefixes that route here automatically (e.g.
    /// `claude-` → Anthropic). May be empty for providers whose model
    /// names collide with others — those require the explicit
    /// `provider:model` form.
    pub prefixes: &'static [&'static str],
}

/// All cloud providers teleia knows about out of the box. The order
/// matters: detection scans top-to-bottom and returns the first match.
/// Anything not matched falls back to a local Ollama endpoint.
pub const PROVIDERS: &[Provider] = &[
    Provider {
        name: "Anthropic",
        base_url: "https://api.anthropic.com/v1",
        env_var: "ANTHROPIC_API_KEY",
        prefixes: &["claude-"],
    },
    Provider {
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        env_var: "OPENAI_API_KEY",
        prefixes: &["gpt-", "o1", "o3", "o4"],
    },
    Provider {
        name: "Google",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        env_var: "GEMINI_API_KEY",
        prefixes: &["gemini-"],
    },
    Provider {
        name: "xAI",
        base_url: "https://api.x.ai/v1",
        env_var: "XAI_API_KEY",
        prefixes: &["grok-"],
    },
    Provider {
        name: "DeepSeek",
        base_url: "https://api.deepseek.com/v1",
        env_var: "DEEPSEEK_API_KEY",
        prefixes: &["deepseek-"],
    },
    Provider {
        name: "Mistral",
        base_url: "https://api.mistral.ai/v1",
        env_var: "MISTRAL_API_KEY",
        // Every family Mistral's API serves, because `provider_selector`
        // uses these to tell `mistral:pixtral-large-latest` (cloud) from
        // `mistral:7b` (Ollama). A family missing here silently routes a
        // paying customer's model to localhost.
        prefixes: &[
            "mistral-",
            "codestral-",
            "ministral-",
            "pixtral-",
            "magistral-",
            "open-mistral-",
            "open-mixtral-",
        ],
    },
    Provider {
        name: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        env_var: "GROQ_API_KEY",
        // Groq hosts open models whose names (`llama-*`, etc.) collide
        // with what users might run locally on Ollama. Require the
        // explicit `groq:` prefix to route here.
        prefixes: &[],
    },
    Provider {
        name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        env_var: "OPENROUTER_API_KEY",
        // OpenRouter mirrors every other provider's catalog; explicit
        // `openrouter:` prefix only.
        prefixes: &[],
    },
    Provider {
        name: "Cohere",
        // OpenAI-compatible endpoint; the native `/v1/chat` uses a
        // different request shape so we point at /compatibility/v1.
        base_url: "https://api.cohere.ai/compatibility/v1",
        env_var: "COHERE_API_KEY",
        prefixes: &["command-"],
    },
    Provider {
        name: "Perplexity",
        base_url: "https://api.perplexity.ai",
        env_var: "PERPLEXITY_API_KEY",
        prefixes: &["sonar"],
    },
    Provider {
        name: "Together",
        base_url: "https://api.together.xyz/v1",
        env_var: "TOGETHER_API_KEY",
        // Together hosts open models whose names collide with Ollama;
        // explicit `together:` prefix only.
        prefixes: &[],
    },
    Provider {
        name: "Fireworks",
        base_url: "https://api.fireworks.ai/inference/v1",
        env_var: "FIREWORKS_API_KEY",
        prefixes: &[],
    },
    Provider {
        name: "Cerebras",
        base_url: "https://api.cerebras.ai/v1",
        env_var: "CEREBRAS_API_KEY",
        prefixes: &[],
    },
    Provider {
        name: "Hyperbolic",
        base_url: "https://api.hyperbolic.xyz/v1",
        env_var: "HYPERBOLIC_API_KEY",
        prefixes: &[],
    },
    Provider {
        name: "Nvidia",
        base_url: "https://integrate.api.nvidia.com/v1",
        env_var: "NVIDIA_API_KEY",
        prefixes: &[],
    },
    Provider {
        name: "AI21",
        base_url: "https://api.ai21.com/studio/v1",
        env_var: "AI21_API_KEY",
        prefixes: &["jamba-"],
    },
    Provider {
        name: "Anyscale",
        base_url: "https://api.endpoints.anyscale.com/v1",
        env_var: "ANYSCALE_API_KEY",
        prefixes: &[],
    },
    Provider {
        name: "Lepton",
        base_url: "https://api.lepton.ai/v1",
        env_var: "LEPTON_API_KEY",
        prefixes: &[],
    },
    Provider {
        name: "DeepInfra",
        base_url: "https://api.deepinfra.com/v1/openai",
        env_var: "DEEPINFRA_API_KEY",
        prefixes: &[],
    },
    Provider {
        name: "SambaNova",
        base_url: "https://api.sambanova.ai/v1",
        env_var: "SAMBANOVA_API_KEY",
        prefixes: &[],
    },
];

/// Provider names that are also Ollama model families, where `name:tag`
/// is ambiguous: `mistral:7b` is Ollama's Mistral 7B, not a selector for
/// Mistral's cloud API. Only these pay the body test in
/// `provider_selector` — `groq:`/`openrouter:`/… can't collide, so they
/// stay plain selectors and keep accepting off-catalog model names.
/// Spell entries exactly as the [`PROVIDERS`] `name` field, and only add
/// a provider whose `prefixes` is non-empty — with no prefixes the body
/// test can never pass and every `name:` form would route local.
const OLLAMA_FAMILY_NAMES: &[&str] = &["Mistral"];

/// Split an explicit `provider:model` selector into its provider and the
/// bare model name to put on the wire. `None` when the name isn't a
/// selector — Ollama's `family:tag` form shares the syntax. For a head
/// that doubles as an Ollama family the body must name something that
/// provider actually serves (one of its [`Provider::prefixes`], or a
/// `vendor/model` path); Ollama tags (`7b`, `latest`,
/// `7b-instruct-v0.3-q4_K_M`) never do. The bias is deliberate: a cloud
/// name misread as local 404s at Ollama, a local name misread as cloud
/// ships the conversation and the API key to a third party.
fn provider_selector(model: &str) -> Option<(&'static Provider, &str)> {
    let (head, body) = model.split_once(':')?;
    let p = PROVIDERS
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(head))?;
    let names_a_cloud_model =
        body.contains('/') || p.prefixes.iter().any(|pre| body.starts_with(pre));
    // `p.name` is the canonical spelling — `head` matched it
    // case-insensitively — so an exact compare is enough.
    if OLLAMA_FAMILY_NAMES.contains(&p.name) && !names_a_cloud_model {
        return None;
    }
    Some((p, body))
}

/// Resolve a model name to its provider. An explicit `provider:model`
/// selector wins (case-insensitive on the provider name); any other
/// colon is an Ollama `family:tag` and routes local; only an untagged
/// name falls through to prefix matching against the [`PROVIDERS`]
/// table. Returns `None` for names that should route to Ollama.
pub fn provider_for_model(model: &str) -> Option<&'static Provider> {
    if let Some((p, _)) = provider_selector(model) {
        return Some(p);
    }
    // Prefix matching is for untagged names only. `gpt-oss:20b`,
    // `deepseek-r1:8b` and `command-r:latest` come out of Ollama's
    // /api/tags with a head that happens to share a cloud prefix —
    // matching it would send a local model, and the conversation, to
    // OpenAI/DeepSeek/Cohere.
    if model.contains(':') {
        return None;
    }
    PROVIDERS
        .iter()
        .find(|p| p.prefixes.iter().any(|pre| model.starts_with(pre)))
}

/// Strip a leading `provider:` selector when present and recognized —
/// providers don't accept their own name in the `"model"` request field.
/// `groq:llama-3.3-70b-versatile` → `llama-3.3-70b-versatile`;
/// `hf.co/FoolDev/Thanatos-27B-HERETIC:Q4_K_M` and `mistral:7b` →
/// unchanged, because an Ollama tag is part of the model's name, not a
/// selector. Shares `provider_selector` with [`provider_for_model`] so
/// the endpoint and the wire name can never disagree.
pub fn resolve_model_name(model: &str) -> &str {
    provider_selector(model).map_or(model, |(_, body)| body)
}
/// Detect the base URL and API-key env-var value for a model name. Used
/// as a fallback when the caller doesn't supply `--base-url` /
/// `--api-key` explicitly. See [`PROVIDERS`] for the full routing table;
/// unrecognized names fall back to local Ollama with no key.
pub fn detect_endpoint(model: &str) -> (String, Option<String>) {
    match provider_for_model(model) {
        Some(p) => (p.base_url.to_string(), std::env::var(p.env_var).ok()),
        None => (DEFAULT_BASE_URL.to_string(), None),
    }
}

/// True when the URL points at the default localhost Ollama port —
/// the only case where the model-pull pre-flight makes sense.
pub fn looks_like_ollama(base_url: &str) -> bool {
    base_url.contains("11434") || base_url.contains("ollama")
}

/// Pull the `num_ctx` value out of Ollama `/api/show`'s `parameters` blob (a
/// newline-separated list of the model's baked `PARAMETER` lines, e.g.
/// `num_ctx                        32768`). Parsed as `f64` first because
/// Ollama prints large values in scientific notation — a `num_ctx` of 1000000
/// comes back as `1e+06`, which a direct `u64` parse rejects (leaving the
/// budget mis-sized and the turn free to overflow). `None` when the model has
/// no `num_ctx` set (it then runs at Ollama's own default).
fn parse_num_ctx_param(parameters: &str) -> Option<u64> {
    parameters.lines().find_map(|line| {
        let mut it = line.split_whitespace();
        (it.next() == Some("num_ctx")).then(|| it.next()?.parse::<f64>().ok().map(|n| n as u64))?
    })
}

/// Effective `num_ctx` for an Ollama model from its `/api/show` body: the
/// baked `num_ctx` capped at the model's trained context window (Ollama clamps
/// a larger baked value down at load, so a `1e+06` default really runs at the
/// 262144 native window). `None` when the body has no `num_ctx` parameter (the
/// model then runs at Ollama's own default). Factored out of
/// [`LlmClient::detect_ollama_num_ctx`] so the clamp — the load-bearing half of
/// the budget calc — is unit-testable without an HTTP mock.
fn effective_num_ctx(body: &Value) -> Option<u64> {
    let num_ctx = parse_num_ctx_param(body.get("parameters")?.as_str()?)?;
    let trained = body
        .get("model_info")
        .and_then(Value::as_object)
        .and_then(|mi| mi.iter().find(|(k, _)| k.ends_with(".context_length")))
        .and_then(|(_, v)| v.as_u64());
    Some(trained.map_or(num_ctx, |t| num_ctx.min(t)))
}

/// True when an error message means "the request no longer fits the
/// model's context window" — i.e. the *input* overflowed, as opposed to
/// a truncated response (`finish_reason: "length"`, handled separately).
/// Providers phrase this differently; the substrings cover the backends
/// in [`PROVIDERS`]: OpenAI-compatible (`context_length_exceeded`,
/// "maximum context length") and native Anthropic ("prompt is too long",
/// `request_too_large`, "exceed context limit").
pub fn is_context_overflow(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("context_length_exceeded")
        || t.contains("maximum context length")
        || t.contains("prompt is too long")
        || t.contains("request_too_large")
        || t.contains("exceed context limit")
}

/// True when `text` looks like the local model backend's runner process
/// *crashed* mid-request — an Ollama llama-server / llama.cpp abort (a ggml
/// assertion or "process has terminated"), as opposed to a normal 4xx/5xx.
/// It's a backend/GPU/version-skew failure, not a teleia bug; the TUI rewraps
/// it into actionable guidance instead of dumping the raw runner backtrace.
pub fn is_backend_crash(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("process has terminated")
        || t.contains("ggml_assert")
        || t.contains("llama runner process")
}

/// Cap on the un-drained streaming buffer. A single SSE event / NDJSON
/// line larger than this means the backend is streaming without the
/// expected delimiter — bail rather than grow a `String` toward OOM.
const MAX_STREAM_BUF: usize = 16 * 1024 * 1024;

/// Append `chunk` to `buf` as UTF-8, carrying any trailing bytes that
/// form an *incomplete* multibyte sequence in `carry` until the next
/// chunk arrives. `bytes_stream()` splits on transport boundaries that
/// don't respect char boundaries; decoding each chunk with
/// `from_utf8_lossy` in isolation would corrupt a character straddling a
/// boundary into U+FFFD. This decodes only complete scalars and holds
/// the rest back. Genuinely invalid bytes still become U+FFFD, matching
/// lossy semantics.
fn push_utf8_chunk(buf: &mut String, carry: &mut Vec<u8>, chunk: &[u8]) {
    carry.extend_from_slice(chunk);
    loop {
        match std::str::from_utf8(carry) {
            Ok(s) => {
                buf.push_str(s);
                carry.clear();
                return;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                buf.push_str(std::str::from_utf8(&carry[..valid]).expect("valid_up_to prefix"));
                match e.error_len() {
                    // Real invalid bytes: emit a replacement and skip them.
                    Some(bad) => {
                        buf.push('\u{FFFD}');
                        carry.drain(..valid + bad);
                    }
                    // Incomplete tail: keep it for the next chunk.
                    None => {
                        carry.drain(..valid);
                        return;
                    }
                }
            }
        }
    }
}

impl LlmClient {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self::with_api_key(base_url, model, None)
    }

    pub fn with_api_key(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key,
            reasoning_effort: None,
            // connect_timeout bounds the TCP/TLS handshake so a dead
            // endpoint can't stall a request forever. Deliberately no
            // overall `.timeout()` — chat/pull streams are long-lived and
            // a whole-request cap would kill legitimate long generations.
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client init (TLS backend / resolver)"),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Best-effort: ask Ollama's native `/api/show` for the window the model
    /// actually loads with, so the caller can size the context budget to it
    /// (teleia can't otherwise see it — the OpenAI-compat `/v1` path ignores a
    /// per-request `num_ctx`). Returns the baked `num_ctx` **capped at the
    /// model's trained `context_length`**, because Ollama clamps a larger
    /// `num_ctx` down to the trained window at load — so a capability-first
    /// `1e+06` default is reported here as the 262144 it really runs at, not a
    /// budget that would never trigger compaction. `None` for non-Ollama
    /// backends, or on any network / parse failure — callers fall back to a
    /// default. The native API sits at the host root, not `/v1`.
    pub async fn detect_ollama_num_ctx(&self) -> Option<u64> {
        if !looks_like_ollama(&self.base_url) {
            return None;
        }
        let root = self.base_url.trim_end_matches('/');
        let root = root.strip_suffix("/v1").unwrap_or(root);
        let resp = self
            .http
            .post(format!("{root}/api/show"))
            .json(&serde_json::json!({ "model": self.model }))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: Value = resp.json().await.ok()?;
        effective_num_ctx(&body)
    }

    /// Re-point at a different model. If the new name resolves to a
    /// different provider (e.g. switching from `claude-opus-4-7` to
    /// `gpt-5`), the base URL and API key are re-read from the
    /// [`PROVIDERS`] table — without this, the request would still hit
    /// the old provider's endpoint with the wrong model name. Within
    /// the same provider the existing `api_key` is preserved so any
    /// manual `--api-key` override survives a `/model` swap. Inline
    /// `provider:` prefixes are stripped before storage so the API call
    /// uses the bare model name.
    pub fn set_model(&mut self, model: String) {
        let (new_base, new_key) = detect_endpoint(&model);
        if new_base != self.base_url {
            self.base_url = new_base;
            self.api_key = new_key;
        }
        self.model = resolve_model_name(&model).to_string();
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    pub fn set_api_key(&mut self, key: Option<String>) {
        self.api_key = key.filter(|k| !k.is_empty());
    }

    pub fn reasoning_effort(&self) -> Option<&str> {
        self.reasoning_effort.as_deref()
    }

    /// Set the `reasoning_effort` sent on chat requests. `None` (or an
    /// empty string) clears it, restoring the default of omitting the
    /// field entirely.
    pub fn set_reasoning_effort(&mut self, effort: Option<String>) {
        self.reasoning_effort = effort.filter(|e| !e.is_empty());
    }

    /// List models Ollama has cached locally. Returns the `name` field of
    /// each entry in `/api/tags` (e.g. `"llama3:latest"`,
    /// `"hf.co/FoolDev/Thanatos-27B-HERETIC:Q4_K_M"`). Best-effort — returns an
    /// empty Vec if the endpoint isn't reachable or doesn't look like
    /// Ollama.
    pub async fn list_models(&self) -> Vec<String> {
        let base = self.base_url.trim_end_matches('/');
        let native = base.strip_suffix("/v1").unwrap_or(base);
        let url = format!("{native}/api/tags");

        #[derive(serde::Deserialize)]
        struct ModelEntry {
            name: String,
        }
        #[derive(serde::Deserialize)]
        struct Tags {
            #[serde(default)]
            models: Vec<ModelEntry>,
        }

        let resp = match self.http.get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            _ => return Vec::new(),
        };
        match resp.json::<Tags>().await {
            Ok(t) => t.models.into_iter().map(|m| m.name).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Best-effort: does Ollama have this model locally?
    /// `Some(true)` / `Some(false)` for definite answers (via /api/show),
    /// `None` if the endpoint isn't reachable or doesn't respond as expected
    /// (e.g. the base URL points at a non-Ollama server).
    pub async fn has_model(&self) -> Option<bool> {
        let base = self.base_url.trim_end_matches('/');
        let native = base.strip_suffix("/v1").unwrap_or(base);
        let url = format!("{native}/api/show");
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "name": self.model }))
            .send()
            .await
            .ok()?;
        Some(resp.status().is_success())
    }

    /// Stream progress updates from Ollama's `/api/pull`. Each yielded
    /// `PullProgress` reflects one NDJSON line from the server: a status
    /// message, optional digest of the current layer, and byte counters
    /// when known. The stream completes when Ollama emits `status:
    /// "success"` (or the connection closes).
    pub fn pull_model(&self) -> impl Stream<Item = Result<PullProgress>> + '_ {
        try_stream! {
            let base = self.base_url.trim_end_matches('/');
            let native = base.strip_suffix("/v1").unwrap_or(base);
            let url = format!("{native}/api/pull");
            let body = serde_json::json!({ "model": self.model, "stream": true });

            let resp = self.http.post(&url).json(&body).send().await
                .with_context(|| format!("POST {url}"))?;

            if !resp.status().is_success() {
                let s = resp.status();
                let body = resp.text().await.unwrap_or_default();
                Err(anyhow!("ollama pull returned {s}: {body}"))?;
                return;
            }

            let mut bytes = resp.bytes_stream();
            let mut buf = String::new();
            let mut carry: Vec<u8> = Vec::new();
            while let Some(chunk) = bytes.next().await {
                let chunk = chunk.context("read pull chunk")?;
                push_utf8_chunk(&mut buf, &mut carry, &chunk);

                while let Some(pos) = buf.find('\n') {
                    let line: String = buf.drain(..=pos).collect();
                    let line = line.trim();
                    if line.is_empty() { continue; }
                    if let Ok(prog) = serde_json::from_str::<PullProgress>(line) {
                        yield prog;
                    }
                }
                if buf.len() > MAX_STREAM_BUF {
                    Err(anyhow!("ollama pull stream line exceeded {MAX_STREAM_BUF} bytes"))?;
                }
            }
        }
    }

    /// True when requests should use Anthropic's native Messages API
    /// (`/v1/messages`, `x-api-key` auth, content-block messages, native
    /// tool_use/tool_result, prompt caching) rather than the OpenAI-compatible
    /// `/chat/completions` path. Keyed on the host so a `claude-*` model
    /// routed at a proxy (which speaks OpenAI-compat) still takes the OpenAI
    /// path; only Anthropic's own endpoint gets the native protocol.
    pub fn uses_anthropic_api(&self) -> bool {
        self.base_url.contains("api.anthropic.com")
    }

    /// Stream chat completions as `ChatEvent`s. The final event is always
    /// `Done`. Dispatches to the native Anthropic Messages API for Anthropic
    /// endpoints and the OpenAI-compatible `/chat/completions` path for
    /// everything else; both surface the same `ChatEvent`s so the agent loop
    /// is protocol-agnostic.
    pub fn stream<'a>(
        &'a self,
        messages: &'a [Message],
        tools: Option<&'a [ToolDef]>,
    ) -> impl Stream<Item = Result<ChatEvent>> + 'a {
        if self.uses_anthropic_api() {
            self.stream_anthropic(messages, tools).boxed()
        } else {
            self.stream_openai(messages, tools).boxed()
        }
    }

    fn stream_openai<'a>(
        &'a self,
        messages: &'a [Message],
        tools: Option<&'a [ToolDef]>,
    ) -> impl Stream<Item = Result<ChatEvent>> + 'a {
        try_stream! {
            let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
            let body = ChatRequest {
                model: &self.model,
                messages,
                tools,
                stream: true,
                stream_options: Some(StreamOptions { include_usage: true }),
                reasoning_effort: self.reasoning_effort.as_deref().map(wire_reasoning_effort),
            };

            let mut req = self.http.post(&url).json(&body);
            if let Some(key) = &self.api_key {
                req = req.bearer_auth(key);
            }
            let resp = req.send().await
                .with_context(|| format!("POST {url}"))?;

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                Err(anyhow!("backend returned {status}: {body}"))?;
                return;
            }

            let mut bytes = resp.bytes_stream();
            let mut buf = String::new();
            let mut carry: Vec<u8> = Vec::new();
            let mut accumulated: Vec<AccTool> = Vec::new();
            let mut last_usage: Option<Usage> = None;
            let mut last_finish: Option<String> = None;

            while let Some(chunk) = bytes.next().await {
                let chunk = chunk.context("read stream chunk")?;
                push_utf8_chunk(&mut buf, &mut carry, &chunk);

                while let Some(event) = next_sse_event(&mut buf) {
                    for line in event.lines() {
                        let Some(payload) = line.strip_prefix("data:") else { continue; };
                        let payload = payload.trim();
                        if payload.is_empty() || payload == "[DONE]" { continue; }

                        let parsed: StreamChunk = match serde_json::from_str(payload) {
                            Ok(p) => p,
                            Err(_) => continue,
                        };
                        if let Some(u) = parsed.usage {
                            last_usage = Some(u);
                        }
                        for choice in parsed.choices {
                            if let Some(text) = choice.delta.content {
                                if !text.is_empty() {
                                    yield ChatEvent::ContentDelta(text);
                                }
                            }
                            if let Some(text) = choice.delta.reasoning {
                                if !text.is_empty() {
                                    yield ChatEvent::ReasoningDelta(text);
                                }
                            }
                            for tcd in choice.delta.tool_calls {
                                accumulate(&mut accumulated, tcd);
                            }
                            if choice.finish_reason.is_some() {
                                last_finish = choice.finish_reason;
                            }
                        }
                    }
                }
                // Cap the residual after draining complete events, matching
                // pull_model: a single undelimited event over the cap means
                // the backend is misbehaving — bail before growing toward OOM.
                if buf.len() > MAX_STREAM_BUF {
                    Err(anyhow!("chat stream event exceeded {MAX_STREAM_BUF} bytes"))?;
                }
            }

            let tool_calls = accumulated.into_iter().map(|a| ToolCall {
                id: a.id,
                kind: a.kind,
                function: ToolCallFunction { name: a.name, arguments: a.arguments },
            }).collect();
            yield ChatEvent::Done { tool_calls, usage: last_usage, finish_reason: last_finish };
        }
    }

    /// Stream from Anthropic's native Messages API (`/v1/messages`). Converts
    /// teleia's flat message list to Anthropic's content-block shape, parses
    /// the SSE event stream (text / thinking / tool_use / usage), and emits
    /// the same `ChatEvent`s as the OpenAI path. `stop_reason: "max_tokens"`
    /// maps to `finish_reason: "length"` so the truncation notice still fires.
    fn stream_anthropic<'a>(
        &'a self,
        messages: &'a [Message],
        tools: Option<&'a [ToolDef]>,
    ) -> impl Stream<Item = Result<ChatEvent>> + 'a {
        try_stream! {
            let url = format!("{}/messages", self.base_url.trim_end_matches('/'));
            let body = build_anthropic_body(&self.model, messages, tools);

            let mut req = self
                .http
                .post(&url)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&body);
            if let Some(key) = &self.api_key {
                req = req.header("x-api-key", key.as_str());
            }
            let resp = req.send().await.with_context(|| format!("POST {url}"))?;

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                Err(anyhow!("anthropic returned {status}: {body}"))?;
                return;
            }

            let mut bytes = resp.bytes_stream();
            let mut buf = String::new();
            let mut carry: Vec<u8> = Vec::new();
            let mut tools_acc: Vec<AccTool> = Vec::new();
            let mut cur_tool: Option<usize> = None;
            let mut usage = Usage::default();
            let mut finish: Option<String> = None;

            while let Some(chunk) = bytes.next().await {
                let chunk = chunk.context("read stream chunk")?;
                push_utf8_chunk(&mut buf, &mut carry, &chunk);

                while let Some(event) = next_sse_event(&mut buf) {
                    for line in event.lines() {
                        let Some(payload) = line.strip_prefix("data:") else { continue; };
                        let payload = payload.trim();
                        if payload.is_empty() { continue; }
                        let ev: AnthropicEvent = match serde_json::from_str(payload) {
                            Ok(e) => e,
                            Err(_) => continue,
                        };
                        match ev {
                            AnthropicEvent::MessageStart { message } => {
                                if let Some(u) = message.usage {
                                    usage.prompt_tokens = u
                                        .input_tokens
                                        .saturating_add(u.cache_read_input_tokens)
                                        .saturating_add(u.cache_creation_input_tokens);
                                }
                            }
                            AnthropicEvent::ContentBlockStart { content_block } => match content_block {
                                AnthropicBlockStart::ToolUse { id, name } => {
                                    cur_tool = push_tool_capped(
                                        &mut tools_acc,
                                        AccTool {
                                            id,
                                            kind: "function".into(),
                                            name,
                                            arguments: String::new(),
                                        },
                                    );
                                }
                                AnthropicBlockStart::Other => cur_tool = None,
                            },
                            AnthropicEvent::ContentBlockDelta { delta } => match delta {
                                AnthropicBlockDelta::TextDelta { text } => {
                                    if !text.is_empty() {
                                        yield ChatEvent::ContentDelta(text);
                                    }
                                }
                                AnthropicBlockDelta::ThinkingDelta { thinking } => {
                                    if !thinking.is_empty() {
                                        yield ChatEvent::ReasoningDelta(thinking);
                                    }
                                }
                                AnthropicBlockDelta::InputJsonDelta { partial_json } => {
                                    if let Some(i) = cur_tool {
                                        tools_acc[i].arguments.push_str(&partial_json);
                                    }
                                }
                                AnthropicBlockDelta::Other => {}
                            },
                            AnthropicEvent::MessageDelta { delta, usage: u } => {
                                if let Some(reason) = delta.stop_reason {
                                    // Only "length" is acted on downstream (the
                                    // truncation notice); other reasons pass through
                                    // verbatim and are ignored.
                                    finish = Some(if reason == "max_tokens" {
                                        "length".to_string()
                                    } else {
                                        reason
                                    });
                                }
                                if let Some(u) = u {
                                    usage.completion_tokens = u.output_tokens;
                                }
                            }
                            AnthropicEvent::Error { error } => {
                                Err(anyhow!(
                                    "anthropic stream error: {}: {}",
                                    error.kind,
                                    error.message
                                ))?;
                            }
                            AnthropicEvent::Other => {}
                        }
                    }
                }
                if buf.len() > MAX_STREAM_BUF {
                    Err(anyhow!("chat stream event exceeded {MAX_STREAM_BUF} bytes"))?;
                }
            }

            // A tool_use with no input deltas must still dispatch with valid
            // JSON, not an empty string.
            for t in tools_acc.iter_mut() {
                if t.arguments.trim().is_empty() {
                    t.arguments = "{}".into();
                }
            }
            let tool_calls = tools_acc
                .into_iter()
                .map(|a| ToolCall {
                    id: a.id,
                    kind: a.kind,
                    function: ToolCallFunction { name: a.name, arguments: a.arguments },
                })
                .collect();
            yield ChatEvent::Done { tool_calls, usage: Some(usage), finish_reason: finish };
        }
    }
}

/// Anthropic Messages API version pinned in the `anthropic-version` header.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Output-token ceiling for the native Anthropic path. Anthropic *requires*
/// `max_tokens` (the OpenAI path sends none). 128000 is the max output for
/// Claude Fable 5 / Mythos 5 (and Opus 5 / Sonnet 5 / Opus 4.6-4.8 / Sonnet
/// 4.6) on the sync Messages API, no beta header needed. Models with a lower
/// cap (Sonnet 4.5 / Opus 4.5 / Haiku 4.5 = 64k, Opus 4.1 = 32k) will 400 at
/// this value — clamp per model if you target those. Hitting the ceiling
/// surfaces as a `length` truncation notice, same as the OpenAI path.
const ANTHROPIC_MAX_TOKENS: u32 = 128_000;

/// Output ceilings for the families that sit below [`ANTHROPIC_MAX_TOKENS`].
/// Prefix-matched in order, so the more specific `claude-3-5-` / `claude-3-7-`
/// entries must precede the `claude-3-` catch-all. Sending a `max_tokens`
/// above a model's own cap is a 400 on the very first message, which is why
/// this clamps down and never up: an unlisted model keeps the constant, and
/// too *low* a value only truncates with a `length` notice.
const ANTHROPIC_MAX_TOKENS_BY_PREFIX: &[(&str, u32)] = &[
    ("claude-3-5-", 8_192),
    ("claude-3-7-", 8_192),
    ("claude-3-", 4_096),
    ("claude-opus-4-1", 32_000),
    ("claude-sonnet-4-5", 64_000),
    ("claude-haiku-4-5", 64_000),
    ("claude-opus-4-5", 64_000),
];

/// The `max_tokens` to request for `model`.
fn anthropic_max_tokens(model: &str) -> u32 {
    ANTHROPIC_MAX_TOKENS_BY_PREFIX
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map_or(ANTHROPIC_MAX_TOKENS, |(_, cap)| *cap)
}

/// Build the JSON body for a native Anthropic `/v1/messages` request. `system`
/// is lifted out of the message list into the top-level field (with a cache
/// breakpoint); a second breakpoint on the final message caches the growing
/// history across the agent's tool-call loop.
fn build_anthropic_body(model: &str, messages: &[Message], tools: Option<&[ToolDef]>) -> Value {
    let (system, mut msgs) = to_anthropic_messages(messages);
    if let Some(last) = msgs.last_mut() {
        set_cache_control(last);
    }
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": anthropic_max_tokens(model),
        "messages": msgs,
        "stream": true,
    });
    if let Some(sys) = system {
        body["system"] = sys;
    }
    if let Some(tools) = tools {
        if !tools.is_empty() {
            body["tools"] = Value::Array(to_anthropic_tools(tools));
        }
    }
    body
}

/// Convert teleia's flat message list into Anthropic's `(system, messages)`
/// shape. `System` turns are concatenated into the returned system value;
/// consecutive same-role turns are merged into one message with multiple
/// content blocks — Anthropic requires strict user/assistant alternation, so
/// parallel `Tool` results (all user-role `tool_result` blocks) must share one
/// user turn.
fn to_anthropic_messages(messages: &[Message]) -> (Option<Value>, Vec<Value>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut out: Vec<Value> = Vec::new();

    for m in messages {
        match m {
            Message::System { content } => system_parts.push(content.clone()),
            Message::User { content } => {
                push_block(
                    &mut out,
                    "user",
                    serde_json::json!({ "type": "text", "text": content }),
                );
            }
            Message::Assistant {
                content,
                tool_calls,
            } => {
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(text) = content {
                    if !text.is_empty() {
                        blocks.push(serde_json::json!({ "type": "text", "text": text }));
                    }
                }
                for tc in tool_calls {
                    blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.function.name,
                        "input": parse_tool_input(&tc.function.arguments),
                    }));
                }
                // An assistant turn must be non-empty.
                if blocks.is_empty() {
                    blocks.push(serde_json::json!({ "type": "text", "text": "" }));
                }
                for b in blocks {
                    push_block(&mut out, "assistant", b);
                }
            }
            Message::Tool {
                tool_call_id,
                content,
            } => {
                push_block(
                    &mut out,
                    "user",
                    serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": content,
                    }),
                );
            }
        }
    }

    let system = (!system_parts.is_empty()).then(|| {
        serde_json::json!([{
            "type": "text",
            "text": system_parts.join("\n\n"),
            "cache_control": { "type": "ephemeral" },
        }])
    });
    (system, out)
}

/// Append a content block to `out`, merging into the trailing message when it
/// already has `role` (keeping user/assistant strictly alternating), else
/// starting a new message.
fn push_block(out: &mut Vec<Value>, role: &str, block: Value) {
    if let Some(last) = out.last_mut() {
        if last["role"] == role {
            last["content"]
                .as_array_mut()
                .expect("message content is an array")
                .push(block);
            return;
        }
    }
    out.push(serde_json::json!({ "role": role, "content": [block] }));
}

/// Anthropic's `tool_use.input` is a JSON object; teleia stores tool-call
/// arguments as a raw JSON string. Parse it back, defaulting to `{}` for an
/// empty or malformed value so the request stays well-formed.
fn parse_tool_input(arguments: &str) -> Value {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(trimmed).unwrap_or_else(|_| serde_json::json!({}))
}

/// Convert teleia's OpenAI-style `ToolDef`s to Anthropic tool specs
/// (`name` / `description` / `input_schema`). A cache breakpoint on the last
/// tool caches the whole (stable) tool catalogue.
fn to_anthropic_tools(tools: &[ToolDef]) -> Vec<Value> {
    let mut out: Vec<Value> = tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.function.name,
                "description": t.function.description,
                "input_schema": t.function.parameters,
            })
        })
        .collect();
    if let Some(last) = out.last_mut() {
        last["cache_control"] = serde_json::json!({ "type": "ephemeral" });
    }
    out
}

/// Mark a message as a cache breakpoint by tagging its last content block —
/// everything up to and including it is served from Anthropic's prompt cache
/// on the next request.
fn set_cache_control(msg: &mut Value) {
    if let Some(last) = msg["content"].as_array_mut().and_then(|b| b.last_mut()) {
        last["cache_control"] = serde_json::json!({ "type": "ephemeral" });
    }
}

/// A single Server-Sent Event from Anthropic's streaming Messages API. Only
/// the fields teleia consumes are modelled; `#[serde(other)]` swallows the
/// event types it ignores (`ping`, `message_stop`, `content_block_stop`).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicEvent {
    MessageStart {
        message: AnthropicMessageStart,
    },
    ContentBlockStart {
        content_block: AnthropicBlockStart,
    },
    ContentBlockDelta {
        delta: AnthropicBlockDelta,
    },
    MessageDelta {
        delta: AnthropicMessageDelta,
        #[serde(default)]
        usage: Option<AnthropicUsage>,
    },
    Error {
        error: AnthropicApiError,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct AnthropicMessageStart {
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

/// The `content_block` of a `content_block_start`. Only `tool_use` starts are
/// load-bearing (they open a tool call whose input streams as
/// `input_json_delta`); text / thinking / redacted_thinking collapse to
/// `Other`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicBlockStart {
    ToolUse {
        id: String,
        name: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicBlockDelta {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct AnthropicMessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct AnthropicApiError {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

struct AccTool {
    id: String,
    kind: String,
    name: String,
    arguments: String,
}

/// Pop the next complete SSE event (through its blank-line terminator) off
/// the front of `buf`, or `None` if no terminator is buffered yet. Frames on
/// whichever separator appears first — `\n\n` (LF streams) or `\r\n\r\n`
/// (CRLF streams) — so a CRLF backend's events don't sit unframed until the
/// stream ends (which stalls output and the MAX_STREAM_BUF guard eventually
/// aborts). `event.lines()` already strips the trailing `\r`.
fn next_sse_event(buf: &mut String) -> Option<String> {
    let (pos, len) = match (buf.find("\n\n"), buf.find("\r\n\r\n")) {
        (Some(a), Some(b)) => {
            if a <= b {
                (a, 2)
            } else {
                (b, 4)
            }
        }
        (Some(a), None) => (a, 2),
        (None, Some(b)) => (b, 4),
        (None, None) => return None,
    };
    Some(buf.drain(..pos + len).collect())
}

/// Cap on accumulated tool calls in one assistant turn. A stream claiming
/// more is malformed/hostile — dropping the excess bounds the accumulator
/// (OOM guard). Shared by the OpenAI (`accumulate`) and Anthropic
/// (`push_tool_capped`) streaming paths.
const MAX_TOOL_CALLS: usize = 256;

/// Push a streamed tool-call block, bounded by [`MAX_TOOL_CALLS`]. Returns
/// its index, or `None` at the cap so the caller drops the block (and its
/// later argument deltas) rather than misattributing them to another tool.
fn push_tool_capped(acc: &mut Vec<AccTool>, tool: AccTool) -> Option<usize> {
    if acc.len() >= MAX_TOOL_CALLS {
        return None;
    }
    acc.push(tool);
    Some(acc.len() - 1)
}

fn accumulate(acc: &mut Vec<AccTool>, delta: ToolCallDelta) {
    // A single assistant turn never emits hundreds of parallel tool calls;
    // an index this large is a malformed/hostile stream trying to make the
    // pad loop below allocate unboundedly (OOM). Drop it.
    if delta.index >= MAX_TOOL_CALLS {
        return;
    }
    while acc.len() <= delta.index {
        acc.push(AccTool {
            id: String::new(),
            kind: "function".into(),
            name: String::new(),
            arguments: String::new(),
        });
    }
    let slot = &mut acc[delta.index];
    if let Some(id) = delta.id {
        if !id.is_empty() {
            slot.id = id;
        }
    }
    if let Some(kind) = delta.kind {
        slot.kind = kind;
    }
    if let Some(func) = delta.function {
        if let Some(name) = func.name {
            if !name.is_empty() {
                slot.name = name;
            }
        }
        if let Some(args) = func.arguments {
            slot.arguments.push_str(&args);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn effective_num_ctx_clamps_to_trained_window() {
        // Baked 1e+06 num_ctx, trained window 262144 → clamped to the window.
        let body = json!({"parameters": "num_ctx 1e+06\nstop \"<|im_end|>\"", "model_info": {"qwen3moe.context_length": 262144}});
        assert_eq!(effective_num_ctx(&body), Some(262144));
        // num_ctx below the trained window → passes through unclamped.
        let body = json!({"parameters": "num_ctx 8192", "model_info": {"qwen3moe.context_length": 262144}});
        assert_eq!(effective_num_ctx(&body), Some(8192));
        // No model_info → num_ctx passes through (nothing to clamp against).
        let body = json!({"parameters": "num_ctx 4096"});
        assert_eq!(effective_num_ctx(&body), Some(4096));
        // No num_ctx parameter → None (model runs at Ollama's own default).
        let body = json!({"parameters": "temperature 0.6", "model_info": {"qwen3moe.context_length": 262144}});
        assert_eq!(effective_num_ctx(&body), None);
    }

    #[test]
    fn is_backend_crash_detects_runner_aborts() {
        assert!(is_backend_crash(
            "backend returned 500: llama-server process has terminated: GGML_ASSERT(tensor->op == GGML_OP_UNARY) failed"
        ));
        assert!(is_backend_crash("llama runner process has terminated"));
        // Normal errors are not runner crashes.
        assert!(!is_backend_crash("context_length_exceeded"));
        assert!(!is_backend_crash("connection refused"));
    }

    #[test]
    fn parse_num_ctx_param_reads_the_ollama_show_blob() {
        // /api/show `parameters` is space-padded PARAMETER lines.
        let blob = "num_ctx                        4096\nstop                           \"<|im_end|>\"\ntemperature                    0.6";
        assert_eq!(parse_num_ctx_param(blob), Some(4096));
        // Ollama prints large num_ctx in scientific notation (1000000 -> 1e+06);
        // a plain u64 parse rejects it, so it must go through f64.
        assert_eq!(
            parse_num_ctx_param("num_ctx                        1e+06"),
            Some(1_000_000)
        );
        // No num_ctx line → None (model runs at Ollama's own default).
        assert_eq!(
            parse_num_ctx_param("stop \"<|im_end|>\"\ntemperature 0.6"),
            None
        );
        // Garbage value → None, not a panic.
        assert_eq!(parse_num_ctx_param("num_ctx notanumber"), None);
    }

    #[test]
    fn chunk_delta_parses_reasoning_and_its_alias() {
        // Ollama streams the thinking channel as `reasoning` with `content`
        // empty; DeepSeek-style backends use `reasoning_content`. Both must
        // land in the reasoning field so the turn isn't rendered blank.
        let d: ChunkDelta = serde_json::from_str(r#"{"content":"","reasoning":"think"}"#).unwrap();
        assert_eq!(d.content.as_deref(), Some(""));
        assert_eq!(d.reasoning.as_deref(), Some("think"));
        let aliased: ChunkDelta = serde_json::from_str(r#"{"reasoning_content":"deep"}"#).unwrap();
        assert_eq!(aliased.reasoning.as_deref(), Some("deep"));
    }

    #[test]
    fn stream_choice_parses_finish_reason() {
        let c: StreamChoice =
            serde_json::from_str(r#"{"delta":{"content":""},"finish_reason":"length"}"#).unwrap();
        assert_eq!(c.finish_reason.as_deref(), Some("length"));
        // Absent on non-final chunks.
        let mid: StreamChoice = serde_json::from_str(r#"{"delta":{"content":"x"}}"#).unwrap();
        assert_eq!(mid.finish_reason, None);
    }

    #[test]
    fn context_overflow_matches_provider_phrasings() {
        // OpenAI-compatible: error code and human message.
        assert!(is_context_overflow(
            r#"backend returned 400 Bad Request: {"error":{"message":"This model's maximum context length is 128000 tokens...","code":"context_length_exceeded"}}"#
        ));
        // Anthropic: 400 message and 413 code.
        assert!(is_context_overflow(
            r#"anthropic returned 400 Bad Request: {"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 213462 tokens > 200000 maximum"}}"#
        ));
        assert!(is_context_overflow(
            r#"anthropic returned 413: {"type":"error","error":{"type":"request_too_large","message":"Request exceeds the maximum allowed size"}}"#
        ));
        assert!(is_context_overflow(
            "input length and `max_tokens` exceed context limit: 199999 + 4096 > 200000"
        ));
        // Not overflow: ordinary errors must pass through untouched.
        assert!(!is_context_overflow("backend returned 429: rate limited"));
        assert!(!is_context_overflow(
            r#"anthropic returned 400: {"error":{"message":"messages: roles must alternate"}}"#
        ));
    }

    fn delta(
        index: usize,
        id: Option<&str>,
        name: Option<&str>,
        args: Option<&str>,
    ) -> ToolCallDelta {
        ToolCallDelta {
            index,
            id: id.map(str::to_string),
            kind: None,
            function: Some(ToolCallFunctionDelta {
                name: name.map(str::to_string),
                arguments: args.map(str::to_string),
            }),
        }
    }

    #[test]
    fn accumulate_concatenates_argument_chunks_in_order() {
        let mut acc = Vec::new();
        accumulate(
            &mut acc,
            delta(0, Some("call_1"), Some("read"), Some(r#"{"path":"#)),
        );
        accumulate(&mut acc, delta(0, None, None, Some(r#""foo.txt"}"#)));
        assert_eq!(acc.len(), 1);
        assert_eq!(acc[0].id, "call_1");
        assert_eq!(acc[0].name, "read");
        assert_eq!(acc[0].arguments, r#"{"path":"foo.txt"}"#);
    }

    #[test]
    fn accumulate_handles_multiple_tool_calls_by_index() {
        let mut acc = Vec::new();
        accumulate(&mut acc, delta(0, Some("a"), Some("read"), Some("{}")));
        accumulate(&mut acc, delta(1, Some("b"), Some("write"), Some("{}")));
        assert_eq!(acc.len(), 2);
        assert_eq!(acc[0].name, "read");
        assert_eq!(acc[1].name, "write");
    }

    #[test]
    fn accumulate_ignores_empty_id_and_name_overwrites() {
        let mut acc = Vec::new();
        accumulate(&mut acc, delta(0, Some("real_id"), Some("read"), None));
        // Subsequent empty strings shouldn't clobber prior values.
        accumulate(&mut acc, delta(0, Some(""), Some(""), Some("{}")));
        assert_eq!(acc[0].id, "real_id");
        assert_eq!(acc[0].name, "read");
        assert_eq!(acc[0].arguments, "{}");
    }

    #[test]
    fn accumulate_pads_intermediate_indices() {
        // Some providers stream tool_calls out of order or skip indices.
        let mut acc = Vec::new();
        accumulate(&mut acc, delta(2, Some("c"), Some("bash"), Some("{}")));
        assert_eq!(acc.len(), 3);
        assert_eq!(acc[2].name, "bash");
        assert_eq!(acc[0].id, "");
        assert_eq!(acc[1].id, "");
    }

    #[test]
    fn next_sse_event_frames_lf_and_crlf() {
        // LF-delimited stream.
        let mut buf = String::from("data: a\n\ndata: b\n\n");
        assert_eq!(next_sse_event(&mut buf).as_deref(), Some("data: a\n\n"));
        assert_eq!(next_sse_event(&mut buf).as_deref(), Some("data: b\n\n"));
        assert_eq!(next_sse_event(&mut buf), None);
        // CRLF-delimited stream must frame too, not stall until EOF.
        let mut buf = String::from("data: a\r\n\r\ndata: b\r\n\r\n");
        assert_eq!(next_sse_event(&mut buf).as_deref(), Some("data: a\r\n\r\n"));
        assert_eq!(next_sse_event(&mut buf).as_deref(), Some("data: b\r\n\r\n"));
        assert_eq!(next_sse_event(&mut buf), None);
        // A partial event without its terminator stays buffered.
        let mut buf = String::from("data: partial");
        assert_eq!(next_sse_event(&mut buf), None);
        assert_eq!(buf, "data: partial");
    }

    #[test]
    fn accumulate_drops_pathologically_large_index() {
        // A hostile/garbage stream with a huge index must not make the pad
        // loop allocate unboundedly — the delta is dropped, acc stays empty.
        let mut acc = Vec::new();
        accumulate(
            &mut acc,
            delta(usize::MAX, Some("x"), Some("read"), Some("{}")),
        );
        assert!(acc.is_empty());
        // A normal index still works right after.
        accumulate(&mut acc, delta(0, Some("a"), Some("read"), Some("{}")));
        assert_eq!(acc.len(), 1);
    }

    #[test]
    fn push_tool_capped_indexes_then_stops_at_the_cap() {
        let tool = || AccTool {
            id: String::new(),
            kind: "function".into(),
            name: String::new(),
            arguments: String::new(),
        };
        let mut acc: Vec<AccTool> = Vec::new();
        // Fills sequentially, handing back each new index.
        for i in 0..MAX_TOOL_CALLS {
            assert_eq!(push_tool_capped(&mut acc, tool()), Some(i));
        }
        assert_eq!(acc.len(), MAX_TOOL_CALLS);
        // At the cap: returns None and does not grow the Vec — so the
        // Anthropic caller's cur_tool becomes None and later arg deltas are
        // dropped instead of misattributed.
        assert_eq!(push_tool_capped(&mut acc, tool()), None);
        assert_eq!(acc.len(), MAX_TOOL_CALLS, "Vec did not grow past the cap");
    }

    #[test]
    fn detect_endpoint_routes_claude_to_anthropic() {
        let (url, _key) = detect_endpoint("claude-opus-4-7");
        assert_eq!(url, "https://api.anthropic.com/v1");
    }

    #[test]
    fn detect_endpoint_routes_gpt_to_openai() {
        let (url, _) = detect_endpoint("gpt-5");
        assert_eq!(url, "https://api.openai.com/v1");
        let (url, _) = detect_endpoint("o3-mini");
        assert_eq!(url, "https://api.openai.com/v1");
    }

    #[test]
    fn detect_endpoint_falls_back_to_ollama() {
        let (url, key) = detect_endpoint("hf.co/FoolDev/Thanatos-27B-HERETIC:Q4_K_M");
        assert_eq!(url, DEFAULT_BASE_URL);
        assert!(key.is_none());
        let (url, _) = detect_endpoint("llama3:latest");
        assert_eq!(url, DEFAULT_BASE_URL);
    }

    #[test]
    fn colon_tagged_ollama_names_never_route_to_cloud() {
        // Every one of these comes back verbatim from `/api/tags`, and each
        // head shares a prefix with some PROVIDERS entry. Matching that
        // prefix against the whole tagged name shipped the local model —
        // and the conversation — to a cloud endpoint.
        for model in [
            "gpt-oss:20b",
            "gpt-oss:120b",
            "deepseek-r1:8b",
            "command-r:latest",
            "mistral:7b",
            "mistral:latest",
            "mistral:7b-instruct-v0.3-q4_K_M",
            "mistral-nemo:12b",
            "llama3:latest",
            "hf.co/FoolDev/Thanatos-27B-HERETIC:Q4_K_M",
        ] {
            assert!(
                provider_for_model(model).is_none(),
                "{model} routed to {:?}, expected local Ollama",
                provider_for_model(model).map(|p| p.name)
            );
            // The tag is part of the name Ollama knows — never stripped.
            assert_eq!(resolve_model_name(model), model);
        }
    }

    #[test]
    fn provider_name_that_is_also_an_ollama_family_needs_a_catalog_body() {
        // `Mistral` is both a PROVIDERS entry and an Ollama family, so the
        // body decides. A name from Mistral's own catalog is a real
        // selector: route to the cloud and strip the head.
        let p = provider_for_model("mistral:mistral-large-latest").expect("cloud Mistral");
        assert_eq!(p.name, "Mistral");
        assert_eq!(
            resolve_model_name("mistral:mistral-large-latest"),
            "mistral-large-latest"
        );
        // Case-insensitive on the head, like every other selector.
        let (url, _) = detect_endpoint("MISTRAL:codestral-latest");
        assert!(url.contains("mistral.ai"));
        // An Ollama tag is not a selector: stay local and keep the name
        // whole, or the request body says `"model":"7b"`.
        let (url, _) = detect_endpoint("mistral:7b");
        assert_eq!(url, DEFAULT_BASE_URL);
        assert_eq!(resolve_model_name("mistral:7b"), "mistral:7b");
        // A provider name that can't be an Ollama family keeps the plain
        // selector — no body test, so off-catalog names still work.
        assert_eq!(
            provider_for_model("groq:llama-3.3-70b-versatile").map(|p| p.name),
            Some("Groq")
        );
    }

    #[test]
    fn every_mistral_catalog_family_is_a_selector() {
        // The body test in `provider_selector` only lets through names that
        // match one of Mistral's `prefixes`, so a family missing from that
        // list routes a paying customer to localhost. Every family the
        // dropdown offers must survive the round trip.
        for model in [
            "mistral-large-latest",
            "ministral-8b-latest",
            "ministral-3b-2410",
            "codestral-latest",
            "pixtral-large-latest",
            "open-mistral-nemo",
            "open-mixtral-8x22b",
            "magistral-medium-latest",
        ] {
            let tagged = format!("mistral:{model}");
            let (url, _) = detect_endpoint(&tagged);
            assert!(url.contains("mistral.ai"), "{tagged} went to {url}");
            assert_eq!(resolve_model_name(&tagged), model, "{tagged}");
        }
        // …while the Ollama tags stay local, which is what the body test
        // exists for in the first place.
        for tag in ["mistral:7b", "mistral:latest", "mistral:7b-instruct-v0.3"] {
            let (url, _) = detect_endpoint(tag);
            assert_eq!(url, DEFAULT_BASE_URL, "{tag}");
        }
    }

    #[test]
    fn anthropic_max_tokens_clamps_the_lower_families() {
        // Above a model's own ceiling the API 400s on the first message.
        assert_eq!(anthropic_max_tokens("claude-sonnet-4-5"), 64_000);
        assert_eq!(anthropic_max_tokens("claude-haiku-4-5-20251001"), 64_000);
        assert_eq!(anthropic_max_tokens("claude-opus-4-1"), 32_000);
        assert_eq!(anthropic_max_tokens("claude-3-5-sonnet-20241022"), 8_192);
        assert_eq!(anthropic_max_tokens("claude-3-7-sonnet-latest"), 8_192);
        assert_eq!(anthropic_max_tokens("claude-3-opus-20240229"), 4_096);
        // The specific prefixes must win over the `claude-3-` catch-all.
        assert_ne!(anthropic_max_tokens("claude-3-5-haiku-latest"), 4_096);
        // Anything unlisted keeps the constant: clamping down is safe,
        // clamping a new model down by guess is not.
        assert_eq!(anthropic_max_tokens("claude-fable-5"), ANTHROPIC_MAX_TOKENS);
        assert_eq!(
            anthropic_max_tokens("claude-opus-4-7"),
            ANTHROPIC_MAX_TOKENS
        );
        // And the body actually carries it.
        let body = build_anthropic_body("claude-opus-4-1", &[], None);
        assert_eq!(body["max_tokens"], 32_000);
    }

    #[test]
    fn looks_like_ollama_matches_default_and_explicit() {
        assert!(looks_like_ollama("http://127.0.0.1:11434/v1"));
        assert!(looks_like_ollama("http://ollama.example.com/v1"));
        assert!(!looks_like_ollama("https://api.anthropic.com/v1"));
        assert!(!looks_like_ollama("https://api.openai.com/v1"));
    }

    #[test]
    fn detect_endpoint_routes_all_prefix_providers() {
        for (model, host) in [
            ("claude-opus-4-7", "anthropic.com"),
            ("gpt-5", "openai.com"),
            ("o4-mini", "openai.com"),
            ("gemini-2.5-pro", "googleapis.com"),
            ("grok-4", "x.ai"),
            ("deepseek-chat", "deepseek.com"),
            ("mistral-large-latest", "mistral.ai"),
            ("codestral-latest", "mistral.ai"),
        ] {
            let (url, _) = detect_endpoint(model);
            assert!(
                url.contains(host),
                "{model} routed to {url}, expected {host}"
            );
        }
    }

    #[test]
    fn detect_endpoint_routes_explicit_provider_prefix() {
        let (url, _) = detect_endpoint("groq:llama-3.3-70b-versatile");
        assert!(url.contains("groq.com"));
        let (url, _) = detect_endpoint("openrouter:anthropic/claude-opus-4-7");
        assert!(url.contains("openrouter.ai"));
        // Case-insensitive on the provider name.
        let (url, _) = detect_endpoint("GROQ:llama-3.3-70b-versatile");
        assert!(url.contains("groq.com"));
    }

    #[test]
    fn resolve_model_name_strips_only_known_provider_prefix() {
        assert_eq!(
            resolve_model_name("groq:llama-3.3-70b-versatile"),
            "llama-3.3-70b-versatile"
        );
        assert_eq!(
            resolve_model_name("openrouter:anthropic/claude-opus-4-7"),
            "anthropic/claude-opus-4-7"
        );
        // Ollama quant tags must NOT be stripped — `hf.co/...` isn't a provider.
        assert_eq!(
            resolve_model_name("hf.co/FoolDev/Thanatos-27B-HERETIC:Q4_K_M"),
            "hf.co/FoolDev/Thanatos-27B-HERETIC:Q4_K_M"
        );
        assert_eq!(resolve_model_name("llama3:latest"), "llama3:latest");
        assert_eq!(resolve_model_name("claude-opus-4-7"), "claude-opus-4-7");
    }

    #[test]
    fn set_model_switches_endpoint_across_providers() {
        let mut client = LlmClient::with_api_key(
            "https://api.anthropic.com/v1",
            "claude-opus-4-7",
            Some("manual-anthropic-key".to_string()),
        );
        // Within Anthropic: manual key is preserved, model name swapped.
        client.set_model("claude-sonnet-4-6".to_string());
        assert_eq!(client.base_url(), "https://api.anthropic.com/v1");
        assert_eq!(client.model(), "claude-sonnet-4-6");
        // Cross-provider swap: base_url updates; manual key dropped (it was
        // for the old provider and wouldn't authenticate anyway).
        client.set_model("gpt-5".to_string());
        assert_eq!(client.base_url(), "https://api.openai.com/v1");
        assert_eq!(client.model(), "gpt-5");
        // Explicit-prefix swap strips the provider tag from the stored name.
        client.set_model("groq:llama-3.3-70b-versatile".to_string());
        assert!(client.base_url().contains("groq.com"));
        assert_eq!(client.model(), "llama-3.3-70b-versatile");
        // Back to a colon-tagged local model: the endpoint returns to
        // Ollama, the cloud key is dropped, and the tag stays on the name —
        // storing `7b` here is what made the request body ask Ollama for a
        // model that doesn't exist.
        client.set_model("mistral:7b".to_string());
        assert_eq!(client.base_url(), DEFAULT_BASE_URL);
        assert_eq!(client.model(), "mistral:7b");
        assert!(client.api_key().is_none());
    }

    #[test]
    fn set_reasoning_effort_round_trips_and_filters_empty() {
        let mut client = LlmClient::new("http://127.0.0.1:0/v1", "m");
        assert_eq!(client.reasoning_effort(), None);
        client.set_reasoning_effort(Some("high".to_string()));
        assert_eq!(client.reasoning_effort(), Some("high"));
        // An empty string clears it back to the omit-the-field default.
        client.set_reasoning_effort(Some(String::new()));
        assert_eq!(client.reasoning_effort(), None);
    }

    #[test]
    fn chat_request_omits_reasoning_effort_unless_set() {
        let base = |effort| ChatRequest {
            model: "m",
            messages: &[],
            tools: None,
            stream: true,
            stream_options: None,
            reasoning_effort: effort,
        };
        let without = serde_json::to_value(base(None)).unwrap();
        assert!(without.get("reasoning_effort").is_none());
        let with = serde_json::to_value(base(Some("medium"))).unwrap();
        assert_eq!(
            with.get("reasoning_effort").and_then(|v| v.as_str()),
            Some("medium")
        );
    }

    #[test]
    fn wire_reasoning_effort_maps_nonstandard_tiers_to_max() {
        // Standard tiers pass through untouched.
        for e in ["low", "medium", "high", "max"] {
            assert_eq!(wire_reasoning_effort(e), e);
        }
        // teleia's extra labels have no wire value — they request `max` so
        // a strict backend (none/low/medium/high/max) doesn't 400.
        assert_eq!(wire_reasoning_effort("xhigh"), "max");
        assert_eq!(wire_reasoning_effort("leetcode"), "max");
    }

    #[test]
    fn assistant_content_always_serializes_as_a_string() {
        // Regression: Ollama's OpenAI-compat layer rejects a message whose
        // `content` is absent/null with `invalid message content type:
        // <nil>`. Every assistant variant must emit a string `content`.

        // Degenerate empty turn (model produced no text and no tool calls) —
        // the exact shape that wedged a persisted session.
        let empty = Message::Assistant {
            content: None,
            tool_calls: Vec::new(),
        };
        let v = serde_json::to_value(&empty).unwrap();
        assert_eq!(v.get("content").and_then(Value::as_str), Some(""));

        // Tool-call-only turn: content still present as "".
        let tc = ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: "read".into(),
                arguments: "{}".into(),
            },
        };
        let tool_turn = Message::Assistant {
            content: None,
            tool_calls: vec![tc],
        };
        let v = serde_json::to_value(&tool_turn).unwrap();
        assert_eq!(v.get("content").and_then(Value::as_str), Some(""));
        assert!(v.get("tool_calls").and_then(Value::as_array).is_some());

        // Normal text turn: content passes through unchanged.
        let text = Message::Assistant {
            content: Some("hi".into()),
            tool_calls: Vec::new(),
        };
        let v = serde_json::to_value(&text).unwrap();
        assert_eq!(v.get("content").and_then(Value::as_str), Some("hi"));
    }

    #[test]
    fn persisted_content_less_assistant_heals_on_reserialize() {
        // A session persisted before the fix stored `{"role":"assistant"}`.
        // Loading and re-sending it must now yield a string `content` so the
        // stuck session recovers without touching the sqlite store.
        let stored = r#"{"role":"assistant"}"#;
        let msg: Message = serde_json::from_str(stored).unwrap();
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v.get("content").and_then(Value::as_str), Some(""));
    }

    #[test]
    fn push_utf8_chunk_reassembles_split_multibyte() {
        // Feed "café🚀" one byte at a time — the worst case for boundary
        // splitting (é is 2 bytes, 🚀 is 4). A per-chunk from_utf8_lossy
        // would corrupt every split scalar; the incremental decoder must
        // reassemble them exactly.
        let mut buf = String::new();
        let mut carry = Vec::new();
        for b in "café🚀".as_bytes() {
            push_utf8_chunk(&mut buf, &mut carry, &[*b]);
        }
        assert_eq!(buf, "café🚀");
        assert!(carry.is_empty());
    }

    #[test]
    fn push_utf8_chunk_replaces_truly_invalid_bytes() {
        // 0xFF is never valid UTF-8: it must become U+FFFD and not linger
        // in the carry buffer (which would stall all following output).
        let mut buf = String::new();
        let mut carry = Vec::new();
        push_utf8_chunk(&mut buf, &mut carry, &[b'a', 0xFF, b'b']);
        assert_eq!(buf, "a\u{FFFD}b");
        assert!(carry.is_empty());
    }

    fn tc(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: name.into(),
                arguments: args.into(),
            },
        }
    }

    #[test]
    fn to_anthropic_messages_extracts_system_and_coalesces_tool_results() {
        // Two parallel tool calls → two tool_results. Anthropic requires
        // strict user/assistant alternation, so both results must land in a
        // SINGLE user turn, not two consecutive user messages (which 400).
        let msgs = vec![
            Message::System {
                content: "be terse".into(),
            },
            Message::User {
                content: "hi".into(),
            },
            Message::Assistant {
                content: Some("on it".into()),
                tool_calls: vec![
                    tc("t1", "read", r#"{"path":"a"}"#),
                    tc("t2", "read", r#"{"path":"b"}"#),
                ],
            },
            Message::Tool {
                tool_call_id: "t1".into(),
                content: "A".into(),
            },
            Message::Tool {
                tool_call_id: "t2".into(),
                content: "B".into(),
            },
        ];
        let (system, out) = to_anthropic_messages(&msgs);

        let system = system.expect("system lifted out");
        assert_eq!(system[0]["text"], "be terse");
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");

        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"][0]["text"], "hi");

        assert_eq!(out[1]["role"], "assistant");
        assert_eq!(out[1]["content"][0]["type"], "text");
        assert_eq!(out[1]["content"][1]["type"], "tool_use");
        assert_eq!(out[1]["content"][1]["id"], "t1");
        assert_eq!(out[1]["content"][1]["input"]["path"], "a");
        assert_eq!(out[1]["content"][2]["id"], "t2");

        assert_eq!(out[2]["role"], "user");
        let results = out[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2, "both tool_results share one user turn");
        assert_eq!(results[0]["type"], "tool_result");
        assert_eq!(results[0]["tool_use_id"], "t1");
        assert_eq!(results[1]["tool_use_id"], "t2");
    }

    #[test]
    fn build_anthropic_body_sets_required_fields_and_cache_breakpoints() {
        let msgs = vec![
            Message::System {
                content: "sys".into(),
            },
            Message::User {
                content: "hello".into(),
            },
        ];
        let tools = vec![ToolDef::new(
            "read",
            "read a file",
            json!({ "type": "object" }),
        )];
        let body = build_anthropic_body("claude-x", &msgs, Some(&tools));

        assert_eq!(body["model"], "claude-x");
        assert_eq!(body["max_tokens"], 128000);
        assert_eq!(body["stream"], true);
        assert_eq!(body["system"][0]["text"], "sys");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["tools"][0]["name"], "read");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");

        let last = body["messages"].as_array().unwrap().last().unwrap();
        let last_block = last["content"].as_array().unwrap().last().unwrap();
        assert_eq!(last_block["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn parse_tool_input_defaults_empty_and_malformed_to_object() {
        assert_eq!(parse_tool_input(""), json!({}));
        assert_eq!(parse_tool_input("   "), json!({}));
        assert_eq!(parse_tool_input("not json"), json!({}));
        assert_eq!(parse_tool_input(r#"{"a":1}"#), json!({ "a": 1 }));
    }

    #[test]
    fn anthropic_events_parse_the_streamed_shapes() {
        // message_start carries input usage (incl. cache hits).
        let ev: AnthropicEvent = serde_json::from_str(
            r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":5}}}"#,
        )
        .unwrap();
        match ev {
            AnthropicEvent::MessageStart { message } => {
                let u = message.usage.unwrap();
                assert_eq!(u.input_tokens, 10);
                assert_eq!(u.cache_read_input_tokens, 5);
            }
            _ => panic!("expected message_start"),
        }

        // tool_use block start opens a tool call.
        let ev: AnthropicEvent = serde_json::from_str(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu","name":"read","input":{}}}"#,
        )
        .unwrap();
        assert!(matches!(
            ev,
            AnthropicEvent::ContentBlockStart {
                content_block: AnthropicBlockStart::ToolUse { id, name }
            } if id == "tu" && name == "read"
        ));

        // text / thinking / input_json deltas.
        let text: AnthropicEvent = serde_json::from_str(
            r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"hi"}}"#,
        )
        .unwrap();
        assert!(matches!(
            text,
            AnthropicEvent::ContentBlockDelta { delta: AnthropicBlockDelta::TextDelta { text } } if text == "hi"
        ));
        let think: AnthropicEvent = serde_json::from_str(
            r#"{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
        )
        .unwrap();
        assert!(matches!(
            think,
            AnthropicEvent::ContentBlockDelta { delta: AnthropicBlockDelta::ThinkingDelta { thinking } } if thinking == "hmm"
        ));
        let json_delta: AnthropicEvent = serde_json::from_str(
            r#"{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"p\":1}"}}"#,
        )
        .unwrap();
        assert!(matches!(
            json_delta,
            AnthropicEvent::ContentBlockDelta { delta: AnthropicBlockDelta::InputJsonDelta { partial_json } } if partial_json == r#"{"p":1}"#
        ));

        // message_delta: stop_reason + cumulative output usage.
        let ev: AnthropicEvent = serde_json::from_str(
            r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":42}}"#,
        )
        .unwrap();
        match ev {
            AnthropicEvent::MessageDelta { delta, usage } => {
                assert_eq!(delta.stop_reason.as_deref(), Some("max_tokens"));
                assert_eq!(usage.unwrap().output_tokens, 42);
            }
            _ => panic!("expected message_delta"),
        }

        // Ignored event types collapse to Other rather than erroring the parse.
        assert!(matches!(
            serde_json::from_str::<AnthropicEvent>(r#"{"type":"ping"}"#).unwrap(),
            AnthropicEvent::Other
        ));
        assert!(matches!(
            serde_json::from_str::<AnthropicEvent>(r#"{"type":"content_block_stop","index":0}"#)
                .unwrap(),
            AnthropicEvent::Other
        ));

        // Mid-stream errors surface.
        let ev: AnthropicEvent = serde_json::from_str(
            r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#,
        )
        .unwrap();
        match ev {
            AnthropicEvent::Error { error } => {
                assert_eq!(error.kind, "overloaded_error");
                assert_eq!(error.message, "busy");
            }
            _ => panic!("expected error"),
        }
    }
}
