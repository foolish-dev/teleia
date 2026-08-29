//! Minimal MCP (Model Context Protocol) client. Spawns a server as a
//! child process, exchanges newline-delimited JSON-RPC over its
//! stdin/stdout, and exposes the server's tools to teleia's agent.
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
use teleia_agent::ToolRouter;
use teleia_llm::ToolDef;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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
    /// stderr is discarded so npx install spam and runtime warnings
    /// don't paint over the live TUI; spawn / handshake failures still
    /// surface via the `Result` and the boot-time warning in
    /// [`McpRegistry::spawn_all`].
    pub async fn spawn(name: &str, entry: &McpEntry) -> Result<Self> {
        let mut cmd = Command::new(&entry.command);
        cmd.args(&entry.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
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
        // Bound the handshake: an MCP server that spawns but never answers
        // `initialize` would otherwise hang boot indefinitely (spawn_all
        // awaits each spawn serially). On timeout the error flows into
        // spawn_all's warnings, demoting a boot hang to an `/mcps` warning.
        tokio::time::timeout(std::time::Duration::from_secs(10), client.initialize())
            .await
            .map_err(|_| anyhow!("MCP `{name}` handshake timed out after 10s"))??;
        Ok(client)
    }

    async fn initialize(&mut self) -> Result<()> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "teleia", "version": env!("CARGO_PKG_VERSION") },
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
        tool_call_outcome(&self.name, name, &result)
    }

    /// MCP `resources/read`. Returns the resource body as text — for
    /// binary blobs we drop a `[binary resource <uri>]` placeholder so
    /// the agent at least sees they exist.
    pub async fn read_resource(&mut self, uri: &str) -> Result<String> {
        let params = json!({ "uri": uri });
        let result = self.request("resources/read", params).await?;
        Ok(flatten_resource_contents(&result))
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
            if is_response_to(&msg, id) {
                if let Some(err) = msg.get("error") {
                    return Err(anyhow!("MCP `{}` returned error: {err}", self.name));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            // Not our response. A server->client *request* (has `method` and
            // `id`) must be answered or a server that blocks on our reply
            // deadlocks the turn — including one whose `id` collides with
            // ours, which `is_response_to` deliberately excludes. Answer
            // MethodNotFound; plain notifications (no `id`) are ignored.
            if let Some(reply) = method_not_found_reply(&msg) {
                let _ = self.send(&reply).await;
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
        // Bounded like lsp.rs's MAX_LSP_FRAME: a third-party server that
        // never emits a newline would otherwise grow this String until the
        // session OOMs. Well above any real JSON-RPC line.
        const MAX_MCP_LINE: u64 = 64 * 1024 * 1024;
        let mut buf = String::new();
        let n = (&mut self.stdout)
            .take(MAX_MCP_LINE)
            .read_line(&mut buf)
            .await?;
        if n == 0 {
            return Err(anyhow!("MCP `{}` closed stdout (server exited)", self.name));
        }
        if n as u64 == MAX_MCP_LINE && !buf.ends_with('\n') {
            return Err(anyhow!(
                "MCP `{}` sent a line over {MAX_MCP_LINE} bytes with no newline",
                self.name
            ));
        }
        Ok(buf)
    }
}

/// If `msg` is a server->client *request* (has both `method` and `id`),
/// build the JSON-RPC MethodNotFound reply that unblocks a server which
/// waits on our response. Returns `None` for responses and notifications.
fn method_not_found_reply(msg: &Value) -> Option<Value> {
    msg.get("method")?;
    let id = msg.get("id").cloned()?;
    Some(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32601, "message": "method not found" }
    }))
}

/// True iff `msg` is the response to our request `id`: it carries a
/// matching `id` and no `method`. A server-initiated *request* also has an
/// `id` (plus a `method`); without the `method` check, one whose id
/// happened to collide with ours would be mis-read as our response —
/// dropping the real reply and leaving the server's request unanswered.
///
/// The id is matched both as a number and as its decimal string: JSON-RPC
/// permits string ids, and a server that echoes ours as `"5"` would
/// otherwise never match, hanging the request loop on the next read.
fn is_response_to(msg: &Value, id: u64) -> bool {
    if msg.get("method").is_some() {
        return false;
    }
    match msg.get("id") {
        Some(v) => v.as_u64() == Some(id) || v.as_str() == Some(id.to_string().as_str()),
        None => false,
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // kill_on_drop is set on the Command; this is belt-and-braces.
        let _ = self.child.start_kill();
    }
}

/// Convert a `tools/call` result into the tool's outcome. MCP surfaces
/// tool-execution failures in-band — a normal result carrying
/// `isError: true` — not as JSON-RPC errors, so returning the flattened
/// content unconditionally would show the model a failure as a clean
/// success and its error-recovery hints would never engage.
fn tool_call_outcome(server: &str, tool: &str, result: &Value) -> Result<String> {
    let text = flatten_content(result);
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(anyhow!("MCP `{server}` tool `{tool}` failed: {text}"));
    }
    Ok(text)
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

/// Flatten an MCP `resources/read` result's `contents` array into a
/// plain string. Text resources concatenate; binary blobs collapse to a
/// `[binary resource <uri>]` marker.
fn flatten_resource_contents(result: &Value) -> String {
    let Some(items) = result.get("contents").and_then(Value::as_array) else {
        return result.to_string();
    };
    let mut parts: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        if let Some(t) = item.get("text").and_then(Value::as_str) {
            parts.push(t.to_string());
        } else if item.get("blob").is_some() {
            let uri = item.get("uri").and_then(Value::as_str).unwrap_or("?");
            parts.push(format!("[binary resource {uri}]"));
        }
    }
    if parts.is_empty() {
        result.to_string()
    } else {
        parts.join("\n\n")
    }
}

/// Synthetic tool name surfaced by [`McpRegistry`] when at least one
/// server advertised resources via `resources/list`.
pub const READ_RESOURCE_TOOL: &str = "mcp_read_resource";

/// Wraps a set of [`McpClient`]s and routes each tool call to the
/// server that exposes that tool. Names must be unique across servers
/// (later registrations shadow earlier ones; the dup is recorded in
/// `warnings`).
pub struct McpRegistry {
    clients: Vec<McpClient>,
    /// tool name → index into `clients`
    index: HashMap<String, usize>,
    /// Cached union of tool definitions, in registration order.
    tools: Vec<ToolDef>,
    /// Resources advertised by each server, parallel to `clients`.
    resources: Vec<Vec<McpResource>>,
    /// Boot-time warnings (spawn failures, shadowed tool names).
    /// Surfaced via the `/mcps` panel instead of stderr so loading
    /// stays silent at the terminal level.
    warnings: Vec<String>,
}

impl McpRegistry {
    pub fn new() -> Self {
        Self {
            clients: Vec::new(),
            index: HashMap::new(),
            tools: Vec::new(),
            resources: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Spawn every configured MCP server, list their tools, and merge
    /// them into the registry. Failures don't abort startup — teleia
    /// keeps booting with whatever servers did come up, and the error
    /// is stashed in `warnings()` for the `/mcps` panel.
    ///
    /// `on_step` is invoked once per entry with `(name, index, total)`
    /// (1-based index) before that entry's spawn begins, so the boot
    /// splash can report which server is currently being contacted.
    pub async fn spawn_all<'a, I, F>(entries: I, mut on_step: F) -> Self
    where
        I: IntoIterator<Item = (&'a String, &'a McpEntry)>,
        F: FnMut(&str, usize, usize),
    {
        let entries: Vec<(&'a String, &'a McpEntry)> = entries.into_iter().collect();
        let total = entries.len();
        let mut reg = Self::new();
        for (i, (name, entry)) in entries.into_iter().enumerate() {
            on_step(name, i + 1, total);
            match Self::spawn_one(name, entry).await {
                Ok((client, tools, resources)) => reg.add(client, tools, resources),
                Err(e) => reg
                    .warnings
                    .push(format!("MCP `{name}` failed to start: {e:#}")),
            }
        }
        reg
    }

    async fn spawn_one(
        name: &str,
        entry: &McpEntry,
    ) -> Result<(McpClient, Vec<ToolDef>, Vec<McpResource>)> {
        let mut client = McpClient::spawn(name, entry).await?;
        // Same bound as the `initialize` handshake: a server that answers
        // the handshake but stalls on `tools/list` would otherwise hang
        // boot forever (spawn_all awaits each spawn serially).
        let tools = tokio::time::timeout(std::time::Duration::from_secs(10), client.list_tools())
            .await
            .map_err(|_| anyhow!("MCP `{name}` tools/list timed out after 10s"))??;
        // Resources are optional — many servers don't expose any.
        // Treat a list error as "no resources"; a stall likewise degrades —
        // the late reply is skipped by the id match in `request`.
        let resources =
            match tokio::time::timeout(std::time::Duration::from_secs(10), client.list_resources())
                .await
            {
                Ok(res) => res.unwrap_or_default(),
                Err(_) => Vec::new(),
            };
        Ok((client, tools, resources))
    }

    fn add(&mut self, client: McpClient, tools: Vec<ToolDef>, resources: Vec<McpResource>) {
        let idx = self.clients.len();
        self.clients.push(client);
        for tool in tools {
            let tool_name = tool.function.name.clone();
            if self.index.contains_key(&tool_name) {
                self.warnings.push(format!(
                    "MCP tool `{tool_name}` already registered; shadowing earlier server"
                ));
            }
            if let Some(w) = reserved_name_warning(&tool_name) {
                // Warn *and* drop it. Registering the name anyway put it in
                // `definitions()` twice and advertised the server's schema
                // for a name `handles`/`dispatch` short-circuit, so the call
                // either ran the synthetic reader or died as `unknown tool`.
                self.warnings.push(w);
                continue;
            }
            self.index.insert(tool_name, idx);
            self.tools.push(tool);
        }
        self.resources.push(resources);
    }

    /// Boot-time warnings collected by `spawn_all` (spawn failures,
    /// shadowed tool names). Empty on a clean boot.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
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

    /// Server name → owned tool defs that came from that server. Lets
    /// the agent record the breakdown before the registry is moved into
    /// a boxed router, so `/mcps enable|disable NAME` can hide / restore
    /// a server's contribution without tearing down its child process.
    pub fn server_tool_defs(&self) -> std::collections::BTreeMap<String, Vec<ToolDef>> {
        let mut out: std::collections::BTreeMap<String, Vec<ToolDef>> =
            std::collections::BTreeMap::new();
        for def in &self.tools {
            let Some(&idx) = self.index.get(&def.function.name) else {
                continue;
            };
            let server = self.clients[idx].name.clone();
            out.entry(server).or_default().push(def.clone());
        }
        out
    }
}

impl McpRegistry {
    /// Build the synthetic `mcp_read_resource` tool def, or `None` if
    /// no server advertised any resources. The description embeds the
    /// full URI list so the model can pick one without a separate
    /// listing call.
    fn read_resource_def(&self) -> Option<ToolDef> {
        let uris: Vec<&str> = self
            .resources
            .iter()
            .flat_map(|rs| rs.iter().map(|r| r.uri.as_str()))
            .collect();
        if uris.is_empty() {
            return None;
        }
        let description = format!(
            "Read an MCP resource by URI. Available URIs: {}",
            uris.join(", ")
        );
        Some(ToolDef::new(
            READ_RESOURCE_TOOL,
            description,
            json!({
                "type": "object",
                "properties": {
                    "uri": {
                        "type": "string",
                        "description": "URI of the resource to read (must be one of the advertised URIs)"
                    }
                },
                "required": ["uri"]
            }),
        ))
    }
}

/// Warning text for a server tool name teleia has already reserved, or
/// `None` when the name is free. A built-in wins both the catalogue and
/// the dispatcher (`teleia_agent::Agent::set_tool_router`), and
/// [`READ_RESOURCE_TOOL`] is answered by the synthetic resource reader in
/// `handles` / `dispatch` below — either way the server's tool never runs,
/// so say so at boot rather than let it look live in `/mcps`.
fn reserved_name_warning(tool: &str) -> Option<String> {
    if tool == READ_RESOURCE_TOOL {
        return Some(format!(
            "MCP tool `{tool}` collides with teleia's synthetic resource reader; teleia answers that name itself and the server's tool is unreachable"
        ));
    }
    teleia_tools::definitions()
        .iter()
        .any(|d| d.function.name == tool)
        .then(|| {
            format!(
                "MCP tool `{tool}` collides with teleia's built-in `{tool}`; the built-in wins and the server's tool is unreachable"
            )
        })
}

impl ToolRouter for McpRegistry {
    fn definitions(&self) -> Vec<ToolDef> {
        let mut defs = self.tools.clone();
        if let Some(d) = self.read_resource_def() {
            defs.push(d);
        }
        defs
    }
    fn handles(&self, name: &str) -> bool {
        if name == READ_RESOURCE_TOOL {
            return self.resources.iter().any(|rs| !rs.is_empty());
        }
        self.index.contains_key(name)
    }
    fn dispatch<'a>(&'a mut self, name: &'a str, args: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            if name == READ_RESOURCE_TOOL {
                let v: Value = serde_json::from_str(args)
                    .with_context(|| format!("invalid JSON args for `{name}`"))?;
                let uri = v
                    .get("uri")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("`{name}` requires a string `uri` argument"))?;
                // Locate the server that advertised this URI and route
                // the read through that client.
                let idx = self
                    .resources
                    .iter()
                    .position(|rs| rs.iter().any(|r| r.uri == uri))
                    .ok_or_else(|| anyhow!("no MCP server advertises resource `{uri}`"))?;
                return self.clients[idx].read_resource(uri).await;
            }
            let idx = *self
                .index
                .get(name)
                .ok_or_else(|| anyhow!("MCP tool `{name}` not registered"))?;
            self.clients[idx].call_tool(name, args).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_call_outcome_surfaces_is_error_as_failure() {
        let v = json!({
            "isError": true,
            "content": [{ "type": "text", "text": "boom: no such row" }]
        });
        let err = tool_call_outcome("srv", "frob", &v).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("frob"), "{msg}");
        assert!(msg.contains("boom: no such row"), "{msg}");
    }

    #[test]
    fn tool_call_outcome_ok_without_is_error() {
        let v = json!({ "content": [{ "type": "text", "text": "fine" }] });
        assert_eq!(tool_call_outcome("srv", "frob", &v).unwrap(), "fine");
        // An explicit `isError: false` is a success too.
        let v = json!({
            "isError": false,
            "content": [{ "type": "text", "text": "fine" }]
        });
        assert_eq!(tool_call_outcome("srv", "frob", &v).unwrap(), "fine");
    }

    #[test]
    fn flatten_resource_text_joins_with_blank_lines() {
        let v = json!({
            "contents": [
                { "uri": "file:///a", "text": "first" },
                { "uri": "file:///a", "text": "second" }
            ]
        });
        assert_eq!(flatten_resource_contents(&v), "first\n\nsecond");
    }

    #[test]
    fn method_not_found_reply_only_for_server_requests() {
        // server->client request (method + id) → MethodNotFound reply
        let req = json!({ "jsonrpc": "2.0", "id": 5, "method": "sampling/createMessage" });
        let reply = method_not_found_reply(&req).expect("server request must be answered");
        assert_eq!(reply["id"], json!(5));
        assert_eq!(reply["error"]["code"], json!(-32601));
        // notification (method, no id) → ignored
        assert!(method_not_found_reply(&json!({ "method": "notifications/message" })).is_none());
        // response (id, no method) → ignored
        assert!(method_not_found_reply(&json!({ "id": 1, "result": {} })).is_none());
    }

    #[test]
    fn is_response_to_excludes_colliding_server_request() {
        // Our response: matching id, no method.
        assert!(is_response_to(&json!({ "id": 2, "result": {} }), 2));
        // Different id → not ours.
        assert!(!is_response_to(&json!({ "id": 9, "result": {} }), 2));
        // A server->client request whose id collides with ours: must NOT
        // be mistaken for our response (it has a `method`).
        assert!(!is_response_to(
            &json!({ "id": 2, "method": "sampling/createMessage" }),
            2
        ));
        // A notification (no id) is never our response.
        assert!(!is_response_to(
            &json!({ "method": "notifications/message" }),
            2
        ));
        // A server that echoes our id as a JSON-RPC string id still matches —
        // otherwise the request loop would never recognize it and hang.
        assert!(is_response_to(&json!({ "id": "2", "result": {} }), 2));
        assert!(!is_response_to(&json!({ "id": "9", "result": {} }), 2));
    }

    #[test]
    fn flatten_resource_blob_renders_placeholder() {
        let v = json!({
            "contents": [
                { "uri": "file:///pic.png", "blob": "ZGF0YQ==" }
            ]
        });
        assert_eq!(
            flatten_resource_contents(&v),
            "[binary resource file:///pic.png]"
        );
    }

    #[test]
    fn flatten_resource_unknown_shape_falls_back_to_raw_json() {
        // No `contents` array → echo the raw JSON so the agent can
        // still inspect whatever the server actually returned.
        let v = json!({ "unexpected": true });
        let out = flatten_resource_contents(&v);
        assert!(out.contains("unexpected"));
    }

    #[test]
    fn read_resource_def_absent_when_no_resources() {
        let reg = McpRegistry::new();
        assert!(reg.read_resource_def().is_none());
        // `mcp_read_resource` must NOT appear in definitions either.
        assert!(reg
            .definitions()
            .iter()
            .all(|d| d.function.name != READ_RESOURCE_TOOL));
        assert!(!reg.handles(READ_RESOURCE_TOOL));
    }

    #[test]
    fn reserved_tool_names_are_warned() {
        // A server may name a tool anything (`tools/list` is its own
        // word); the names teleia has already claimed must be visible in
        // /mcps, since teleia will never dispatch the server's version.
        let w = reserved_name_warning("read").expect("`read` is a built-in");
        assert!(w.contains("built-in"), "got: {w}");
        assert!(w.contains("unreachable"), "got: {w}");
        // The synthetic resource tool is reserved too: `handles` and
        // `dispatch` both answer that name before the index is consulted.
        let w = reserved_name_warning(READ_RESOURCE_TOOL).expect("synthetic name");
        assert!(w.contains("unreachable"), "got: {w}");
        assert!(reserved_name_warning("ctx7_query").is_none());
    }
}
