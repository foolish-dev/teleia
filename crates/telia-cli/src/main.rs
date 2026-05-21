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
/// inside `detect_endpoint`.
const KNOWN_CLOUD_MODELS: &[&str] = &[
    "claude-opus-4-7",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
];

#[derive(Parser, Debug)]
#[command(name = "telia", version, about = "Minimal TUI coding agent")]
struct Args {
    /// Model to chat with. Detection by name: `claude-*` → Anthropic,
    /// `gpt-*`/`o1*`/`o3*` → OpenAI, everything else → local Ollama.
    /// The choice of base-url and API key follow this detection unless
    /// overridden.
    #[arg(long, default_value = "hf.co/FoolDev/Thanatos-27B:Q4_K_M")]
    model: String,
    /// Override the auto-detected base URL. By default `claude-*` models
    /// route to https://api.anthropic.com/v1, `gpt-*` to OpenAI, and
    /// everything else to local Ollama at http://127.0.0.1:11434/v1.
    #[arg(long)]
    base_url: Option<String>,
    /// API key for cloud backends. Falls back to $ANTHROPIC_API_KEY /
    /// $OPENAI_API_KEY based on the active model. Ignored for Ollama.
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

    // Resolve provider — explicit CLI overrides win, otherwise auto-detect
    // from the model name (Anthropic / OpenAI / Ollama).
    let (auto_url, auto_key) = detect_endpoint(&args.model);
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
