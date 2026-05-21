mod highlight;
mod tui;

use anyhow::Result;
use clap::Parser;
use futures_util::{pin_mut, StreamExt};
use std::io::Write;
use telia_agent::Agent;
use telia_llm::{LlmClient, PullProgress, DEFAULT_BASE_URL};
use telia_store::Store;

/// Models telia tries to keep cached on startup so `/model` can switch
/// between them without a fresh download. The active `--model` is added
/// to this set automatically if it isn't already in it.
const DEFAULT_MODELS: &[&str] = &[
    "hf.co/FoolDev/Thanatos-27B:Q4_K_M",
    "hf.co/FoolDev/Janus-35B:Q4_K_M",
];

#[derive(Parser, Debug)]
#[command(name = "telia", version, about = "Minimal TUI coding agent")]
struct Args {
    #[arg(long, default_value = "hf.co/FoolDev/Thanatos-27B:Q4_K_M")]
    model: String,
    #[arg(long, default_value = DEFAULT_BASE_URL)]
    base_url: String,
    /// Colour theme. Known: tokyo-night (default), catppuccin, dracula.
    #[arg(long, default_value = "tokyo-night")]
    theme: String,
    /// Skip the pre-flight check that auto-pulls the model via Ollama when
    /// it isn't already cached locally. Useful for non-Ollama backends.
    #[arg(long)]
    no_pull: bool,
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
    if !args.no_pull {
        // Pull every default model plus the active --model (deduped) so
        // /model can switch between them without a fresh download.
        let mut want: Vec<String> = DEFAULT_MODELS.iter().map(|s| s.to_string()).collect();
        if !want.iter().any(|m| m == &args.model) {
            want.push(args.model.clone());
        }
        for model in &want {
            let pre = LlmClient::new(args.base_url.clone(), model.clone());
            ensure_model(&pre).await?;
        }
    }

    let llm = LlmClient::new(args.base_url, args.model);
    let store = Store::open()?;
    let mut agent = Agent::new(llm, store)?;
    // Best-effort: cache the installed-model list once so the /model
    // dropdown has something to show without hitting the network on
    // every keypress.
    agent.refresh_models().await;

    tui::run(agent).await
}

/// Pre-flight: if Ollama can be reached and reports the model isn't
/// cached, stream `/api/pull` and render an animated progress bar before
/// the TUI takes over the screen. Models like the default
/// `hf.co/FoolDev/Thanatos-27B:Q4_K_M` resolve to a HuggingFace pull
/// automatically via Ollama's bridge.
///
/// If `/api/show` is unreachable (non-Ollama backend, or Ollama not
/// running) we silently skip — let the actual chat request surface the
/// real failure later.
async fn ensure_model(llm: &LlmClient) -> Result<()> {
    let model = llm.model().to_string();
    match llm.has_model().await {
        Some(true) | None => return Ok(()),
        Some(false) => {}
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
