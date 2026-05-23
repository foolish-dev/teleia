// Optional user-config file at $XDG_CONFIG_HOME/teleia/config.toml
// (falls back to ~/.config/teleia/config.toml). Schema:
//
//   # Register a custom LLM under the name "groq-llama".
//   # When --model groq-llama is passed (or /model groq-llama is selected),
//   # teleia will use the configured base_url and pick up the API key from
//   # `api_key_env` (env var name) or `api_key` (inline string).
//   [llms.groq-llama]
//   base_url    = "https://api.groq.com/openai/v1"
//   api_key_env = "GROQ_API_KEY"
//
//   # Register an LSP server. Parsed but not yet wired into the tool
//   # dispatch loop — placeholder for the eventual LSP feature.
//   [lsps.rust]
//   command       = "rust-analyzer"
//   args          = []
//   root_patterns = ["Cargo.toml"]

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub llms: BTreeMap<String, LlmEntry>,
    #[serde(default)]
    pub lsps: BTreeMap<String, LspEntry>,
    #[serde(default)]
    pub mcps: BTreeMap<String, McpEntry>,
}

/// MCP (Model Context Protocol) server. teleia spawns the command on
/// startup, exchanges an `initialize` handshake, lists the server's
/// tools, and merges them into the agent's tool catalogue. `env`
/// values override the parent process for the spawned child only.
#[derive(Debug, Clone, Deserialize)]
pub struct McpEntry {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmEntry {
    pub base_url: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

impl LlmEntry {
    /// Resolve the API key: prefer an inline `api_key`, otherwise look up
    /// `api_key_env` in the process env. Returns `None` if neither yields
    /// a non-empty string.
    pub fn resolve_key(&self) -> Option<String> {
        if let Some(k) = self.api_key.as_ref().filter(|k| !k.is_empty()) {
            return Some(k.clone());
        }
        let var = self.api_key_env.as_ref()?;
        std::env::var(var).ok().filter(|v| !v.is_empty())
    }
}

// Fields are read at deserialise time only — the LSP runtime hasn't
// been wired yet, so rustc rightly notices nothing in the rest of the
// codebase touches `command`/`args`/`root_patterns`. Suppress the
// warning until the LSP feature lands.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct LspEntry {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub root_patterns: Vec<String>,
}

/// Read the config file if it exists. Missing file → empty config; parse
/// failure → empty config + a warning on stderr (the TUI is about to take
/// the screen, so we'd rather warn loudly than crash).
pub fn load() -> Config {
    let path = config_path();
    if !path.exists() {
        return Config::default();
    }
    let body = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("warning: reading {}: {e}", path.display());
            return Config::default();
        }
    };
    match toml::from_str::<Config>(&body) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: parsing {}: {e}", path.display());
            Config::default()
        }
    }
}

/// Where the optional `config.toml` lives. Same precedence as the
/// data path: `$XDG_CONFIG_HOME` first, then OS-native:
/// - Linux  : `$HOME/.config/teleia/config.toml`
/// - macOS  : `$HOME/Library/Application Support/teleia/config.toml`
/// - Windows: `%APPDATA%\teleia\config.toml`
pub fn config_path() -> PathBuf {
    if let Some(v) = std::env::var_os("XDG_CONFIG_HOME") {
        if !v.is_empty() {
            return PathBuf::from(v).join("teleia").join("config.toml");
        }
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").unwrap_or_default();
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("teleia")
            .join("config.toml")
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA").unwrap_or_default();
        PathBuf::from(appdata).join("teleia").join("config.toml")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let home = std::env::var_os("HOME").unwrap_or_default();
        PathBuf::from(home)
            .join(".config")
            .join("teleia")
            .join("config.toml")
    }
}
