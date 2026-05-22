<p align="center">
  <img src="assets/banner.svg" alt="τέλεια — the distilled coding agent" width="720">
</p>

<p align="center">
  <a href="https://github.com/foolish-dev/telia/actions/workflows/ci.yml"><img alt="ci" src="https://github.com/foolish-dev/telia/actions/workflows/ci.yml/badge.svg?branch=dev"></a>
  <a href="LICENSE"><img alt="MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
  <a href="https://buymeacoffee.com/cardoffoolm"><img alt="Buy me a coffee" src="https://img.shields.io/badge/buy%20me%20a%20coffee-cardoffoolm-FFDD00?logo=buymeacoffee&logoColor=1a1b26"></a>
</p>

<p align="center">
  <a href="https://huggingface.co/FoolDev/Thanatos-27B"><img alt="FoolDev/Thanatos-27B on Hugging Face" src="https://img.shields.io/badge/%F0%9F%A4%97-FoolDev%2FThanatos--27B-bb9af7?logo=huggingface&logoColor=1a1b26&labelColor=24283b"></a>
  <a href="https://huggingface.co/FoolDev/Janus-35B"><img alt="FoolDev/Janus-35B on Hugging Face" src="https://img.shields.io/badge/%F0%9F%A4%97-FoolDev%2FJanus--35B-7aa2f7?logo=huggingface&logoColor=1a1b26&labelColor=24283b"></a>
</p>

**A minimal TUI coding agent.** One binary, no daemon. Talks to a local Ollama or any of fifteen cloud chat-completions endpoints. Runs seven built-in tools, hosts MCP servers, persists sessions to SQLite, and resumes where you left off.

<p align="center">
  <img src="assets/screenshot.svg" alt="τέλεια TUI session" width="780">
</p>

## Install

```sh
# one-liner: clones, builds, drops `telia` into ~/.local/bin
curl -fsSL https://raw.githubusercontent.com/foolish-dev/telia/dev/install.sh | sh

# or cargo install from the workspace
cargo install --git https://github.com/foolish-dev/telia telia-cli
```

`PREFIX=/usr/local/bin sh` overrides the install location. `cargo run --release` works from a workspace clone.

## Run

```sh
# default: local Ollama with hf.co/FoolDev/Thanatos-27B
telia

# cloud — picks up $ANTHROPIC_API_KEY (or prompts for one)
telia --model claude-opus-4-7

# resume the last session you ran
telia --resume

# start with all tool prompts skipped
telia --auto
```

Inside the TUI, switch models live with `/model NAME`; switch providers by changing the prefix. Type `/help` to list every slash command.

## Permission modes

Three stances control tool execution, cycle with `Shift+Tab` or set explicitly:

| mode      | chip          | behaviour                                                                  | trigger              |
| --------- | ------------- | -------------------------------------------------------------------------- | -------------------- |
| **PLAN**  | blue          | only `read` / `list` / `glob` / `grep` run; write/edit/bash short-circuit  | `/plan`, `--plan`    |
| **BUILD** | green (default) | every tool call pauses for `y` / `n` / `a`                               | `/build`, default    |
| **AUTO**  | red           | every tool dispatches immediately, no prompts                              | `/auto`, `--auto`    |

`a` at any approval prompt allow-alls and flips into AUTO for the rest of the session. `Esc` always denies.

## Tools

Built-in, capped at a 16-hop loop per turn:

| tool   | does                                                                       |
| ------ | -------------------------------------------------------------------------- |
| `read` | read a file; output is syntax-highlighted (`syntect`, 50+ languages)       |
| `write`| overwrite a file                                                           |
| `edit` | unique-substring replace inside a file                                     |
| `bash` | run a shell command (combined stdout/stderr, 30s timeout)                  |
| `list` | directory listing, dirs suffixed with `/`                                  |
| `glob` | shell-style glob, 200-match cap                                            |
| `grep` | rust regex over a file or recursively walked dir (skips `target/`, `.git/`, …) |

Plus any tools exposed by MCP servers you configure — see [Configuration](#configuration).

## Providers

The model name picks the provider. Use `provider:NAME` to disambiguate names that collide with local Ollama models.

| pattern                            | provider     | API key env             |
| ---------------------------------- | ------------ | ----------------------- |
| `claude-*`                         | Anthropic    | `ANTHROPIC_API_KEY`     |
| `gpt-*`, `o1*`, `o3*`, `o4*`       | OpenAI       | `OPENAI_API_KEY`        |
| `gemini-*`                         | Google       | `GEMINI_API_KEY`        |
| `grok-*`                           | xAI          | `XAI_API_KEY`           |
| `deepseek-*`                       | DeepSeek     | `DEEPSEEK_API_KEY`      |
| `mistral-*`, `codestral-*`         | Mistral      | `MISTRAL_API_KEY`       |
| `command-*`                        | Cohere       | `COHERE_API_KEY`        |
| `sonar*`                           | Perplexity   | `PERPLEXITY_API_KEY`    |
| `groq:NAME`                        | Groq         | `GROQ_API_KEY`          |
| `openrouter:NAME`                  | OpenRouter   | `OPENROUTER_API_KEY`    |
| `together:NAME`                    | Together AI  | `TOGETHER_API_KEY`      |
| `fireworks:NAME`                   | Fireworks    | `FIREWORKS_API_KEY`     |
| `cerebras:NAME`                    | Cerebras     | `CEREBRAS_API_KEY`      |
| `hyperbolic:NAME`                  | Hyperbolic   | `HYPERBOLIC_API_KEY`    |
| `nvidia:NAME`                      | NVIDIA NIM   | `NVIDIA_API_KEY`        |
| anything else                      | local Ollama | _none_                  |

The `/model` dropdown pre-populates with ~130 named models across all fifteen providers (Tab to accept, type to filter; any name is accepted). `/keys` lists every provider and marks which env vars are set.

**Key handling.** `--api-key KEY` overrides the env-var fallback. If no key is found for a cloud provider, telia prompts at startup with hidden input (`rpassword`); the same prompt fires inside the TUI when `/model NAME` switches to a provider without a key. Keys live in process memory only — persist via env var or a `[llms.NAME]` config entry.

**Ollama pre-flight.** When the resolved endpoint is local Ollama, missing models trigger an interactive `pull from Ollama now? [Y/n]` with an animated progress bar. `--pull-yes` auto-confirms, `--no-pull` skips. Non-interactive stdin (scripts, CI, pipes) auto-confirms.

## Features

- **Modes** — Insert (default), Normal (`Esc`), Command (`:`). The status bar chip and input border colour signal the active mode.
- **Slash commands** — `/reset`, `/clear`, `/save`, `/load`, `/delete`, `/list`, `/model`, `/keys`, `/plan`, `/build`, `/auto`, `/prompt`, `/theme`, `/notify`, `/show`, `/help`, `/quit` — vim-style `:q`, `:w`, `:e`, `:colo` aliases work too.
- **Prompt templates** — `/prompt` lists 10 starters (`review`, `debug`, `explain`, `refactor`, `test`, `docs`, `security`, `perf`, `commit`, `plan`); `/prompt NAME` drops the template into the input box.
- **Autocomplete** — ghost-text completion as you type, plus a drop-down panel listing every match (commands, aliases, themes, models). `Up`/`Down` navigate, `Tab` accepts, `Esc` dismisses.
- **Input editing** — readline-style `Left`/`Right`/`Home`/`End`, `Backspace`/`Delete`, `Ctrl+A/E/U/W`; `Up`/`Down` recall prior submissions. Typing stays live during a streaming turn; the input box grows vertically with content.
- **Scrollback** — `↑/↓` 1 line, `PageUp/Down` 5 lines, mouse wheel 3 lines; in Normal mode `Ctrl+U/D` half-page and `G` jumps to the bottom.
- **Streaming** — tokens render live via SSE with a blinking caret, braille spinners on tool calls, and a running-dot "thinking" indicator. Animations stay alive during a turn.
- **Interrupt** — `Esc` or `Ctrl+C` mid-turn aborts the stream and any pending tool hops. Status bar shows `esc / ^c interrupt` while a turn is in flight.
- **Drag-select** — left-click + drag in the chat area highlights cells; on release the text copies to the system clipboard via [`arboard`](https://crates.io/crates/arboard) (Wayland, X11, macOS, Windows). Works during streaming.
- **Themes** — `tokyo-night` (default), `catppuccin`, `dracula`. `--theme NAME` at launch or `/theme NAME` live.
- **Token tracker** — status bar shows `↑prompt ↓completion` totals for the session. Counts come from the provider's `usage` block on the final stream chunk.
- **Desktop notifications** — `notify-send` fires at end-of-turn with the first ~120 chars of the reply. Toggle with `/notify on|off`.

## Session persistence

Everything important survives a restart.

- **Messages** are streamed to SQLite (`$XDG_DATA_HOME/telia/telia.sqlite`) as they arrive — there's no "unsaved" state.
- **Auto-bookmarks**: every launch is tagged `last`; `/reset` rotates the outgoing session to `prev`. So `--resume` (alias `--continue`, short `-r`) always picks up where you left off, and the run before that is recoverable with `/load prev`.
- **Aliases**: `/save NAME` and `/load NAME` for sessions you want to keep by hand.
- **Sticky preferences**: theme, `/notify` toggle, and permission mode persist across launches (CLI flags override).
- **Input history**: up to the last 500 submissions reload into the `Up`/`Down` recall buffer, deduped shell-style.

## Configuration

Optional TOML at `$XDG_CONFIG_HOME/telia/config.toml` (falls back to `~/.config/telia/config.toml`).

```toml
# ─── Custom LLM endpoint ────────────────────────────────────────────
# Adds `groq-llama` to the /model dropdown; routing + key resolution
# follow the entry instead of the provider-prefix rule.
[llms.groq-llama]
base_url    = "https://api.groq.com/openai/v1"
api_key_env = "GROQ_API_KEY"

# ─── MCP server ─────────────────────────────────────────────────────
# Spawned at startup; tools join the agent's catalogue automatically.
# `env` overrides the parent process for this child only.
[mcps.filesystem]
command = "npx"
args    = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[mcps.github]
command = "docker"
args    = ["run", "-i", "--rm", "ghcr.io/github/github-mcp-server"]

[mcps.github.env]
GITHUB_PERSONAL_ACCESS_TOKEN = "ghp_…"

# ─── LSP server (parsed today, runtime stub) ─────────────────────────
[lsps.rust]
command       = "rust-analyzer"
root_patterns = ["Cargo.toml"]
```

**MCP support.** telia speaks stdio JSON-RPC: `initialize` + `notifications/initialized` + `tools/list` + `tools/call`. Spawn failures stderr-warn but don't abort boot. Servers receive `SIGKILL` on telia's exit (`kill_on_drop`). No resources, prompts, sampling, or cancellation yet.

**LSP**: config is parsed but the runtime isn't wired — entries log a TODO note at startup and otherwise sit idle until tools land.

## Layout

```
crates/
  telia-cli       # `telia` binary + TUI + MCP client
  telia-agent     # turn loop, 16-hop cap, permission gate, event stream
  telia-llm       # chat / pull / tags streaming, provider detection
  telia-tools     # read / write / edit / bash / list / glob / grep
  telia-tools-bin # same dispatch as a stdin/stdout CLI
  telia-store     # sqlite session + prefs + input-history persistence
```

Built on `ratatui` + `crossterm`, `reqwest` (rustls), `rusqlite` (bundled), `syntect` (no C deps), `arboard`, `rpassword`.

## License

MIT — see [LICENSE](LICENSE).
