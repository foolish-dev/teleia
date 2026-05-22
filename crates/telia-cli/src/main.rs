mod config;
mod highlight;
mod tui;

use anyhow::Result;
use clap::Parser;
use futures_util::{pin_mut, StreamExt};
use std::io::{BufRead, IsTerminal, Write};
use telia_agent::Agent;
use telia_llm::{detect_endpoint, looks_like_ollama, LlmClient, PullProgress};
use telia_store::Store;

/// Local Ollama models telia tries to keep cached on startup so `/model`
/// can switch between them without a fresh download. The active `--model`
/// is added to this set automatically if it isn't already in it. Only
/// pulled when the resolved base URL looks like Ollama.
const DEFAULT_OLLAMA_MODELS: &[&str] = &[
    "hf.co/FoolDev/Thanatos-27B:Q4_K_M",
    "hf.co/FoolDev/Janus-35B:Q4_K_M",
];

/// Cloud models surfaced in the `/model` dropdown even though they
/// aren't backed by anything that can be `ollama pull`-ed. Selecting
/// one of these effectively routes telia's chat requests through that
/// provider; the API key comes from `--api-key` or the env-var fallback
/// inside `detect_endpoint`. `provider:model` form is used for entries
/// whose bare name would collide with an Ollama-hosted local model
/// (e.g. `groq:llama-...`).
const KNOWN_CLOUD_MODELS: &[&str] = &[
    // Anthropic
    "claude-opus-4-7",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
    // OpenAI
    "gpt-5",
    "gpt-5-mini",
    "o3",
    "o4-mini",
    // Google
    "gemini-2.5-pro",
    "gemini-2.5-flash",
    // xAI
    "grok-4",
    "grok-3",
    // DeepSeek
    "deepseek-chat",
    "deepseek-reasoner",
    // Mistral
    "mistral-large-latest",
    "codestral-latest",
    // Groq (explicit prefix — model names collide with local Ollama)
    "groq:llama-3.3-70b-versatile",
    "groq:deepseek-r1-distill-llama-70b",
    // OpenRouter (catalog router; explicit prefix required)
    "openrouter:anthropic/claude-opus-4-7",
    "openrouter:openai/gpt-5",
];

#[derive(Parser, Debug)]
#[command(
    name = "τέλεια",
    bin_name = "τέλεια",
    version,
    about = "τέλεια — terminal coding agent. Streams chat, runs read/write/edit/bash, persists sessions to SQLite.",
    long_about = "τέλεια is a small TUI coding agent.\n\
                  \n\
                  It talks to any OpenAI-compatible chat-completions endpoint — local Ollama (default), \
                  Anthropic, OpenAI, Google, xAI, DeepSeek, Mistral, Groq, or OpenRouter — picking the \
                  provider automatically from the model name unless --base-url is overridden. Tools \
                  round-trip through a 16-hop loop: `read`, `write`, `edit`, `bash`, `list`, `glob`, \
                  and `grep`.\n\
                  \n\
                  The TUI offers vim-style Insert / Normal / Command modes, autocomplete with drop-down \
                  menus for slash commands and saved aliases, ghost-text suggestions, readline-style input \
                  history (Up/Down), Tokyo Night / Catppuccin / Dracula themes, a token tracker, and \
                  desktop notifications when a turn completes. Sessions live in SQLite at \
                  $XDG_DATA_HOME/telia/telia.sqlite and can be saved or loaded by alias across runs.\n\
                  \n\
                  When the resolved endpoint looks like Ollama, missing models trigger an interactive \
                  pull prompt with an animated progress bar; pass --pull-yes to auto-confirm or --no-pull \
                  to skip the pre-flight entirely."
)]
struct Args {
    /// Model to chat with. Detection by name: `claude-*` → Anthropic,
    /// `gpt-*` / `o1*` / `o3*` / `o4*` → OpenAI, `gemini-*` → Google,
    /// `grok-*` → xAI, `deepseek-*` → DeepSeek, `mistral-*` /
    /// `codestral-*` → Mistral. Use `groq:NAME` or `openrouter:NAME` for
    /// providers whose model names collide with local Ollama; anything
    /// else routes to local Ollama.
    #[arg(long, default_value = "hf.co/FoolDev/Thanatos-27B:Q4_K_M")]
    model: String,
    /// Override the auto-detected base URL. See --model for the
    /// per-provider defaults; Ollama is http://127.0.0.1:11434/v1.
    #[arg(long)]
    base_url: Option<String>,
    /// API key for cloud backends. Falls back to the env var for the
    /// detected provider — $ANTHROPIC_API_KEY, $OPENAI_API_KEY,
    /// $GEMINI_API_KEY, $XAI_API_KEY, $DEEPSEEK_API_KEY,
    /// $MISTRAL_API_KEY, $GROQ_API_KEY, $OPENROUTER_API_KEY. Run /keys
    /// inside telia to see which are set. Ignored for Ollama.
    #[arg(long)]
    api_key: Option<String>,
    /// Colour theme. Known: tokyo-night (default), catppuccin, dracula.
    #[arg(long, default_value = "tokyo-night")]
    theme: String,
    /// Skip the pre-flight check that pulls missing models via Ollama.
    /// Useful for non-Ollama backends, or when you've already pulled
    /// what you want by hand.
    #[arg(long)]
    no_pull: bool,
    /// Auto-confirm every "pull this model?" prompt during the
    /// pre-flight. Without this flag, telia asks interactively for
    /// each missing model (non-interactive runs default to yes so
    /// scripts don't hang on stdin).
    #[arg(long, alias = "yes", short = 'y')]
    pull_yes: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    if tui::set_theme(&args.theme).is_none() {
        eprintln!(
            "warning: unknown theme '{}'. Known: {}",
            args.theme,
            tui::theme_names().join(", ")
        );
    }

    // Optional user config — register custom LLM endpoints + LSP servers.
    let cfg = config::load();
    if !cfg.lsps.is_empty() {
        eprintln!(
            "note: {} LSP entr{} loaded from config (LSP runtime not yet wired)",
            cfg.lsps.len(),
            if cfg.lsps.len() == 1 { "y" } else { "ies" }
        );
    }

    // Resolve provider:
    //   1. Explicit --base-url / --api-key win.
    //   2. Otherwise, if the model matches a configured [llms.NAME] entry,
    //      take base_url + api_key from there.
    //   3. Otherwise fall back to detect_endpoint's prefix detection
    //      (claude-* → Anthropic, gpt-* → OpenAI, else Ollama).
    let (auto_url, auto_key) = if let Some(entry) = cfg.llms.get(&args.model) {
        (entry.base_url.clone(), entry.resolve_key())
    } else {
        detect_endpoint(&args.model)
    };
    let base_url = args.base_url.unwrap_or(auto_url);
    let api_key = args.api_key.or(auto_key);

    if !args.no_pull && looks_like_ollama(&base_url) {
        // Walk every default Ollama model plus the active --model
        // (deduped). For each missing one, ask the user before pulling —
        // unless --pull-yes / -y is set or stdin isn't a TTY (scripts).
        let mut want: Vec<String> = DEFAULT_OLLAMA_MODELS
            .iter()
            .map(|s| s.to_string())
            .collect();
        if !want.iter().any(|m| m == &args.model) {
            want.push(args.model.clone());
        }
        for model in &want {
            let pre = LlmClient::new(base_url.clone(), model.clone());
            ensure_model(&pre, args.pull_yes).await?;
        }
    }

    let llm = LlmClient::with_api_key(base_url, args.model, api_key);
    let store = Store::open()?;
    let mut agent = Agent::new(llm, store)?;
    // Best-effort: cache the installed-model list once so the /model
    // dropdown has something to show without hitting the network on
    // every keypress.
    agent.refresh_models().await;
    agent.extend_models(KNOWN_CLOUD_MODELS.iter().copied());
    // Custom LLM names from config become first-class /model targets too.
    agent.extend_models(cfg.llms.keys().cloned());

    tui::run(agent).await
}

/// Pre-flight: if Ollama can be reached and reports the model isn't
/// cached, prompt the user (or auto-confirm when `pull_yes` is set or
/// stdin isn't a TTY) and on a "yes" stream `/api/pull` with an
/// animated progress bar. Models like the default
/// `hf.co/FoolDev/Thanatos-27B:Q4_K_M` resolve to a HuggingFace pull
/// automatically via Ollama's bridge.
///
/// If `/api/show` is unreachable (non-Ollama backend, or Ollama not
/// running) we silently skip — let the actual chat request surface the
/// real failure later.
async fn ensure_model(llm: &LlmClient, pull_yes: bool) -> Result<()> {
    let model = llm.model().to_string();
    match llm.has_model().await {
        Some(true) | None => return Ok(()),
        Some(false) => {}
    }

    if !pull_yes && !confirm_pull(&model) {
        eprintln!("· skipped {model} (not cached locally)");
        return Ok(());
    }

    eprintln!("↓ pulling {model}");
    let stream = llm.pull_model();
    pin_mut!(stream);

    const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut tick: usize = 0;

    while let Some(event) = stream.next().await {
        let prog = event?;
        if prog.status == "success" {
            break;
        }
        render_pull_line(&prog, SPINNER[tick % SPINNER.len()]);
        tick = tick.wrapping_add(1);
    }
    eprintln!("\r  ✓ ready{:>40}", " ");
    Ok(())
}

fn render_pull_line(prog: &PullProgress, spinner: &str) {
    const BAR_WIDTH: usize = 24;
    let digest_short = prog
        .digest
        .as_deref()
        .map(|d| {
            let trimmed = d.trim_start_matches("sha256:");
            &trimmed[..12.min(trimmed.len())]
        })
        .unwrap_or("");

    let line = match (prog.completed, prog.total) {
        (Some(done), Some(total)) if total > 0 => {
            let pct = (done * 100 / total) as usize;
            let filled = pct * BAR_WIDTH / 100;
            let bar = format!("[{}{}]", "█".repeat(filled), "░".repeat(BAR_WIDTH - filled));
            format!(
                "\r  {spinner} {status} {digest_short} {bar} {pct:>3}% ({done}/{total})  ",
                status = prog.status,
                done = human_bytes(done),
                total = human_bytes(total),
            )
        }
        _ => format!("\r  {spinner} {} {digest_short}{:>40}", prog.status, " "),
    };
    let _ = std::io::stderr().write_all(line.as_bytes());
    let _ = std::io::stderr().flush();
}

/// Ask the user whether to pull a missing model. Non-TTY stdin (scripts,
/// pipes, CI) defaults to yes so unattended runs still get the model.
/// Defaults to yes on an empty line; anything starting with 'n' is a no.
fn confirm_pull(model: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return true;
    }
    eprint!("· pull {model} from Ollama now? [Y/n] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return true;
    }
    let trimmed = line.trim().to_lowercase();
    !trimmed.starts_with('n')
}

fn human_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;
    if n >= GB {
        format!("{:.1}GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1}MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1}KB", n as f64 / KB as f64)
    } else {
        format!("{n}B")
    }
}
