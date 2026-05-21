# Teleia (Rust)

Tokio async runtime, ratatui TUI, reqwest with rustls, rusqlite with bundled libsqlite3.

## Build

```sh
cargo build --release
```

## Run

```sh
cargo run --release
# or with options
cargo run --release -- --model hf.co/FoolDev/Janus-35B:Q4_K_M
```

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
- `crates/teleia-store` — sqlite session persistence
- `crates/teleia-agent` — turn loop, 16-hop tool cap, stream-of-events
