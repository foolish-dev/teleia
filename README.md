<p align="center">
  <img src="assets/banner.svg" alt="Teleia — the distilled coding agent" width="720">
</p>

<p align="center">
  <a href="https://github.com/foolish-dev/Teleia/actions/workflows/ci.yml"><img alt="ci" src="https://github.com/foolish-dev/Teleia/actions/workflows/ci.yml/badge.svg?branch=dev"></a>
  <a href="LICENSE"><img alt="MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
</p>

Minimal TUI coding agent in Rust. Talks to a local Ollama (OpenAI-chat-compatible) endpoint, runs tools, persists sessions to SQLite.

## Build

```sh
cargo build --release
```

## Run

Requires Ollama on `127.0.0.1:11434` with a tool-capable model.

```sh
cargo run --release
# or pick a model:
cargo run --release -- --model hf.co/FoolDev/Janus-35B:Q4_K_M
```

Default model: `hf.co/FoolDev/Thanatos-27B:Q4_K_M`.

## Layout

| crate          | purpose                                       |
| -------------- | --------------------------------------------- |
| `teleia-cli`   | clap entry, ratatui TUI, input loop           |
| `teleia-llm`   | Ollama (OpenAI-chat) client + message types   |
| `teleia-agent` | tool-call loop, session orchestration         |
| `teleia-tools` | read / write / edit / bash                    |
| `teleia-store` | rusqlite session + message persistence        |

## Scope

v0 only: single provider, single session per run, four tools, turn-based UI.
Not in v0: MCP, LSP, plugins, subagents, streaming responses, multi-provider.
