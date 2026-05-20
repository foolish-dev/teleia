# Teleia (Lua)

Pure-Lua + shell-out to `curl` and `sqlite3`. Vendored JSON, no luarocks deps.

## Run

```sh
lua teleia.lua
# or
lua teleia.lua --model hf.co/FoolDev/Janus-35B:Q4_K_M
```

## Requires

- Lua 5.4+ (or LuaJIT)
- `curl` and `sqlite3` binaries in PATH (Ollama traffic + storage)

## Stack

| layer | choice |
| ----- | ------ |
| TUI   | ANSI colors + `io.read` (line-based, no full TUI library) |
| HTTP  | shell-out to `curl` |
| JSON  | vendored `teleia/json.lua` (rxi-style, ~150 LoC) |
| Store | shell-out to `sqlite3` CLI |

## Note

Lua's TUI ecosystem is much thinner than Rust/Python/Go's. This implementation
uses line-based stdio with ANSI styling instead of a notcurses/lua-tui dep, to
keep the impl pure-Lua and luarocks-free.
