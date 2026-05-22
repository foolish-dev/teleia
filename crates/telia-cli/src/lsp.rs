//! Minimal LSP (Language Server Protocol) client. Spawns each
//! configured server, exchanges the `initialize` handshake over the
//! Content-Length-framed JSON-RPC the protocol mandates, and reports
//! whether the server came up cleanly.
//!
//! Tool exposure (hover / definition / diagnostics) isn't wired yet —
//! this just gets each server reachable so the `/lsps` panel can
//! report `running` instead of `(runtime stub)`. Document
//! synchronisation, cancellation, workspace updates, and capability-
//! gated tool registration are TODO.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};

use crate::config::LspEntry;

pub struct LspClient {
    pub name: String,
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    #[allow(dead_code)]
    child: tokio::process::Child,
    #[allow(dead_code)]
    stdin: ChildStdin,
    #[allow(dead_code)]
    stdout: BufReader<ChildStdout>,
    #[allow(dead_code)]
    next_id: u64,
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
        };
        client.initialize().await?;
        Ok(client)
    }

    async fn initialize(&mut self) -> Result<()> {
        let root = std::env::current_dir()
            .ok()
            .and_then(|p| url_from_path(&p))
            .unwrap_or_else(|| "file:///".to_string());
        let params = json!({
            "processId": std::process::id(),
            "rootUri": root,
            "capabilities": {
                "textDocument": {},
                "workspace": {}
            },
            "clientInfo": { "name": "telia", "version": env!("CARGO_PKG_VERSION") },
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
            match msg.get("id").and_then(Value::as_u64) {
                Some(rid) if rid == id => {
                    if let Some(err) = msg.get("error") {
                        return Err(anyhow!("LSP `{}` returned error: {err}", self.name));
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
        self.write_frame(&payload).await
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
        let n =
            content_length.ok_or_else(|| anyhow!("LSP `{}` sent no Content-Length", self.name))?;
        let mut buf = vec![0u8; n];
        self.stdout.read_exact(&mut buf).await?;
        let v: Value = serde_json::from_slice(&buf)
            .with_context(|| format!("LSP `{}` returned non-JSON body", self.name))?;
        Ok(v)
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// Convert an absolute path to a `file://` URI. Returns None on
/// non-absolute paths or non-utf8 segments.
fn url_from_path(p: &std::path::Path) -> Option<String> {
    let p = p.to_str()?;
    if p.starts_with('/') {
        Some(format!("file://{p}"))
    } else {
        None
    }
}

/// Set of running LSP clients. Used by the TUI's `/lsps` panel — for
/// now the registry just owns the clients so they stay alive (the LSP
/// children are killed on Drop via `kill_on_drop`) and exposes a
/// formatted summary.
pub struct LspRegistry {
    clients: Vec<LspClient>,
}

impl LspRegistry {
    pub async fn spawn_all<'a, I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (&'a String, &'a LspEntry)>,
    {
        let mut clients = Vec::new();
        for (name, entry) in entries {
            match LspClient::spawn(name, entry).await {
                Ok(c) => clients.push(c),
                Err(e) => eprintln!("warning: LSP `{name}` failed to start: {e:#}"),
            }
        }
        Self { clients }
    }

    pub fn server_count(&self) -> usize {
        self.clients.len()
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
}
