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
        #[serde(default, skip_serializing_if = "Option::is_none")]
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
    Done {
        tool_calls: Vec<ToolCall>,
        usage: Option<Usage>,
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
}

#[derive(Debug, Default, Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
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

/// All cloud providers telia knows about out of the box. The order
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
        prefixes: &["mistral-", "codestral-"],
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
];

/// Resolve a model name to its provider. Checks the explicit
/// `provider:model` form first (case-insensitive on the provider name),
/// then falls back to prefix matching against the [`PROVIDERS`] table.
/// Returns `None` for names that should route to Ollama.
pub fn provider_for_model(model: &str) -> Option<&'static Provider> {
    if let Some((head, _)) = model.split_once(':') {
        if let Some(p) = PROVIDERS.iter().find(|p| p.name.eq_ignore_ascii_case(head)) {
            return Some(p);
        }
    }
    PROVIDERS
        .iter()
        .find(|p| p.prefixes.iter().any(|pre| model.starts_with(pre)))
}

/// Strip a leading `provider:` prefix when present and recognized —
/// providers don't accept their own name in the `"model"` request field.
/// `groq:llama-3.3-70b-versatile` → `llama-3.3-70b-versatile`;
/// `hf.co/FoolDev/Thanatos-27B:Q4_K_M` → unchanged (no provider
/// matches `hf.co/...`).
pub fn resolve_model_name(model: &str) -> &str {
    if let Some((head, rest)) = model.split_once(':') {
        if PROVIDERS.iter().any(|p| p.name.eq_ignore_ascii_case(head)) {
            return rest;
        }
    }
    model
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
            http: reqwest::Client::new(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn model(&self) -> &str {
        &self.model
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

    /// List models Ollama has cached locally. Returns the `name` field of
    /// each entry in `/api/tags` (e.g. `"llama3:latest"`,
    /// `"hf.co/FoolDev/Thanatos-27B:Q4_K_M"`). Best-effort — returns an
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
            while let Some(chunk) = bytes.next().await {
                let chunk = chunk.context("read pull chunk")?;
                buf.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(pos) = buf.find('\n') {
                    let line: String = buf.drain(..=pos).collect();
                    let line = line.trim();
                    if line.is_empty() { continue; }
                    if let Ok(prog) = serde_json::from_str::<PullProgress>(line) {
                        yield prog;
                    }
                }
            }
        }
    }

    /// Stream chat completions as `ChatEvent`s. The final event is always `Done`.
    pub fn stream<'a>(
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
            let mut accumulated: Vec<AccTool> = Vec::new();
            let mut last_usage: Option<Usage> = None;

            while let Some(chunk) = bytes.next().await {
                let chunk = chunk.context("read stream chunk")?;
                buf.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(pos) = buf.find("\n\n") {
                    let event: String = buf.drain(..pos + 2).collect();
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
                            for tcd in choice.delta.tool_calls {
                                accumulate(&mut accumulated, tcd);
                            }
                        }
                    }
                }
            }

            let tool_calls = accumulated.into_iter().map(|a| ToolCall {
                id: a.id,
                kind: a.kind,
                function: ToolCallFunction { name: a.name, arguments: a.arguments },
            }).collect();
            yield ChatEvent::Done { tool_calls, usage: last_usage };
        }
    }
}

struct AccTool {
    id: String,
    kind: String,
    name: String,
    arguments: String,
}

fn accumulate(acc: &mut Vec<AccTool>, delta: ToolCallDelta) {
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
        let (url, key) = detect_endpoint("hf.co/FoolDev/Thanatos-27B:Q4_K_M");
        assert_eq!(url, DEFAULT_BASE_URL);
        assert!(key.is_none());
        let (url, _) = detect_endpoint("llama3:latest");
        assert_eq!(url, DEFAULT_BASE_URL);
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
            resolve_model_name("hf.co/FoolDev/Thanatos-27B:Q4_K_M"),
            "hf.co/FoolDev/Thanatos-27B:Q4_K_M"
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
    }
}
