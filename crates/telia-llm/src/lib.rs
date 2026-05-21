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
    http: reqwest::Client,
}

impl LlmClient {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            http: reqwest::Client::new(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn set_model(&mut self, model: String) {
        self.model = model;
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

            let resp = self.http.post(&url).json(&body).send().await
                .with_context(|| format!("POST {url}"))?;

            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                Err(anyhow!("ollama returned {status}: {body}"))?;
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
}
