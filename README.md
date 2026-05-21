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

Minimal TUI coding agent. Talks to a local Ollama or a cloud chat-completions endpoint, runs four tools (`read` / `write` / `edit` / `bash`), persists sessions to SQLite. Single binary, no daemon.

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

The model name picks the provider automatically:

| pattern                | provider  | base URL (default)             | API key env         |
| ---------------------- | --------- | ------------------------------ | ------------------- |
| `claude-*`             | Anthropic | `https://api.anthropic.com/v1` | `ANTHROPIC_API_KEY` |
| `gpt-*`, `o1*`, `o3*`  | OpenAI    | `https://api.openai.com/v1`    | `OPENAI_API_KEY`    |
| anything else          | Ollama    | `http://127.0.0.1:11434/v1`    | _none_              |

`--base-url URL` overrides the detected endpoint; `--api-key KEY` overrides the env-var fallback. The `/model` drop-down pre-populates with [`claude-opus-4-7`](https://www.anthropic.com/claude), `claude-sonnet-4-6`, and `claude-haiku-4-5-20251001` alongside whatever Ollama has cached locally, so switching is one Tab away.

### Ollama pre-flight

When the resolved base URL looks like Ollama, `telia` walks the default Ollama list — [`hf.co/FoolDev/Thanatos-27B`](https://huggingface.co/FoolDev/Thanatos-27B) and [`hf.co/FoolDev/Janus-35B`](https://huggingface.co/FoolDev/Janus-35B), plus the active `--model` — and for each one missing locally asks:

```
· pull hf.co/FoolDev/Janus-35B:Q4_K_M from Ollama now? [Y/n]
```

A `y` (default) streams `/api/pull` with an animated in-place progress bar. `n` skips that model and moves on. Pass `-y` / `--pull-yes` to auto-confirm every prompt, or `--no-pull` to skip the pre-flight entirely. Non-interactive stdin (scripts, CI, pipes) auto-confirms so unattended runs don't hang.

## Features

- **Modes** — Insert (default), Normal (`Esc`), Command (`:`). The status bar chip and the input border colour signal the active mode.
- **Slash commands** — `/reset`, `/clear`, `/save NAME`, `/load NAME`, `/delete NAME`, `/list`, `/model [NAME]`, `/theme [NAME]`, `/notify [on|off]`, `/show`, `/help`, `/quit`. Vim users get the same set as `:q`, `:w NAME`, `:e NAME`, `:colo NAME`, etc.
- **Autocomplete** — ghost-text suggests the best single completion as you type; a drop-down panel above the input lists every match (commands, saved aliases, themes, installed Ollama models, ex commands). Up/Down navigates, Tab accepts, Esc dismisses.
- **Input editing** — full readline-style: Left/Right/Home/End, Backspace/Delete, Ctrl+A/E/U/W, plus Up/Down to recall previous submissions when the input is empty.
- **Scrollback** — ↑/↓ (1 line), PageUp/PageDown (5 lines), mouse wheel (3 lines/tick); in Normal mode `Ctrl+U` / `Ctrl+D` half-page (12 lines), `G` jumps to the bottom.
- **Streaming** — tokens render live via SSE. The streaming caret blinks; tool calls show a braille spinner; the "thinking" indicator has a running dot. All animations stay alive during a turn.
- **Tools** — `read` / `write` / `edit` / `bash`, capped at a 16-hop loop. `read` output is syntax-highlighted by file extension (`syntect`, 50+ languages); assistant code fences (```` ```rust ````) highlight by language hint.
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
- `crates/telia-tools` — `read` / `write` / `edit` / `bash` dispatch
- `crates/telia-tools-bin` — same dispatch, exposed as a stdin/stdout CLI
- `crates/telia-store` — sqlite session persistence

## License

MIT — see [LICENSE](LICENSE).
