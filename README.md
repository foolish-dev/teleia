<p align="center">
  <img src="assets/banner.svg" alt="τέλεια — the distilled coding agent" width="720">
</p>

<p align="center">
  <a href="https://github.com/foolish-dev/telia/actions/workflows/ci.yml"><img alt="ci" src="https://github.com/foolish-dev/telia/actions/workflows/ci.yml/badge.svg?branch=dev"></a>
  <a href="LICENSE"><img alt="MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
</p>

Minimal TUI coding agent — local Ollama backend, four tools, persistent sessions. Single binary, no daemon, no cloud round-trip.

<p align="center">
  <img src="assets/screenshot.svg" alt="τέλεια TUI session" width="780">
</p>

## Install

One-liner — clones, builds, drops `telia` into `~/.local/bin` (requires cargo + git):

```sh
curl -fsSL https://raw.githubusercontent.com/foolish-dev/telia/dev/install.sh | sh
```

Override with `PREFIX=/usr/local/bin` (or any target) by piping into a `sh` that already has it set:

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
# with options
telia --model hf.co/FoolDev/Janus-35B:Q4_K_M --base-url http://127.0.0.1:11434/v1
```

### Providers

The model name picks the provider automatically:

| pattern                | provider  | base URL (default)                  | API key env         |
| ---------------------- | --------- | ----------------------------------- | ------------------- |
| `claude-*`             | Anthropic | `https://api.anthropic.com/v1`      | `ANTHROPIC_API_KEY` |
| `gpt-*`, `o1*`, `o3*`  | OpenAI    | `https://api.openai.com/v1`         | `OPENAI_API_KEY`    |
| anything else          | Ollama    | `http://127.0.0.1:11434/v1`         | _none_              |

`--base-url URL` overrides the detected endpoint; `--api-key KEY` overrides the env-var fallback. Cloud models such as `claude-opus-4-7`, `claude-sonnet-4-6`, and `claude-haiku-4-5-20251001` are pre-populated in the `/model` dropdown so they're switchable mid-session.

### Ollama pre-flight

When the resolved base URL looks like Ollama, `telia` walks a list of default Ollama models — currently [`hf.co/FoolDev/Thanatos-27B`](https://huggingface.co/FoolDev/Thanatos-27B) and [`hf.co/FoolDev/Janus-35B`](https://huggingface.co/FoolDev/Janus-35B), plus the active `--model` if it isn't already on that list — asks Ollama which of them are already cached, and for each missing one prompts:

```
· pull hf.co/FoolDev/Thanatos-27B:Q4_K_M from Ollama now? [Y/n]
```

A `y` (default) streams `/api/pull` with an animated in-place progress bar; an `n` skips that model. Pass `-y` / `--pull-yes` to auto-confirm every prompt (the old always-pull behaviour), or `--no-pull` to skip the whole pre-flight. Non-interactive runs (stdin isn't a TTY — scripts, CI, pipes) auto-confirm so they don't block waiting for input. Once a model is cached, `/model` switches into it without another download. The pre-flight is automatically skipped for non-Ollama endpoints.

For development from a workspace clone:

```sh
cargo run --release
```

## Features

- **Streaming** — tokens render live via SSE.
- **Tools** — `read` / `write` / `edit` / `bash`, with a 16-hop loop cap. While the agent is working the status bar shows an animated spinner.
- **Syntax highlighting** — `read` output highlights by file extension; assistant code fences (```` ```rust ````) highlight by language hint. Powered by `syntect`.
- **Slash commands** — `/reset`, `/clear`, `/save NAME`, `/load NAME`, `/delete NAME`, `/list`, `/model [NAME]`, `/theme [NAME]`, `/show`, `/help`, `/quit`. Tab accepts ghost-text autocomplete (commands + saved alias names); a drop-down menu appears above the input when typing `/` (commands) or `/load`/`/delete`/`/rm` (alias names) — Up/Down to navigate, Tab to accept, Esc to dismiss.
- **Themes** — `tokyo-night` (default), `catppuccin`, `dracula`. Pick at startup with `--theme NAME`, or switch live with `/theme NAME` (vim users: `:colo NAME`).
- **Input history** — readline-style. Up/Down (with empty input or already recalling) walks back through previous submissions; further edits exit recall mode. Consecutive duplicates are deduplicated.
- **Vim keys** — `Esc` enters Normal mode; `i`/`a`/`I`/`A` go back to Insert; `h`/`l`/`0`/`$` move the cursor, `j`/`k` scroll history, `x` deletes a char. `:` opens an ex command line: `:q`, `:w NAME`, `:e NAME`, `:d NAME`, `:ls`, `:model …`, `:reset`, `:clear`, `:help` (slash commands also still work).
- **Scrollback** — ↑ / ↓ (PageUp / PageDown).
- **Sessions** — sqlite at `$XDG_DATA_HOME/telia/telia.sqlite`. Save/load by alias across runs.
- **Token tracker** — status bar shows `↑prompt ↓completion` totals for the current session (cumulative across turns; resets on `/reset` or `/load`). Counts come from the `usage` field in Ollama's final stream chunk (request includes `stream_options.include_usage`).

Not yet: MCP, LSP, plugins, subagents, multi-provider, web UI.

## Stack

| layer | crate |
| ----- | ----- |
| TUI   | `ratatui` + `crossterm` |
| HTTP  | `reqwest` (rustls) |
| JSON  | `serde_json` |
| Store | `rusqlite` (bundled) |

## Layout

- `crates/telia-cli` — `telia` binary, TUI entry point
- `crates/telia-agent` — turn loop, 16-hop cap, event stream
- `crates/telia-llm` — Ollama streaming client, message + tool types
- `crates/telia-tools` — `read` / `write` / `edit` / `bash` dispatch
- `crates/telia-tools-bin` — same dispatch, exposed as a stdin/stdout CLI
- `crates/telia-store` — sqlite session persistence

## License

MIT — see [LICENSE](LICENSE).
