//! Minimal LSP (Language Server Protocol) client. Spawns each
//! configured server, exchanges the `initialize` handshake over the
//! Content-Length-framed JSON-RPC the protocol mandates, and exposes
//! `textDocument/diagnostic` (pull diagnostics, LSP 3.17) and
//! `textDocument/hover` as the `lsp_diagnostics` and `lsp_hover` agent
//! tools.
//!
//! Document synchronisation is best-effort: a file gets a one-shot
//! `textDocument/didOpen` the first time the agent asks about it, then
//! requests fan out to every running server so we don't have to
//! maintain a language-id → server routing table here. Definition,
//! references, and workspace operations are still TODO.

use anyhow::{anyhow, Context, Result};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use teleia_agent::ToolRouter;
use teleia_llm::ToolDef;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};

use crate::config::LspEntry;

/// Synthetic tool name surfaced by [`LspRegistry`] when at least one
/// LSP server is running. Pulls diagnostics for a file via
/// `textDocument/diagnostic`.
pub const DIAGNOSTICS_TOOL: &str = "lsp_diagnostics";

/// Synthetic tool name surfaced by [`LspRegistry`] when at least one
/// LSP server is running. Requests hover info for a 1-based
/// `(line, character)` position via `textDocument/hover`.
pub const HOVER_TOOL: &str = "lsp_hover";

#[derive(Debug, Clone, Deserialize)]
struct Position {
    line: u32,
    character: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct Range {
    start: Position,
    #[allow(dead_code)]
    end: Position,
}

#[derive(Debug, Clone, Deserialize)]
struct Diagnostic {
    range: Range,
    #[serde(default)]
    severity: Option<u8>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    source: Option<String>,
}

pub struct LspClient {
    pub name: String,
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    child: tokio::process::Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    /// URIs the agent has pushed a `didOpen` for, mapped to the last
    /// document version we sent. A re-query bumps the version and pushes a
    /// `didChange` with the current on-disk text, so the server never keeps
    /// reporting against the stale buffer it first opened.
    open_docs: HashMap<String, i32>,
}

impl LspClient {
    /// Spawn the LSP server, exchange the `initialize` handshake, send
    /// the `initialized` notification, and return a client ready for
    /// (eventually) request/response cycles.
    pub async fn spawn(name: &str, entry: &LspEntry) -> Result<Self> {
        let mut cmd = Command::new(&entry.command);
        cmd.args(&entry.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn LSP server `{}`", entry.command))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("LSP `{name}` exposed no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("LSP `{name}` exposed no stdout"))?;
        let mut client = Self {
            name: name.to_string(),
            server_name: None,
            server_version: None,
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            open_docs: HashMap::new(),
        };
        // Bound the handshake: a server that spawns but never answers
        // `initialize` (or stays stdout-silent) would otherwise hang boot
        // indefinitely, since spawn_all awaits each spawn serially. On
        // timeout the error flows into spawn_all's warnings, demoting a
        // boot hang to a `/lsps` warning.
        tokio::time::timeout(std::time::Duration::from_secs(10), client.initialize())
            .await
            .map_err(|_| anyhow!("LSP `{name}` handshake timed out after 10s"))??;
        Ok(client)
    }

    async fn initialize(&mut self) -> Result<()> {
        let root = std::env::current_dir()
            .ok()
            .and_then(|p| url_from_path(&p))
            .unwrap_or_else(|| "file:///".to_string());
        // Advertise pull-diagnostic support so capable servers
        // (rust-analyzer, pyright, tsserver, etc.) accept our
        // `textDocument/diagnostic` calls. `synchronization` is the
        // tiny capability needed to legally send `didOpen` later.
        let params = json!({
            "processId": std::process::id(),
            "rootUri": root,
            "capabilities": {
                "textDocument": {
                    "synchronization": {
                        "dynamicRegistration": false,
                        "didSave": false
                    },
                    "diagnostic": {
                        "dynamicRegistration": false,
                        "relatedDocumentSupport": false
                    },
                    "hover": {
                        "dynamicRegistration": false,
                        "contentFormat": ["markdown", "plaintext"]
                    }
                },
                "workspace": {}
            },
            "clientInfo": { "name": "teleia", "version": env!("CARGO_PKG_VERSION") },
        });
        let result = self.request("initialize", params).await?;
        // Pluck out serverInfo for the /lsps panel — purely informational.
        #[derive(Deserialize)]
        struct ServerInfo {
            name: String,
            #[serde(default)]
            version: Option<String>,
        }
        #[derive(Deserialize)]
        struct InitResult {
            #[serde(rename = "serverInfo", default)]
            server_info: Option<ServerInfo>,
        }
        if let Ok(init) = serde_json::from_value::<InitResult>(result) {
            if let Some(s) = init.server_info {
                self.server_name = Some(s.name);
                self.server_version = s.version;
            }
        }
        self.notify("initialized", json!({})).await?;
        Ok(())
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
        self.write_frame(&payload).await?;
        // Skip over server-initiated notifications (no `id`) until the
        // matching response arrives.
        loop {
            let msg = self.read_frame().await?;
            if is_response_to(&msg, id) {
                if let Some(err) = msg.get("error") {
                    return Err(anyhow!("LSP `{}` returned error: {err}", self.name));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            // Not our response. A server->client *request* (has `method` and
            // `id`) must be answered or a server that blocks on our reply
            // deadlocks the turn — including one whose `id` collides with
            // ours, which `is_response_to` deliberately excludes. Answer
            // MethodNotFound; plain notifications (no `id`) are ignored.
            if let Some(reply) = method_not_found_reply(&msg) {
                let _ = self.write_frame(&reply).await;
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_frame(&payload).await
    }

    /// Push the current `text` for `uri` to the server. The first call
    /// sends `textDocument/didOpen` (version 1); every later call bumps the
    /// version and sends a full-sync `textDocument/didChange`. Without the
    /// didChange, an agentic edit-then-re-query loop would keep getting
    /// diagnostics/hover for the buffer the server opened with — stale the
    /// moment the file is edited on disk.
    pub async fn open_document(&mut self, uri: &str, language_id: &str, text: &str) -> Result<()> {
        if let Some(version) = self.open_docs.get(uri).copied() {
            let version = version + 1;
            self.open_docs.insert(uri.to_string(), version);
            let params = json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [ { "text": text } ],
            });
            return self.notify("textDocument/didChange", params).await;
        }
        let params = json!({
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": 1,
                "text": text,
            }
        });
        self.notify("textDocument/didOpen", params).await?;
        self.open_docs.insert(uri.to_string(), 1);
        Ok(())
    }

    /// LSP 3.17 pull diagnostics. Returns rendered `path:line:col
    /// severity[source]: message` rows, one per diagnostic, or an
    /// empty string when the document is clean. Servers that don't
    /// implement `textDocument/diagnostic` surface a method-not-found
    /// error — the registry treats that as "no diagnostics from this
    /// server" and moves on.
    pub async fn pull_diagnostics(&mut self, uri: &str) -> Result<Vec<String>> {
        let params = json!({
            "textDocument": { "uri": uri }
        });
        let result = self.request("textDocument/diagnostic", params).await?;
        // Full report: { kind: "full", items: [Diagnostic...] }.
        // Unchanged report: { kind: "unchanged", resultId: "..." } — no items.
        let items = result
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut lines = Vec::with_capacity(items.len());
        for item in items {
            let Ok(d) = serde_json::from_value::<Diagnostic>(item) else {
                continue;
            };
            lines.push(format_diagnostic(uri, &d));
        }
        Ok(lines)
    }

    /// LSP `textDocument/hover`. `line` and `character` are 0-based
    /// (LSP wire convention). Returns the rendered hover text, or
    /// `None` when the server has nothing to say at that position
    /// (response is `null` or has empty contents). Servers that don't
    /// implement hover surface a method-not-found error — the registry
    /// treats that as "no hover from this server" and moves on.
    pub async fn hover(&mut self, uri: &str, line: u32, character: u32) -> Result<Option<String>> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });
        let result = self.request("textDocument/hover", params).await?;
        if result.is_null() {
            return Ok(None);
        }
        let Some(contents) = result.get("contents") else {
            return Ok(None);
        };
        let rendered = render_hover_contents(contents);
        Ok(if rendered.is_empty() {
            None
        } else {
            Some(rendered)
        })
    }

    /// LSP framing: `Content-Length: N\r\n\r\n<N bytes of JSON>`.
    async fn write_frame(&mut self, payload: &Value) -> Result<()> {
        let body = serde_json::to_vec(payload)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin.write_all(header.as_bytes()).await?;
        self.stdin.write_all(&body).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read_frame(&mut self) -> Result<Value> {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).await?;
            if n == 0 {
                return Err(anyhow!("LSP `{}` closed stdout", self.name));
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }
            if let Some((k, v)) = trimmed.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    content_length = Some(v.trim().parse().with_context(|| {
                        format!("invalid Content-Length from LSP `{}`", self.name)
                    })?);
                }
            }
        }
        let n = checked_frame_len(content_length, &self.name)?;
        let mut buf = vec![0u8; n];
        self.stdout.read_exact(&mut buf).await?;
        let v: Value = serde_json::from_slice(&buf)
            .with_context(|| format!("LSP `{}` returned non-JSON body", self.name))?;
        Ok(v)
    }
}

/// Bound the server-declared LSP frame size before allocating it, so a
/// bogus or corrupt `Content-Length` can't trigger a huge one-shot
/// allocation that OOM-aborts the process. 64 MiB is far above any real
/// LSP payload.
const MAX_LSP_FRAME: usize = 64 * 1024 * 1024;

fn checked_frame_len(content_length: Option<usize>, name: &str) -> Result<usize> {
    let n = content_length.ok_or_else(|| anyhow!("LSP `{name}` sent no Content-Length"))?;
    if n > MAX_LSP_FRAME {
        return Err(anyhow!(
            "LSP `{name}` frame too large: {n} bytes (cap {MAX_LSP_FRAME})"
        ));
    }
    Ok(n)
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
fn is_response_to(msg: &Value, id: u64) -> bool {
    if msg.get("method").is_some() {
        return false;
    }
    // Match string-form ids too (mirrors mcp.rs): we send numeric ids,
    // but a server that echoes ours back as a JSON string would
    // otherwise never be recognized and the request loop would hang.
    match msg.get("id") {
        Some(v) => v.as_u64() == Some(id) || v.as_str() == Some(id.to_string().as_str()),
        None => false,
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// Convert an absolute path to a `file://` URI. Returns None on
/// non-absolute paths or non-utf8 segments.
fn url_from_path(p: &Path) -> Option<String> {
    let p = p.to_str()?;
    if p.starts_with('/') {
        Some(format!("file://{p}"))
    } else {
        None
    }
}

/// Render one LSP diagnostic as `path:line:col severity[source]: msg`.
/// LSP line/column are 0-based; we add 1 to match how editors display
/// positions. `path` is derived from the URI by stripping `file://`.
fn format_diagnostic(uri: &str, d: &Diagnostic) -> String {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    let line = d.range.start.line + 1;
    let col = d.range.start.character + 1;
    let sev = match d.severity {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "info",
        Some(4) => "hint",
        _ => "diag",
    };
    match d.source.as_deref() {
        Some(s) if !s.is_empty() => format!("{path}:{line}:{col} {sev} [{s}]: {}", d.message),
        _ => format!("{path}:{line}:{col} {sev}: {}", d.message),
    }
}

/// Flatten an LSP Hover `contents` field into a single string. The
/// spec allows three shapes: a bare string, a `{ language, value }`
/// MarkedString, a `{ kind, value }` MarkupContent, or an array of
/// MarkedStrings. We collapse the array case with blank-line
/// separators and pull `value` out of either object shape.
fn render_hover_contents(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .map(render_hover_contents)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::Object(obj) => obj
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

/// Guess an LSP language id from a file extension. Falls through to
/// the extension itself for anything we don't have a canonical id for —
/// modern language servers tend to accept the extension as the id.
fn guess_language_id(p: &Path) -> String {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "rs" => "rust".into(),
        "ts" => "typescript".into(),
        "tsx" => "typescriptreact".into(),
        "js" => "javascript".into(),
        "jsx" => "javascriptreact".into(),
        "py" => "python".into(),
        "go" => "go".into(),
        "lua" => "lua".into(),
        "c" | "h" => "c".into(),
        "cpp" | "cxx" | "cc" | "hpp" => "cpp".into(),
        "java" => "java".into(),
        "rb" => "ruby".into(),
        "sh" | "bash" => "shellscript".into(),
        "" => "plaintext".into(),
        other => other.into(),
    }
}

/// Set of running LSP clients. Used by the TUI's `/lsps` panel — for
/// now the registry just owns the clients so they stay alive (the LSP
/// children are killed on Drop via `kill_on_drop`) and exposes a
/// formatted summary.
pub struct LspRegistry {
    clients: Vec<LspClient>,
    /// Boot-time warnings (spawn failures). Surfaced via the `/lsps`
    /// panel instead of stderr so loading stays silent at the
    /// terminal level.
    warnings: Vec<String>,
}

impl LspRegistry {
    /// Spawn every configured LSP server. Failures don't abort
    /// startup — teleia keeps booting and the error is stashed in
    /// `warnings()` for the `/lsps` panel.
    ///
    /// `on_step` is invoked once per entry with `(name, index, total)`
    /// (1-based index) before that entry's spawn begins, so the boot
    /// splash can report which server is currently being contacted.
    pub async fn spawn_all<'a, I, F>(entries: I, mut on_step: F) -> Self
    where
        I: IntoIterator<Item = (&'a String, &'a LspEntry)>,
        F: FnMut(&str, usize, usize),
    {
        let entries: Vec<(&'a String, &'a LspEntry)> = entries.into_iter().collect();
        let total = entries.len();
        let mut clients = Vec::new();
        let mut warnings = Vec::new();
        for (i, (name, entry)) in entries.into_iter().enumerate() {
            on_step(name, i + 1, total);
            match LspClient::spawn(name, entry).await {
                Ok(c) => clients.push(c),
                Err(e) => warnings.push(format!("LSP `{name}` failed to start: {e:#}")),
            }
        }
        Self { clients, warnings }
    }

    /// Boot-time warnings collected by `spawn_all`. Empty on a clean boot.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Per-server (name, serverInfo, version) — drives the `/lsps`
    /// panel rendering.
    pub fn server_summaries(&self) -> Vec<(String, Option<String>, Option<String>)> {
        self.clients
            .iter()
            .map(|c| {
                (
                    c.name.clone(),
                    c.server_name.clone(),
                    c.server_version.clone(),
                )
            })
            .collect()
    }

    /// Fan a pull-diagnostic request across every running server.
    /// Each client gets a `didOpen` on first sight, then the pull.
    /// Per-server failures (method-not-found, connection drops) are
    /// swallowed — they just mean "no diagnostics from that server".
    pub async fn diagnostics_for(&mut self, path: &str) -> Result<String> {
        let raw = Path::new(path);
        let abs = raw
            .canonicalize()
            .with_context(|| format!("resolving `{path}`"))?;
        let uri = url_from_path(&abs)
            .ok_or_else(|| anyhow!("could not form file:// URI for `{path}`"))?;
        let text = std::fs::read_to_string(&abs).with_context(|| format!("reading `{path}`"))?;
        let language_id = guess_language_id(&abs);

        let mut all: Vec<String> = Vec::new();
        for client in self.clients.iter_mut() {
            // Best-effort per server: ignore didOpen failures (some
            // servers refuse languages they don't recognise).
            if client
                .open_document(&uri, &language_id, &text)
                .await
                .is_err()
            {
                continue;
            }
            if let Ok(lines) = client.pull_diagnostics(&uri).await {
                all.extend(lines);
            }
        }
        if all.is_empty() {
            Ok(format!("no diagnostics for {}", abs.display()))
        } else {
            Ok(all.join("\n"))
        }
    }

    /// Fan a hover request across every running server. Positions are
    /// 1-based on the way in (to match how diagnostics render
    /// `path:line:col`) and converted to LSP's 0-based wire form
    /// inside. Per-server failures are swallowed the same way as
    /// `diagnostics_for`. Hover blobs from multiple servers are joined
    /// with a `---` separator.
    pub async fn hover_for(&mut self, path: &str, line: u32, character: u32) -> Result<String> {
        let raw = Path::new(path);
        let abs = raw
            .canonicalize()
            .with_context(|| format!("resolving `{path}`"))?;
        let uri = url_from_path(&abs)
            .ok_or_else(|| anyhow!("could not form file:// URI for `{path}`"))?;
        let text = std::fs::read_to_string(&abs).with_context(|| format!("reading `{path}`"))?;
        let language_id = guess_language_id(&abs);

        let lsp_line = line.saturating_sub(1);
        let lsp_char = character.saturating_sub(1);

        let mut blobs: Vec<String> = Vec::new();
        for client in self.clients.iter_mut() {
            if client
                .open_document(&uri, &language_id, &text)
                .await
                .is_err()
            {
                continue;
            }
            if let Ok(Some(h)) = client.hover(&uri, lsp_line, lsp_char).await {
                blobs.push(h);
            }
        }
        if blobs.is_empty() {
            Ok(format!(
                "no hover info at {}:{}:{}",
                abs.display(),
                line,
                character
            ))
        } else {
            Ok(blobs.join("\n\n---\n\n"))
        }
    }
}

impl ToolRouter for LspRegistry {
    fn definitions(&self) -> Vec<ToolDef> {
        if self.clients.is_empty() {
            return Vec::new();
        }
        let n = self.clients.len();
        let diagnostics_description = format!(
            "Get diagnostics (errors, warnings) for a source file from the \
             {n} configured language server(s). Sends textDocument/didOpen \
             on first use, then issues a pull-diagnostic request. Returns \
             one `<path>:<line>:<col> <severity>[<source>]: <message>` row \
             per diagnostic, or `no diagnostics for <path>` when clean.",
        );
        let hover_description = format!(
            "Get hover info (type, signature, doc-comment) at a source \
             position from the {n} configured language server(s). `line` \
             and `character` are 1-based to match the `<path>:<line>:<col>` \
             positions printed by `lsp_diagnostics`. Returns the rendered \
             hover blob (markdown when the server provides it), or `no \
             hover info at <path>:<line>:<col>` when the server has \
             nothing at that position.",
        );
        vec![
            ToolDef::new(
                DIAGNOSTICS_TOOL,
                diagnostics_description,
                json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file to diagnose (absolute or relative to teleia's cwd)"
                        }
                    },
                    "required": ["path"]
                }),
            ),
            ToolDef::new(
                HOVER_TOOL,
                hover_description,
                json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file (absolute or relative to teleia's cwd)"
                        },
                        "line": {
                            "type": "integer",
                            "description": "1-based line number (matches lsp_diagnostics output)"
                        },
                        "character": {
                            "type": "integer",
                            "description": "1-based column number (matches lsp_diagnostics output)"
                        }
                    },
                    "required": ["path", "line", "character"]
                }),
            ),
        ]
    }
    fn handles(&self, name: &str) -> bool {
        (name == DIAGNOSTICS_TOOL || name == HOVER_TOOL) && !self.clients.is_empty()
    }
    fn dispatch<'a>(&'a mut self, name: &'a str, args: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let v: Value = serde_json::from_str(args)
                .with_context(|| format!("invalid JSON args for `{name}`"))?;
            let path = v
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("`{name}` requires a string `path` argument"))?;
            match name {
                DIAGNOSTICS_TOOL => self.diagnostics_for(path).await,
                HOVER_TOOL => {
                    let line =
                        v.get("line").and_then(Value::as_u64).ok_or_else(|| {
                            anyhow!("`{name}` requires an integer `line` argument")
                        })? as u32;
                    let character = v.get("character").and_then(Value::as_u64).ok_or_else(|| {
                        anyhow!("`{name}` requires an integer `character` argument")
                    })? as u32;
                    self.hover_for(path, line, character).await
                }
                _ => Err(anyhow!("LSP tool `{name}` not registered")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(line: u32, col: u32, sev: u8, msg: &str, source: Option<&str>) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position {
                    line,
                    character: col,
                },
                end: Position {
                    line,
                    character: col,
                },
            },
            severity: Some(sev),
            message: msg.into(),
            source: source.map(String::from),
        }
    }

    #[test]
    fn format_diagnostic_with_source_includes_brackets() {
        let d = diag(9, 4, 1, "no method foo", Some("rustc"));
        let out = format_diagnostic("file:///work/src/main.rs", &d);
        // LSP is 0-based, displayed 1-based → 10:5.
        assert_eq!(out, "/work/src/main.rs:10:5 error [rustc]: no method foo");
    }

    #[test]
    fn checked_frame_len_rejects_oversized_and_missing() {
        assert_eq!(checked_frame_len(Some(1024), "srv").unwrap(), 1024);
        assert!(checked_frame_len(Some(MAX_LSP_FRAME + 1), "srv").is_err());
        assert!(checked_frame_len(None, "srv").is_err());
    }

    #[test]
    fn method_not_found_reply_only_for_server_requests() {
        // server->client request (method + id) → MethodNotFound reply
        let req = json!({ "jsonrpc": "2.0", "id": 7, "method": "workspace/configuration" });
        let reply = method_not_found_reply(&req).expect("server request must be answered");
        assert_eq!(reply["id"], json!(7));
        assert_eq!(reply["error"]["code"], json!(-32601));
        // notification (method, no id) → ignored
        assert!(method_not_found_reply(&json!({ "method": "window/logMessage" })).is_none());
        // response (id, no method) → ignored
        assert!(method_not_found_reply(&json!({ "id": 3, "result": {} })).is_none());
    }

    #[test]
    fn is_response_to_excludes_colliding_server_request() {
        // Our response: matching id, no method.
        assert!(is_response_to(&json!({ "id": 4, "result": {} }), 4));
        // Different id → not ours.
        assert!(!is_response_to(&json!({ "id": 5, "result": {} }), 4));
        // A server->client request whose id collides with ours: must NOT
        // be mistaken for our response (it has a `method`).
        assert!(!is_response_to(
            &json!({ "id": 4, "method": "workspace/configuration" }),
            4
        ));
        // A notification (no id) is never our response.
        assert!(!is_response_to(
            &json!({ "method": "window/logMessage" }),
            4
        ));
        // A server that echoes our id as a JSON-RPC string id still matches —
        // otherwise the request loop would never recognize it and hang.
        assert!(is_response_to(&json!({ "id": "4", "result": {} }), 4));
        assert!(!is_response_to(&json!({ "id": "5", "result": {} }), 4));
    }

    #[test]
    fn format_diagnostic_without_source_omits_brackets() {
        let d = diag(0, 0, 2, "unused", None);
        let out = format_diagnostic("file:///x.rs", &d);
        assert_eq!(out, "/x.rs:1:1 warning: unused");
    }

    #[test]
    fn format_diagnostic_severity_table() {
        for (sev, expected) in [(1, "error"), (2, "warning"), (3, "info"), (4, "hint")] {
            let d = diag(0, 0, sev, "x", None);
            let out = format_diagnostic("file:///f", &d);
            assert!(
                out.contains(expected),
                "severity {sev} → missing `{expected}` in `{out}`"
            );
        }
    }

    #[test]
    fn guess_language_id_table() {
        assert_eq!(guess_language_id(Path::new("a.rs")), "rust");
        assert_eq!(guess_language_id(Path::new("a.py")), "python");
        assert_eq!(guess_language_id(Path::new("a.tsx")), "typescriptreact");
        assert_eq!(guess_language_id(Path::new("a.cpp")), "cpp");
        // Unknown extension falls through to the extension itself.
        assert_eq!(guess_language_id(Path::new("a.zig")), "zig");
        // No extension → plaintext sentinel.
        assert_eq!(guess_language_id(Path::new("Makefile")), "plaintext");
    }

    #[test]
    fn render_hover_handles_bare_string() {
        let v = json!("hello world");
        assert_eq!(render_hover_contents(&v), "hello world");
    }

    #[test]
    fn render_hover_handles_markup_content() {
        let v = json!({ "kind": "markdown", "value": "```rs\nfn foo()\n```" });
        assert_eq!(render_hover_contents(&v), "```rs\nfn foo()\n```");
    }

    #[test]
    fn render_hover_handles_marked_string_object() {
        let v = json!({ "language": "rust", "value": "fn foo()" });
        assert_eq!(render_hover_contents(&v), "fn foo()");
    }

    #[test]
    fn render_hover_joins_array_with_blank_lines() {
        let v = json!([
            "Type: `u32`",
            { "language": "rust", "value": "fn foo()" },
            { "kind": "markdown", "value": "Computes the answer." }
        ]);
        assert_eq!(
            render_hover_contents(&v),
            "Type: `u32`\n\nfn foo()\n\nComputes the answer."
        );
    }

    #[test]
    fn render_hover_skips_empty_array_entries() {
        let v = json!(["", { "value": "" }, "real text"]);
        assert_eq!(render_hover_contents(&v), "real text");
    }

    #[test]
    fn render_hover_object_without_value_is_empty() {
        let v = json!({ "language": "rust" });
        assert_eq!(render_hover_contents(&v), "");
    }

    #[test]
    fn url_from_path_absolute_only() {
        assert_eq!(
            url_from_path(Path::new("/etc/hosts")).as_deref(),
            Some("file:///etc/hosts")
        );
        assert!(url_from_path(Path::new("relative/path")).is_none());
    }
}
