// Optional user-config file at $XDG_CONFIG_HOME/telia/config.toml
// (falls back to ~/.config/telia/config.toml). Schema:
//
//   # Register a custom LLM under the name "groq-llama".
//   # When --model groq-llama is passed (or /model groq-llama is selected),
//   # telia will use the configured base_url and pick up the API key from
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

/// `$XDG_CONFIG_HOME/telia/config.toml`, else `$HOME/.config/telia/config.toml`.
pub fn config_path() -> PathBuf {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(".config")
        }
    };
    base.join("telia").join("config.toml")
}
