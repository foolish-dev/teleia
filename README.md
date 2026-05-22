<p align="center">
  <img src="assets/banner.svg" alt="τέλεια — a minimal TUI coding agent" width="720">
</p>

<p align="center">
  <a href="https://github.com/foolish-dev/telia/actions/workflows/ci.yml"><img alt="ci" src="https://github.com/foolish-dev/telia/actions/workflows/ci.yml/badge.svg?branch=dev"></a>
  <a href="LICENSE"><img alt="MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
  <a href="https://buymeacoffee.com/cardoffoolm"><img alt="Buy me a coffee" src="https://img.shields.io/badge/buy%20me%20a%20coffee-cardoffoolm-FFDD00?logo=buymeacoffee&logoColor=1a1b26"></a>
</p>

<p align="center">
  <a href="https://huggingface.co/FoolDev/Thanatos-27B"><img alt="FoolDev/Thanatos-27B on Hugging Face" src="https://img.shields.io/badge/%F0%9F%A4%97-FoolDev%2FThanatos--27B-bb9af7?logo=huggingface&logoColor=1a1b26&labelColor=24283b"></a>
  <a href="https://huggingface.co/FoolDev/Janus-35B"><img alt="FoolDev/Janus-35B on Hugging Face" src="https://img.shields.io/badge/%F0%9F%A4%97-FoolDev%2FJanus--35B-7aa2f7?logo=huggingface&logoColor=1a1b26&labelColor=24283b"></a>
</p>

**λ τέλεια — a minimal TUI coding agent.** One binary, no daemon. Talks to a local Ollama or any of twenty-one cloud chat-completions endpoints (≈210 named models in the dropdown). Runs twenty-two built-in tools, hosts MCP servers, persists sessions to SQLite, resumes where you left off, and paints an OS-aware welcome banner on launch.

<p align="center">
  <img src="assets/screenshot.svg" alt="τέλεια TUI session" width="780">
</p>

## Install

```sh
# one-liner: clones, builds, drops `telia` into ~/.local/bin
curl -fsSL https://raw.githubusercontent.com/foolish-dev/telia/dev/install.sh | sh

# or via cargo
cargo install --git https://github.com/foolish-dev/telia telia-cli
```

`PREFIX=/usr/local/bin sh` overrides the install location. `cargo run --release` works from a workspace clone.

## Run

```sh
# default: local Ollama with hf.co/FoolDev/Thanatos-27B
telia

# cloud — uses $ANTHROPIC_API_KEY, or prompts and saves the key
telia --model claude-opus-4-7

# resume the last session
telia --resume     # alias: --continue, -r

# start in plan mode (read-only tools) or auto (no prompts)
telia --plan
telia --auto
```

Inside the TUI, switch models live with `/model NAME`; switch providers by changing the prefix. `Shift+Tab` cycles permission modes. Type `/help` to list every slash command.

## Permission modes

Three stances control tool execution — cycle with `Shift+Tab` or set explicitly:

| mode      | chip    | behaviour                                                                  | trigger              |
| --------- | ------- | -------------------------------------------------------------------------- | -------------------- |
| **PLAN**  | blue    | only read-only tools (`read` / `list` / `glob` / `grep` / `head` / `tail` / `tree` / `stat` / `diff` / `which` / `fetch` / `wc` / `sha256` / `date`) run; mutating tools short-circuit with a synthetic "blocked: plan mode" result | `/plan`, `--plan`    |
| **BUILD** | green (default) | every tool call pauses for `y` allow / `n` deny / `a` allow-all (auto)                          | `/build`, default    |
| **AUTO**  | red     | every tool dispatches immediately, no prompts                              | `/auto`, `--auto`    |

`a` at any approval prompt allow-alls and flips into AUTO for the rest of the session. `Esc` denies.

## Tools

Twenty-two built-ins, capped at a 16-hop loop per turn. MCP servers add more to the same dispatch loop.

| tool        | does                                                                              |
| ----------- | --------------------------------------------------------------------------------- |
| `read`      | read a file; output is syntax-highlighted via scope-aware syntect mapping         |
| `write`     | overwrite a file                                                                  |
| `edit`      | unique-substring replace inside a file                                            |
| `apply_patch` | unified diff via `/usr/bin/patch -pN`                                           |
| `bash`      | shell command (combined stdout/stderr, 30s timeout)                               |
| `list`      | directory listing, dirs suffixed with `/`                                         |
| `glob`      | shell-style glob, 200-match cap                                                   |
| `grep`      | Rust regex over a file or recursive dir walk (skips `target/`/`.git/`/etc)        |
| `head`      | first N lines of a file (default 40, cap 2000)                                    |
| `tail`      | last N lines (default 40, cap 2000)                                               |
| `tree`      | depth-limited dir tree (default 3, max 8), Unicode box-drawing branches            |
| `stat`      | type / size / mode / mtime                                                        |
| `diff`      | unified diff between two files via `/usr/bin/diff -u`                             |
| `which`     | first match on `$PATH` (with exec-bit check)                                      |
| `fetch`     | HTTP GET → response body (10s timeout, 1 MiB cap)                                 |
| `mkdir`     | `create_dir_all`, idempotent                                                      |
| `mv` / `cp` | refuse-to-clobber rename / copy                                                   |
| `touch`     | create-or-bump mtime (via `futimens` on unix)                                     |
| `wc`        | line / word / byte counts                                                         |
| `sha256`    | hex-encoded SHA-256 (hand-rolled, no hash crate)                                  |
| `date`      | unix + utc + local time                                                           |

## Providers

The model name picks the provider. Use `provider:NAME` to disambiguate names that collide with local Ollama models.

| pattern                            | provider     | env var                 |
| ---------------------------------- | ------------ | ----------------------- |
| `claude-*`                         | Anthropic    | `ANTHROPIC_API_KEY`     |
| `gpt-*`, `o1*`, `o3*`, `o4*`       | OpenAI       | `OPENAI_API_KEY`        |
| `gemini-*`                         | Google       | `GEMINI_API_KEY`        |
| `grok-*`                           | xAI          | `XAI_API_KEY`           |
| `deepseek-*`                       | DeepSeek     | `DEEPSEEK_API_KEY`      |
| `mistral-*`, `codestral-*`         | Mistral      | `MISTRAL_API_KEY`       |
| `command-*`                        | Cohere       | `COHERE_API_KEY`        |
| `sonar*`                           | Perplexity   | `PERPLEXITY_API_KEY`    |
| `jamba-*`                          | AI21         | `AI21_API_KEY`          |
| `groq:NAME`                        | Groq         | `GROQ_API_KEY`          |
| `openrouter:NAME`                  | OpenRouter   | `OPENROUTER_API_KEY`    |
| `together:NAME`                    | Together AI  | `TOGETHER_API_KEY`      |
| `fireworks:NAME`                   | Fireworks    | `FIREWORKS_API_KEY`     |
| `cerebras:NAME`                    | Cerebras     | `CEREBRAS_API_KEY`      |
| `hyperbolic:NAME`                  | Hyperbolic   | `HYPERBOLIC_API_KEY`    |
| `nvidia:NAME`                      | NVIDIA NIM   | `NVIDIA_API_KEY`        |
| `anyscale:NAME`                    | Anyscale     | `ANYSCALE_API_KEY`      |
| `lepton:NAME`                      | Lepton       | `LEPTON_API_KEY`        |
| `deepinfra:NAME`                   | DeepInfra    | `DEEPINFRA_API_KEY`     |
| `sambanova:NAME`                   | SambaNova    | `SAMBANOVA_API_KEY`     |
| anything else                      | local Ollama | _none_                  |

The `/model` dropdown pre-populates with ~210 named models across all twenty-one providers. `Tab` accepts, type to filter; any name is accepted including ones not in the catalog.

**Keys.** `--api-key KEY` overrides the env-var fallback. Missing a key at startup or after `/model NAME`? telia prompts with hidden input (`rpassword` at startup, in-TUI masked prompt mid-session) and saves the entered key under `prefs.api_key:<ENV_VAR>` so the next launch picks it up. `/keys` lists every provider + status; `/key PROVIDER` opens the prompt on demand. Env-var values win when set, so exporting overrides the saved one.

**Ollama pre-flight.** When the resolved endpoint is local Ollama, missing models trigger an interactive `pull from Ollama now? [Y/n]` with an animated progress bar. `--pull-yes` auto-confirms; `--no-pull` skips. Non-TTY stdin (scripts, CI, pipes) auto-confirms.

## Features

- **OS-aware welcome banner** — `/etc/os-release` (or `cfg!(target_os = …)` on macOS / Windows / FreeBSD) picks a pixel-art panel: Arch / Ubuntu / Debian / Fedora / Alpine / NixOS / Gentoo / Void / openSUSE / macOS / Windows / FreeBSD or a generic Linux fallback. Lambda mark + `powered by λ τέλεια` watermark below.
- **Modes** — Insert (default), Normal (`Esc`), Command (`:`). The status bar chip and input border colour signal the active mode.
- **Slash commands** — `/reset`, `/clear`, `/save NAME`, `/load NAME`, `/delete NAME`, `/list`, `/model [NAME]`, `/key PROVIDER`, `/keys`, `/mcps`, `/lsps`, `/plan`, `/build`, `/auto`, `/prompt [NAME]`, `/theme [NAME]`, `/notify [on|off]`, `/show`, `/help`, `/quit`.
- **Vim commands** — `:q`/`:x`/`:qa`/`:qall` → quit, `:w`/`:wa` → save, `:e`/`:l` → load, `:colo`/`:theme` → theme, `:!CMD` → shell out, `:cd PATH`, `:pwd`, `:noh` (clear drag-select), `:version`, `:r FILE` (read into chat), `:enew` → reset. `EX_COMMANDS` dropdown autocompletes via Tab.
- **Prompt templates** — `/prompt` lists 10 starters (`review`, `debug`, `explain`, `refactor`, `test`, `docs`, `security`, `perf`, `commit`, `plan`); `/prompt NAME` drops the template into the input box.
- **Autocomplete** — ghost-text completion as you type + a drop-down listing every match. Up to 10 entries visible at a time; longer lists scroll via Up/Down keeping the selected row in view. Title shows total count (`models · 211`).
- **Input editing** — readline-style `Left`/`Right`/`Home`/`End`, `Backspace`/`Delete`, `Ctrl+A/E/U/W`; `Up`/`Down` recall prior submissions. Typing stays live during a streaming turn; the input box grows vertically with content (cap = 1/3 of screen).
- **Smart auto-scroll** — defaults to following the bottom. Scrolling up disengages bottom-following so streaming deltas don't yank you back down mid-read; reaching the bottom (scroll 0) via any down gesture re-engages. `G` jumps to bottom + re-engages. Scroll math uses `Paragraph::line_count` so it always lands at the last rendered visual row.
- **Scrollbar** — right-edge thumb in `th.purple` over a `th.dim` track; only renders when there's overflow.
- **Drag-select** — left-click + drag in the chat area highlights cells; release copies via [`arboard`](https://crates.io/crates/arboard) (Wayland / X11 / macOS / Windows). Works during streaming.
- **Themes** — `tokyo-night` (default), `catppuccin`, `dracula`. Code highlighting follows the active theme (scope-aware mapping via syntect).
- **Token tracker** — status bar shows `↑prompt ↓completion` totals for the session.
- **Desktop notifications** — Linux `notify-send`, macOS `osascript`, Windows BurntToast (silently no-op if not installed). Toggle with `/notify on|off`.
- **Streaming** — tokens render live via SSE with a blinking caret, braille spinners on tool calls, and a running-dot "thinking" indicator. `Esc` / `Ctrl+C` mid-turn aborts.

## Session persistence

Everything important survives a restart.

- **Messages** stream to SQLite as they arrive — no "unsaved" state. Store lives at `$XDG_DATA_HOME/telia/telia.sqlite` (Linux), `~/Library/Application Support/telia/telia.sqlite` (macOS), `%APPDATA%\telia\telia.sqlite` (Windows). `$XDG_DATA_HOME` wins on any platform when set.
- **Auto-bookmarks** — every launch is tagged `last`; `/reset` rotates the outgoing session to `prev`. So `telia --resume` always picks up where you left off, and the run before that is recoverable with `/load prev`.
- **Aliases** — `/save NAME` + `/load NAME` for sessions you want to keep by hand.
- **Sticky preferences** — theme, `/notify` toggle, permission mode, active model, and per-provider API keys all persist (CLI flags override). Launching `telia` with no `--model` picks up wherever you last `/model`-ed.
- **Input history** — last 500 submissions reload into the `Up`/`Down` recall buffer at startup, deduped against the most recent entry.

## Configuration

Optional TOML at `$XDG_CONFIG_HOME/telia/config.toml` (Linux), or the OS-native config dir on macOS / Windows.

```toml
# Custom LLM endpoint. The name shows up in /model alongside the
# pre-populated cloud + Ollama-cached entries.
[llms.groq-llama]
base_url    = "https://api.groq.com/openai/v1"
api_key_env = "GROQ_API_KEY"

# MCP server. Spawned at startup; tools join the agent's catalogue.
# `env` overrides the parent process for this child only.
[mcps.filesystem]
command = "npx"
args    = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[mcps.github]
command = "docker"
args    = ["run", "-i", "--rm", "ghcr.io/github/github-mcp-server"]

[mcps.github.env]
GITHUB_PERSONAL_ACCESS_TOKEN = "ghp_…"

# LSP server. Spawned + initialised at startup (handshake only);
# tool exposure (hover / definition / diagnostics) is still TODO.
[lsps.rust]
command       = "rust-analyzer"
root_patterns = ["Cargo.toml"]
```

**MCP**: telia speaks newline-delimited JSON-RPC over stdio — `initialize` + `notifications/initialized` + `tools/list` + `tools/call` + `resources/list`. Spawn failures stderr-warn but don't abort boot. `/mcps` lists running servers with tool + resource counts.

**LSP**: real Content-Length-framed JSON-RPC client. Each configured server gets `initialize` + `initialized`. `/lsps` shows live status + advertised `serverInfo`. Document sync + hover/definition/diagnostics tools are not wired yet.

## Layout

```
crates/
  telia-cli       # `telia` binary + TUI + MCP client + LSP client
  telia-agent     # turn loop, 16-hop cap, permission gate, event stream
  telia-llm       # chat / pull / tags streaming, provider detection
  telia-tools     # 22 built-in tools
  telia-tools-bin # same dispatch as a stdin/stdout CLI
  telia-store     # sqlite session + prefs + input-history persistence
```

Built on `ratatui` + `crossterm`, `reqwest` (rustls), `rusqlite` (bundled), `syntect` (no C deps), `arboard`, `rpassword`.

## License

MIT — see [LICENSE](LICENSE).
