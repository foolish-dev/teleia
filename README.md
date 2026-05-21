<p align="center">
  <img src="assets/banner.svg" alt="Teleia — the distilled coding agent" width="720">
</p>

<p align="center">
  <a href="https://github.com/foolish-dev/Teleia/actions/workflows/ci.yml"><img alt="ci" src="https://github.com/foolish-dev/Teleia/actions/workflows/ci.yml/badge.svg?branch=dev"></a>
  <a href="LICENSE"><img alt="MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
</p>

Minimal TUI coding agent — one MVP, four parallel implementations. Each talks to a local Ollama (OpenAI-chat-compatible) endpoint, runs four tools (`read` / `write` / `edit` / `bash`), and persists sessions to SQLite.

## Implementations

| dir | language | TUI | HTTP | store |
| --- | -------- | --- | ---- | ----- |
| [`rust/`](rust/)     | Rust 1.95+   | ratatui + crossterm        | reqwest          | rusqlite              |
| [`python/`](python/) | Python 3.11+ | curses (stdlib)            | urllib (stdlib)  | sqlite3 (stdlib)      |
| [`go/`](go/)         | Go 1.22+     | bubbletea + lipgloss       | net/http         | modernc.org/sqlite    |
| [`lua/`](lua/)       | Lua 5.4+     | ANSI + io.read (line mode) | shell-out `curl` | shell-out `sqlite3`   |
| [`bun/`](bun/)       | Bun 1.3+ / TS | ink (React)               | global `fetch`   | `bun:sqlite`          |

All four implementations share scope and behaviour: same system prompt, same four tools, same 16-hop tool-call cap, same default model (`hf.co/FoolDev/Thanatos-27B:Q4_K_M`). They write to the same on-disk sqlite at `$XDG_DATA_HOME/teleia/teleia.sqlite`.

## Quick start

```sh
# rust
cd rust && cargo run --release

# python
cd python && python -m teleia

# go
cd go && go run ./cmd/teleia

# lua
cd lua && lua teleia.lua

# bun
cd bun && bun install && bun run start
```

Requires Ollama running at `127.0.0.1:11434` with a tool-capable model installed.

## Features (v1)

- **Streaming**: tokens render live as the model produces them (SSE).
- **Tool loop**: read / write / edit / bash, with a 16-hop cap.
- **Slash commands**: `/reset`, `/save NAME`, `/load NAME`, `/help`.
- **Scrollback**: ↑ / ↓ (PageUp / PageDown) navigate history in rust/python/go/bun. Lua relies on terminal scrollback.
- **Persistent sessions**: every message persists to `$XDG_DATA_HOME/teleia/teleia.sqlite`. Save/load by alias across runs and across implementations.
- **Single provider**: Ollama (OpenAI-chat-compatible), one model per process.

Not yet: MCP, LSP, plugins, subagents, multi-provider, web UI.

## Why polyglot

Same problem, four languages, side-by-side. Useful for comparing idiom, terseness,
dependency posture, and TUI ergonomics across stacks.

## License

MIT — see [LICENSE](LICENSE).
