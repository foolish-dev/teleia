# Teleia (Go)

Bubbletea TUI, stdlib HTTP, pure-Go SQLite.

## Build

```sh
go build -o teleia ./cmd/teleia
```

## Run

```sh
./teleia
# or
go run ./cmd/teleia --model hf.co/FoolDev/Janus-35B:Q4_K_M
```

## Stack

| layer | choice |
| ----- | ------ |
| TUI   | `charmbracelet/bubbletea` + `lipgloss` |
| HTTP  | `net/http` (stdlib) |
| JSON  | `encoding/json` (stdlib) |
| Store | `modernc.org/sqlite` (pure-Go, no cgo) |
