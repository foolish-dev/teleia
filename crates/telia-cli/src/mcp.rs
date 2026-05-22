//! Minimal MCP (Model Context Protocol) client. Spawns a server as a
//! child process, exchanges newline-delimited JSON-RPC over its
//! stdin/stdout, and exposes the server's tools to telia's agent.
//!
//! Covers the bare minimum: `initialize` handshake, the
//! `notifications/initialized` ack, `tools/list`, and `tools/call`.
//! No capabilities negotiation beyond the protocol version, no
//! resources, no prompts, no cancellation. Enough to let an off-the-
//! shelf MCP server (filesystem, github, etc.) plug into the chat.
//!
//! Each [`McpClient`] owns a single child process; the [`McpRegistry`]
//! holds the set and routes tool calls by tool name.

use anyhow::{anyhow, Context, Result};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use telia_agent::ToolRouter;
use telia_llm::ToolDef;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};

use crate::config::McpEntry;

const PROTOCOL_VERSION: &str = "2024-11-05";

/// One entry from a server's `resources/list` reply. Fields are
/// stashed for future per-resource rendering (resources/read tool
/// exposure); for now only the count is consumed via `/mcps`.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct McpResource {
    pub uri: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

pub struct McpClient {
    name: String,
    child: tokio::process::Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    /// Spawn the server, do the JSON-RPC handshake, and return a
    /// client ready for `list_tools` / `call_tool`. The server's
    /// stderr is left attached to telia's stderr so any startup errors
    /// surface in the user's terminal.
    pub async fn spawn(name: &str, entry: &McpEntry) -> Result<Self> {
        let mut cmd = Command::new(&entry.command);
        cmd.args(&entry.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        for (k, v) in &entry.env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn MCP server `{}`", entry.command))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("MCP server `{name}` exposed no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("MCP server `{name}` exposed no stdout"))?;

        let mut client = Self {
            name: name.to_string(),
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        };
        client.initialize().await?;
        Ok(client)
    }

    async fn initialize(&mut self) -> Result<()> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "telia", "version": env!("CARGO_PKG_VERSION") },
        });
        let _ = self.request("initialize", params).await?;
        // Per spec, follow up with the `initialized` notification.
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    /// MCP `resources/list`. Servers that don't expose resources
    /// return an error (or an empty list); both are treated as
    /// "no resources" by the caller.
    pub async fn list_resources(&mut self) -> Result<Vec<McpResource>> {
        #[derive(Deserialize)]
        struct Entry {
            uri: String,
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            description: Option<String>,
            #[serde(rename = "mimeType", default)]
            mime_type: Option<String>,
        }
        #[derive(Deserialize)]
        struct ListResult {
            #[serde(default)]
            resources: Vec<Entry>,
        }
        let result = self.request("resources/list", json!({})).await?;
        let list: ListResult =
            serde_json::from_value(result).unwrap_or(ListResult { resources: vec![] });
        Ok(list
            .resources
            .into_iter()
            .map(|r| McpResource {
                uri: r.uri,
                name: r.name,
                description: r.description,
                mime_type: r.mime_type,
            })
            .collect())
    }

    pub async fn list_tools(&mut self) -> Result<Vec<ToolDef>> {
        #[derive(Deserialize)]
        struct ToolEntry {
            name: String,
            #[serde(default)]
            description: Option<String>,
            #[serde(rename = "inputSchema", default)]
            input_schema: Value,
        }
        #[derive(Deserialize)]
        struct ListResult {
            tools: Vec<ToolEntry>,
        }
        let result = self.request("tools/list", json!({})).await?;
        let list: ListResult =
            serde_json::from_value(result).context("malformed tools/list response")?;
        Ok(list
            .tools
            .into_iter()
            .map(|t| {
                ToolDef::new(
                    t.name,
                    t.description.unwrap_or_default(),
                    if t.input_schema.is_null() {
                        json!({ "type": "object" })
                    } else {
                        t.input_schema
                    },
                )
            })
            .collect())
    }

    /// Invoke an MCP tool. Arguments come in as the raw JSON string the
    /// model produced; we parse it to a Value first so the server gets
    /// real JSON (not a stringified blob). The result's `content` array
    /// is flattened to a newline-joined string for the agent log.
    pub async fn call_tool(&mut self, name: &str, args_json: &str) -> Result<String> {
        let arguments: Value = if args_json.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(args_json)
                .with_context(|| format!("invalid JSON args for `{name}`"))?
        };
        let params = json!({ "name": name, "arguments": arguments });
        let result = self.request("tools/call", params).await?;
        Ok(flatten_content(&result))
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send(&payload).await?;
        // MCP servers may emit unrelated server-initiated notifications
        // (no `id`) before responding. Skip past those.
        loop {
            let line = self.read_line().await?;
            let msg: Value = serde_json::from_str(&line)
                .with_context(|| format!("MCP `{}` returned non-JSON line: {line}", self.name))?;
            match msg.get("id").and_then(Value::as_u64) {
                Some(rid) if rid == id => {
                    if let Some(err) = msg.get("error") {
                        return Err(anyhow!("MCP `{}` returned error: {err}", self.name));
                    }
                    return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
                }
                _ => continue,
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.send(&payload).await
    }

    async fn send(&mut self, payload: &Value) -> Result<()> {
        let line = serde_json::to_string(payload)?;
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read_line(&mut self) -> Result<String> {
        let mut buf = String::new();
        let n = self.stdout.read_line(&mut buf).await?;
        if n == 0 {
            return Err(anyhow!("MCP `{}` closed stdout (server exited)", self.name));
        }
        Ok(buf)
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // kill_on_drop is set on the Command; this is belt-and-braces.
        let _ = self.child.start_kill();
    }
}

/// Flatten an MCP `tools/call` result's `content` array into a plain
/// string. Multiple content blocks join with a blank line. Non-text
/// blocks render as `[type: …]` so the model at least sees they exist.
fn flatten_content(result: &Value) -> String {
    let Some(items) = result.get("content").and_then(Value::as_array) else {
        return result.to_string();
    };
    let mut parts: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "text" => {
                if let Some(t) = item.get("text").and_then(Value::as_str) {
                    parts.push(t.to_string());
                }
            }
            other => parts.push(format!("[{other} content]")),
        }
    }
    parts.join("\n\n")
}

/// Wraps a set of [`McpClient`]s and routes each tool call to the
/// server that exposes that tool. Names must be unique across servers
/// (later registrations shadow earlier ones, with a stderr warning).
pub struct McpRegistry {
    clients: Vec<McpClient>,
    /// tool name → index into `clients`
    index: HashMap<String, usize>,
    /// Cached union of tool definitions, in registration order.
    tools: Vec<ToolDef>,
    /// Resources advertised by each server, parallel to `clients`.
    resources: Vec<Vec<McpResource>>,
}

impl McpRegistry {
    pub fn new() -> Self {
        Self {
            clients: Vec::new(),
            index: HashMap::new(),
            tools: Vec::new(),
            resources: Vec::new(),
        }
    }

    /// Spawn every configured MCP server, list their tools, and merge
    /// them into the registry. Failures are surfaced as stderr lines
    /// but don't abort startup — telia keeps booting with whatever
    /// servers did come up.
    pub async fn spawn_all<'a, I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (&'a String, &'a McpEntry)>,
    {
        let mut reg = Self::new();
        for (name, entry) in entries {
            match Self::spawn_one(name, entry).await {
                Ok((client, tools, resources)) => reg.add(client, tools, resources),
                Err(e) => eprintln!("warning: MCP `{name}` failed to start: {e:#}"),
            }
        }
        reg
    }

    async fn spawn_one(
        name: &str,
        entry: &McpEntry,
    ) -> Result<(McpClient, Vec<ToolDef>, Vec<McpResource>)> {
        let mut client = McpClient::spawn(name, entry).await?;
        let tools = client.list_tools().await?;
        // Resources are optional — many servers don't expose any.
        // Treat a list error as "no resources".
        let resources = client.list_resources().await.unwrap_or_default();
        Ok((client, tools, resources))
    }

    fn add(&mut self, client: McpClient, tools: Vec<ToolDef>, resources: Vec<McpResource>) {
        let idx = self.clients.len();
        self.clients.push(client);
        for tool in tools {
            let tool_name = tool.function.name.clone();
            if self.index.contains_key(&tool_name) {
                eprintln!(
                    "warning: MCP tool `{tool_name}` already registered; shadowing earlier server"
                );
            }
            self.index.insert(tool_name, idx);
            self.tools.push(tool);
        }
        self.resources.push(resources);
    }

    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    pub fn server_count(&self) -> usize {
        self.clients.len()
    }

    pub fn resource_count(&self) -> usize {
        self.resources.iter().map(|r| r.len()).sum()
    }

    /// Per-server (name, tool count, resource count). Used by the
    /// `/mcps` command to summarise what each server contributed.
    pub fn server_summaries(&self) -> Vec<(String, usize, usize)> {
        let mut tool_counts = vec![0usize; self.clients.len()];
        for &idx in self.index.values() {
            if let Some(slot) = tool_counts.get_mut(idx) {
                *slot += 1;
            }
        }
        self.clients
            .iter()
            .enumerate()
            .map(|(idx, c)| (c.name.clone(), tool_counts[idx], self.resources[idx].len()))
            .collect()
    }
}

impl ToolRouter for McpRegistry {
    fn definitions(&self) -> Vec<ToolDef> {
        self.tools.clone()
    }
    fn handles(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }
    fn dispatch<'a>(&'a mut self, name: &'a str, args: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let idx = *self
                .index
                .get(name)
                .ok_or_else(|| anyhow!("MCP tool `{name}` not registered"))?;
            self.clients[idx].call_tool(name, args).await
        })
    }
}
