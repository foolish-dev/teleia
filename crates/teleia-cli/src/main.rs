mod tui;

use anyhow::Result;
use clap::Parser;
use teleia_agent::Agent;
use teleia_llm::{LlmClient, DEFAULT_BASE_URL};
use teleia_store::Store;

#[derive(Parser, Debug)]
#[command(name = "teleia", about = "Minimal TUI coding agent (Rust)")]
struct Args {
    #[arg(long, default_value = "hf.co/FoolDev/Thanatos-27B:Q4_K_M")]
    model: String,
    #[arg(long, default_value = DEFAULT_BASE_URL)]
    base_url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let llm = LlmClient::new(args.base_url, args.model);
    let store = Store::open()?;
    let agent = Agent::new(llm, store)?;

    tui::run(agent).await
}
