mod highlight;
mod tui;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use telia_agent::Agent;
use telia_llm::{LlmClient, DEFAULT_BASE_URL};
use telia_store::Store;

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
    let llm = LlmClient::new(args.base_url, args.model);
    if !args.no_pull {
        ensure_model(&llm).await?;
    }
    let store = Store::open()?;
    let agent = Agent::new(llm, store)?;

    tui::run(agent).await
}

/// Pre-flight: if Ollama can be reached and reports the model isn't
/// cached, shell out to `ollama pull MODEL` so the user sees Ollama's
/// native download progress before the TUI takes over the screen. Models
/// like the default `hf.co/FoolDev/Thanatos-27B:Q4_K_M` resolve to a
/// HuggingFace pull automatically.
///
/// If `/api/show` is unreachable (non-Ollama backend, or Ollama not
/// running) we silently skip — let the actual chat request surface the
/// real failure later.
async fn ensure_model(llm: &LlmClient) -> Result<()> {
    let model = llm.model().to_string();
    match llm.has_model().await {
        Some(true) | None => Ok(()),
        Some(false) => {
            eprintln!("· model '{model}' not cached locally — pulling via ollama...");
            let status = std::process::Command::new("ollama")
                .args(["pull", &model])
                .status()
                .context("running `ollama pull` (is the ollama CLI installed and on PATH?)")?;
            if status.success() {
                Ok(())
            } else {
                Err(anyhow!(
                    "ollama pull failed for {model} (exit {})",
                    status.code().unwrap_or(-1)
                ))
            }
        }
    }
}
