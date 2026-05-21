mod highlight;
mod tui;

use anyhow::Result;
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
    let store = Store::open()?;
    let agent = Agent::new(llm, store)?;

    tui::run(agent).await
}
