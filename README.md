<p align="center">
  <img src="assets/banner.svg" alt="Teleia — the distilled coding agent" width="720">
</p>

<p align="center">
  <a href="https://github.com/foolish-dev/Teleia/actions/workflows/ci.yml"><img alt="ci" src="https://github.com/foolish-dev/Teleia/actions/workflows/ci.yml/badge.svg?branch=dev"></a>
  <a href="LICENSE"><img alt="MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
</p>

Minimal TUI coding agent — local Ollama backend, four tools, persistent sessions. Single binary, no daemon, no cloud round-trip.

<p align="center">
  <img src="assets/screenshot.svg" alt="Teleia TUI session" width="780">
</p>

## Install

One-liner — clones, builds, drops `teleia` into `~/.local/bin` (requires cargo + git):

```sh
curl -fsSL https://raw.githubusercontent.com/foolish-dev/Teleia/dev/install.sh | sh
```

Override with `PREFIX=/usr/local/bin` (or any target) by piping into a `sh` that already has it set:

```sh
curl -fsSL https://raw.githubusercontent.com/foolish-dev/Teleia/dev/install.sh | PREFIX=/usr/local/bin sh
```

Or with cargo directly:

```sh
cargo install --git https://github.com/foolish-dev/Teleia teleia-cli
```

## Run

```sh
teleia
# with options
teleia --model hf.co/FoolDev/Janus-35B:Q4_K_M --base-url http://127.0.0.1:11434/v1
```

Requires Ollama running locally with a tool-capable model pulled. Default model: `hf.co/FoolDev/Thanatos-27B:Q4_K_M`.

For development from a workspace clone:

```sh
cargo run --release
```

## Features

- **Streaming** — tokens render live via SSE.
- **Tools** — `read` / `write` / `edit` / `bash`, with a 16-hop loop cap. While the agent is working the status bar shows an animated spinner and a `hops X/16` counter.
- **Syntax highlighting** — `read` output highlights by file extension; assistant code fences (```` ```rust ````) highlight by language hint. Powered by `syntect`.
- **Slash commands** — `/reset`, `/clear`, `/save NAME`, `/load NAME`, `/delete NAME`, `/list`, `/model [NAME]`, `/show`, `/help`, `/quit`. Tab accepts ghost-text autocomplete (commands + saved alias names).
- **Vim keys** — `Esc` enters Normal mode; `i`/`a`/`I`/`A` go back to Insert; `h`/`l`/`0`/`$` move the cursor, `j`/`k` scroll history, `x` deletes a char. `:` opens an ex command line: `:q`, `:w NAME`, `:e NAME`, `:d NAME`, `:ls`, `:model …`, `:reset`, `:clear`, `:help` (slash commands also still work).
- **Scrollback** — ↑ / ↓ (PageUp / PageDown).
- **Sessions** — sqlite at `$XDG_DATA_HOME/teleia/teleia.sqlite`. Save/load by alias across runs.

Not yet: MCP, LSP, plugins, subagents, multi-provider, web UI.

## Stack

| layer | crate |
| ----- | ----- |
| TUI   | `ratatui` + `crossterm` |
| HTTP  | `reqwest` (rustls) |
| JSON  | `serde_json` |
| Store | `rusqlite` (bundled) |

## Layout

- `crates/teleia-cli` — `teleia` binary, TUI entry point
- `crates/teleia-agent` — turn loop, 16-hop cap, event stream
- `crates/teleia-llm` — Ollama streaming client, message + tool types
- `crates/teleia-tools` — `read` / `write` / `edit` / `bash` dispatch
- `crates/teleia-tools-bin` — same dispatch, exposed as a stdin/stdout CLI
- `crates/teleia-store` — sqlite session persistence

## License

MIT — see [LICENSE](LICENSE).
