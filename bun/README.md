# Teleia (Bun)

Bun runtime, TypeScript source. Ink TUI, native `fetch`, `bun:sqlite`.

## Install

```sh
bun install
```

## Run

```sh
bun run start
# or with options
bun src/index.ts --model hf.co/FoolDev/Janus-35B:Q4_K_M
```

## Stack

| layer | choice |
| ----- | ------ |
| TUI   | `ink` + React |
| HTTP  | global `fetch` (Bun) |
| JSON  | native |
| Store | `bun:sqlite` (built-in) |

## Note

This is the only non-systems-language impl. Added on explicit request alongside the Rust/Python/Go/Lua siblings.
