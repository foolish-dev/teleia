<p align="center">
  <img src="assets/banner.svg" alt="Teleia — the distilled coding agent" width="720">
</p>

<p align="center">
  <a href="https://github.com/foolish-dev/Teleia/actions/workflows/ci.yml"><img alt="ci" src="https://github.com/foolish-dev/Teleia/actions/workflows/ci.yml/badge.svg?branch=dev"></a>
  <a href="LICENSE"><img alt="MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
</p>

Minimal TUI coding agent. Talks to a local Ollama (OpenAI-chat-compatible) endpoint, runs four tools (`read` / `write` / `edit` / `bash`), and persists sessions to SQLite.

## Install

```sh
cargo install --path crates/teleia-cli
```

Or run directly from the workspace:

```sh
cargo run --release
# with options
cargo run --release -- --model hf.co/FoolDev/Janus-35B:Q4_K_M --base-url http://127.0.0.1:11434
```

Requires Ollama running at `127.0.0.1:11434` with a tool-capable model installed. Default model: `hf.co/FoolDev/Thanatos-27B:Q4_K_M`.

<p align="center">
  <img src="assets/screenshot.svg" alt="Teleia TUI session" width="780">
</p>

## Features

- **Streaming** SSE — tokens render live as the model produces them.
- **Tool loop**: `read` / `write` / `edit` / `bash`, with a 16-hop cap.
- **Slash commands**: `/reset`, `/save NAME`, `/load NAME`, `/help`.
- **Scrollback**: ↑ / ↓ (PageUp / PageDown).
- **Persistent sessions** at `$XDG_DATA_HOME/teleia/teleia.sqlite`. Save/load by alias across runs.
- **Single provider**: Ollama (OpenAI-chat-compatible), one model per process.
- **Tokyo Night palette.**

Not yet: MCP, LSP, plugins, subagents, multi-provider, web UI.

## Stack

| layer | choice |
| ----- | ------ |
| TUI   | `ratatui` + `crossterm` |
| HTTP  | `reqwest` (rustls — no openssl dep) |
| JSON  | `serde_json` |
| Store | `rusqlite` (bundled — no system libsqlite3) |

## Layout

Multi-crate workspace:

- `crates/teleia-cli` — binary + TUI entry point
- `crates/teleia-llm` — Ollama streaming client + message/tool types
- `crates/teleia-tools` — `read` / `write` / `edit` / `bash` dispatch
- `crates/teleia-tools-bin` — shared tool dispatch binary
- `crates/teleia-store` — sqlite session persistence
- `crates/teleia-agent` — turn loop, 16-hop tool cap, stream-of-events

## License

MIT — see [LICENSE](LICENSE).
