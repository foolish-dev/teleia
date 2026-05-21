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

/// Streaming events from the chat endpoint.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    ContentDelta(String),
    Done { tool_calls: Vec<ToolCall> },
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ToolDef]>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
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

    /// Stream chat completions as `ChatEvent`s. The final event is always `Done`.
    pub fn stream<'a>(
        &'a self,
        messages: &'a [Message],
        tools: Option<&'a [ToolDef]>,
    ) -> impl Stream<Item = Result<ChatEvent>> + 'a {
        try_stream! {
            let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
            let body = ChatRequest { model: &self.model, messages, tools, stream: true };

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
            yield ChatEvent::Done { tool_calls };
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
