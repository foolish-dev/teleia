<p align="center">
  <img src="assets/banner.svg" alt="τέλεια — a minimal TUI coding agent" width="720">
</p>

<p align="center">
  <a href="https://github.com/foolish-dev/teleia/actions/workflows/ci.yml"><img alt="ci" src="https://github.com/foolish-dev/teleia/actions/workflows/ci.yml/badge.svg?branch=dev"></a>
  <a href="LICENSE"><img alt="MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
  <a href="https://buymeacoffee.com/cardoffoolm"><img alt="Buy me a coffee" src="https://img.shields.io/badge/buy%20me%20a%20coffee-cardoffoolm-FFDD00?logo=buymeacoffee&logoColor=1a1b26"></a>
</p>

<p align="center">
  <a href="https://huggingface.co/FoolDev/Janus-35B"><img alt="FoolDev/Janus-35B on Hugging Face" src="https://img.shields.io/badge/%F0%9F%A4%97-FoolDev%2FJanus--35B-7aa2f7?logo=huggingface&logoColor=1a1b26&labelColor=24283b"></a>
  <a href="https://huggingface.co/FoolDev/Thanatos-27B"><img alt="FoolDev/Thanatos-27B on Hugging Face" src="https://img.shields.io/badge/%F0%9F%A4%97-FoolDev%2FThanatos--27B-bb9af7?logo=huggingface&logoColor=1a1b26&labelColor=24283b"></a>
</p>

**λ τέλεια — a minimal TUI coding agent (rewritten in Rust btw).** One binary, no daemon. Talks to a local Ollama or any of twenty cloud chat-completions endpoints (≈210 named models in the dropdown). Runs one hundred fifty-nine built-in tools (read/write/edit/multi_edit/rm + bash + list/glob/grep/find + head/tail/tree/stat/diff/which/fetch/wc/sha256/date + apply_patch/mkdir/mv/cp/touch/symlink/readlink/hardlink/chown/chmod/truncate/fallocate/mktemp/pathinfo/du/realpath + pwd/file_type/exists/is_dir_empty/path_join/path_normalize/relpath/split_file/join_files/cat + slice/sort/cut/comm/column/tr/expand/dedent/strings/count_matches/uniq/paste/fold/tac/indent/join/squeeze_blank/reflow/trim/seq + lint/format/typecheck/test/test_one/build/bench/coverage/run_bin/run_script/clippy_fix/cargo_metadata/cargo_tree/cargo_add/cargo_search/cargo_expand/make_target/npm_install/audit/cloc + git/git_branch/git_stash/git_remote/git_grep/git_log_file/git_apply/git_reset/git_checkout_file/git_status_json/changelog_gen + env/env_run/nproc/os_release/epoch/calc/kill/pid/sleep + replace/json/jsonl/json_diff/json_merge/json_validate/json_format/json_keys/json_flatten/ndjson_query/dotenv_parse/ini_to_json/ndjson_to_json/properties_to_json/html_to_text + base64/base64url/base32/base85/hex/url_encode/rot13/unicode_escape + md5/sha1/crc32/crc32c/adler32/crc16/hash/hmac_sha256/hmac_verify/hash_verify/jwt_decode/jwt_verify_hmac/jwt_sign_hmac/totp/secret_scan/hexdump + web_search/download/http_request/http_form_post/follow_redirects/tcp_check/port_scan/dns_resolve/public_ip/local_ip/ip_geolocate/mac_lookup + todo_write), hosts MCP servers, persists sessions to SQLite, resumes where you left off, and paints an OS-aware welcome banner on launch.

<p align="center">
  <img src="assets/screenshot.svg" alt="τέλεια TUI session" width="780">
</p>

## Install

Auto-detects your platform, downloads the latest prebuilt binary from GitHub Releases, falls back to a cargo source build if no prebuilt exists. Prebuilts cover Linux (x86_64 · aarch64 · armv7 · i686 · riscv64 · ppc64le · s390x), macOS (Intel + Apple Silicon), Windows (x86_64 · ARM64 · i686, MSVC + MinGW), and FreeBSD.

```sh
# Linux / macOS — drops `teleia` into ~/.local/bin
curl -fsSL https://raw.githubusercontent.com/foolish-dev/teleia/dev/install.sh | sh
```

```powershell
# Windows (PowerShell) — drops `teleia.exe` into %USERPROFILE%\.local\bin
irm https://raw.githubusercontent.com/foolish-dev/teleia/dev/install.ps1 | iex
```

```sh
# any platform with Rust installed
cargo install --git https://github.com/foolish-dev/teleia teleia-cli
```

After install it verifies the binary, appends the install dir to your shell rc / Windows user PATH if needed (idempotent), and hints at the next step for either an Ollama or a cloud-provider session.

Overrides (env vars before the pipe on Unix, `$env:NAME = 'value'` on Windows):
`PREFIX` install location (default `~/.local/bin`) ·
`TAG` pin a release tag (default `latest`) ·
`FROM_SOURCE=1` skip the prebuilt download and cargo-build from source ·
`BRANCH` source-build branch (default `dev`) ·
`NO_PATH=1` skip the PATH edit ·
`NO_OLLAMA_HINT=1` skip the post-install nudge ·
`AUTO_INSTALL=1` bootstrap rustup automatically if cargo is missing on a source build (no prompt) ·
`NO_AUTO_INSTALL=1` never bootstrap; fall back to the manual-install error.

On a source build with no Rust toolchain present, the installer otherwise prompts on `/dev/tty` (Unix) or `Read-Host` (Windows) before running the official rustup-init.

`cargo run --release` works from a workspace clone.

## Run

```sh
# default: local Ollama with hf.co/FoolDev/Thanatos-27B
teleia

# cloud — uses $ANTHROPIC_API_KEY, or prompts and saves the key
teleia --model claude-fable-5

# resume the last session
teleia --resume     # alias: --continue, -r

# start in plan mode (read-only tools) or auto (no prompts)
teleia --plan
teleia --auto
```

Inside the TUI, switch models live with `/model NAME`; switch providers by changing the prefix. `Shift+Tab` cycles permission modes. Type `/help` to list every slash command.

## Permission modes

Three stances control tool execution — cycle with `Shift+Tab` or set explicitly:

| mode      | chip    | behaviour                                                                  | trigger              |
| --------- | ------- | -------------------------------------------------------------------------- | -------------------- |
| **PLAN**  | blue    | only read-only tools (`read` / `list` / `glob` / `grep` / `head` / `tail` / `tree` / `stat` / `diff` / `which` / `fetch` / `wc` / `sha256` / `date` / `lint` / `typecheck` / `test` / `env` / `json` / `base64` / `hexdump` / `du` / `realpath` / `web_search` / `find` / `readlink` / `pathinfo` / `slice` / `sort` / `cut` / `comm` / `column` / `tr` / `expand` / `dedent` / `strings` / `count_matches` / `nproc` / `os_release` / `epoch` / `calc` / `tcp_check` / `dns_resolve` / `cargo_metadata` / `cargo_tree` / `cloc` / `json_diff` / `json_merge` / `jsonl` / `dotenv_parse` / `ini_to_json` / `ndjson_to_json` / `md5` / `sha1` / `crc32` / `hash` / `hmac_sha256` / `hex` / `base32` / `url_encode` / `hash_verify` / `jwt_decode` / `pwd` / `file_type` / `path_join` / `path_normalize` / `relpath` / `is_dir_empty` / `exists` / `uniq` / `paste` / `fold` / `tac` / `indent` / `join` / `squeeze_blank` / `reflow` / `trim` / `crc32c` / `adler32` / `crc16` / `base64url` / `base85` / `rot13` / `json_validate` / `json_format` / `ndjson_query` / `properties_to_json` / `html_to_text` / `json_keys` / `json_flatten` / `unicode_escape` / `port_scan` / `public_ip` / `local_ip` / `ip_geolocate` / `follow_redirects` / `mac_lookup` / `pid` / `git_remote` / `list_scripts` / `cargo_expand` / `changelog_gen` / `git_grep` / `git_log_file` / `cargo_search` / `audit` / `git_status_json` / `jwt_verify_hmac` / `jwt_sign_hmac` / `hmac_verify` / `totp` / `secret_scan` / `sleep` / `cat` / `seq`, plus `git`'s read-only subcommands `status`/`diff`/`log`/`show`/`blame`/`diff_stat`) run; mutating tools short-circuit with a synthetic "blocked: plan mode" result | `/plan`, `--plan`    |
| **BUILD** | green (default) | every tool call pauses for `y` allow / `n` deny / `a` allow-all (auto)                          | `/build`, default    |
| **AUTO**  | red     | every tool dispatches immediately, no prompts                              | `/auto`, `--auto`    |

`a` at any approval prompt allow-alls and flips into AUTO for the rest of the session. `Esc` denies.

## Keybindings

Four modes: **Insert** (default — type and edit), **Normal** (`Esc` to enter; vim motions), **Visual** (`v` from Normal; charwise selection over the chat scrollback), **Command** (`:` from Normal; vim ex-commands). The status-bar chip and input border colour signal the active mode.

### Insert mode

| key                       | does                                                                |
| ------------------------- | ------------------------------------------------------------------- |
| `Tab`                     | accept autocomplete / ghost suggestion                              |
| `Shift+Tab`               | cycle permission mode (PLAN → BUILD → AUTO)                         |
| `Enter`                   | submit                                                              |
| `Esc`                     | dismiss menu → clear drag-select → switch to Normal                 |
| `Up` / `Down`             | history recall, or menu nav, or 1-line scroll (in priority order)   |
| `PageUp` / `PageDown`     | scroll chat 5 lines                                                 |
| `Left` / `Right`          | cursor                                                              |
| `Home` / `End` / `Ctrl+A` / `Ctrl+E` | line start / end                                         |
| `Ctrl+U`                  | clear input                                                         |
| `Ctrl+W`                  | delete word before cursor                                           |
| `Backspace` / `Delete`    | delete char before / at cursor                                      |
| `Ctrl+Shift+V`            | paste from system clipboard (bracketed paste)                       |
| middle-click              | paste from system clipboard (X11-style; works in any mode)          |
| drag-select               | copy highlighted chat text (Wayland: wl-copy; else arboard; → tmux → OSC 52) |
| `Ctrl+C`                  | abort the streaming turn                                            |

### Normal mode (`Esc` to enter)

| key                          | does                                                  |
| ---------------------------- | ----------------------------------------------------- |
| `i` / `a` / `I` / `A`        | back to Insert (cursor / after / line start / line end) |
| `v`                          | Visual mode (charwise selection in chat scrollback)   |
| `:`                          | Command mode                                          |
| `h` / `l` or `Left` / `Right` | cursor                                                |
| `0` / `$` or `Home` / `End`  | line start / end                                      |
| `w` / `b` / `e`              | word forward / back / end                             |
| `j` / `k` or `Down` / `Up`   | scroll chat 1 line                                    |
| `Ctrl+D` / `Ctrl+U`          | half-page scroll                                      |
| `PageDown` / `PageUp`        | 5-line scroll                                         |
| `gg` / `G`                   | jump to oldest / latest entries (G re-engages follow) |
| `x` / `X`                    | delete char at / before cursor                        |
| `D`                          | delete from cursor to end of input                    |
| `dd`                         | clear input                                           |
| `Tab`                        | accept suggestion                                     |
| `Shift+Tab`                  | cycle permission mode                                 |
| `Enter`                      | submit                                                |

### Visual mode (`v` from Normal)

Charwise selection over the rendered chat scrollback — the keyboard equivalent of a left-click drag. The selection anchor drops at the top-left of the inner chat area; the hardware terminal cursor tracks the selection head so you can see where motions are taking you. `y` copies the highlighted text through the same channel stack as mouse drag-select (Wayland: wl-copy; else arboard; → tmux → OSC 52) and returns to Normal with the highlight still visible. `Esc` cancels and clears the highlight.

| key                          | does                                                  |
| ---------------------------- | ----------------------------------------------------- |
| `h` / `l` or `Left` / `Right` | move selection head left / right (anchor stays)      |
| `j` / `k` or `Down` / `Up`   | move selection head down / up                         |
| `0` / `Home`                 | snap selection head to row start                      |
| `$` / `End`                  | snap selection head to row end                        |
| `y`                          | yank selection to clipboard, exit to Normal           |
| `Esc`                        | cancel selection, exit to Normal                      |

### Command mode (vim ex-commands)

From Normal mode, `:` opens a command line. The dropdown filters `EX_COMMANDS` by prefix; `Tab` / `Shift+Tab` cycle, `Enter` runs, `Esc` (or `Backspace` on an empty buffer) cancels. Vim aliases dispatch to the same handlers as the slash-commands:

| ex command                                              | does                                                  |
| ------------------------------------------------------- | ----------------------------------------------------- |
| `:q` / `:qa` / `:qall` / `:x` / `:exit`                 | quit                                                  |
| `:w NAME` / `:wa` / `:wq NAME`                          | save session (no quit on `:wq`)                       |
| `:e NAME` / `:l NAME` / `:edit NAME` / `:load NAME`     | load session                                          |
| `:d NAME` / `:bd NAME` / `:delete NAME`                 | delete session                                        |
| `:ls` / `:list`                                         | list sessions                                         |
| `:enew` / `:new` / `:reset`                             | reset session                                         |
| `:clear`                                                | clear scrollback                                      |
| `:colo NAME` / `:colorscheme NAME` / `:theme NAME`      | theme                                                 |
| `:cd PATH` / `:pwd`                                     | working directory                                     |
| `:f` / `:file` / `:show` / `:info`                      | session info                                          |
| `:model NAME`                                           | switch model                                          |
| `:notify on|off` / `:transparent on|off`                | toggles                                               |
| `:key PROVIDER` / `:keys`                               | api keys                                              |
| `:mcps` / `:lsps` / `:tools`                            | tool inventory                                        |
| `:plan` / `:build` / `:auto` / `:ask`                   | permission mode                                       |
| `:copy` / `:yank` / `:y`                                | copy last assistant reply                             |
| `:prompt NAME`                                          | prompt template                                       |
| `:help` / `:h` / `:version`                             | misc                                                  |

### Mouse

| gesture                       | does                                                       |
| ----------------------------- | ---------------------------------------------------------- |
| left-click + drag in chat     | drag-select; release copies via arboard / tmux / OSC 52    |
| scroll wheel                  | scroll chat 3 lines per tick                               |

## Tools

Twenty-eight built-ins, dispatched in a tool-call loop that runs until the model stops requesting tools. MCP servers add more to the same dispatch loop.

| tool        | does                                                                              |
| ----------- | --------------------------------------------------------------------------------- |
| `read`      | read a file; output is syntax-highlighted via scope-aware syntect mapping         |
| `write`     | overwrite a file                                                                  |
| `edit`      | unique-substring replace inside a file; `replace_all: true` substitutes every occurrence |
| `multi_edit` | apply a sequence of edits to one file atomically — writes only if every step succeeds   |
| `rm`        | delete a file, or a directory tree with `recursive: true`; refuses `/`            |
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
| `lint`      | `cargo clippy` / `ruff` (→ `flake8`) / `eslint` / `go vet` / `shellcheck` by extension |
| `format`    | `rustfmt` / `ruff format` (→ `black`) / `prettier` / `gofmt` (writes in place)    |
| `typecheck` | `cargo check` / `mypy` / `tsc --noEmit` / `go build` by extension                 |
| `test`      | `cargo test` / `pytest` / `go test ./...` / `npm test` by extension               |
| `git`       | bounded subcommands: `status` / `diff` / `log` / `add` / `commit` / `show` / `blame` / `diff_stat` |
| `symlink`   | create a symlink (refuse-to-clobber)                                              |
| `env`       | read one env var by `name`, or list all as sorted `KEY=VALUE`                     |
| `replace`   | regex find/replace in one file (`$1` capture refs); `all: false` for first-only  |
| `json`      | extract a value from a JSON file by RFC 6901 pointer (`/a/b/0`)                   |
| `base64`    | encode/decode a UTF-8 string (`decode: true` to decode)                          |
| `hexdump`   | hex + ASCII dump of a file's first N bytes (default 256, cap 4096)               |
| `du`        | recursive byte size of a file or directory tree (symlinks not followed)          |
| `realpath`  | canonicalize a path (resolves `.` / `..` / symlinks; must exist)                 |
| `web_search` | DuckDuckGo (keyless, HTML endpoint) → numbered title / url / snippet results     |
| `find`      | recursive search by name-glob / path-regex / type / size / mtime / depth          |
| `readlink`  | read a symlink's raw target                                                       |
| `hardlink`  | create a hard link (refuse-to-clobber)                                            |
| `chmod`     | set Unix mode bits (`755`); on Windows toggles the read-only attribute            |
| `truncate`  | set a file's length (grow with zeros / shrink)                                    |
| `mktemp`    | create a unique temp file (or dir) and return its path                           |
| `pathinfo`  | split a path into parent / filename / stem / extension                           |
| `slice`     | extract an inclusive 1-based line range (optional line numbers)                  |
| `sort`      | sort a file's lines (numeric / reverse / unique)                                 |
| `cut`       | extract delimited fields / character ranges per line                             |
| `comm`      | compare two sorted files → common / only-in-A / only-in-B                        |
| `strings`   | printable ASCII runs from a binary (min length)                                  |
| `column`    | align whitespace/delimited columns into a table                                  |
| `tr`        | translate or delete character sets                                               |
| `expand`    | tabs ↔ spaces (expand / unexpand)                                                |
| `dedent`    | strip common leading indentation                                                 |
| `count_matches` | count regex matches in a file (total / per-line)                             |
| `epoch`     | convert between Unix timestamps and ISO-8601                                     |
| `calc`      | evaluate an arithmetic expression (+ - * / % ** parens)                          |
| `nproc`     | logical CPU count                                                                |
| `os_release` | OS / arch / family summary                                                       |
| `kill`      | send a signal to a pid (unix); terminate on Windows                             |
| `tcp_check` | test TCP connectivity to host:port with a timeout                                |
| `dns_resolve` | resolve a hostname to IP addresses                                              |
| `download`  | fetch a URL to a file                                                            |
| `http_request` | HTTP request with method / headers / body → status + headers + body            |
| `cargo_metadata` | `cargo metadata` as JSON                                                     |
| `cargo_tree` | `cargo tree` dependency graph                                                    |
| `test_one`  | run a single test by name (cargo / pytest / go / npm)                           |
| `cloc`      | count lines of code by language across a tree                                    |
| `json_diff` | structural diff of two JSON files                                                |
| `json_merge` | deep-merge two JSON files                                                        |
| `jsonl`     | extract a JSON Pointer from each line of a JSONL file                            |
| `dotenv_parse` | parse a `.env` file to JSON                                                    |
| `ini_to_json` | parse INI to JSON                                                               |
| `ndjson_to_json` | fold NDJSON into a JSON array                                                |
| `md5` / `sha1` / `crc32` | hand-rolled checksums of a file or literal string                      |
| `hash`      | SHA-2 (`sha224` / `sha384` / `sha512`) digest                                    |
| `hmac_sha256` | HMAC-SHA256 of a message under a key (webhook signatures)                       |
| `hash_verify` | check a file/string against an expected SHA-256                                 |
| `hex` / `base32` | encode/decode a string (hex / RFC 4648 base32)                              |
| `url_encode` | percent-encode/decode a string (RFC 3986)                                        |
| `jwt_decode` | decode a JWT's header + payload (no signature check)                            |
| `chown` | Change the owner (and optionally group) of a file or directory (Unix only) |
| `pwd` | Print the current working directory as an absolute path. |
| `file_type` | Identify a file's type by inspecting its contents |
| `path_join` | Join path segments into a single path using the OS separator |
| `path_normalize` | Lexically normalize a path string: collapse `.` segments and resolve `..` segme... |
| `relpath` | Compute the relative path from `base` to `path` (pure string/component math, no... |
| `split_file` | Split a file into numbered chunk files by byte-count or line-count |
| `join_files` | Concatenate the byte contents of several input files, in order, into a single o... |
| `is_dir_empty` | Report whether a directory contains no entries |
| `exists` | Check whether a path exists on disk without erroring when it doesn't |
| `fallocate` | Preallocate or resize a file to an exact byte size, creating it if it does not ... |
| `uniq` | Collapse runs of ADJACENT equal lines in a file (like GNU `uniq`), order-preser... |
| `paste` | Merge lines of multiple files side by side, like Unix `paste` |
| `fold` | Hard-wrap each line of a file to at most `width` characters (default 80), like ... |
| `tac` | Print the lines of a file in reverse order (last line first), like the Unix `tac` |
| `indent` | Prepend an indentation prefix to each line of a text file |
| `join` | Relational inner join of two text files on a shared key field (like the Unix `j... |
| `squeeze_blank` | Read a text file and collapse every run of two or more consecutive blank (empty... |
| `reflow` | Reflow text to a maximum line width using greedy word-wrapping |
| `trim` | Trim whitespace from each line of text |
| `crc32c` | CRC-32C (Castagnoli, iSCSI/ext4/SSE4.2 polynomial) of a file (`path`) or litera... |
| `adler32` | Adler-32 checksum (RFC 1950 / zlib) of a file (`path`) or literal string (`data... |
| `crc16` | CRC-16 checksum of a file (`path`) or literal string (`data`) — exactly one |
| `base64url` | URL-safe Base64 encode or decode a UTF-8 string (RFC 4648 §5: `-_` instead of `... |
| `base85` | Encode or decode base85 text |
| `rot13` | Apply the ROT13 substitution cipher to text |
| `json_validate` | Validate that a JSON document is well-formed |
| `json_format` | Reformat a JSON document: pretty-print (default) or minify |
| `ndjson_query` | Filter a JSONL/NDJSON file by a per-record predicate, then optionally project a... |
| `properties_to_json` | Parse a Java `.properties` file into a flat JSON object of string key/value pai... |
| `html_to_text` | Strip HTML markup to plain text: drops tags, decodes common entities, collapses... |
| `json_keys` | List every key path in a JSON document as RFC-6901-style JSON Pointers (`/a/b`,... |
| `json_flatten` | Flatten a nested JSON document into a single-level object whose keys are dotted... |
| `unicode_escape` | Escape or unescape the non-ASCII characters in a string |
| `port_scan` | Scan a host for open TCP ports by attempting a timed connect to each (batch for... |
| `public_ip` | Return this machine's public (WAN) IP address as seen from the internet |
| `local_ip` | Report this machine's primary local (LAN) IPv4/IPv6 address — the source addres... |
| `ip_geolocate` | Geolocate an IPv4/IPv6 address via the free ip-api.com service (no API key) |
| `follow_redirects` | Trace an HTTP redirect chain WITHOUT auto-following: issues requests with redir... |
| `http_form_post` | POST an HTML form |
| `mac_lookup` | Look up the hardware vendor / manufacturer for a MAC address by querying the pu... |
| `pid` | Report the process id (pid) of the teleia tool process itself |
| `env_run` | Run a program with a controlled environment and capture its output |
| `git_branch` | Manage git branches in the current repo |
| `git_stash` | Manage the git stash in the current repo |
| `git_remote` | List configured git remotes in the current repository |
| `git_checkout_file` | Restore file(s) in the working tree from git: runs `git checkout [<ref>] -- <pa... |
| `run_script` | Run a project script/target by name, auto-detecting the runner from a manifest ... |
| `list_scripts` | List runnable entrypoints in a project directory: package.json scripts, Makefil... |
| `coverage` | Run the standard test-coverage tool for the path's language and return its report |
| `bench` | Run the standard benchmark runner for the path's language |
| `build` | Compile artifacts for the path's language (the artifact-producing counterpart t... |
| `cargo_expand` | Expand Rust macros in the current crate using the `cargo expand` subcommand (th... |
| `clippy_fix` | Run the language's auto-fixing linter, mutating files in place |
| `run_bin` | Build and run the current project's binary/executable, auto-detecting the entry... |
| `changelog_gen` | Generate a Markdown changelog from git history, grouping Conventional-Commit me... |
| `git_grep` | Search tracked files for a regex/string with `git grep -n` |
| `git_log_file` | Show the commit history of a single file via `git log -- <path>`, following ren... |
| `cargo_add` | Add a dependency to a Rust workspace's Cargo.toml via `cargo add` |
| `cargo_search` | Search crates.io for Rust crates matching a query, via `cargo search` |
| `make_target` | Run a Makefile target with `make <target>` in an optional directory, returning ... |
| `git_apply` | Apply a real git unified diff via `git apply` (reading the patch from stdin) |
| `git_reset` | Run `git reset` in the current repo to move HEAD and/or unstage changes |
| `npm_install` | Install Node.js dependencies using the project's package manager |
| `audit` | Run a dependency-vulnerability advisory scan for the project in `dir` (default ... |
| `git_status_json` | Report `git status` as machine-readable JSON: {branch, ahead, behind, entries:[... |
| `jwt_verify_hmac` | Verify a JWT's HMAC signature (HS256/HS384/HS512) against a shared secret and r... |
| `jwt_sign_hmac` | Sign a JWT with an HMAC-SHA2 secret (HS256/HS384/HS512) |
| `hmac_verify` | Verify an HMAC tag: recompute HMAC over `data` under `key` (raw UTF-8, or hex b... |
| `totp` | RFC 6238 TOTP code from a base32 `secret` |
| `secret_scan` | Scan a file or directory for hard-coded secrets: AWS access keys, GitHub tokens... |
| `sleep` | Pause for a fixed duration, then return |
| `cat` | Read and concatenate multiple files in order, returning their combined contents |
| `seq` | Generate a numeric sequence from start to end (inclusive) stepping by `step`, j... |
| `todo_write` | replace the session todo list (`pending` / `in_progress` / `completed`); resets on restart |

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

The `/model` dropdown pre-populates with ~210 named models across all twenty providers. `Tab` accepts, type to filter; any name is accepted including ones not in the catalog.

**Keys.** `--api-key KEY` overrides the env-var fallback. Missing a key at startup or after `/model NAME`? teleia prompts with hidden input (`rpassword` at startup, in-TUI masked prompt mid-session) and saves the entered key under `prefs.api_key:<ENV_VAR>` so the next launch picks it up. `/keys` lists every provider + status; `/key PROVIDER` opens the prompt on demand. Env-var values win when set, so exporting overrides the saved one.

**Ollama pre-flight.** When the resolved endpoint is local Ollama, missing models trigger an interactive `pull from Ollama now? [Y/n]` with an animated progress bar. `--pull-yes` auto-confirms; `--no-pull` skips. Non-TTY stdin (scripts, CI, pipes) auto-confirms.

## Features

- **OS-aware welcome banner** — `/etc/os-release` (or `cfg!(target_os = …)` on macOS / Windows / FreeBSD) picks a pixel-art panel: Arch / Ubuntu / Debian / Fedora / Alpine / NixOS / Gentoo / Void / openSUSE / macOS / Windows / FreeBSD or a generic Linux fallback. Lambda mark + `powered by λ τέλεια` watermark below.
- **Modes** — Insert (default), Normal (`Esc`), Visual (`v` from Normal), Command (`:`). The status bar chip and input border colour signal the active mode.
- **Slash commands** — `/reset`, `/clear`, `/save NAME`, `/load NAME`, `/delete NAME`, `/list`, `/model [NAME]`, `/key PROVIDER`, `/keys`, `/mcps`, `/lsps`, `/tools`, `/plan`, `/build`, `/auto`, `/prompt [NAME]`, `/theme [NAME]`, `/notify [on|off]`, `/transparent [on|off]`, `/copy`, `/cd PATH`, `/pwd`, `/version`, `/show`, `/help`, `/quit`.
- **Vim commands** — `:q`/`:x`/`:qa`/`:qall` → quit, `:w`/`:wa` → save, `:e`/`:l` → load, `:colo`/`:theme` → theme, `:!CMD` → shell out, `:cd PATH`, `:pwd`, `:noh` (clear drag-select), `:version`, `:r FILE` (read into chat), `:enew` → reset. `EX_COMMANDS` dropdown autocompletes via Tab.
- **Prompt templates** — `/prompt` lists 10 starters (`review`, `debug`, `explain`, `refactor`, `test`, `docs`, `security`, `perf`, `commit`, `plan`); `/prompt NAME` drops the template into the input box.
- **Autocomplete** — ghost-text completion as you type + a drop-down listing every match. Up to 10 entries visible at a time; longer lists scroll via Up/Down keeping the selected row in view. Title shows total count (`models · 211`).
- **Input editing** — readline-style `Left`/`Right`/`Home`/`End`, `Backspace`/`Delete`, `Ctrl+A/E/U/W`; `Up`/`Down` recall prior submissions. Typing stays live during a streaming turn; the input box grows vertically with content (cap = 1/3 of screen).
- **Smart auto-scroll** — defaults to following the bottom. Scrolling up disengages bottom-following so streaming deltas don't yank you back down mid-read; reaching the bottom (scroll 0) via any down gesture re-engages. `G` jumps to bottom + re-engages. Scroll math uses `Paragraph::line_count` so it always lands at the last rendered visual row.
- **Scrollbar** — right-edge thumb in `th.purple` over a `th.dim` track; only renders when there's overflow.
- **Drag-select** — left-click + drag in the chat area highlights cells; release copies via [`arboard`](https://crates.io/crates/arboard) (Wayland / X11 / macOS / Windows). Works during streaming.
- **Themes** — `tokyo-night` (default), `catppuccin`, `dracula`. Code highlighting follows the active theme (scope-aware mapping via syntect).
- **Blur & transparency** — `/transparent on` (or `TELEIA_TRANSPARENT=1`) swaps the theme background for `Color::Reset`, letting the terminal's alpha and the compositor's blur bleed through. Pair with `alpha=0.85` in foot/alacritty + Hyprland's blur decoration; selection highlight, status chips, and mode badges stay opaque so they remain legible. See [`docs/RICE.md`](docs/RICE.md).
- **Desktop autotheme** — pair teleia with the external [`grogu`](https://github.com/foolish-dev/grogu) binary to fan the active theme out to Noctalia shell, niri, vim/neovim, and back into teleia's own prefs. `grogu apply` (no args) reads the `theme` row teleia writes to its sqlite store, so `/theme dracula` → `grogu apply` repaints the desktop without typing the theme name twice. Designed to run as a Noctalia post-wallpaper-change hook so wallpaper rotation re-themes the system.
- **Token tracker** — status bar shows `↑prompt ↓completion` totals for the session.
- **Desktop notifications** — Linux `notify-send`, macOS `osascript`, Windows BurntToast (silently no-op if not installed). Toggle with `/notify on|off`.
- **Streaming** — tokens render live via SSE with a blinking caret, braille spinners on tool calls, and a running-dot "thinking" indicator. `Esc` / `Ctrl+C` mid-turn aborts.

## Session persistence

Everything important survives a restart.

- **Messages** stream to SQLite as they arrive — no "unsaved" state. Store lives at `$XDG_DATA_HOME/teleia/teleia.sqlite` (Linux), `~/Library/Application Support/teleia/teleia.sqlite` (macOS), `%APPDATA%\teleia\teleia.sqlite` (Windows). `$XDG_DATA_HOME` wins on any platform when set.
- **Auto-bookmarks** — every launch is tagged `last`; `/reset` rotates the outgoing session to `prev`. So `teleia --resume` always picks up where you left off, and the run before that is recoverable with `/load prev`.
- **Aliases** — `/save NAME` + `/load NAME` for sessions you want to keep by hand.
- **Sticky preferences** — theme, `/notify` toggle, `/transparent` toggle, permission mode, active model, and per-provider API keys all persist (CLI flags override). Launching `teleia` with no `--model` picks up wherever you last `/model`-ed.
- **Input history** — last 500 submissions reload into the `Up`/`Down` recall buffer at startup, deduped against the most recent entry.

## Configuration

Optional TOML at `$XDG_CONFIG_HOME/teleia/config.toml` (Linux), or the OS-native config dir on macOS / Windows.

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

# LSP server. Spawned + initialised at startup; pull diagnostics and
# hover are exposed to the agent as `lsp_diagnostics` and `lsp_hover`.
# Definition / references / workspace ops are still TODO.
[lsps.rust]
command       = "rust-analyzer"
root_patterns = ["Cargo.toml"]
```

**MCP**: teleia speaks newline-delimited JSON-RPC over stdio — `initialize` + `notifications/initialized` + `tools/list` + `tools/call` + `resources/list`. Spawn failures stderr-warn but don't abort boot. `/mcps` lists running servers with tool + resource counts.

**LSP**: real Content-Length-framed JSON-RPC client. Each configured server gets `initialize` + `initialized`. `/lsps` shows live status + advertised `serverInfo`. `lsp_diagnostics` (pull diagnostics) and `lsp_hover` are exposed to the agent; one-shot `textDocument/didOpen` lazily on first request. Definition / references / workspace ops are still TODO.

## Layout

```
crates/
  teleia-cli       # `teleia` binary + TUI + MCP client + LSP client
  teleia-agent     # turn loop, permission gate, event stream
  teleia-llm       # chat / pull / tags streaming, provider detection
  teleia-tools     # 22 built-in tools
  teleia-tools-bin # same dispatch as a stdin/stdout CLI
  teleia-store     # sqlite session + prefs + input-history persistence
```

Built on `ratatui` + `crossterm`, `reqwest` (rustls), `rusqlite` (bundled), `syntect` (no C deps), `arboard`, `rpassword`.

## Rice

Hyprland / niri starter configs for floating τέλεια in a Tokyo
Night-palette terminal with matching borders, rounded corners, and a
purple→cyan accent gradient: [`docs/RICE.md`](docs/RICE.md). Foot /
alacritty / kitty palette + JetBrains Mono Nerd Font included.

## License

MIT — see [LICENSE](LICENSE).
