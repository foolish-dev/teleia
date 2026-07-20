mod boot;
mod config;
mod highlight;
mod lsp;
mod mcp;
mod router;
mod tui;
mod update;

use anyhow::Result;
use clap::Parser;
use futures_util::{pin_mut, StreamExt};
use std::io::{BufRead, IsTerminal, Write};
use teleia_agent::Agent;
use teleia_llm::{detect_endpoint, looks_like_ollama, LlmClient, PullProgress};
use teleia_store::Store;

/// Local Ollama models teleia tries to keep cached on startup so `/model`
/// can switch between them without a fresh download. The active `--model`
/// is added to this set automatically if it isn't already in it. Only
/// pulled when the resolved base URL looks like Ollama.
/// Active model on first launch — also used to detect "user didn't
/// supply --model" so a saved `current_model` pref can take over.
const DEFAULT_MODEL: &str = "hf.co/FoolDev/Thanatos-27B:Q4_K_M";

const DEFAULT_OLLAMA_MODELS: &[&str] = &[
    "hf.co/FoolDev/Thanatos-27B:Q4_K_M",
    "hf.co/FoolDev/Janus-35B:Q4_K_M",
];

/// Cloud models surfaced in the `/model` dropdown even though they
/// aren't backed by anything that can be `ollama pull`-ed. Selecting
/// one of these effectively routes teleia's chat requests through that
/// provider; the API key comes from `--api-key` or the env-var fallback
/// inside `detect_endpoint`. `provider:model` form is used for entries
/// whose bare name would collide with an Ollama-hosted local model
/// (e.g. `groq:llama-...`).
///
/// Best-effort snapshot of the major providers' public chat-completions
/// catalogs as of early 2026. Names that 404 at request time are easy
/// to spot in the error log and easy to override via the config file
/// (see `crates/teleia-cli/src/config.rs`).
#[rustfmt::skip]
const KNOWN_CLOUD_MODELS: &[&str] = &[
    // ---------- Anthropic ----------
    "claude-fable-5",
    "claude-opus-4-7",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
    "claude-opus-4-1",
    "claude-sonnet-4-5",
    "claude-haiku-4-5",
    "claude-3-7-sonnet-latest",
    "claude-3-5-sonnet-20241022",
    "claude-3-5-sonnet-20240620",
    "claude-3-5-haiku-20241022",
    "claude-3-5-haiku-latest",
    "claude-3-opus-20240229",
    "claude-3-opus-latest",
    "claude-3-sonnet-20240229",
    "claude-3-haiku-20240307",

    // ---------- OpenAI ----------
    "gpt-5",
    "gpt-5-mini",
    "gpt-5-nano",
    "gpt-4.1",
    "gpt-4.1-mini",
    "gpt-4.1-nano",
    "gpt-4o",
    "gpt-4o-mini",
    "o4-mini",
    "o3",
    "o3-mini",
    "o3-pro",
    "o1",
    "o1-mini",
    "o1-pro",
    "o1-preview",
    "chatgpt-4o-latest",
    "gpt-4-turbo",
    "gpt-4-turbo-preview",
    "gpt-4",
    "gpt-4-32k",
    "gpt-3.5-turbo",
    "gpt-3.5-turbo-16k",

    // ---------- Google ----------
    "gemini-2.5-pro",
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
    "gemini-2.0-flash",
    "gemini-2.0-flash-lite",
    "gemini-1.5-pro",
    "gemini-1.5-flash",
    "gemini-1.5-flash-8b",
    "gemini-exp-1206",
    "gemini-pro",
    "gemini-pro-vision",

    // ---------- xAI ----------
    "grok-4",
    "grok-4-fast",
    "grok-3",
    "grok-3-mini",
    "grok-3-fast",
    "grok-2",
    "grok-2-vision",

    // ---------- DeepSeek ----------
    "deepseek-chat",
    "deepseek-reasoner",

    // ---------- Mistral ----------
    "mistral-large-latest",
    "mistral-medium-latest",
    "mistral-small-latest",
    "ministral-8b-latest",
    "ministral-3b-latest",
    "codestral-latest",
    "pixtral-large-latest",
    "open-mistral-nemo",
    "mistral-saba-2502",
    "ministral-3b-2410",
    "ministral-8b-2410",
    "open-mixtral-8x7b",
    "open-mixtral-8x22b",

    // ---------- Groq (explicit prefix — names collide with local Ollama) ----------
    "groq:llama-3.3-70b-versatile",
    "groq:llama-3.1-8b-instant",
    "groq:meta-llama/llama-4-scout-17b-16e-instruct",
    "groq:meta-llama/llama-4-maverick-17b-128e-instruct",
    "groq:deepseek-r1-distill-llama-70b",
    "groq:qwen-qwq-32b",
    "groq:qwen-2.5-coder-32b",
    "groq:qwen-2.5-32b",
    "groq:mixtral-8x7b-32768",
    "groq:gemma2-9b-it",
    "groq:llama3-70b-8192",
    "groq:llama3-8b-8192",
    "groq:llama-3.2-1b-preview",
    "groq:llama-3.2-3b-preview",
    "groq:llama-3.2-11b-vision-preview",
    "groq:llama-3.2-90b-vision-preview",

    // ---------- OpenRouter (popular routes; the full catalog is in the thousands) ----------
    "openrouter:anthropic/claude-opus-4-7",
    "openrouter:anthropic/claude-sonnet-4-6",
    "openrouter:anthropic/claude-haiku-4-5",
    "openrouter:openai/gpt-5",
    "openrouter:openai/gpt-5-mini",
    "openrouter:openai/o3",
    "openrouter:openai/o4-mini",
    "openrouter:google/gemini-2.5-pro",
    "openrouter:google/gemini-2.5-flash",
    "openrouter:x-ai/grok-4",
    "openrouter:x-ai/grok-3",
    "openrouter:deepseek/deepseek-chat",
    "openrouter:deepseek/deepseek-r1",
    "openrouter:meta-llama/llama-3.3-70b-instruct",
    "openrouter:meta-llama/llama-4-scout",
    "openrouter:meta-llama/llama-4-maverick",
    "openrouter:qwen/qwen-2.5-coder-32b-instruct",
    "openrouter:qwen/qwen-2.5-72b-instruct",
    "openrouter:mistralai/mistral-large-2411",
    "openrouter:mistralai/codestral-2501",
    "openrouter:nvidia/llama-3.1-nemotron-70b-instruct",
    "openrouter:cohere/command-r-plus",
    "openrouter:perplexity/sonar-pro",
    "openrouter:nousresearch/hermes-3-llama-3.1-405b",
    "openrouter:microsoft/wizardlm-2-8x22b",
    "openrouter:google/gemma-2-27b-it",
    "openrouter:01-ai/yi-large",
    "openrouter:databricks/dbrx-instruct",
    "openrouter:anthropic/claude-3-5-sonnet",
    "openrouter:anthropic/claude-3-opus",
    "openrouter:openai/gpt-4o",
    "openrouter:openai/o1",
    "openrouter:meta-llama/llama-3.1-405b-instruct",
    "openrouter:meta-llama/llama-3.1-70b-instruct",

    // ---------- Cohere ----------
    "command-a-03-2025",
    "command-r-plus",
    "command-r",
    "command-r-08-2024",
    "command-r7b-12-2024",

    // ---------- Perplexity (web-search-augmented) ----------
    "sonar",
    "sonar-pro",
    "sonar-reasoning",
    "sonar-reasoning-pro",
    "sonar-deep-research",

    // ---------- Together AI (explicit prefix; hosts open models) ----------
    "together:meta-llama/Llama-3.3-70B-Instruct-Turbo",
    "together:meta-llama/Meta-Llama-3.1-405B-Instruct-Turbo",
    "together:meta-llama/Llama-4-Maverick-17B-128E-Instruct-FP8",
    "together:meta-llama/Llama-4-Scout-17B-16E-Instruct",
    "together:Qwen/Qwen2.5-Coder-32B-Instruct",
    "together:Qwen/Qwen2.5-72B-Instruct-Turbo",
    "together:deepseek-ai/DeepSeek-V3",
    "together:deepseek-ai/DeepSeek-R1",
    "together:mistralai/Mixtral-8x22B-Instruct-v0.1",
    "together:mistralai/Mistral-7B-Instruct-v0.3",
    "together:google/gemma-2-27b-it",
    "together:google/gemma-2-9b-it",
    "together:Qwen/Qwen2.5-7B-Instruct-Turbo",
    "together:NousResearch/Hermes-3-Llama-3.1-405B",
    "together:databricks/dbrx-instruct",

    // ---------- Fireworks AI ----------
    "fireworks:accounts/fireworks/models/llama-v3p3-70b-instruct",
    "fireworks:accounts/fireworks/models/llama4-maverick-instruct-basic",
    "fireworks:accounts/fireworks/models/llama4-scout-instruct-basic",
    "fireworks:accounts/fireworks/models/qwen2p5-coder-32b-instruct",
    "fireworks:accounts/fireworks/models/qwen2p5-72b-instruct",
    "fireworks:accounts/fireworks/models/deepseek-v3",
    "fireworks:accounts/fireworks/models/deepseek-r1",
    "fireworks:accounts/fireworks/models/llama-v3p1-8b-instruct",
    "fireworks:accounts/fireworks/models/mixtral-8x22b-instruct",
    "fireworks:accounts/fireworks/models/firefunction-v2",
    "fireworks:accounts/fireworks/models/mythomax-l2-13b",

    // ---------- Cerebras (extremely fast inference) ----------
    "cerebras:llama-3.3-70b",
    "cerebras:llama3.1-8b",
    "cerebras:llama-4-scout-17b-16e-instruct",
    "cerebras:llama-4-maverick-17b-128e-instruct",
    "cerebras:qwen-3-32b",
    "cerebras:qwen-3-235b-a22b-instruct-2507",
    "cerebras:deepseek-r1-distill-llama-70b",

    // ---------- Hyperbolic ----------
    "hyperbolic:meta-llama/Meta-Llama-3.1-405B-Instruct",
    "hyperbolic:meta-llama/Llama-3.3-70B-Instruct",
    "hyperbolic:Qwen/Qwen2.5-Coder-32B-Instruct",
    "hyperbolic:deepseek-ai/DeepSeek-V3",
    "hyperbolic:deepseek-ai/DeepSeek-R1",
    "hyperbolic:Qwen/Qwen2.5-72B-Instruct",
    "hyperbolic:meta-llama/Meta-Llama-3.1-70B-Instruct",
    "hyperbolic:NousResearch/Hermes-3-Llama-3.1-70B",

    // ---------- NVIDIA NIM ----------
    "nvidia:meta/llama-3.3-70b-instruct",
    "nvidia:meta/llama-4-maverick-17b-128e-instruct",
    "nvidia:meta/llama-4-scout-17b-16e-instruct",
    "nvidia:nvidia/llama-3.1-nemotron-70b-instruct",
    "nvidia:deepseek-ai/deepseek-r1",
    "nvidia:qwen/qwen2.5-coder-32b-instruct",
    "nvidia:mistralai/mistral-large-2-instruct",
    "nvidia:google/gemma-2-27b-it",
    "nvidia:microsoft/phi-3-medium-4k-instruct",
    "nvidia:nv-mistralai/mistral-nemo-12b-instruct",

    // ---------- AI21 Labs (Jamba family) ----------
    "jamba-1.5-large",
    "jamba-1.5-mini",
    "jamba-large-1.6",
    "jamba-mini-1.6",

    // ---------- Anyscale Endpoints ----------
    "anyscale:meta-llama/Meta-Llama-3-70B-Instruct",
    "anyscale:meta-llama/Meta-Llama-3-8B-Instruct",
    "anyscale:mistralai/Mixtral-8x22B-Instruct-v0.1",
    "anyscale:codellama/CodeLlama-70b-Instruct-hf",

    // ---------- Lepton AI ----------
    "lepton:llama3-1-405b",
    "lepton:llama3-1-70b",
    "lepton:llama3-1-8b",
    "lepton:mixtral-8x7b",
    "lepton:qwen2-72b",

    // ---------- DeepInfra ----------
    "deepinfra:meta-llama/Meta-Llama-3.1-405B-Instruct",
    "deepinfra:meta-llama/Meta-Llama-3.1-70B-Instruct",
    "deepinfra:Qwen/Qwen2.5-72B-Instruct",
    "deepinfra:mistralai/Mixtral-8x22B-Instruct-v0.1",
    "deepinfra:deepseek-ai/DeepSeek-V3",
    "deepinfra:nvidia/Nemotron-4-340B-Instruct",

    // ---------- SambaNova Cloud (fast Llama inference) ----------
    "sambanova:Meta-Llama-3.3-70B-Instruct",
    "sambanova:Meta-Llama-3.1-405B-Instruct",
    "sambanova:Meta-Llama-3.1-70B-Instruct",
    "sambanova:Meta-Llama-3.1-8B-Instruct",
    "sambanova:Llama-3.2-11B-Vision-Instruct",
    "sambanova:Llama-3.2-90B-Vision-Instruct",
    "sambanova:Qwen2.5-72B-Instruct",
    "sambanova:Qwen2.5-Coder-32B-Instruct",
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
                  round-trip in a loop until the model stops requesting them: `read`, `write`, \
                  `edit`, `bash`, `list`, `glob`, and `grep`.\n\
                  \n\
                  The TUI offers vim-style Insert / Normal / Visual / Command modes, autocomplete with drop-down \
                  menus for slash commands and saved aliases, ghost-text suggestions, readline-style input \
                  history (Up/Down), Tokyo Night / Catppuccin / Dracula themes, a token tracker, and \
                  desktop notifications when a turn completes. Sessions live in SQLite at \
                  $XDG_DATA_HOME/teleia/teleia.sqlite and can be saved or loaded by alias across runs.\n\
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
    #[arg(long, default_value = DEFAULT_MODEL)]
    model: String,
    /// Override the auto-detected base URL. See --model for the
    /// per-provider defaults; Ollama is http://127.0.0.1:11434/v1.
    #[arg(long)]
    base_url: Option<String>,
    /// API key for cloud backends. Falls back to the env var for the
    /// detected provider — $ANTHROPIC_API_KEY, $OPENAI_API_KEY,
    /// $GEMINI_API_KEY, $XAI_API_KEY, $DEEPSEEK_API_KEY,
    /// $MISTRAL_API_KEY, $COHERE_API_KEY, $PERPLEXITY_API_KEY,
    /// $GROQ_API_KEY, $OPENROUTER_API_KEY, $TOGETHER_API_KEY,
    /// $FIREWORKS_API_KEY, $CEREBRAS_API_KEY, $HYPERBOLIC_API_KEY,
    /// $NVIDIA_API_KEY, $AI21_API_KEY, $ANYSCALE_API_KEY,
    /// $LEPTON_API_KEY, $DEEPINFRA_API_KEY, $SAMBANOVA_API_KEY. Run
    /// /keys inside teleia to see which are set. Ignored for Ollama.
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
    /// pre-flight. Without this flag, teleia asks interactively for
    /// each missing model (non-interactive runs default to yes so
    /// scripts don't hang on stdin).
    #[arg(long, alias = "yes", short = 'y')]
    pull_yes: bool,
    /// Start in auto mode — every tool call dispatches without a y/n/a
    /// prompt. Equivalent to typing `/auto` after launch. Use with care:
    /// the agent can run arbitrary `bash`, `write`, and `edit` without
    /// asking. Toggle off mid-session with `/build` (or Shift+Tab).
    #[arg(long, conflicts_with = "plan")]
    auto: bool,
    /// Start in plan mode — `read` / `list` / `glob` / `grep` run, but
    /// `write` / `edit` / `bash` are blocked. The model is pushed toward
    /// describing what it would do rather than executing. Toggle off
    /// mid-session with `/build` (or Shift+Tab).
    #[arg(long, conflicts_with = "auto")]
    plan: bool,
    /// Resume the most recent session instead of starting a fresh one.
    /// Every session is auto-bookmarked as `last` at startup; `/reset`
    /// rotates the bookmark to `prev` before opening a new session, so
    /// the previous-but-one is also recoverable via `/load prev`.
    #[arg(long, alias = "continue", short = 'r')]
    resume: bool,
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

    // Optional user config — register custom LLM endpoints + MCP / LSP servers.
    let cfg = config::load();

    // Open the store early so saved prefs (current model, API keys) can
    // feed into the rest of startup before LlmClient is built.
    let store = Store::open()?;

    // Restore previously-active model when the user didn't pass --model
    // explicitly. `args.model == DEFAULT_MODEL` is the heuristic for
    // "no flag supplied" since clap derive doesn't surface value-source
    // cleanly.
    let model = if args.model == DEFAULT_MODEL {
        store
            .get_pref("current_model")
            .ok()
            .flatten()
            .unwrap_or_else(|| args.model.clone())
    } else {
        args.model.clone()
    };

    // Resolve provider:
    //   1. Explicit --base-url / --api-key win.
    //   2. Otherwise, if the model matches a configured [llms.NAME] entry,
    //      take base_url + api_key from there.
    //   3. Otherwise fall back to detect_endpoint's prefix detection.
    //   4. As a last fallback, restore a key saved to prefs from a
    //      previous run (via the startup or in-TUI key prompt).
    let (auto_url, auto_key) = if let Some(entry) = cfg.llms.get(&model) {
        (entry.base_url.clone(), entry.resolve_key())
    } else {
        detect_endpoint(&model)
    };
    let base_url = args.base_url.unwrap_or(auto_url);
    let stored_key = teleia_llm::provider_for_model(&model).and_then(|p| {
        store
            .get_pref(&pref_key_for(p.env_var))
            .ok()
            .flatten()
            .filter(|v| !v.is_empty())
    });
    let mut api_key = args.api_key.or(auto_key).or(stored_key);

    // If the resolved provider is a cloud one and we still don't have a
    // key, offer to read one from the terminal now. Skipped when stdin
    // isn't a TTY (scripts, pipes, CI) so unattended runs keep working
    // with whatever the env said. On success, persist for next launch.
    if api_key.is_none() {
        if let Some(prov) = teleia_llm::provider_for_model(&model) {
            if let Some(key) = prompt_api_key(prov) {
                let _ = store.set_pref(&pref_key_for(prov.env_var), &key);
                api_key = Some(key);
            }
        }
    }

    if !args.no_pull && looks_like_ollama(&base_url) {
        // Walk every default Ollama model plus the active model (deduped),
        // filter out anything the user previously declined (pref persists),
        // and ask once for the rest. --pull-yes / -y bypasses the decline
        // filter so users can opt back in without editing the pref store.
        // Non-TTY stdin auto-confirms.
        let mut want: Vec<String> = DEFAULT_OLLAMA_MODELS
            .iter()
            .map(|s| s.to_string())
            .collect();
        if !want.iter().any(|m| m == &model) {
            want.push(model.clone());
        }
        let mut missing: Vec<String> = Vec::new();
        for m in &want {
            if !args.pull_yes
                && store
                    .get_pref(&format!("pull_declined:{m}"))
                    .ok()
                    .flatten()
                    .is_some()
            {
                continue;
            }
            let pre = LlmClient::new(base_url.clone(), m.clone());
            if let Some(false) = pre.has_model().await {
                missing.push(m.clone());
            }
        }
        if !missing.is_empty() {
            if args.pull_yes || confirm_pull_many(&missing) {
                for m in &missing {
                    let pre = LlmClient::new(base_url.clone(), m.clone());
                    pull_with_progress(&pre).await?;
                }
            } else {
                eprintln!("· skipped {} model(s) (not cached locally)", missing.len());
                // Persist so the next launch doesn't re-prompt for the same set.
                for m in &missing {
                    let _ = store.set_pref(&format!("pull_declined:{m}"), "true");
                }
            }
        }
    }

    // Record the active model so the next launch picks it up unless
    // overridden by --model.
    let _ = store.set_pref("current_model", &model);

    let llm = LlmClient::with_api_key(base_url, model, api_key);
    // The TUI status bar reports resumed-session info on first paint;
    // no need to announce it here on stderr.
    let mut agent = if args.resume {
        Agent::resume(llm, store)?
    } else {
        Agent::new(llm, store)?
    };
    // Restore sticky preferences from prior runs before applying CLI
    // overrides. First-launch users see the existing defaults because
    // get_pref returns None.
    if let Some(mode) = agent.get_pref("permission_mode") {
        match mode.as_str() {
            "Plan" => agent.set_permission_mode(teleia_agent::PermissionMode::Plan),
            "Build" => agent.set_permission_mode(teleia_agent::PermissionMode::Build),
            "Auto" => agent.set_permission_mode(teleia_agent::PermissionMode::Auto),
            _ => {}
        }
    }
    if let Some(effort) = agent.get_pref("reasoning_effort") {
        if teleia_agent::is_reasoning_effort(&effort) {
            agent.set_reasoning_effort(Some(effort));
        }
    }
    if args.theme == "tokyo-night" {
        apply_theme_from_agent(&agent);
    }
    // --auto / --plan flag → permission mode. The TUI status bar
    // already surfaces the active mode, so no boot-line announcement
    // is needed here.
    if args.auto {
        agent.set_permission_mode(teleia_agent::PermissionMode::Auto);
    } else if args.plan {
        agent.set_permission_mode(teleia_agent::PermissionMode::Plan);
    }

    // Boot splash: single-line stderr indicator that updates as each
    // phase starts. Created here — after the interactive section
    // (API-key prompt, Ollama pull) — so the splash line never gets
    // stranded by multi-line interactive output. Wiped before
    // `tui::run` takes over the screen.
    let mut boot = boot::Boot::new();

    // LSP servers: initialize handshake + expose `lsp_diagnostics`
    // tool. The registry is folded into the agent's tool router below
    // (alongside MCP) so the children stay alive for the whole run.
    let lsp_registry: Option<lsp::LspRegistry> = if cfg.lsps.is_empty() {
        None
    } else {
        let reg = lsp::LspRegistry::spawn_all(cfg.lsps.iter(), |name, i, n| {
            boot.step(&format!("starting LSP server {i}/{n}: {name}"));
        })
        .await;
        Some(reg)
    };

    // Best-effort: cache the installed-model list once so the /model
    // dropdown has something to show without hitting the network on
    // every keypress.
    boot.step("refreshing models");
    agent.refresh_models().await;
    // Surface the two FoolDev HF GGUFs in the dropdown even when Ollama
    // hasn't pulled them yet — they're teleia's headline local picks.
    agent.extend_models(DEFAULT_OLLAMA_MODELS.iter().copied());
    agent.extend_models(KNOWN_CLOUD_MODELS.iter().copied());
    // Custom LLM names from config become first-class /model targets too.
    agent.extend_models(cfg.llms.keys().cloned());

    // Boot MCP servers (if any). Each surfaces its tool catalogue,
    // which is merged into the agent's tool list. Loading is silent at
    // the terminal level: spawn failures and tool-name collisions are
    // captured in `registry.warnings()` and folded into the `/mcps`
    // panel below. The registry itself is held aside and combined with
    // the LSP registry into a single tool router further down.
    let mut mcp_registry: Option<mcp::McpRegistry> = None;
    let mut mcp_summary: Option<String> = None;
    if !cfg.mcps.is_empty() {
        let registry = mcp::McpRegistry::spawn_all(cfg.mcps.iter(), |name, i, n| {
            boot.step(&format!("starting MCP server {i}/{n}: {name}"));
        })
        .await;
        let tools = registry.tool_count();
        let servers = registry.server_count();
        let warnings: Vec<String> = registry.warnings().to_vec();
        if servers > 0 {
            let resources = registry.resource_count();
            // Pre-format the /mcps panel: one row per server with its
            // command line plus tool + resource counts.
            let mut text =
                format!("{servers} MCP server(s), {tools} tool(s), {resources} resource(s):");
            let summaries: std::collections::HashMap<String, (usize, usize)> = registry
                .server_summaries()
                .into_iter()
                .map(|(n, t, r)| (n, (t, r)))
                .collect();
            for (name, entry) in &cfg.mcps {
                let cmd = if entry.args.is_empty() {
                    entry.command.clone()
                } else {
                    format!("{} {}", entry.command, entry.args.join(" "))
                };
                let (t, r) = summaries.get(name).copied().unwrap_or((0, 0));
                text.push_str(&format!(
                    "\n  {name}  ·  {cmd}  ·  {t} tool(s), {r} resource(s)"
                ));
            }
            for w in &warnings {
                text.push_str(&format!("\n  ⚠ {w}"));
            }
            mcp_summary = Some(text);
            mcp_registry = Some(registry);
        } else if !warnings.is_empty() {
            // No servers came up at all — keep the warnings visible in
            // /mcps instead of dropping them on the floor.
            let mut text = String::from("no MCP servers connected:");
            for w in &warnings {
                text.push_str(&format!("\n  ⚠ {w}"));
            }
            mcp_summary = Some(text);
        }
    }

    // /lsps panel — pulls live serverInfo from the registry when a
    // server came up; otherwise just lists the config row.
    let lsp_summary = if cfg.lsps.is_empty() {
        None
    } else {
        let live: std::collections::HashMap<String, (Option<String>, Option<String>)> =
            lsp_registry
                .as_ref()
                .map(|r| {
                    r.server_summaries()
                        .into_iter()
                        .map(|(n, s, v)| (n, (s, v)))
                        .collect()
                })
                .unwrap_or_default();
        let warnings: Vec<String> = lsp_registry
            .as_ref()
            .map(|r| r.warnings().to_vec())
            .unwrap_or_default();
        let running = live.len();
        let total = cfg.lsps.len();
        let label = if running > 0 {
            "running (lsp_diagnostics tool wired)"
        } else {
            "running"
        };
        let mut text = format!("{running}/{total} LSP server(s) {label}:");
        for (name, entry) in &cfg.lsps {
            let cmd = if entry.args.is_empty() {
                entry.command.clone()
            } else {
                format!("{} {}", entry.command, entry.args.join(" "))
            };
            let roots = if entry.root_patterns.is_empty() {
                "(any)".to_string()
            } else {
                entry.root_patterns.join(", ")
            };
            let status = match live.get(name) {
                Some((Some(sname), Some(ver))) => format!("  ·  {sname} {ver}"),
                Some((Some(sname), None)) => format!("  ·  {sname}"),
                Some(_) => "  ·  running".to_string(),
                None => "  ·  not running".to_string(),
            };
            text.push_str(&format!("\n  {name}  ·  {cmd}  ·  roots: {roots}{status}"));
        }
        for w in &warnings {
            text.push_str(&format!("\n  ⚠ {w}"));
        }
        Some(text)
    };

    // Combine MCP + LSP into a single tool router for the agent. Each
    // registry is its own `ToolRouter` impl; the combinator routes by
    // `handles()`. Empty router skipped so the agent's built-in tools
    // stay the only option when neither registry has anything to add.
    {
        // Snapshot the per-server tool breakdown before mcp_registry is
        // boxed into the router — the agent needs it to hide / restore a
        // server's tools via `/mcps disable|enable NAME`.
        let mcp_server_defs = mcp_registry
            .as_ref()
            .map(|r| r.server_tool_defs())
            .unwrap_or_default();
        let mut routers: Vec<Box<dyn teleia_agent::ToolRouter>> = Vec::new();
        if let Some(r) = mcp_registry {
            routers.push(Box::new(r));
        }
        if let Some(r) = lsp_registry {
            routers.push(Box::new(r));
        }
        if !routers.is_empty() {
            agent.set_tool_router(Box::new(router::CombinedRouter::new(routers)));
        }
        if !mcp_server_defs.is_empty() {
            // Applies the persisted `mcp_disabled` pref, so a server
            // disabled in a previous session stays disabled here too.
            agent.set_mcp_servers(mcp_server_defs);
        }
    }

    // Best-effort update check against the GitHub releases API. 5s
    // timeout; offline / rate-limited returns None silently. The
    // result rides into the TUI so `/update` can re-display it — the
    // TUI also pushes an Info entry + desktop notification on first
    // paint when a newer release is found, so no boot-line announce
    // is needed here either.
    boot.step("checking for updates");
    let update_check = update::check().await;

    spawn_sigusr1_theme_reload();
    boot.finish();
    tui::run(agent, mcp_summary, lsp_summary, update_check).await
}

#[cfg(unix)]
fn spawn_sigusr1_theme_reload() {
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sig = match signal(SignalKind::user_defined1()) {
            Ok(s) => s,
            Err(_) => return,
        };
        while sig.recv().await.is_some() {
            let Ok(store) = Store::open() else { continue };
            apply_theme_from_store(&store);
        }
    });
}

#[cfg(not(unix))]
fn spawn_sigusr1_theme_reload() {}

#[cfg(unix)]
fn apply_theme_from_store(store: &Store) {
    let custom = store
        .get_pref("grogu_palette")
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .map(|s| tui::set_custom_palette_from_json(&s))
        .unwrap_or(false);
    if custom {
        return;
    }
    tui::clear_custom_theme();
    if let Ok(Some(name)) = store.get_pref("theme") {
        let _ = tui::set_theme(&name);
    }
}

fn apply_theme_from_agent(agent: &teleia_agent::Agent) {
    let custom = agent
        .get_pref("grogu_palette")
        .filter(|s| !s.trim().is_empty())
        .map(|s| tui::set_custom_palette_from_json(&s))
        .unwrap_or(false);
    if custom {
        return;
    }
    tui::clear_custom_theme();
    if let Some(name) = agent.get_pref("theme") {
        let _ = tui::set_theme(&name);
    }
}

/// Stream `/api/pull` for a single model with an animated progress bar.
/// Models like `hf.co/FoolDev/Thanatos-27B:Q4_K_M` resolve to a
/// HuggingFace pull automatically via Ollama's bridge.
async fn pull_with_progress(llm: &LlmClient) -> Result<()> {
    let model = llm.model().to_string();
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
            // Clamp: a misbehaving pull server can report completed > total,
            // which would drive `filled` past BAR_WIDTH and underflow the
            // `BAR_WIDTH - filled` subtraction (release builds wrap and abort
            // in the huge `repeat`).
            let pct = ((done * 100 / total) as usize).min(100);
            let filled = (pct * BAR_WIDTH / 100).min(BAR_WIDTH);
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

/// Namespace under which API keys are stored in the prefs table. Keys
/// live alongside other prefs (theme, mode, etc.) but the prefix keeps
/// them isolated and easy to recognise.
pub fn pref_key_for(env_var: &str) -> String {
    format!("api_key:{env_var}")
}

/// Read an API key for `prov` from the terminal with input hidden, after
/// asking the user for permission. Returns `None` if stdin isn't a TTY,
/// the user declines, or the typed string is empty. The key lives in
/// process memory only — persistence is up to the user (env var or the
/// `[llms.NAME]` config entry).
fn prompt_api_key(prov: &teleia_llm::Provider) -> Option<String> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    eprintln!(
        "· {} API key not found (looked at ${}).",
        prov.name, prov.env_var
    );
    eprint!("  enter a key now? [Y/n] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return None;
    }
    if line.trim().to_lowercase().starts_with('n') {
        return None;
    }
    let prompt = format!("  {} key: ", prov.name);
    match rpassword::prompt_password(prompt) {
        Ok(k) if !k.is_empty() => {
            eprintln!("  ✓ key stored for this session ({} chars)", k.len());
            Some(k)
        }
        _ => None,
    }
}

/// Ask once whether to pull every missing model in the set. Non-TTY stdin
/// (scripts, pipes, CI) defaults to yes so unattended runs still get the
/// models. Defaults to yes on an empty line; anything starting with 'n'
/// is a no.
fn confirm_pull_many(models: &[String]) -> bool {
    if !std::io::stdin().is_terminal() {
        return true;
    }
    if models.len() == 1 {
        eprint!("· pull {} from Ollama now? [Y/n] ", models[0]);
    } else {
        eprintln!("· {} models not cached locally:", models.len());
        for m in models {
            eprintln!("    {m}");
        }
        eprint!("· pull all from Ollama now? [Y/n] ");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_pull_line_survives_completed_over_total() {
        // A misbehaving server reporting completed > total must not
        // underflow BAR_WIDTH - filled (which aborts release builds).
        let prog = PullProgress {
            status: "pulling".into(),
            digest: Some("sha256:deadbeefcafe0000".into()),
            total: Some(50),
            completed: Some(100),
        };
        render_pull_line(&prog, "-");
    }
}
