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

## Run

```sh
cargo run --release
```

With options:

```sh
cargo run --release -- \
  --model hf.co/FoolDev/Janus-35B:Q4_K_M \
  --base-url http://127.0.0.1:11434
```

Or drop the `teleia` binary onto your PATH:

```sh
cargo install --path crates/teleia-cli
```

Requires Ollama running locally with a tool-capable model pulled. Default model: `hf.co/FoolDev/Thanatos-27B:Q4_K_M`.

## Features

- **Streaming** — tokens render live via SSE.
- **Tools** — `read` / `write` / `edit` / `bash`, with a 16-hop loop cap.
- **Slash commands** — `/reset`, `/save NAME`, `/load NAME`, `/help`.
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
