# Teleia (Python)

Stdlib-only Python implementation. No pip install required to run from source — just Python 3.11+.

## Run

```sh
python -m teleia
# or
python -m teleia --model hf.co/FoolDev/Janus-35B:Q4_K_M
```

## Install (optional)

```sh
pip install .
teleia
```

## Stack

| layer | choice         |
| ----- | -------------- |
| TUI   | `curses` (stdlib) |
| HTTP  | `urllib.request` (stdlib) |
| JSON  | `json` (stdlib) |
| Store | `sqlite3` (stdlib) |
