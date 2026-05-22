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

> Currently adding other models, local models run fine

Minimal TUI coding agent. Talks to a local Ollama or a cloud chat-completions endpoint, runs seven tools (`read` / `write` / `edit` / `bash` / `list` / `glob` / `grep`), persists sessions to SQLite. Single binary, no daemon.

<p align="center">
  <img src="assets/screenshot.svg" alt="τέλεια TUI session" width="780">
</p>

## Install

One-liner — clones, builds, drops `telia` into `~/.local/bin` (requires cargo + git):

```sh
curl -fsSL https://raw.githubusercontent.com/foolish-dev/telia/dev/install.sh | sh
```

Override the install location:

```sh
curl -fsSL https://raw.githubusercontent.com/foolish-dev/telia/dev/install.sh | PREFIX=/usr/local/bin sh
```

Or with cargo directly:

```sh
cargo install --git https://github.com/foolish-dev/telia telia-cli
```

## Run

```sh
telia
# explicit options
telia --model hf.co/FoolDev/Janus-35B:Q4_K_M --base-url http://127.0.0.1:11434/v1
# cloud
telia --model claude-opus-4-7  # picks up $ANTHROPIC_API_KEY
```

`cargo run --release` works from a workspace clone.

## Providers

The model name picks the provider automatically. For names whose prefixes collide with local Ollama models, use the explicit `provider:NAME` form:

| pattern                                          | provider     | base URL                                                     | API key env            |
| ------------------------------------------------ | ------------ | ------------------------------------------------------------ | ---------------------- |
| `claude-*`                                       | Anthropic    | `https://api.anthropic.com/v1`                               | `ANTHROPIC_API_KEY`    |
| `gpt-*`, `o1*`, `o3*`, `o4*`                     | OpenAI       | `https://api.openai.com/v1`                                  | `OPENAI_API_KEY`       |
| `gemini-*`                                       | Google       | `https://generativelanguage.googleapis.com/v1beta/openai`    | `GEMINI_API_KEY`       |
| `grok-*`                                         | xAI          | `https://api.x.ai/v1`                                        | `XAI_API_KEY`          |
| `deepseek-*`                                     | DeepSeek     | `https://api.deepseek.com/v1`                                | `DEEPSEEK_API_KEY`     |
| `mistral-*`, `codestral-*`                       | Mistral      | `https://api.mistral.ai/v1`                                  | `MISTRAL_API_KEY`      |
| `groq:NAME`                                      | Groq         | `https://api.groq.com/openai/v1`                             | `GROQ_API_KEY`         |
| `openrouter:NAME`                                | OpenRouter   | `https://openrouter.ai/api/v1`                               | `OPENROUTER_API_KEY`   |
| anything else                                    | Ollama       | `http://127.0.0.1:11434/v1`                                  | _none_                 |

`--base-url URL` overrides the detected endpoint; `--api-key KEY` overrides the env-var fallback. If you launch telia pointed at a cloud provider whose env var isn't set, you'll be prompted at startup — `Y` then a hidden-input field reads the key for this session only (no disk write; persist via env var or `[llms.NAME]` in the config file). The same hidden-input prompt fires inside the TUI whenever `/model NAME` switches to a provider without a key — type the key (echoed as `•`), `Enter` to set, `Esc` to skip. `/keys` lists every provider and marks which env vars are set. The `/model` drop-down pre-populates with ~75 named models across all eight cloud providers — the full Claude 3 / 4 family, GPT-5 / GPT-4.1 / GPT-4o / o-series, Gemini 1.5 / 2.0 / 2.5, Grok 2 / 3 / 4, DeepSeek chat + reasoner, the Mistral/Ministral/Codestral/Pixtral catalog, Groq-hosted Llama 3 / Llama 4 / Qwen / Mixtral / Gemma / DeepSeek distills, and the most-used OpenRouter routes — alongside whatever Ollama has cached locally. Type to filter, Tab to accept; any name (including bare strings the catalog doesn't list yet) is accepted.

### Ollama pre-flight

When the resolved base URL looks like Ollama, `telia` walks the default Ollama list — [`hf.co/FoolDev/Thanatos-27B`](https://huggingface.co/FoolDev/Thanatos-27B) and [`hf.co/FoolDev/Janus-35B`](https://huggingface.co/FoolDev/Janus-35B), plus the active `--model` — and for each one missing locally asks:

```
· pull hf.co/FoolDev/Janus-35B:Q4_K_M from Ollama now? [Y/n]
```

A `y` (default) streams `/api/pull` with an animated in-place progress bar. `n` skips that model and moves on. Pass `-y` / `--pull-yes` to auto-confirm every prompt, or `--no-pull` to skip the pre-flight entirely. Non-interactive stdin (scripts, CI, pipes) auto-confirms so unattended runs don't hang.

## Configuration

### Model

Set the active model with `--model NAME`. The provider auto-detects from the name prefix (see the table above); `/model NAME` switches mid-session — the drop-down lists every Ollama-cached model plus the pre-populated cloud entries (`claude-opus-4-7`, `claude-sonnet-4-6`, `claude-haiku-4-5-20251001`). `/model` with no arg prints the current model.

Defaults:

| flag         | env-var fallback                | default                                 |
| ------------ | ------------------------------- | --------------------------------------- |
| `--model`    | _none_                          | `hf.co/FoolDev/Thanatos-27B:Q4_K_M`     |
| `--base-url` | _none_ (auto from model prefix) | `http://127.0.0.1:11434/v1` (Ollama)    |
| `--api-key`  | `$ANTHROPIC_API_KEY` for `claude-*`; `$OPENAI_API_KEY` for `gpt-*`/`o1*`/`o3*` | _none_ (Ollama needs none) |
| `--theme`    | _none_                          | `tokyo-night`                           |
| `--no-pull`  | _none_                          | `false` (run the Ollama pre-flight)     |
| `--pull-yes` | _none_                          | `false` (prompt before each pull)       |

Sessions live in `$XDG_DATA_HOME/telia/telia.sqlite` (falls back to `~/.local/share/telia/telia.sqlite` when `XDG_DATA_HOME` is unset). Set `XDG_DATA_HOME` to move it.

### Config file

Optional TOML at `$XDG_CONFIG_HOME/telia/config.toml` (falls back to `~/.config/telia/config.toml`). Used to register custom LLMs and LSP servers without touching the source:

```toml
# Add a custom LLM. Passing `--model groq-llama` (or `/model groq-llama`)
# routes the chat request through this endpoint and pulls the key from
# `api_key_env` (preferred) or `api_key` (inline string). The name shows
# up in the /model dropdown alongside Ollama-cached + the default cloud
# entries.
[llms.groq-llama]
base_url    = "https://api.groq.com/openai/v1"
api_key_env = "GROQ_API_KEY"

[llms.openrouter-sonnet]
base_url    = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"

# Register an LSP server. Parsed today, ready to be wired into the
# tool-dispatch loop when LSP support lands — same placeholder
# treatment as `[mcpServers]` below.
[lsps.rust]
command       = "rust-analyzer"
args          = []
root_patterns = ["Cargo.toml"]

[lsps.python]
command       = "pylsp"
root_patterns = ["pyproject.toml", "setup.py"]
```

Resolution order for `--model NAME` (and `/model NAME` mid-session): explicit `--base-url` / `--api-key` win; otherwise a matching `[llms.NAME]` entry; otherwise the prefix rule (`claude-*` → Anthropic, `gpt-*`/`o1*`/`o3*` → OpenAI, else Ollama). Parse errors print a warning on stderr at startup and fall back to empty config.

### MCP

Not yet supported. The roadmap is to read an Anthropic-style MCP config — likely the same shape Claude Desktop uses:

```jsonc
// $XDG_CONFIG_HOME/telia/mcp.json (proposed)
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
    },
    "github": {
      "command": "docker",
      "args": ["run", "-i", "--rm", "ghcr.io/github/github-mcp-server"]
    }
  }
}
```

— and merge any tools each server exposes into the same dispatch loop the built-in seven (`read` / `write` / `edit` / `bash` / `list` / `glob` / `grep`) already run through. Until then, treat MCP as a TODO: tracked on the "Not yet" line below.

## Features

- **Modes** — Insert (default), Normal (`Esc`), Command (`:`). The status bar chip and the input border colour signal the active mode.
- **Slash commands** — `/reset`, `/clear`, `/save NAME`, `/load NAME`, `/delete NAME`, `/list`, `/model [NAME]`, `/keys`, `/plan`, `/build`, `/auto`, `/prompt [NAME]`, `/theme [NAME]`, `/notify [on|off]`, `/show`, `/help`, `/quit`. Vim users get the same set as `:q`, `:w NAME`, `:e NAME`, `:colo NAME`, etc.
- **Prompt templates** — `/prompt` lists 10 canned starters: `review`, `debug`, `explain`, `refactor`, `test`, `docs`, `security`, `perf`, `commit`, `plan`. `/prompt review` drops the template text into the input box so you can append the code or context and submit — no system-prompt swap, just an expander to save typing.
- **Autocomplete** — ghost-text suggests the best single completion as you type; a drop-down panel above the input lists every match (commands, saved aliases, themes, installed Ollama models, ex commands). Up/Down navigates, Tab accepts, Esc dismisses.
- **Input editing** — full readline-style: Left/Right/Home/End, Backspace/Delete, Ctrl+A/E/U/W, plus Up/Down to recall previous submissions when the input is empty.
- **Scrollback** — ↑/↓ (1 line), PageUp/PageDown (5 lines), mouse wheel (3 lines/tick); in Normal mode `Ctrl+U` / `Ctrl+D` half-page (12 lines), `G` jumps to the bottom.
- **Streaming** — tokens render live via SSE. The streaming caret blinks; tool calls show a braille spinner; the "thinking" indicator has a running dot. All animations stay alive during a turn.
- **Interrupt** — `Esc` or `Ctrl+C` mid-turn aborts the stream, drops any pending tool hops, and returns the prompt to you. The status bar shows `esc / ^c interrupt` whenever a turn is in flight.
- **Permission modes** — three stances, cycled with `Shift+Tab` or set explicitly via slash commands:
  - **BUILD** (default, green chip) — every tool call pauses for `y` allow / `n` deny / `a` allow-all (auto). `Esc` denies. Use `/build` or `/ask`.
  - **PLAN** (blue chip) — `read` / `list` / `glob` / `grep` run; `write` / `edit` / `bash` short-circuit with a synthetic "blocked: plan mode" tool result so the model is pushed to describe rather than execute. Use `/plan` or `--plan`.
  - **AUTO** (red chip) — every tool dispatches immediately, no prompts. Use `/auto` or `--auto`. Per-call `a` also flips into auto for the rest of the session.
- **Drag-select** — left-click and drag in the chat area to highlight text; releasing copies it to the system clipboard via [`arboard`](https://crates.io/crates/arboard) (Wayland + X11 + macOS + Windows). Text-style selection wraps across rows; the status bar reports `copied N chars to clipboard`. `Esc` clears the highlight. Works during streaming.
- **Tools** — `read` / `write` / `edit` / `bash` / `list` / `glob` / `grep`, capped at a 16-hop loop. `read` output is syntax-highlighted by file extension (`syntect`, 50+ languages); assistant code fences (```` ```rust ````) highlight by language hint. `grep` walks directories with Rust regex, skipping `target/`/`node_modules/`/`dist/`/`build/` and hidden dirs.
- **Themes** — `tokyo-night` (default), `catppuccin`, `dracula`. Pick at startup with `--theme NAME` or switch live with `/theme NAME`.
- **Sessions** — sqlite at `$XDG_DATA_HOME/telia/telia.sqlite`. Save/load by alias across runs.
- **Token tracker** — status bar shows `↑prompt ↓completion` totals for the current session (cumulative, resets on `/reset` / `/load`). Counts come from Ollama's `usage` block on the final stream chunk.
- **Desktop notifications** — `notify-send` fires when a chat turn ends, with the first ~120 chars of the assistant's reply. Toggle with `/notify on|off`.
- **Hostname / username** — title bar shows `τέλεια @ HOST`, user-message header is the login name. Distinguishes remote sessions and saved transcripts.

Not yet: MCP, LSP, plugins, subagents, web UI.

## Stack

| layer | crate |
| ----- | ----- |
| TUI   | `ratatui` + `crossterm` |
| HTTP  | `reqwest` (rustls) |
| JSON  | `serde_json` |
| Store | `rusqlite` (bundled) |
| Highlight | `syntect` (default-fancy, no C deps) |

## Layout

- `crates/telia-cli` — `telia` binary, TUI entry point
- `crates/telia-agent` — turn loop, 16-hop cap, event stream, model cache
- `crates/telia-llm` — chat / pull / tags streaming client, provider detection
- `crates/telia-tools` — `read` / `write` / `edit` / `bash` / `list` / `glob` / `grep` dispatch
- `crates/telia-tools-bin` — same dispatch, exposed as a stdin/stdout CLI
- `crates/telia-store` — sqlite session persistence

## License

MIT — see [LICENSE](LICENSE).
