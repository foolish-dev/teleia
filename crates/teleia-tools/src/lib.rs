use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use teleia_llm::ToolDef;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub fn definitions() -> Vec<ToolDef> {
    vec![
        ToolDef::new(
            "read",
            "Read a file from disk. Returns the file contents as text.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute or relative path to the file" }
                },
                "required": ["path"]
            }),
        ),
        ToolDef::new(
            "write",
            "Write contents to a file, creating or overwriting it.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        ),
        ToolDef::new(
            "edit",
            "Replace a unique substring inside a file. Fails if old_string is not found, or — when replace_all is false (the default) — if it matches more than once. Set replace_all: true to substitute every occurrence.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence instead of requiring uniqueness (default false)" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        ),
        ToolDef::new(
            "multi_edit",
            "Apply a sequence of edits to a single file atomically: each step is validated against the in-memory buffer before the next runs, and the file is only written if every step succeeds. Each edit has the same shape as `edit` (old_string, new_string, optional replace_all).",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": { "type": "string" },
                                "new_string": { "type": "string" },
                                "replace_all": { "type": "boolean" }
                            },
                            "required": ["old_string", "new_string"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }),
        ),
        ToolDef::new(
            "rm",
            "Delete a file. To delete a directory, set recursive: true (rm -rf). Refuses to touch `/` or an empty path.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "recursive": { "type": "boolean", "description": "Allow deleting a directory and its contents (default false)" }
                },
                "required": ["path"]
            }),
        ),
        ToolDef::new(
            "todo_write",
            "Replace the session todo list. Pass `todos` as an array of {content, status} where status is one of `pending` / `in_progress` / `completed`. Returns the formatted list. Pass an empty array to clear. The list is process-local — it survives across turns but resets when teleia restarts.",
            json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string" },
                                "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        ),
        ToolDef::new(
            "bash",
            "Run a shell command and return its combined stdout/stderr. 30s timeout.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                },
                "required": ["command"]
            }),
        ),
        ToolDef::new(
            "list",
            "List the contents of a directory. Returns one entry per line; directories are suffixed with '/'.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        ),
        ToolDef::new(
            "glob",
            "Find files matching a shell-style glob pattern (e.g. '**/*.rs'). Returns up to 200 matching paths.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" }
                },
                "required": ["pattern"]
            }),
        ),
        ToolDef::new(
            "grep",
            "Search for a regex pattern across files. `path` may be a single file or a directory (walked recursively, skipping hidden dirs and target/node_modules). Returns up to 200 file:line:text matches.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Rust regex syntax" },
                    "path":    { "type": "string", "description": "File or directory to search" }
                },
                "required": ["pattern", "path"]
            }),
        ),
        ToolDef::new(
            "head",
            "Read the first N lines of a file (default 40). Cheaper than `read` for huge files when only the top matters.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "lines": { "type": "integer", "description": "How many lines to return (default 40, capped at 2000)" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "tail",
            "Read the last N lines of a file (default 40). Useful for logs and append-only files.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "lines": { "type": "integer", "description": "How many lines to return (default 40, capped at 2000)" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "tree",
            "Recursive directory tree, depth-limited. Skips hidden dirs and target/node_modules/dist/build.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "depth": { "type": "integer", "description": "Max recursion depth (default 3, capped at 8)" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "stat",
            "File metadata: size, mtime, mode, type. Cheap inspection that doesn't burn tokens on the contents.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "diff",
            "Line-based diff between two files. Shells out to `/usr/bin/diff -u`; returns the unified-diff output (or `(no differences)`).",
            json!({ "type": "object", "properties": {
                "a": { "type": "string", "description": "Path to the original file" },
                "b": { "type": "string", "description": "Path to the changed file" }
            }, "required": ["a", "b"] }),
        ),
        ToolDef::new(
            "which",
            "Locate an executable on $PATH. Returns the first match or an error.",
            json!({ "type": "object", "properties": {
                "name": { "type": "string" }
            }, "required": ["name"] }),
        ),
        ToolDef::new(
            "fetch",
            "HTTP GET a URL and return the response body as text. 10s timeout, 1 MiB cap. Use for fetching docs / API JSON without shelling to curl.",
            json!({ "type": "object", "properties": {
                "url": { "type": "string", "description": "Fully-qualified http(s) URL" }
            }, "required": ["url"] }),
        ),
        ToolDef::new(
            "mkdir",
            "Create a directory (and any missing parents). Idempotent — succeeds if it already exists.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "mv",
            "Rename or move a file/dir. Fails if the destination already exists (refuse-to-clobber).",
            json!({ "type": "object", "properties": {
                "src": { "type": "string" },
                "dst": { "type": "string" }
            }, "required": ["src", "dst"] }),
        ),
        ToolDef::new(
            "cp",
            "Copy a file. Fails if the destination already exists (refuse-to-clobber). For directories, use bash.",
            json!({ "type": "object", "properties": {
                "src": { "type": "string" },
                "dst": { "type": "string" }
            }, "required": ["src", "dst"] }),
        ),
        ToolDef::new(
            "apply_patch",
            "Apply a unified diff to the working tree via `/usr/bin/patch -pN`. Returns patch's stdout+stderr; non-zero exit appended as `[exit N]`.",
            json!({ "type": "object", "properties": {
                "diff": { "type": "string", "description": "Unified diff payload, as `diff -u` or `git diff` produces" },
                "strip": { "type": "integer", "description": "Strip level passed as -p (default 0)" }
            }, "required": ["diff"] }),
        ),
        ToolDef::new(
            "wc",
            "Count lines / words / bytes of a file. Cheap inspection — useful before deciding whether to `read` everything.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "touch",
            "Create an empty file (or update mtime if it already exists). Idempotent.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "sha256",
            "SHA-256 of a file's contents, hex-encoded.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "date",
            "Current local + UTC time. RFC3339-shaped.",
            json!({ "type": "object", "properties": {} }),
        ),
        ToolDef::new(
            "lint",
            "Run the standard linter for the path's language. Auto-detects via extension: .rs → cargo clippy, .py → ruff check / flake8, .js/.ts/.tsx → eslint, .go → go vet, .sh → shellcheck. Returns combined stdout/stderr + exit code. Falls back to an error if no linter is known.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string", "description": "File or directory to lint" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "format",
            "Run the standard formatter for the path's language (writes in place). Auto-detects via extension: .rs → rustfmt, .py → black / ruff format, .js/.ts/.tsx/.json/.md → prettier, .go → gofmt. Returns the formatter's output.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string", "description": "File to format" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "typecheck",
            "Run the static type-checker for the path's language. .rs → cargo check, .py → mypy, .ts/.tsx → tsc --noEmit, .go → go build -o /dev/null. Returns combined stdout/stderr.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string", "description": "File or directory" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "test",
            "Run the standard test runner for the path's language. Auto-detects via extension: .rs → cargo test, .py → pytest, .go → go test ./..., .js/.ts/.tsx → npm test. Returns combined stdout/stderr + exit code.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string", "description": "File or directory whose language selects the runner" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "git",
            "Run a bounded set of git subcommands in the current repo. `subcommand` is one of status / diff / log / add / commit / show / blame / diff_stat. `paths` scopes diff/diff_stat and is required by add; `message` is required by commit. `show` needs `ref` (optional `path` for a file at that ref, i.e. ref:path). `blame` needs `path` (optional `start_line`+`end_line` to scope). `diff_stat` accepts optional `ref`, `staged`, and `paths`. Returns git's combined output.",
            json!({ "type": "object", "properties": {
                "subcommand": { "type": "string", "enum": ["status", "diff", "log", "add", "commit", "show", "blame", "diff_stat"] },
                "paths": { "type": "array", "items": { "type": "string" }, "description": "Paths for add (required) or to scope diff/diff_stat (optional)" },
                "message": { "type": "string", "description": "Commit message (required for commit)" },
                "ref": { "type": "string", "description": "Commit/ref for show (required) or diff_stat (optional)" },
                "path": { "type": "string", "description": "File path for show (with ref) or blame (required)" },
                "staged": { "type": "boolean", "description": "diff_stat: stat the staged index (--cached)" },
                "start_line": { "type": "integer", "description": "blame: first line (1-based; with end_line)" },
                "end_line": { "type": "integer", "description": "blame: last line (with start_line)" }
            }, "required": ["subcommand"] }),
        ),
        ToolDef::new(
            "symlink",
            "Create a symbolic link at `dst` pointing to `src`. Refuses if `dst` already exists (refuse-to-clobber).",
            json!({ "type": "object", "properties": {
                "src": { "type": "string", "description": "Link target (what the symlink points to)" },
                "dst": { "type": "string", "description": "Path of the symlink to create" }
            }, "required": ["src", "dst"] }),
        ),
        ToolDef::new(
            "env",
            "Read environment variables. With `name`, returns that variable's value (errors if unset). Without it, returns every variable as sorted `KEY=VALUE` lines.",
            json!({ "type": "object", "properties": {
                "name": { "type": "string", "description": "Single variable to read; omit to list all" }
            } }),
        ),
        ToolDef::new(
            "replace",
            "Regex find-and-replace inside a single file, written in place. Unlike `edit` (literal, unique-match), this substitutes a Rust regex; `replacement` may reference capture groups as `$1` / `$name`. Replaces every match unless `all` is false (then only the first). Returns the occurrence count.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "pattern": { "type": "string", "description": "Rust regex to match" },
                "replacement": { "type": "string", "description": "Replacement text; `$1`/`$name` expand capture groups" },
                "all": { "type": "boolean", "description": "Replace every match (default true); false replaces only the first" }
            }, "required": ["path", "pattern", "replacement"] }),
        ),
        ToolDef::new(
            "json",
            "Extract a value from a JSON file by RFC 6901 JSON Pointer (e.g. `/dependencies/serde`, `/scripts/0`). An empty pointer returns the whole document. Result is pretty-printed JSON. Cheaper and more precise than reading + eyeballing a big JSON file.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "pointer": { "type": "string", "description": "JSON Pointer; empty string selects the root document" }
            }, "required": ["path", "pointer"] }),
        ),
        ToolDef::new(
            "base64",
            "Base64-encode or -decode a UTF-8 string. Encodes `data` by default; set `decode: true` to decode. Standard alphabet, with `=` padding. Handy for JWT segments, data URIs, and small blobs without shelling out.",
            json!({ "type": "object", "properties": {
                "data": { "type": "string" },
                "decode": { "type": "boolean", "description": "Decode `data` instead of encoding it (default false)" }
            }, "required": ["data"] }),
        ),
        ToolDef::new(
            "hexdump",
            "Hex + ASCII dump of a file's first N bytes (default 256, capped at 4096), 16 bytes per row with offsets — like `xxd`. For inspecting binaries or encoding issues without dumping raw bytes into the transcript.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "bytes": { "type": "integer", "description": "How many leading bytes to dump (default 256, cap 4096)" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "du",
            "Total size in bytes of a file, or of a directory tree (recursive, symlinks not followed). Reports the byte count and a human-readable figure.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "realpath",
            "Canonicalize a path to its absolute form, resolving `.`, `..`, and symlinks. The path must exist.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "md5",
            "MD5 digest (hex) of a file (`path`) or a literal string (`data`) — exactly one. Broken for security; for checksums/ETags/legacy fixtures only.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "data": { "type": "string", "description": "Literal UTF-8 string to hash instead of a file" }
            } }),
        ),
        ToolDef::new(
            "sha1",
            "SHA-1 digest (hex) of a file (`path`) or a literal string (`data`) — exactly one. Cryptographically weak; for git/legacy checksum parity.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "data": { "type": "string" }
            } }),
        ),
        ToolDef::new(
            "crc32",
            "CRC-32 (IEEE/zip/gzip/PNG) of a file (`path`) or literal string (`data`) — exactly one. Returns hex (0x-prefixed) and unsigned decimal.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "data": { "type": "string" }
            } }),
        ),
        ToolDef::new(
            "hash",
            "SHA-2 hex digest under a chosen algorithm (sha224|sha384|sha512) of a file (`path`) or literal string (`data`) — exactly one. Complements the sha256 tool.",
            json!({ "type": "object", "properties": {
                "algo": { "type": "string", "enum": ["sha224", "sha384", "sha512"] },
                "path": { "type": "string" },
                "data": { "type": "string" }
            }, "required": ["algo"] }),
        ),
        ToolDef::new(
            "hmac_sha256",
            "HMAC-SHA256 (hex) of `data` under secret `key`. Set `key_hex: true` if `key` is hex-encoded. For webhook/API signatures (GitHub/Stripe/Slack).",
            json!({ "type": "object", "properties": {
                "data": { "type": "string" },
                "key": { "type": "string" },
                "key_hex": { "type": "boolean", "description": "Decode `key` from hex to raw bytes first (default false)" }
            }, "required": ["data", "key"] }),
        ),
        ToolDef::new(
            "hex",
            "Hex-encode a UTF-8 string, or decode a hex string back to text (`decode: true`). Whitespace is ignored on decode.",
            json!({ "type": "object", "properties": {
                "data": { "type": "string" },
                "decode": { "type": "boolean", "description": "Decode `data` (hex) instead of encoding (default false)" }
            }, "required": ["data"] }),
        ),
        ToolDef::new(
            "base32",
            "Base32-encode or -decode a UTF-8 string per RFC 4648 (A-Z2-7, `=` padding). `decode: true` to decode; `hex_variant: true` for the base32hex (0-9A-V) alphabet. Handy for TOTP/2FA secrets.",
            json!({ "type": "object", "properties": {
                "data": { "type": "string" },
                "decode": { "type": "boolean" },
                "hex_variant": { "type": "boolean" }
            }, "required": ["data"] }),
        ),
        ToolDef::new(
            "url_encode",
            "Percent-encode or decode (`decode: true`) a string per RFC 3986. With `component: true`, also escapes reserved URL chars (encodeURIComponent-style); otherwise they pass through. Decode does not map '+' to space.",
            json!({ "type": "object", "properties": {
                "data": { "type": "string" },
                "decode": { "type": "boolean" },
                "component": { "type": "boolean" }
            }, "required": ["data"] }),
        ),
        ToolDef::new(
            "hash_verify",
            "Verify a file (`path`) or literal string (`data`) against an `expected` SHA-256 digest (case-insensitive). Returns match plus the computed and expected digests.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "data": { "type": "string" },
                "expected": { "type": "string", "description": "64-char hex SHA-256 to compare against" }
            }, "required": ["expected"] }),
        ),
        ToolDef::new(
            "jwt_decode",
            "Decode a JWT's header and payload JSON without verifying the signature (base64url-decode segments 1-2). Signature is never checked.",
            json!({ "type": "object", "properties": {
                "token": { "type": "string" },
                "pretty": { "type": "boolean", "description": "Pretty-print the JSON (default true)" }
            }, "required": ["token"] }),
        ),
        ToolDef::new(
            "find",
            "Recursively search a directory for entries matching predicates: name glob, path regex, entry type (file/dir/symlink), byte-size range, mtime age in seconds, and max depth. Does not follow symlinks. Returns matching paths, capped at a limit (default 200).",
            json!({ "type": "object", "properties": {
                "path": { "type": "string", "description": "Directory to search" },
                "name": { "type": "string", "description": "Shell-style glob matched against the file name only (e.g. '*.rs')" },
                "regex": { "type": "string", "description": "Rust regex matched against the full path" },
                "type": { "type": "string", "enum": ["file", "dir", "symlink"], "description": "Restrict to entries of this type" },
                "min_size": { "type": "integer", "description": "Minimum size in bytes (files only)" },
                "max_size": { "type": "integer", "description": "Maximum size in bytes (files only)" },
                "newer_than": { "type": "integer", "description": "Only entries modified within the last N seconds" },
                "older_than": { "type": "integer", "description": "Only entries modified at least N seconds ago" },
                "max_depth": { "type": "integer", "description": "Max recursion depth below path" },
                "limit": { "type": "integer", "description": "Max results (default 200, hard-capped at 1000)" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "chmod",
            "Change a file's Unix permission bits from an octal mode string (e.g. \"755\"). On Windows, Unix mode bits don't exist, so only the read-only attribute is toggled: if the owner-write bit (0o200) is set the file is made writable, otherwise read-only; the result message states that only the read-only attribute was applied. The path must exist.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "mode": { "type": "string", "description": "Octal mode, e.g. \"644\" or \"755\" (parsed base-8, must be <= 0o7777)" }
            }, "required": ["path", "mode"] }),
        ),
        ToolDef::new(
            "readlink",
            "Read the immediate target a symlink points to (one hop, not canonicalized). Works on dangling and relative links. Errors if the path is not a symlink or does not exist.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string", "description": "Path to the symlink to read" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "hardlink",
            "Create a hard link at `dst` referring to the same inode as `src` (a distinct filesystem entry sharing the same data/inode as src). Refuses if `dst` already exists (refuse-to-clobber). Fails on directories (OS-enforced).",
            json!({ "type": "object", "properties": {
                "src": { "type": "string", "description": "Existing file to link to (the shared inode)" },
                "dst": { "type": "string", "description": "Path of the new hard link to create (must not already exist)" }
            }, "required": ["src", "dst"] }),
        ),
        ToolDef::new(
            "pathinfo",
            "Decompose a path string into its parts: parent (dirname), file_name (basename), file_stem, and extension. Pure string operation, does not touch the filesystem.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "mktemp",
            "Create a uniquely-named temporary file (or directory, when dir is true) under the system temp directory and return its absolute path. Collision-free: files are created with atomic O_CREAT|O_EXCL semantics, directories with create_dir, retrying on the rare name clash. Optional prefix (default `teleia-`) and suffix (default empty) wrap the random token.",
            json!({ "type": "object", "properties": {
                "dir": { "type": "boolean", "description": "Create a directory instead of a file (default false)" },
                "prefix": { "type": "string", "description": "Filename prefix (default `teleia-`)" },
                "suffix": { "type": "string", "description": "Filename suffix, e.g. `.txt` (default empty)" }
            } }),
        ),
        ToolDef::new(
            "truncate",
            "Set a file's length to an exact byte count: shrink (discarding trailing bytes) or grow (zero-padding the new space). Emptying a log to 0 is the common case. Errors if the file does not exist (does not create it).",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "size": { "type": "integer", "minimum": 0, "description": "New length in bytes; smaller truncates, larger zero-pads" }
            }, "required": ["path", "size"] }),
        ),
        ToolDef::new(
            "slice",
            "Return an inclusive 1-based line range [start..=end] of a file, optionally prefixing each line with its number. Omit end to read to EOF. Emitted range is capped at 2000 lines. Reads the file top-to-bottom like head/tail (does not stream-seek).",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "start": { "type": "integer", "description": "First line to emit (1-based, must be >= 1)" },
                "end": { "type": "integer", "description": "Last line to emit (inclusive, must be >= start); omit to read to EOF" },
                "number": { "type": "boolean", "description": "Prefix each emitted line with its 1-based line number and a tab (default false)" }
            }, "required": ["path", "start"] }),
        ),
        ToolDef::new(
            "sort",
            "Sort the lines of a file. Options: `reverse` (descending), `numeric` (compare by leading number), `unique` (drop adjacent duplicate lines after sorting), `ignore_case`, and an optional 1-based `field` key (splits each line on `delimiter`, or on whitespace when `delimiter` is omitted). Sort is stable; capped at 2000 lines.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "reverse": { "type": "boolean", "description": "Sort descending (default false)" },
                "numeric": { "type": "boolean", "description": "Compare keys as numbers, parsing a leading number (default false)" },
                "unique": { "type": "boolean", "description": "Remove duplicate lines after sorting (default false)" },
                "ignore_case": { "type": "boolean", "description": "Case-insensitive comparison (default false)" },
                "field": { "type": "integer", "description": "1-based field index to sort on; empty when out of range" },
                "delimiter": { "type": "string", "description": "Field delimiter; defaults to whitespace when omitted" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "cut",
            "Extract delimited fields or character ranges from each line of a file (like `cut -f`/`cut -c`). `fields` selects delimiter-separated columns; `chars` selects character positions. Exactly one of `fields`/`chars` is required. Specs are 1-based and accept comma lists and ranges, including open ranges: e.g. \"1,3\", \"2-4\", \"2-\" (to end), \"-3\" (from start). `complement` inverts the selection. Fields output is re-joined with the delimiter; chars output is concatenated. Lines with no delimiter are passed through unchanged in field mode (matching GNU cut default). Read-only.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "delimiter": { "type": "string", "description": "Field separator (literal string, default tab). Field mode only." },
                "fields": { "type": "string", "description": "1-based field spec, e.g. \"1,3-5\", \"2-\", \"-3\"" },
                "chars": { "type": "string", "description": "1-based character-position spec, same grammar as fields" },
                "complement": { "type": "boolean", "description": "Invert the selection (default false)" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "comm",
            "Line-set comparison of two files: report lines only in A, only in B, and lines common to both. Selectable via only_a / only_b / common flags (default emits all three labeled sections). Optional case-insensitive matching. Complements `diff` (positional edits) by answering set-membership instead. Output is sorted for determinism; duplicate lines within a file are collapsed (set semantics, not POSIX multiplicity).",
            json!({ "type": "object", "properties": {
                "a": { "type": "string", "description": "Path to the first file" },
                "b": { "type": "string", "description": "Path to the second file" },
                "only_a": { "type": "boolean", "description": "Emit lines present only in A (default false)" },
                "only_b": { "type": "boolean", "description": "Emit lines present only in B (default false)" },
                "common": { "type": "boolean", "description": "Emit lines present in both (default false)" },
                "ignore_case": { "type": "boolean", "description": "Case-insensitive line matching (default false)" }
            }, "required": ["a", "b"] }),
        ),
        ToolDef::new(
            "strings",
            "Extract printable ASCII runs (bytes 0x20..=0x7e) of at least min_len characters from a (possibly binary) file, one run per line, like the `strings` utility. Useful for pulling readable text out of binaries/objects without dumping hex.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "min_len": { "type": "integer", "description": "Minimum run length to emit (default 4, clamped to >=1)" },
                "limit": { "type": "integer", "description": "Max number of runs to emit; unbounded if omitted" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "column",
            "Align delimited rows into a padded table so columns line up. Reads a file, splits each line into fields, and pads every field (except the last in each row) to its column's width. Without `delimiter`, splits on runs of whitespace; with one, splits on that literal string. Columns are joined by `output_delimiter` (default two spaces). The final column is never padded, so no line ends in trailing whitespace.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "delimiter": { "type": "string", "description": "Literal input field separator; omit to split on whitespace runs" },
                "output_delimiter": { "type": "string", "description": "String placed between output columns (default two spaces)" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "tr",
            "Translate, delete, or squeeze characters in inline text or a file (tr-style), returning the transformed text without ever writing to disk. Provide exactly one of `data` (inline string) or `path` (file to read). `from`/`to` accept ranges like `a-z`. Translate maps `from` to `to` positionally, padding a short `to` set with its last char; a trailing/leading `-` is a literal dash. With `delete: true` every char in `from` is dropped and `to` is ignored. With `squeeze: true` runs of repeated chars are collapsed (over the `to` set when translating, else the `from` set). `to` is required unless `delete` or `squeeze` is set.",
            json!({ "type": "object", "properties": {
                "data": { "type": "string", "description": "Inline text to transform (mutually exclusive with `path`)" },
                "path": { "type": "string", "description": "File whose contents to transform (mutually exclusive with `data`)" },
                "from": { "type": "string", "description": "Source char set; supports ranges like `a-z`" },
                "to": { "type": "string", "description": "Destination char set for translation; required unless `delete`/`squeeze` is set" },
                "delete": { "type": "boolean", "description": "Delete every char in `from` (ignores `to`); default false" },
                "squeeze": { "type": "boolean", "description": "Collapse runs of repeated chars in the active set; default false" }
            }, "required": ["from"] }),
        ),
        ToolDef::new(
            "expand",
            "Convert tabs to spaces (default) or leading spaces to tabs (unexpand:true) at a given tab width. Reads from inline `data` or a file `path` and RETURNS the converted text (never writes in place); pipe the result into `write`/`edit` to persist. Only leading whitespace is affected when unexpand is set.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string", "description": "File to read (mutually exclusive with data)" },
                "data": { "type": "string", "description": "Inline text (mutually exclusive with path)" },
                "tab_width": { "type": "integer", "description": "Tab stop width (default 8, must be >= 1)" },
                "unexpand": { "type": "boolean", "description": "Convert leading spaces->tabs instead of tabs->spaces (default false)" }
            } }),
        ),
        ToolDef::new(
            "dedent",
            "Strip the longest common leading-whitespace prefix shared by every non-blank line (Python textwrap.dedent semantics). Reads inline `data` or a file `path` (exactly one). Lines that are empty or whitespace-only are ignored when computing the common prefix. The common prefix is matched character-by-character, so mixed tabs/spaces that don't share a literal prefix strip nothing.",
            json!({ "type": "object", "properties": {
                "data": { "type": "string", "description": "Inline text to dedent (provide this or `path`, not both)" },
                "path": { "type": "string", "description": "File whose contents to dedent (provide this or `data`, not both)" }
            } }),
        ),
        ToolDef::new(
            "count_matches",
            "Count regex matches in a single file: total occurrences and number of matching lines. Unlike grep (which caps at 200 hits and returns a truncated file:line list, never a total) and wc (newline/word/byte counts, no regex), this returns exact aggregate counts. Line-oriented, matching grep's own per-line semantics — a pattern cannot span newlines.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "pattern": { "type": "string", "description": "Rust regex syntax" },
                "ignore_case": { "type": "boolean", "description": "Case-insensitive matching (default false)" }
            }, "required": ["path", "pattern"] }),
        ),
        ToolDef::new(
            "epoch",
            "Convert between Unix epoch seconds and a UTC calendar timestamp, both directions. With to=iso, `value` is epoch seconds (number or numeric string) and the result is 'YYYY-MM-DD HH:MM:SS'. With to=epoch, `value` is a 'YYYY-MM-DD[ HH:MM:SS]' UTC string and the result is the epoch seconds. UTC only; no timezone/local handling.",
            json!({ "type": "object", "properties": {
                "value": { "type": ["string", "number"], "description": "Epoch seconds when to=iso; a 'YYYY-MM-DD[ HH:MM:SS]' UTC string when to=epoch" },
                "to": { "type": "string", "enum": ["iso", "epoch"], "description": "Output direction" }
            }, "required": ["value", "to"] }),
        ),
        ToolDef::new(
            "calc",
            "Evaluate an arithmetic expression and return the numeric result: + - * / %, unary -, parentheses, ** (integer power), and integer bit ops << >> & | ^ ~. Operands parse as i128 when integral and f64 otherwise; bitwise/shift/%/** require integer operands (error on floats). Division is always floating-point; division/modulo by zero is an error, not infinity. No variables, no shelling out. Deterministic — avoids LLM mental-arithmetic errors without spawning python/bash for one number.",
            json!({ "type": "object", "properties": {
                "expr": { "type": "string", "description": "Arithmetic expression, e.g. \"(1<<20)/1024\" or \"2**10\"" }
            }, "required": ["expr"] }),
        ),
        ToolDef::new(
            "nproc",
            "Report the number of logical CPUs available for parallelism, respecting cgroup and CPU-affinity limits. Useful for choosing a `-j` level before running builds or tests. Returns a single integer.",
            json!({ "type": "object", "properties": {} }),
        ),
        ToolDef::new(
            "os_release",
            "Report the OS family and CPU architecture of the machine teleia is running on, plus a best-effort human-readable distro name on Linux. Always returns `os` / `arch` / `family` (from the compiled-in target constants); `pretty` is added only when cheaply derivable (Linux PRETTY_NAME from /etc/os-release). Takes no arguments.",
            json!({ "type": "object", "properties": {} }),
        ),
        ToolDef::new(
            "kill",
            "Send a signal to a process by pid (default TERM). Accepts a fixed set of signal names (TERM|KILL|INT|HUP|QUIT) or, on Unix, a positive numeric signal. On Unix uses libc::kill and reports whether the kernel accepted the signal — it does NOT confirm the process actually died (this tool is not the target's parent), and surfaces ESRCH (no such process) / EPERM (not permitted) as errors. On Windows only TERM (graceful) and KILL (/F) work, via taskkill; INT/HUP/QUIT are rejected rather than silently downgraded. `pid` must be > 1 (0/1/negative are refused to block group/broadcast nukes). Optional `group` targets the process group (-pid) on Unix only.",
            json!({ "type": "object", "properties": {
                "pid": { "type": "integer", "description": "Target process id; must be > 1" },
                "signal": { "description": "Signal name (TERM|KILL|INT|HUP|QUIT) or, on Unix, a positive number. Default TERM." },
                "group": { "type": "boolean", "description": "Unix-only: send to the process group (-pid) instead of a single pid (default false)" }
            }, "required": ["pid"] }),
        ),
        ToolDef::new(
            "tcp_check",
            "Test whether a TCP port on a host is reachable by attempting a connect with a timeout. Reports open / closed (refused) / timed-out plus connect latency in ms. Does not send or read any bytes — pure reachability probe. Works for any raw TCP service (redis, postgres, a dev server) that the http-only `fetch` tool cannot reach.",
            json!({ "type": "object", "properties": {
                "host": { "type": "string", "description": "Hostname or IP to connect to" },
                "port": { "type": "integer", "description": "TCP port, 1..=65535" },
                "timeout_ms": { "type": "integer", "description": "Connect timeout in ms (default 3000, clamped 1..=30000)" }
            }, "required": ["host", "port"] }),
        ),
        ToolDef::new(
            "dns_resolve",
            "Resolve a hostname to its IP address(es) (A/AAAA only). Does not query MX/TXT/CNAME/SRV — std cannot. Returns one IP per line.",
            json!({ "type": "object", "properties": {
                "host": { "type": "string", "description": "Hostname to resolve, e.g. `example.com`" }
            }, "required": ["host"] }),
        ),
        ToolDef::new(
            "download",
            "Stream an HTTP(S) URL to a file on disk (no size cap, binary-safe). Returns destination path, bytes written, and content-type. Refuses to overwrite an existing path unless overwrite:true.",
            json!({ "type": "object", "properties": {
                "url": { "type": "string", "description": "Fully-qualified http(s) URL to fetch" },
                "dest": { "type": "string", "description": "Path to write the downloaded body to" },
                "overwrite": { "type": "boolean", "description": "Replace dest if it already exists (default false)" },
                "timeout_secs": { "type": "integer", "description": "Request timeout in seconds (default 30)" }
            }, "required": ["url", "dest"] }),
        ),
        ToolDef::new(
            "http_request",
            "Full HTTP client: any method (GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS), custom request headers, and an optional raw string `body` OR a structured `json` value (sent as application/json — the two are mutually exclusive). Returns the status line, response headers, and body (10s default timeout, 1 MiB default cap, both configurable). Use over the GET-only `fetch` when you need a method, headers, or a request body.",
            json!({ "type": "object", "properties": {
                "url": { "type": "string", "description": "Fully-qualified http(s) URL" },
                "method": { "type": "string", "description": "HTTP method (default GET); case-insensitive" },
                "headers": { "type": "object", "description": "Request headers as a string->string map", "additionalProperties": { "type": "string" } },
                "body": { "type": "string", "description": "Raw request body; mutually exclusive with `json`" },
                "json": { "description": "Structured value serialized as an application/json body; mutually exclusive with `body`" },
                "timeout_secs": { "type": "integer", "description": "Request timeout in seconds (default 10, clamped 1..=120)" },
                "max_bytes": { "type": "integer", "description": "Response body cap in bytes (default 1 MiB, clamped up to 8 MiB)" }
            }, "required": ["url"] }),
        ),
        ToolDef::new(
            "cargo_metadata",
            "Return `cargo metadata --format-version 1` as JSON for a Rust workspace: packages, versions, workspace_members, resolved dependency graph, and target directory. Note: may refresh Cargo.lock and, on a cold registry, access the network to resolve dependencies; pass no_deps to skip resolution and stay offline.",
            json!({ "type": "object", "properties": {
                "manifest_path": { "type": "string", "description": "Path to Cargo.toml (defaults to the manifest in the current directory)" },
                "no_deps": { "type": "boolean", "description": "Skip dependency resolution — stays offline, omits the resolve graph (default false)" }
            }, "required": [] }),
        ),
        ToolDef::new(
            "cargo_tree",
            "Render the Cargo dependency tree (`cargo tree`) for the current workspace. Optionally scope to one package (`package` -> -p), invert the tree to find who depends on a crate (`invert` -> -i <crate>), and/or list only crates present in multiple versions (`duplicates` -> --duplicates). Read-only.",
            json!({ "type": "object", "properties": {
                "package": { "type": "string", "description": "Scope the tree to this package (-p)" },
                "invert": { "type": "string", "description": "Crate name to invert on: show reverse dependencies (-i)" },
                "duplicates": { "type": "boolean", "description": "Only show crates that appear in multiple versions (--duplicates)" }
            } }),
        ),
        ToolDef::new(
            "test_one",
            "Run a single named test / filter instead of the whole suite, auto-detected by extension: .rs → cargo test --all-targets NAME (substring filter), .py → pytest PATH -k NAME (or a `file::node` node-id when NAME contains `::`), .go → go test -run NAME ./... (NAME is a Go regex, so use `^Name$` for an exact match), .js/.ts/.jsx/.tsx → npm test -- -t NAME (Jest-style; vitest/mocha may differ). Returns combined stdout/stderr + exit code.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string", "description": "File or project dir; its extension selects the runner" },
                "name": { "type": "string", "description": "Test name or filter substring (a regex for Go, a `-k` expression for pytest)" }
            }, "required": ["path", "name"] }),
        ),
        ToolDef::new(
            "cloc",
            "Count lines of code across a directory tree, grouped by language and classified by file extension, reporting files / blank / comment / code per language. Pure in-process std::fs walk (skips hidden dirs + target/node_modules/dist/build), no external cloc/tokei binary. Comment counting is best-effort via per-language line-comment markers (does not track block comments or comment markers inside string literals).",
            json!({ "type": "object", "properties": {
                "path": { "type": "string", "description": "Root directory (or single file) to scan (default \".\")" },
                "exclude": { "type": "array", "items": { "type": "string" }, "description": "Glob patterns matched against each entry's file NAME, e.g. \"*.min.js\"" }
            } }),
        ),
        ToolDef::new(
            "json_diff",
            "Structural diff of two JSON documents. Reports values added / removed / changed at a dotted path, comparing objects independent of key order (arrays are compared positionally by index). Output is a stable, sorted list of {added, removed, changed} entries; identical documents report \"(no differences)\".",
            json!({ "type": "object", "properties": {
                "a": { "type": "string", "description": "Path to the first JSON file" },
                "b": { "type": "string", "description": "Path to the second JSON file" }
            }, "required": ["a", "b"] }),
        ),
        ToolDef::new(
            "json_merge",
            "Deep-merge two or more JSON documents into one. Objects are merged recursively (keys unioned, nested objects merged); on any type mismatch or scalar collision the later document's value wins wholesale. Arrays are either replaced by the later value (default) or concatenated, per array_mode. Reads each path, parses as JSON, folds left-to-right, and returns the merged JSON pretty-printed. Read-only: writes nothing. Distinct from RFC 7386 JSON Merge Patch (no null-deletes-key semantics) and from the `json` extract-by-pointer tool.",
            json!({ "type": "object", "properties": {
                "paths": { "type": "array", "items": { "type": "string" }, "description": "File paths read in order; the first doc is the fold seed and later docs win on conflict" },
                "array_mode": { "type": "string", "enum": ["replace", "concat"], "description": "How to combine arrays that collide: `replace` (default) takes the later array; `concat` appends them" }
            }, "required": ["paths"] }),
        ),
        ToolDef::new(
            "jsonl",
            "Process a JSONL/NDJSON file line by line: extract a JSON Pointer field from every record, or reflow each record to compact/pretty JSON. Fails with the offending line number on a parse error or a missing pointer, so bad data is surfaced rather than silently skipped. Blank lines are skipped. Read-only.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "mode": { "type": "string", "enum": ["compact", "pretty"], "description": "How to reflow each whole record when no pointer is given (default compact). Ignored when `pointer` is set." },
                "pointer": { "type": "string", "description": "RFC 6901 JSON Pointer applied to each record (e.g. `/a`); empty string selects the whole record. When set, output is always compact." }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "dotenv_parse",
            "Parse a .env file into a JSON object of key/value pairs. Handles `KEY=VALUE`, single/double-quoted values, an optional `export ` prefix, and full-line `#` comments. Inside double quotes, `\\n` `\\t` `\\r` `\\\\` `\\\"` escapes are expanded; single-quoted values are literal. Unquoted values are trimmed. Full-line comments and blank lines are skipped; lines with no `=` after stripping `export ` are skipped (not an error). Duplicate keys: last wins. Does NOT strip trailing inline comments (`A=1 # x` yields the literal `1 # x`) — quote or drop them.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string", "description": "Path to the .env file to parse" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "ini_to_json",
            "Parse an INI/config file with [sections] into a nested JSON object. Handles key=value pairs, full-line ; and # comments, and collects pre-section keys at the top level. Values are always strings; keys and section names are trimmed. Duplicate keys/sections resolve last-wins. Inline comments are NOT stripped.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string", "description": "Path to the INI/config file" }
            }, "required": ["path"] }),
        ),
        ToolDef::new(
            "ndjson_to_json",
            "Convert newline-delimited JSON (JSONL/NDJSON) to a single pretty JSON array (to:array), or a JSON array back to compact NDJSON lines (to:lines). Read-only: parses `path` and returns the converted text; never writes.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string" },
                "to": { "type": "string", "enum": ["array", "lines"], "description": "`array` = NDJSON -> pretty JSON array; `lines` = JSON array -> compact NDJSON" }
            }, "required": ["path", "to"] }),
        ),
    ]
}

pub async fn dispatch(name: &str, arguments: &str) -> Result<String> {
    let args: Value = serde_json::from_str(arguments)
        .with_context(|| format!("parsing arguments for tool `{name}`: {arguments}"))?;

    match name {
        "read" => read_tool(args).await,
        "write" => write_tool(args).await,
        "edit" => edit_tool(args).await,
        "multi_edit" => multi_edit_tool(args).await,
        "rm" => rm_tool(args).await,
        "todo_write" => todo_write_tool(args).await,
        "bash" => bash_tool(args).await,
        "list" => list_tool(args).await,
        "glob" => glob_tool(args).await,
        "grep" => grep_tool(args).await,
        "head" => head_tool(args).await,
        "tail" => tail_tool(args).await,
        "tree" => tree_tool(args).await,
        "stat" => stat_tool(args).await,
        "diff" => diff_tool(args).await,
        "which" => which_tool(args).await,
        "fetch" => fetch_tool(args).await,
        "mkdir" => mkdir_tool(args).await,
        "mv" => mv_tool(args).await,
        "cp" => cp_tool(args).await,
        "apply_patch" => apply_patch_tool(args).await,
        "wc" => wc_tool(args).await,
        "touch" => touch_tool(args).await,
        "sha256" => sha256_tool(args).await,
        "date" => date_tool(args).await,
        "lint" => lint_tool(args).await,
        "format" => format_tool(args).await,
        "typecheck" => typecheck_tool(args).await,
        "test" => test_tool(args).await,
        "git" => git_tool(args).await,
        "symlink" => symlink_tool(args).await,
        "env" => env_tool(args).await,
        "replace" => replace_tool(args).await,
        "json" => json_tool(args).await,
        "base64" => base64_tool(args).await,
        "hexdump" => hexdump_tool(args).await,
        "du" => du_tool(args).await,
        "realpath" => realpath_tool(args).await,
        "md5" => md5_tool(args).await,
        "sha1" => sha1_tool(args).await,
        "crc32" => crc32_tool(args).await,
        "hash" => hash_tool(args).await,
        "hmac_sha256" => hmac_sha256_tool(args).await,
        "hex" => hex_tool(args).await,
        "base32" => base32_tool(args).await,
        "url_encode" => url_encode_tool(args).await,
        "hash_verify" => hash_verify(args).await,
        "jwt_decode" => jwt_decode_tool(args).await,
        "find" => find_tool(args).await,
        "chmod" => chmod_tool(args).await,
        "readlink" => readlink_tool(args).await,
        "hardlink" => hardlink_tool(args).await,
        "pathinfo" => pathinfo_tool(args).await,
        "mktemp" => mktemp_tool(args).await,
        "truncate" => truncate_tool(args).await,
        "slice" => slice_tool(args).await,
        "sort" => sort_tool(args).await,
        "cut" => cut_tool(args).await,
        "comm" => comm_tool(args).await,
        "strings" => strings_tool(args).await,
        "column" => column_tool(args).await,
        "tr" => tr_tool(args).await,
        "expand" => expand_tool(args).await,
        "dedent" => dedent_tool(args).await,
        "count_matches" => count_matches_tool(args).await,
        "epoch" => epoch_tool(args).await,
        "calc" => calc_tool(args).await,
        "nproc" => nproc_tool(args).await,
        "os_release" => os_release_tool(args).await,
        "kill" => kill_tool(args).await,
        "tcp_check" => tcp_check_tool(args).await,
        "dns_resolve" => dns_resolve_tool(args).await,
        "download" => download_tool(args).await,
        "http_request" => http_request_tool(args).await,
        "cargo_metadata" => cargo_metadata_tool(args).await,
        "cargo_tree" => cargo_tree_tool(args).await,
        "test_one" => test_one_tool(args).await,
        "cloc" => cloc_tool(args).await,
        "json_diff" => json_diff_tool(args).await,
        "json_merge" => json_merge_tool(args).await,
        "jsonl" => jsonl_tool(args).await,
        "dotenv_parse" => dotenv_parse_tool(args).await,
        "ini_to_json" => ini_to_json_tool(args).await,
        "ndjson_to_json" => ndjson_to_json_tool(args).await,
        other => Err(anyhow!("unknown tool: {other}")),
    }
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
}

async fn read_tool(args: Value) -> Result<String> {
    let ReadArgs { path } = serde_json::from_value(args)?;
    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("read {path}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

async fn write_tool(args: Value) -> Result<String> {
    let WriteArgs { path, content } = serde_json::from_value(args)?;
    if let Some(parent) = PathBuf::from(&path).parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
    }
    tokio::fs::write(&path, &content)
        .await
        .with_context(|| format!("write {path}"))?;
    Ok(format!("wrote {} bytes to {path}", content.len()))
}

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Deserialize)]
struct EditStep {
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

fn apply_edit(buf: &str, step: &EditStep) -> Result<(String, usize)> {
    // An empty needle matches at every char boundary: `replace_all`
    // would interleave new_string between every character (shredding the
    // file), and even the single-shot path prepends silently. Reject it
    // — a common model hallucination when it means "insert at the start".
    if step.old_string.is_empty() {
        return Err(anyhow!("old_string is empty"));
    }
    let occurrences = buf.matches(&step.old_string).count();
    if occurrences == 0 {
        return Err(anyhow!("old_string not found"));
    }
    if !step.replace_all && occurrences > 1 {
        return Err(anyhow!(
            "old_string matches {occurrences} times; needs to be unique (or set replace_all)"
        ));
    }
    let updated = if step.replace_all {
        buf.replace(&step.old_string, &step.new_string)
    } else {
        buf.replacen(&step.old_string, &step.new_string, 1)
    };
    Ok((updated, occurrences))
}

async fn edit_tool(args: Value) -> Result<String> {
    let EditArgs {
        path,
        old_string,
        new_string,
        replace_all,
    } = serde_json::from_value(args)?;
    let original = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {path}"))?;
    let step = EditStep {
        old_string,
        new_string,
        replace_all,
    };
    let (updated, occurrences) =
        apply_edit(&original, &step).map_err(|e| anyhow!("{e} in {path}"))?;
    tokio::fs::write(&path, &updated)
        .await
        .with_context(|| format!("write {path}"))?;
    if replace_all {
        Ok(format!("edited {path} ({occurrences} replacements)"))
    } else {
        Ok(format!("edited {path}"))
    }
}

#[derive(Deserialize)]
struct MultiEditArgs {
    path: String,
    edits: Vec<EditStep>,
}

async fn multi_edit_tool(args: Value) -> Result<String> {
    let MultiEditArgs { path, edits } = serde_json::from_value(args)?;
    if edits.is_empty() {
        return Err(anyhow!("edits array is empty"));
    }
    let original = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {path}"))?;
    let mut buf = original;
    for (i, step) in edits.iter().enumerate() {
        let (next, _) = apply_edit(&buf, step)
            .with_context(|| format!("multi_edit step {} on {path}", i + 1))?;
        buf = next;
    }
    tokio::fs::write(&path, &buf)
        .await
        .with_context(|| format!("write {path}"))?;
    Ok(format!("edited {path} ({} steps)", edits.len()))
}

#[derive(Deserialize)]
struct RmArgs {
    path: String,
    #[serde(default)]
    recursive: bool,
}

async fn rm_tool(args: Value) -> Result<String> {
    let RmArgs { path, recursive } = serde_json::from_value(args)?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("path is empty"));
    }
    if trimmed == "/" {
        return Err(anyhow!("refusing to delete `/`"));
    }
    let meta = std::fs::symlink_metadata(&path).with_context(|| format!("stat {path}"))?;
    if meta.is_dir() {
        if !recursive {
            return Err(anyhow!(
                "{path} is a directory; pass recursive: true to delete"
            ));
        }
        // Catch paths that bypass the literal `/` check above — `//`, `/.`,
        // `/foo/..`, etc. — before they reach `remove_dir_all`.
        if resolves_to_root(std::path::Path::new(&path)) {
            return Err(anyhow!("refusing to delete `{path}` (resolves to `/`)"));
        }
        std::fs::remove_dir_all(&path).with_context(|| format!("rm -rf {path}"))?;
        Ok(format!("removed directory {path}"))
    } else {
        std::fs::remove_file(&path).with_context(|| format!("rm {path}"))?;
        Ok(format!("removed {path}"))
    }
}

fn resolves_to_root(p: &std::path::Path) -> bool {
    matches!(std::fs::canonicalize(p), Ok(c) if c == std::path::Path::new("/"))
}

#[derive(Deserialize)]
struct BashArgs {
    command: String,
}

async fn bash_tool(args: Value) -> Result<String> {
    let BashArgs { command } = serde_json::from_value(args)?;
    let mut cmd = Command::new("bash");
    cmd.arg("-lc").arg(&command).stdout(Stdio::piped());
    // SAFETY: pre_exec runs in the forked child before exec. dup2(1, 2)
    // routes the child's stderr fd onto stdout's pipe, so writes from
    // both streams land in one buffer in emit order. setpgid(0, 0) puts
    // bash in its own process group (pgid = bash's pid) so the timeout
    // arm can SIGKILL the whole tree, not just bash. Unix-only; on other
    // platforms stderr stays on the inherited fd and the timeout falls
    // back to start_kill which only reaps bash itself.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::dup2(1, 2) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn bash for: {command}"))?;
    let mut stdout = child.stdout.take().expect("stdout piped");
    // Drain in a separate task so we keep accumulating output regardless of
    // whether the child exits naturally or we kill it on timeout.
    let mut drain = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf).await;
        buf
    });

    // bash's own pid == its process-group id (set via setpgid in
    // pre_exec). Capture it before waiting — child.id() returns None once
    // the child is reaped — so the normal-exit path below can SIGKILL the
    // whole group.
    #[cfg(unix)]
    let pgid = child.id();

    let mut timed_out = false;
    let exit_status = tokio::select! {
        status = child.wait() => status?,
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            timed_out = true;
            kill_child_tree(&mut child);
            child.wait().await?
        }
    };

    // On the normal-exit path the timeout arm's group-kill never ran, so
    // any job bash backgrounded (`sleep 300 &`, a dev server, …) is still
    // alive holding the inherited stdout pipe open — which keeps the
    // drain's read_to_end from ever reaching EOF and hangs the tool far
    // past its 30s budget. Reap the whole group here too so the pipe
    // closes promptly (this also stops orphaned background jobs leaking).
    #[cfg(unix)]
    if !timed_out {
        if let Some(pid) = pgid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }

    // Backstop: bound the drain so a descendant that escaped bash's group
    // (e.g. a daemon that called setsid but kept fd 1 open) still can't
    // hang the tool indefinitely.
    let buf = tokio::select! {
        joined = &mut drain => joined.unwrap_or_default(),
        _ = tokio::time::sleep(Duration::from_secs(5)) => {
            drain.abort();
            Vec::new()
        }
    };
    let mut out = String::from_utf8_lossy(&buf).into_owned();
    if timed_out {
        out.push_str("\n[bash timed out after 30s]");
    } else if !exit_status.success() {
        out.push_str(&format!("\n[exit {}]", exit_status.code().unwrap_or(-1)));
    }
    Ok(out)
}

/// SIGKILL the child's whole process group on Unix (catches anything bash
/// forked — sleep, nested shells, etc. — that would otherwise outlive the
/// timeout as orphans re-parented to PID 1). Falls back to `start_kill`
/// elsewhere so non-Unix builds at least reap bash itself.
fn kill_child_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            // Negative PID = "send to the entire process group with this
            // pgid". We set pgid = pid in pre_exec, so this is safe.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            return;
        }
    }
    let _ = child.start_kill();
}

#[derive(Deserialize)]
struct ListArgs {
    path: String,
}

async fn list_tool(args: Value) -> Result<String> {
    let ListArgs { path } = serde_json::from_value(args)?;
    let mut entries: Vec<String> = std::fs::read_dir(&path)
        .with_context(|| format!("read_dir {path}"))?
        .filter_map(|r| r.ok())
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect();
    entries.sort();
    if entries.is_empty() {
        Ok(format!("{path}: (empty)"))
    } else {
        Ok(entries.join("\n"))
    }
}

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
}

async fn glob_tool(args: Value) -> Result<String> {
    let GlobArgs { pattern } = serde_json::from_value(args)?;
    let paths: Vec<String> = glob::glob(&pattern)
        .with_context(|| format!("invalid glob: {pattern}"))?
        .filter_map(|r| r.ok())
        .take(200)
        .map(|p| p.display().to_string())
        .collect();
    if paths.is_empty() {
        Ok(format!("no matches for {pattern}"))
    } else {
        Ok(paths.join("\n"))
    }
}

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    path: String,
}

async fn grep_tool(args: Value) -> Result<String> {
    let GrepArgs { pattern, path } = serde_json::from_value(args)?;
    let re = regex::Regex::new(&pattern).with_context(|| format!("invalid regex: {pattern}"))?;
    let root = PathBuf::from(&path);

    let mut files = Vec::new();
    if root.is_file() {
        files.push(root);
    } else if root.is_dir() {
        // Cap walk size so a giant tree doesn't explode the search.
        walk_files(&root, &mut files, 5000)?;
    } else {
        return Err(anyhow!("not found: {path}"));
    }

    const MAX_MATCHES: usize = 200;
    let mut matches: Vec<String> = Vec::new();
    'outer: for file in &files {
        let content = match std::fs::read_to_string(file) {
            Ok(c) => c,
            // Binary / unreadable files are silently skipped — typical
            // grep behaviour with --binary-files=without-match.
            Err(_) => continue,
        };
        for (i, line) in content.lines().enumerate() {
            if re.is_match(line) {
                matches.push(format!("{}:{}:{line}", file.display(), i + 1));
                if matches.len() >= MAX_MATCHES {
                    matches.push(format!("[truncated at {MAX_MATCHES} matches]"));
                    break 'outer;
                }
            }
        }
    }
    if matches.is_empty() {
        Ok(format!("no matches for /{pattern}/ in {path}"))
    } else {
        Ok(matches.join("\n"))
    }
}

/// Recursive directory walk for grep_tool. Skips hidden entries, the
/// usual heavy build/install dirs, and stops once `cap` files have been
/// collected so a search over `/` doesn't melt the process.
fn walk_files(root: &std::path::Path, out: &mut Vec<PathBuf>, cap: usize) -> std::io::Result<()> {
    if out.len() >= cap {
        return Ok(());
    }
    for entry in std::fs::read_dir(root)? {
        if out.len() >= cap {
            break;
        }
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || matches!(&*name, "target" | "node_modules" | "dist" | "build") {
            continue;
        }
        let ft = entry.file_type()?;
        if ft.is_dir() {
            // Errors in sub-walks are non-fatal — skip the subtree.
            let _ = walk_files(&path, out, cap);
        } else if ft.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct LinesArgs {
    path: String,
    #[serde(default)]
    lines: Option<usize>,
}

const MAX_LINES: usize = 2000;

async fn head_tool(args: Value) -> Result<String> {
    let LinesArgs { path, lines } = serde_json::from_value(args)?;
    let n = lines.unwrap_or(40).min(MAX_LINES);
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("read_to_string {path}"))?;
    Ok(content.lines().take(n).collect::<Vec<_>>().join("\n"))
}

async fn tail_tool(args: Value) -> Result<String> {
    let LinesArgs { path, lines } = serde_json::from_value(args)?;
    let n = lines.unwrap_or(40).min(MAX_LINES);
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("read_to_string {path}"))?;
    let all: Vec<&str> = content.lines().collect();
    let start = all.len().saturating_sub(n);
    Ok(all[start..].join("\n"))
}

#[derive(Deserialize)]
struct TreeArgs {
    path: String,
    #[serde(default)]
    depth: Option<usize>,
}

async fn tree_tool(args: Value) -> Result<String> {
    let TreeArgs { path, depth } = serde_json::from_value(args)?;
    let max_depth = depth.unwrap_or(3).min(8);
    let root = PathBuf::from(&path);
    if !root.is_dir() {
        return Err(anyhow!("not a directory: {path}"));
    }
    let mut out = String::new();
    out.push_str(&path);
    out.push('\n');
    walk_tree(&root, 0, max_depth, "", &mut out)?;
    Ok(out)
}

fn walk_tree(
    dir: &std::path::Path,
    depth: usize,
    max_depth: usize,
    prefix: &str,
    out: &mut String,
) -> Result<()> {
    if depth >= max_depth {
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|r| r.ok())
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            !(n.starts_with('.') || matches!(&*n, "target" | "node_modules" | "dist" | "build"))
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());
    let last_idx = entries.len().saturating_sub(1);
    for (i, entry) in entries.iter().enumerate() {
        let last = i == last_idx;
        let branch = if last { "└── " } else { "├── " };
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let suffix = if is_dir { "/" } else { "" };
        out.push_str(prefix);
        out.push_str(branch);
        out.push_str(&name);
        out.push_str(suffix);
        out.push('\n');
        if is_dir {
            let child_prefix = format!("{prefix}{}", if last { "    " } else { "│   " });
            let _ = walk_tree(&entry.path(), depth + 1, max_depth, &child_prefix, out);
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct PathArgs {
    path: String,
}

async fn stat_tool(args: Value) -> Result<String> {
    let PathArgs { path } = serde_json::from_value(args)?;
    let m = std::fs::symlink_metadata(&path).with_context(|| format!("stat {path}"))?;
    let kind = if m.is_dir() {
        "directory"
    } else if m.is_symlink() {
        "symlink"
    } else if m.is_file() {
        "file"
    } else {
        "other"
    };
    let mut out = format!("path:  {path}\ntype:  {kind}\nsize:  {} bytes", m.len());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        out.push_str(&format!("\nmode:  {:o}", m.permissions().mode() & 0o7777));
    }
    if let Ok(mt) = m.modified() {
        if let Ok(d) = mt.duration_since(std::time::UNIX_EPOCH) {
            out.push_str(&format!("\nmtime: {} (unix)", d.as_secs()));
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
struct DiffArgs {
    a: String,
    b: String,
}

async fn diff_tool(args: Value) -> Result<String> {
    let DiffArgs { a, b } = serde_json::from_value(args)?;
    let output = Command::new("diff")
        .arg("-u")
        .arg(&a)
        .arg(&b)
        .output()
        .await
        .with_context(|| "running /usr/bin/diff")?;
    if output.stdout.is_empty() && output.status.success() {
        return Ok("(no differences)".to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[derive(Deserialize)]
struct WhichArgs {
    name: String,
}

async fn which_tool(args: Value) -> Result<String> {
    let WhichArgs { name } = serde_json::from_value(args)?;
    let path = std::env::var_os("PATH").ok_or_else(|| anyhow!("$PATH unset"))?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            // crude executability check (mode & 0o111 != 0 on unix).
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(m) = std::fs::metadata(&candidate) {
                    if m.permissions().mode() & 0o111 == 0 {
                        continue;
                    }
                }
            }
            return Ok(candidate.display().to_string());
        }
    }
    Err(anyhow!("`{name}` not found on $PATH"))
}

#[derive(Deserialize)]
struct FetchArgs {
    url: String,
}

async fn fetch_tool(args: Value) -> Result<String> {
    let FetchArgs { url } = serde_json::from_value(args)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    // Cap at 1 MiB so a giant payload can't blow the context window.
    const CAP: usize = 1024 * 1024;
    let bytes = resp
        .bytes()
        .await
        .with_context(|| format!("reading body from {url}"))?;
    let truncated = bytes.len() > CAP;
    let slice = if truncated { &bytes[..CAP] } else { &bytes[..] };
    let body = String::from_utf8_lossy(slice);
    let mut out = format!("HTTP {}\n\n{body}", status.as_u16());
    if truncated {
        out.push_str(&format!(
            "\n\n[truncated at {CAP} bytes; original was {}]",
            bytes.len()
        ));
    }
    Ok(out)
}

async fn mkdir_tool(args: Value) -> Result<String> {
    let PathArgs { path } = serde_json::from_value(args)?;
    std::fs::create_dir_all(&path).with_context(|| format!("mkdir -p {path}"))?;
    Ok(format!("created {path}"))
}

#[derive(Deserialize)]
struct MoveArgs {
    src: String,
    dst: String,
}

async fn mv_tool(args: Value) -> Result<String> {
    let MoveArgs { src, dst } = serde_json::from_value(args)?;
    if std::fs::symlink_metadata(&dst).is_ok() {
        return Err(anyhow!("destination already exists: {dst}"));
    }
    std::fs::rename(&src, &dst).with_context(|| format!("mv {src} -> {dst}"))?;
    Ok(format!("renamed {src} -> {dst}"))
}

async fn cp_tool(args: Value) -> Result<String> {
    let MoveArgs { src, dst } = serde_json::from_value(args)?;
    if std::fs::symlink_metadata(&dst).is_ok() {
        return Err(anyhow!("destination already exists: {dst}"));
    }
    let bytes = std::fs::copy(&src, &dst).with_context(|| format!("cp {src} -> {dst}"))?;
    Ok(format!("copied {bytes} bytes: {src} -> {dst}"))
}

#[derive(Deserialize)]
struct ApplyPatchArgs {
    diff: String,
    #[serde(default)]
    strip: Option<u32>,
}

async fn apply_patch_tool(args: Value) -> Result<String> {
    use tokio::io::AsyncWriteExt;
    let ApplyPatchArgs { diff, strip } = serde_json::from_value(args)?;
    let strip = strip.unwrap_or(0);
    let mut child = Command::new("patch")
        .arg(format!("-p{strip}"))
        .arg("--no-backup-if-mismatch")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| "spawning /usr/bin/patch")?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(diff.as_bytes()).await?;
        stdin.flush().await?;
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().await?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&err);
    }
    if !out.status.success() {
        text.push_str(&format!("\n[exit {}]", out.status.code().unwrap_or(-1)));
    }
    Ok(text)
}

async fn wc_tool(args: Value) -> Result<String> {
    let PathArgs { path } = serde_json::from_value(args)?;
    let bytes = std::fs::read(&path).with_context(|| format!("read {path}"))?;
    let lines = bytes.iter().filter(|&&b| b == b'\n').count();
    // Word count = whitespace-separated runs (matches `wc -w` on most inputs).
    let text = String::from_utf8_lossy(&bytes);
    let words = text.split_whitespace().count();
    Ok(format!(
        "lines: {lines}\nwords: {words}\nbytes: {}",
        bytes.len()
    ))
}

async fn touch_tool(args: Value) -> Result<String> {
    let PathArgs { path } = serde_json::from_value(args)?;
    // Open with create+append so it makes the file when missing and
    // updates mtime when it exists; close immediately.
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("touch {path}"))?;
    // Bump mtime even when the file existed and we wrote zero bytes —
    // a real `touch` updates the timestamp regardless.
    let now = std::time::SystemTime::now();
    let _ = filetime_set(&path, now);
    Ok(format!("touched {path}"))
}

#[cfg(unix)]
fn filetime_set(path: &str, t: std::time::SystemTime) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(0)
        .open(path)?;
    // utimensat via libc — keep deps minimal. `time_t` is i32 on 32-bit
    // unix targets and i64 elsewhere; cast through the platform alias.
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    let tv = [
        libc::timespec {
            tv_sec: secs,
            tv_nsec: 0,
        },
        libc::timespec {
            tv_sec: secs,
            tv_nsec: 0,
        },
    ];
    use std::os::unix::io::AsRawFd;
    let r = unsafe { libc::futimens(f.as_raw_fd(), tv.as_ptr()) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn filetime_set(_path: &str, _t: std::time::SystemTime) -> std::io::Result<()> {
    Ok(())
}

async fn sha256_tool(args: Value) -> Result<String> {
    let PathArgs { path } = serde_json::from_value(args)?;
    let bytes = std::fs::read(&path).with_context(|| format!("read {path}"))?;
    // Tiny hand-rolled SHA-256 — keeps the dep tree small.
    let digest = sha256(&bytes);
    Ok(digest)
}

const SHA256_IV: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// SHA-256/224 compression (FIPS 180-4) over `data` from initial state `iv`,
/// returning the eight state words. Shared by `sha256`, `sha256_raw`, and the
/// `hash` tool's sha224 variant. Hand-rolled so we don't drag in a hash crate.
fn sha256_core(data: &[u8], iv: [u32; 8]) -> [u32; 8] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h = iv;
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let mj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(mj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    h
}

/// SHA-256 (FIPS 180-4) of `data` as a 64-char lowercase hex string.
fn sha256(data: &[u8]) -> String {
    let mut out = String::with_capacity(64);
    for word in sha256_core(data, SHA256_IV) {
        out.push_str(&format!("{word:08x}"));
    }
    out
}

/// SHA-256 of `data` as raw 32 big-endian bytes (HMAC + checksum reuse).
fn sha256_raw(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, word) in sha256_core(data, SHA256_IV).iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Resolve the single input for a hashing tool: exactly one of `path`
/// (file bytes) or `data` (literal UTF-8 string) must be given.
fn one_input_bytes(path: Option<String>, data: Option<String>) -> Result<Vec<u8>> {
    match (path, data) {
        (Some(p), None) => std::fs::read(&p).with_context(|| format!("read {p}")),
        (None, Some(d)) => Ok(d.into_bytes()),
        _ => Err(anyhow!("provide exactly one of `path` or `data`")),
    }
}

#[derive(Deserialize)]
struct HashInputArgs {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    data: Option<String>,
}

/// MD5 (RFC 1321) of `data` as a 32-char lowercase hex string. Broken for
/// security; kept for checksum/ETag/legacy-fixture parity. Little-endian
/// throughout, mirroring the hand-rolled sha256 style.
fn md5(data: &[u8]) -> String {
    #[rustfmt::skip]
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
        5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_le_bytes());
    for chunk in padded.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            m[i] = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = if i < 16 {
                ((b & c) | (!b & d), i)
            } else if i < 32 {
                ((d & b) | (!d & c), (5 * i + 1) % 16)
            } else if i < 48 {
                (b ^ c ^ d, (3 * i + 5) % 16)
            } else {
                (c ^ (b | !d), (7 * i) % 16)
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = String::with_capacity(32);
    for word in [a0, b0, c0, d0] {
        for byte in word.to_le_bytes() {
            out.push_str(&format!("{byte:02x}"));
        }
    }
    out
}

async fn md5_tool(args: Value) -> Result<String> {
    let HashInputArgs { path, data } = serde_json::from_value(args)?;
    Ok(md5(&one_input_bytes(path, data)?))
}

/// SHA-1 (FIPS 180-4) of `data` as a 40-char lowercase hex string.
/// Cryptographically weak; kept for git/legacy checksum parity.
fn sha1(data: &[u8]) -> String {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = if i < 20 {
                ((b & c) | (!b & d), 0x5A827999u32)
            } else if i < 40 {
                (b ^ c ^ d, 0x6ED9EBA1)
            } else if i < 60 {
                ((b & c) | (b & d) | (c & d), 0x8F1BBCDC)
            } else {
                (b ^ c ^ d, 0xCA62C1D6)
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

async fn sha1_tool(args: Value) -> Result<String> {
    let HashInputArgs { path, data } = serde_json::from_value(args)?;
    Ok(sha1(&one_input_bytes(path, data)?))
}

/// CRC-32 (IEEE 802.3 / zip / gzip / PNG polynomial) via the branchless
/// bit-at-a-time algorithm — no 256-entry table needed.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

async fn crc32_tool(args: Value) -> Result<String> {
    let HashInputArgs { path, data } = serde_json::from_value(args)?;
    let crc = crc32(&one_input_bytes(path, data)?);
    Ok(format!("{crc:#010x} {crc}"))
}

const SHA512_IV: [u64; 8] = [
    0x6a09e667f3bcc908,
    0xbb67ae8584caa73b,
    0x3c6ef372fe94f82b,
    0xa54ff53a5f1d36f1,
    0x510e527fade682d1,
    0x9b05688c2b3e6c1f,
    0x1f83d9abfb41bd6b,
    0x5be0cd19137e2179,
];
const SHA384_IV: [u64; 8] = [
    0xcbbb9d5dc1059ed8,
    0x629a292a367cd507,
    0x9159015a3070dd17,
    0x152fecd8f70e5939,
    0x67332667ffc00b31,
    0x8eb44a8768581511,
    0xdb0c2e0d64f98fa7,
    0x47b5481dbefa4fa4,
];

/// SHA-512/384 compression (FIPS 180-4) over `data` from state `iv`, 80
/// rounds of u64 arithmetic with a 128-bit big-endian length.
fn sha512_core(data: &[u8], iv: [u64; 8]) -> [u64; 8] {
    const K: [u64; 80] = [
        0x428a2f98d728ae22,
        0x7137449123ef65cd,
        0xb5c0fbcfec4d3b2f,
        0xe9b5dba58189dbbc,
        0x3956c25bf348b538,
        0x59f111f1b605d019,
        0x923f82a4af194f9b,
        0xab1c5ed5da6d8118,
        0xd807aa98a3030242,
        0x12835b0145706fbe,
        0x243185be4ee4b28c,
        0x550c7dc3d5ffb4e2,
        0x72be5d74f27b896f,
        0x80deb1fe3b1696b1,
        0x9bdc06a725c71235,
        0xc19bf174cf692694,
        0xe49b69c19ef14ad2,
        0xefbe4786384f25e3,
        0x0fc19dc68b8cd5b5,
        0x240ca1cc77ac9c65,
        0x2de92c6f592b0275,
        0x4a7484aa6ea6e483,
        0x5cb0a9dcbd41fbd4,
        0x76f988da831153b5,
        0x983e5152ee66dfab,
        0xa831c66d2db43210,
        0xb00327c898fb213f,
        0xbf597fc7beef0ee4,
        0xc6e00bf33da88fc2,
        0xd5a79147930aa725,
        0x06ca6351e003826f,
        0x142929670a0e6e70,
        0x27b70a8546d22ffc,
        0x2e1b21385c26c926,
        0x4d2c6dfc5ac42aed,
        0x53380d139d95b3df,
        0x650a73548baf63de,
        0x766a0abb3c77b2a8,
        0x81c2c92e47edaee6,
        0x92722c851482353b,
        0xa2bfe8a14cf10364,
        0xa81a664bbc423001,
        0xc24b8b70d0f89791,
        0xc76c51a30654be30,
        0xd192e819d6ef5218,
        0xd69906245565a910,
        0xf40e35855771202a,
        0x106aa07032bbd1b8,
        0x19a4c116b8d2d0c8,
        0x1e376c085141ab53,
        0x2748774cdf8eeb99,
        0x34b0bcb5e19b48a8,
        0x391c0cb3c5c95a63,
        0x4ed8aa4ae3418acb,
        0x5b9cca4f7763e373,
        0x682e6ff3d6b2b8a3,
        0x748f82ee5defb2fc,
        0x78a5636f43172f60,
        0x84c87814a1f0ab72,
        0x8cc702081a6439ec,
        0x90befffa23631e28,
        0xa4506cebde82bde9,
        0xbef9a3f7b2c67915,
        0xc67178f2e372532b,
        0xca273eceea26619c,
        0xd186b8c721c0c207,
        0xeada7dd6cde0eb1e,
        0xf57d4f7fee6ed178,
        0x06f067aa72176fba,
        0x0a637dc5a2c898a6,
        0x113f9804bef90dae,
        0x1b710b35131c471b,
        0x28db77f523047d84,
        0x32caab7b40c72493,
        0x3c9ebe0a15c9bebc,
        0x431d67c49c100d4c,
        0x4cc5d4becb3e42b6,
        0x597f299cfc657e2a,
        0x5fcb6fab3ad6faec,
        0x6c44198c4a475817,
    ];
    let mut h = iv;
    let bit_len = (data.len() as u128).wrapping_mul(8);
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 128 != 112 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in padded.chunks_exact(128) {
        let mut w = [0u64; 80];
        for (i, word) in chunk.chunks_exact(8).enumerate() {
            w[i] = u64::from_be_bytes(word.try_into().unwrap());
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let mj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(mj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    h
}

fn sha512_hex(data: &[u8], iv: [u64; 8], words: usize) -> String {
    let h = sha512_core(data, iv);
    let mut out = String::with_capacity(words * 16);
    for word in &h[..words] {
        out.push_str(&format!("{word:016x}"));
    }
    out
}

#[derive(Deserialize)]
struct HashAlgoArgs {
    algo: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    data: Option<String>,
}

async fn hash_tool(args: Value) -> Result<String> {
    let HashAlgoArgs { algo, path, data } = serde_json::from_value(args)?;
    let bytes = one_input_bytes(path, data)?;
    let digest = match algo.as_str() {
        "sha224" => {
            const SHA224_IV: [u32; 8] = [
                0xc1059ed8, 0x367cd507, 0x3070dd17, 0xf70e5939, 0xffc00b31, 0x68581511, 0x64f98fa7,
                0xbefa4fa4,
            ];
            let mut out = String::with_capacity(56);
            for word in &sha256_core(&bytes, SHA224_IV)[..7] {
                out.push_str(&format!("{word:08x}"));
            }
            out
        }
        "sha384" => sha512_hex(&bytes, SHA384_IV, 6),
        "sha512" => sha512_hex(&bytes, SHA512_IV, 8),
        other => return Err(anyhow!("unknown algo: {other} (want sha224|sha384|sha512)")),
    };
    Ok(digest)
}

/// HMAC-SHA256 (RFC 2104) of `msg` under `key`, lowercase hex.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> String {
    let mut k = if key.len() > 64 {
        sha256_raw(key).to_vec()
    } else {
        key.to_vec()
    };
    k.resize(64, 0);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let mut inner = ipad;
    inner.extend_from_slice(msg);
    let inner_digest = sha256_raw(&inner);
    let mut outer = opad;
    outer.extend_from_slice(&inner_digest);
    sha256_raw(&outer)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Deserialize)]
struct HmacArgs {
    data: String,
    key: String,
    #[serde(default)]
    key_hex: bool,
}

async fn hmac_sha256_tool(args: Value) -> Result<String> {
    let HmacArgs { data, key, key_hex } = serde_json::from_value(args)?;
    let key_bytes = if key_hex {
        hex_decode(&key).context("key_hex")?
    } else {
        key.into_bytes()
    };
    Ok(hmac_sha256(&key_bytes, data.as_bytes()))
}

#[derive(Deserialize)]
struct HashVerifyArgs {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    data: Option<String>,
    expected: String,
}

async fn hash_verify(args: Value) -> Result<String> {
    let HashVerifyArgs {
        path,
        data,
        expected,
    } = serde_json::from_value(args)?;
    let actual = sha256(&one_input_bytes(path, data)?);
    let expected_norm = expected.trim().to_ascii_lowercase();
    if expected_norm.len() != 64 || !expected_norm.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(anyhow!("expected must be a 64-char hex SHA-256 digest"));
    }
    Ok(serde_json::to_string_pretty(&json!({
        "match": actual == expected_norm,
        "actual": actual,
        "expected": expected_norm,
    }))?)
}

#[derive(Deserialize)]
struct HexArgs {
    data: String,
    #[serde(default)]
    decode: bool,
}

async fn hex_tool(args: Value) -> Result<String> {
    let HexArgs { data, decode } = serde_json::from_value(args)?;
    if decode {
        Ok(String::from_utf8_lossy(&hex_decode(&data)?).into_owned())
    } else {
        let mut out = String::with_capacity(data.len() * 2);
        for b in data.as_bytes() {
            out.push_str(&format!("{b:02x}"));
        }
        Ok(out)
    }
}

fn hex_decode(s: &str) -> Result<Vec<u8>> {
    fn nib(c: u8) -> Result<u8> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err(anyhow!("invalid hex character: {:?}", c as char)),
        }
    }
    let stripped: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if !stripped.len().is_multiple_of(2) {
        return Err(anyhow!("hex input has odd length"));
    }
    let mut out = Vec::with_capacity(stripped.len() / 2);
    for pair in stripped.chunks(2) {
        out.push((nib(pair[0])? << 4) | nib(pair[1])?);
    }
    Ok(out)
}

const B32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const B32_HEX_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHIJKLMNOPQRSTUV";

#[derive(Deserialize)]
struct Base32Args {
    data: String,
    #[serde(default)]
    decode: bool,
    #[serde(default)]
    hex_variant: bool,
}

async fn base32_tool(args: Value) -> Result<String> {
    let Base32Args {
        data,
        decode,
        hex_variant,
    } = serde_json::from_value(args)?;
    if decode {
        Ok(String::from_utf8_lossy(&base32_decode(&data, hex_variant)?).into_owned())
    } else {
        let alphabet = if hex_variant {
            B32_HEX_ALPHABET
        } else {
            B32_ALPHABET
        };
        Ok(base32_encode(data.as_bytes(), alphabet))
    }
}

fn base32_encode(data: &[u8], alphabet: &[u8; 32]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = ((buf[0] as u64) << 32)
            | ((buf[1] as u64) << 24)
            | ((buf[2] as u64) << 16)
            | ((buf[3] as u64) << 8)
            | (buf[4] as u64);
        let out_chars = match chunk.len() {
            1 => 2,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => 8,
        };
        for i in 0..8 {
            if i < out_chars {
                out.push(alphabet[((n >> (35 - 5 * i)) & 0x1f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base32_decode(s: &str, hex_variant: bool) -> Result<Vec<u8>> {
    let val = |c: u8| -> Result<u64> {
        let v = if hex_variant {
            match c {
                b'0'..=b'9' => c - b'0',
                b'A'..=b'V' => c - b'A' + 10,
                b'a'..=b'v' => c - b'a' + 10,
                _ => return Err(anyhow!("invalid base32hex character: {:?}", c as char)),
            }
        } else {
            match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a',
                b'2'..=b'7' => c - b'2' + 26,
                _ => return Err(anyhow!("invalid base32 character: {:?}", c as char)),
            }
        };
        Ok(v as u64)
    };
    let clean: Vec<u8> = s
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::new();
    for group in clean.chunks(8) {
        // Only the last chunk may be short, and a well-formed base32 tail is
        // 2/4/5/7 chars — 1/3/6 are impossible and would silently drop or
        // fabricate bytes. Reject them (mirrors base64_decode's len<2 guard).
        if group.len() < 8 && !matches!(group.len(), 2 | 4 | 5 | 7) {
            return Err(anyhow!("invalid base32 group length: {}", group.len()));
        }
        let bits = group.len() * 5;
        let mut n = 0u64;
        for &c in group {
            n = (n << 5) | val(c)?;
        }
        n <<= 64 - bits;
        for i in 0..bits / 8 {
            out.push(((n >> (56 - 8 * i)) & 0xff) as u8);
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
struct UrlEncodeArgs {
    data: String,
    #[serde(default)]
    decode: bool,
    #[serde(default)]
    component: bool,
}

async fn url_encode_tool(args: Value) -> Result<String> {
    let UrlEncodeArgs {
        data,
        decode,
        component,
    } = serde_json::from_value(args)?;
    if decode {
        url_percent_decode(&data)
    } else {
        Ok(url_percent_encode(data.as_bytes(), component))
    }
}

fn url_percent_encode(data: &[u8], component: bool) -> String {
    const RESERVED: &[u8] = b":/?#[]@!$&'()*+,;=";
    let mut out = String::with_capacity(data.len());
    for &b in data {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~');
        if unreserved || (!component && RESERVED.contains(&b)) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn url_percent_decode(s: &str) -> Result<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(anyhow!("invalid percent-encoding"));
            }
            let hi = (bytes[i + 1] as char)
                .to_digit(16)
                .ok_or_else(|| anyhow!("invalid percent-encoding"))?;
            let lo = (bytes[i + 2] as char)
                .to_digit(16)
                .ok_or_else(|| anyhow!("invalid percent-encoding"))?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

#[derive(Deserialize)]
struct JwtDecodeArgs {
    token: String,
    #[serde(default = "default_true")]
    pretty: bool,
}

async fn jwt_decode_tool(args: Value) -> Result<String> {
    let JwtDecodeArgs { token, pretty } = serde_json::from_value(args)?;
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(anyhow!(
            "not a JWT: expected 3 dot-separated segments, got {}",
            parts.len()
        ));
    }
    let decode_seg = |seg: &str| -> Result<Value> {
        let standard = seg.replace('-', "+").replace('_', "/");
        let bytes = base64_decode(&standard).context("segment is not valid base64url")?;
        let text = String::from_utf8(bytes).context("segment is not UTF-8")?;
        serde_json::from_str::<Value>(&text).context("segment is not JSON")
    };
    let out = json!({ "header": decode_seg(parts[0])?, "payload": decode_seg(parts[1])? });
    Ok(if pretty {
        serde_json::to_string_pretty(&out)?
    } else {
        out.to_string()
    })
}

async fn date_tool(_args: Value) -> Result<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Convert via libc::localtime for a quick local-time render — no
    // chrono dep needed.
    Ok(format!(
        "unix:  {now}\nutc:   {}\nlocal: {}",
        format_unix(now, false),
        format_unix(now, true)
    ))
}

#[cfg(unix)]
fn format_unix(t: u64, local: bool) -> String {
    use std::ffi::CStr;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let tt = t as libc::time_t;
    unsafe {
        if local {
            libc::localtime_r(&tt, &mut tm);
        } else {
            libc::gmtime_r(&tt, &mut tm);
        }
    }
    let mut buf = [0u8; 64];
    let fmt = c"%Y-%m-%d %H:%M:%S %Z";
    let n = unsafe { libc::strftime(buf.as_mut_ptr().cast(), buf.len(), fmt.as_ptr(), &tm) };
    if n == 0 {
        return format!("{t}");
    }
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr().cast()) };
    cstr.to_string_lossy().into_owned()
}

#[cfg(not(unix))]
fn format_unix(t: u64, _local: bool) -> String {
    format!("{t}")
}

/// Run `cmd args…` and return combined stdout/stderr + exit hint.
/// Used by lint/format/typecheck; missing binary surfaces as a
/// "not found" error so the agent knows to suggest installing.
async fn run_command(cmd: &str, args: &[&str]) -> Result<String> {
    let mut c = Command::new(cmd);
    c.args(args);
    c.stdin(Stdio::null());
    c.stdout(Stdio::piped());
    c.stderr(Stdio::piped());
    let out = c
        .output()
        .await
        .with_context(|| format!("running `{cmd}`"))?;
    let mut body = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.is_empty() {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&err);
    }
    if !out.status.success() {
        body.push_str(&format!("\n[exit {}]", out.status.code().unwrap_or(-1)));
    }
    if body.trim().is_empty() {
        body = "(no output)".to_string();
    }
    Ok(body)
}

fn ext_of(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
}

async fn lint_tool(args: Value) -> Result<String> {
    let PathArgs { path } = serde_json::from_value(args)?;
    let ext = ext_of(&path);
    match ext.as_deref() {
        Some("rs") => {
            // cargo clippy operates on the whole workspace; the path is
            // informational only here.
            run_command(
                "cargo",
                &["clippy", "--all-targets", "--", "-D", "warnings"],
            )
            .await
        }
        Some("py") => {
            // Prefer ruff (fast), fall back to flake8 if ruff isn't on PATH.
            match run_command("ruff", &["check", &path]).await {
                Ok(o) => Ok(o),
                Err(_) => run_command("flake8", &[&path]).await,
            }
        }
        Some("js") | Some("jsx") | Some("ts") | Some("tsx") => {
            run_command("eslint", &[&path]).await
        }
        Some("go") => run_command("go", &["vet", &path]).await,
        Some("sh") | Some("bash") => run_command("shellcheck", &[&path]).await,
        Some(other) => Err(anyhow!("no linter known for .{other} files")),
        None => Err(anyhow!("no extension on {path} — can't pick a linter")),
    }
}

async fn format_tool(args: Value) -> Result<String> {
    let PathArgs { path } = serde_json::from_value(args)?;
    let ext = ext_of(&path);
    match ext.as_deref() {
        Some("rs") => run_command("rustfmt", &[&path]).await,
        Some("py") => match run_command("ruff", &["format", &path]).await {
            Ok(o) => Ok(o),
            Err(_) => run_command("black", &[&path]).await,
        },
        Some("js") | Some("jsx") | Some("ts") | Some("tsx") | Some("json") | Some("md")
        | Some("css") | Some("html") | Some("yaml") | Some("yml") => {
            run_command("prettier", &["--write", &path]).await
        }
        Some("go") => run_command("gofmt", &["-w", &path]).await,
        Some(other) => Err(anyhow!("no formatter known for .{other} files")),
        None => Err(anyhow!("no extension on {path} — can't pick a formatter")),
    }
}

async fn typecheck_tool(args: Value) -> Result<String> {
    let PathArgs { path } = serde_json::from_value(args)?;
    let ext = ext_of(&path);
    match ext.as_deref() {
        Some("rs") => run_command("cargo", &["check", "--all-targets"]).await,
        Some("py") => run_command("mypy", &[&path]).await,
        Some("ts") | Some("tsx") => run_command("tsc", &["--noEmit", &path]).await,
        Some("go") => run_command("go", &["build", "-o", "/dev/null", &path]).await,
        Some(other) => Err(anyhow!("no type-checker known for .{other} files")),
        None => Err(anyhow!(
            "no extension on {path} — can't pick a type-checker"
        )),
    }
}

async fn test_tool(args: Value) -> Result<String> {
    let PathArgs { path } = serde_json::from_value(args)?;
    let ext = ext_of(&path);
    match ext.as_deref() {
        // cargo test operates on the whole workspace; the path is
        // informational only here, matching `typecheck`.
        Some("rs") => run_command("cargo", &["test", "--all-targets"]).await,
        Some("py") => run_command("pytest", &[&path]).await,
        Some("go") => run_command("go", &["test", "./..."]).await,
        Some("js") | Some("jsx") | Some("ts") | Some("tsx") => run_command("npm", &["test"]).await,
        Some(other) => Err(anyhow!("no test runner known for .{other} files")),
        None => Err(anyhow!("no extension on {path} — can't pick a test runner")),
    }
}

#[derive(Deserialize)]
struct GitArgs {
    subcommand: GitSub,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default, rename = "ref")]
    reference: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    staged: bool,
    #[serde(default)]
    start_line: Option<u64>,
    #[serde(default)]
    end_line: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum GitSub {
    Status,
    Diff,
    Log,
    Add,
    Commit,
    Show,
    Blame,
    DiffStat,
}

async fn git_tool(args: Value) -> Result<String> {
    let GitArgs {
        subcommand,
        paths,
        message,
        reference,
        path,
        staged,
        start_line,
        end_line,
    } = serde_json::from_value(args)?;
    let mut argv: Vec<&str> = match subcommand {
        GitSub::Status => vec!["status", "--short", "--branch"],
        GitSub::Diff => vec!["diff"],
        GitSub::Log => vec!["log", "--oneline", "-n", "20"],
        GitSub::Add => {
            if paths.is_empty() {
                return Err(anyhow!("git add requires `paths`"));
            }
            vec!["add"]
        }
        GitSub::Commit => {
            let msg = message
                .as_deref()
                .ok_or_else(|| anyhow!("git commit requires `message`"))?;
            return run_command("git", &["commit", "-m", msg]).await;
        }
        GitSub::Show => {
            let r = reference
                .as_deref()
                .ok_or_else(|| anyhow!("git show requires `ref`"))?;
            if r.starts_with('-') {
                return Err(anyhow!("invalid ref"));
            }
            let target = match path.as_deref() {
                Some(p) => format!("{r}:{p}"),
                None => r.to_string(),
            };
            return run_command("git", &["show", &target]).await;
        }
        GitSub::Blame => {
            let p = path
                .as_deref()
                .ok_or_else(|| anyhow!("git blame requires `path`"))?;
            let range = match (start_line, end_line) {
                (Some(s), Some(e)) => {
                    if s == 0 || e < s {
                        return Err(anyhow!("git blame: need 1 <= start_line <= end_line"));
                    }
                    Some(format!("{s},{e}"))
                }
                (None, None) => None,
                _ => {
                    return Err(anyhow!(
                        "git blame: start_line and end_line must be given together"
                    ))
                }
            };
            let mut argv: Vec<&str> = vec!["blame"];
            if let Some(r) = range.as_deref() {
                argv.push("-L");
                argv.push(r);
            }
            argv.push("--");
            argv.push(p);
            return run_command("git", &argv).await;
        }
        GitSub::DiffStat => {
            let mut argv: Vec<&str> = vec!["diff", "--stat"];
            if staged {
                argv.push("--cached");
            }
            if let Some(r) = reference.as_deref() {
                if r.starts_with('-') {
                    return Err(anyhow!("invalid ref"));
                }
                argv.push(r);
            }
            if !paths.is_empty() {
                argv.push("--");
                argv.extend(paths.iter().map(String::as_str));
            }
            return run_command("git", &argv).await;
        }
    };
    // add/diff take the caller's paths; status/log ignore them.
    if matches!(subcommand, GitSub::Add | GitSub::Diff) {
        argv.extend(paths.iter().map(String::as_str));
    }
    run_command("git", &argv).await
}

async fn symlink_tool(args: Value) -> Result<String> {
    let MoveArgs { src, dst } = serde_json::from_value(args)?;
    if std::fs::symlink_metadata(&dst).is_ok() {
        return Err(anyhow!("destination already exists: {dst}"));
    }
    make_symlink(&src, &dst).with_context(|| format!("symlink {dst} -> {src}"))?;
    Ok(format!("symlinked {dst} -> {src}"))
}

#[cfg(unix)]
fn make_symlink(src: &str, dst: &str) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn make_symlink(src: &str, dst: &str) -> std::io::Result<()> {
    if std::path::Path::new(src).is_dir() {
        std::os::windows::fs::symlink_dir(src, dst)
    } else {
        std::os::windows::fs::symlink_file(src, dst)
    }
}

#[derive(Deserialize)]
struct EnvArgs {
    #[serde(default)]
    name: Option<String>,
}

async fn env_tool(args: Value) -> Result<String> {
    let EnvArgs { name } = serde_json::from_value(args)?;
    match name {
        Some(n) => std::env::var(&n).map_err(|_| anyhow!("${n} is not set")),
        None => {
            let mut vars: Vec<(String, String)> = std::env::vars().collect();
            vars.sort();
            Ok(vars
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("\n"))
        }
    }
}

#[derive(Deserialize)]
struct ReplaceArgs {
    path: String,
    pattern: String,
    replacement: String,
    #[serde(default = "default_true")]
    all: bool,
}

fn default_true() -> bool {
    true
}

async fn replace_tool(args: Value) -> Result<String> {
    let ReplaceArgs {
        path,
        pattern,
        replacement,
        all,
    } = serde_json::from_value(args)?;
    let re = regex::Regex::new(&pattern).with_context(|| format!("invalid regex: {pattern}"))?;
    let src = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {path}"))?;
    let count = if all {
        re.find_iter(&src).count()
    } else {
        re.find(&src).map_or(0, |_| 1)
    };
    let out = if all {
        re.replace_all(&src, replacement.as_str())
    } else {
        re.replace(&src, replacement.as_str())
    };
    tokio::fs::write(&path, out.as_ref())
        .await
        .with_context(|| format!("write {path}"))?;
    Ok(format!("replaced {count} occurrence(s) in {path}"))
}

#[derive(Deserialize)]
struct JsonArgs {
    path: String,
    pointer: String,
}

async fn json_tool(args: Value) -> Result<String> {
    let JsonArgs { path, pointer } = serde_json::from_value(args)?;
    let text = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {path}"))?;
    let doc: Value =
        serde_json::from_str(&text).with_context(|| format!("parsing JSON from {path}"))?;
    let found = if pointer.is_empty() {
        &doc
    } else {
        doc.pointer(&pointer)
            .ok_or_else(|| anyhow!("no value at pointer `{pointer}`"))?
    };
    Ok(serde_json::to_string_pretty(found)?)
}

#[derive(Deserialize)]
struct Base64Args {
    data: String,
    #[serde(default)]
    decode: bool,
}

async fn base64_tool(args: Value) -> Result<String> {
    let Base64Args { data, decode } = serde_json::from_value(args)?;
    if decode {
        let bytes = base64_decode(&data)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        Ok(base64_encode(data.as_bytes()))
    }
}

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(B64_ALPHABET[(n >> 18 & 0x3f) as usize] as char);
        out.push(B64_ALPHABET[(n >> 12 & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64_ALPHABET[(n >> 6 & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(s: &str) -> Result<Vec<u8>> {
    fn val(c: u8) -> Result<u32> {
        match c {
            b'A'..=b'Z' => Ok((c - b'A') as u32),
            b'a'..=b'z' => Ok((c - b'a' + 26) as u32),
            b'0'..=b'9' => Ok((c - b'0' + 52) as u32),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(anyhow!("invalid base64 character: {:?}", c as char)),
        }
    }
    let stripped: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let body = stripped
        .strip_suffix(b"==")
        .unwrap_or(stripped.strip_suffix(b"=").unwrap_or(&stripped));
    let mut out = Vec::with_capacity(body.len() / 4 * 3);
    for chunk in body.chunks(4) {
        if chunk.len() < 2 {
            return Err(anyhow!("truncated base64 input"));
        }
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        out.push((n >> 16 & 0xff) as u8);
        if chunk.len() >= 3 {
            out.push((n >> 8 & 0xff) as u8);
        }
        if chunk.len() >= 4 {
            out.push((n & 0xff) as u8);
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
struct HexdumpArgs {
    path: String,
    #[serde(default)]
    bytes: Option<usize>,
}

async fn hexdump_tool(args: Value) -> Result<String> {
    let HexdumpArgs { path, bytes } = serde_json::from_value(args)?;
    let cap = bytes.unwrap_or(256).min(4096);
    let data = tokio::fs::read(&path)
        .await
        .with_context(|| format!("read {path}"))?;
    let slice = &data[..data.len().min(cap)];
    let mut out = String::new();
    for (row, bytes) in slice.chunks(16).enumerate() {
        let mut hex = String::new();
        let mut ascii = String::new();
        for (i, &b) in bytes.iter().enumerate() {
            hex.push_str(&format!("{b:02x} "));
            if i == 7 {
                hex.push(' ');
            }
            ascii.push(if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '.'
            });
        }
        out.push_str(&format!("{:08x}  {hex:<49}|{ascii}|\n", row * 16));
    }
    if data.len() > cap {
        out.push_str(&format!("[truncated at {cap} of {} bytes]\n", data.len()));
    }
    if out.is_empty() {
        out.push_str("(empty file)\n");
    }
    Ok(out)
}

async fn du_tool(args: Value) -> Result<String> {
    let PathArgs { path } = serde_json::from_value(args)?;
    let total = dir_size(std::path::Path::new(&path)).with_context(|| format!("sizing {path}"))?;
    Ok(format!("{total} bytes ({}) {path}", human_bytes(total)))
}

/// Recursive byte total of `path`. A file contributes its own size; a
/// directory sums its entries. Symlinks are counted by their own (link)
/// size and never traversed, so cycles can't loop.
fn dir_size(path: &std::path::Path) -> std::io::Result<u64> {
    let md = std::fs::symlink_metadata(path)?;
    if md.file_type().is_dir() {
        let mut total = 0;
        for entry in std::fs::read_dir(path)? {
            total += dir_size(&entry?.path())?;
        }
        Ok(total)
    } else {
        Ok(md.len())
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = n as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} {}", UNITS[0])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

async fn realpath_tool(args: Value) -> Result<String> {
    let PathArgs { path } = serde_json::from_value(args)?;
    let canon = std::fs::canonicalize(&path).with_context(|| format!("canonicalize {path}"))?;
    Ok(canon.display().to_string())
}

#[derive(Deserialize, Clone)]
struct TodoItem {
    content: String,
    status: TodoStatus,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    fn glyph(self) -> &'static str {
        match self {
            TodoStatus::Pending => "[ ]",
            TodoStatus::InProgress => "[~]",
            TodoStatus::Completed => "[x]",
        }
    }
}

#[derive(Deserialize)]
struct TodoArgs {
    todos: Vec<TodoItem>,
}

fn todo_state() -> &'static std::sync::Mutex<Vec<TodoItem>> {
    static STATE: std::sync::OnceLock<std::sync::Mutex<Vec<TodoItem>>> = std::sync::OnceLock::new();
    STATE.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

fn render_todos(todos: &[TodoItem]) -> String {
    if todos.is_empty() {
        return "(no todos)".to_string();
    }
    let mut out = String::new();
    for t in todos {
        out.push_str(t.status.glyph());
        out.push(' ');
        out.push_str(&t.content);
        out.push('\n');
    }
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

async fn todo_write_tool(args: Value) -> Result<String> {
    let TodoArgs { todos } = serde_json::from_value(args)?;
    let mut state = todo_state().lock().expect("todo state poisoned");
    *state = todos.clone();
    Ok(render_todos(&todos))
}

#[derive(Deserialize)]
struct FindArgs {
    path: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    regex: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    min_size: Option<u64>,
    #[serde(default)]
    max_size: Option<u64>,
    #[serde(default)]
    newer_than: Option<u64>,
    #[serde(default)]
    older_than: Option<u64>,
    #[serde(default)]
    max_depth: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

struct FindFilters {
    name: Option<glob::Pattern>,
    regex: Option<regex::Regex>,
    kind: Option<String>,
    min_size: Option<u64>,
    max_size: Option<u64>,
    newer_than: Option<u64>,
    older_than: Option<u64>,
}

async fn find_tool(args: Value) -> Result<String> {
    let FindArgs {
        path,
        name,
        regex,
        kind,
        min_size,
        max_size,
        newer_than,
        older_than,
        max_depth,
        limit,
    } = serde_json::from_value(args)?;

    let root = PathBuf::from(&path);
    if !root.is_dir() {
        return Err(anyhow!("not a directory: {path}"));
    }

    // Compile patterns up front so a bad glob/regex fails fast (grep_tool idiom).
    let name_pat = match &name {
        Some(g) => Some(glob::Pattern::new(g).with_context(|| format!("invalid glob: {g}"))?),
        None => None,
    };
    let re = match &regex {
        Some(r) => Some(regex::Regex::new(r).with_context(|| format!("invalid regex: {r}"))?),
        None => None,
    };

    const HARD_CAP: usize = 1000;
    let cap = limit.unwrap_or(200).min(HARD_CAP);
    let max_depth = max_depth.unwrap_or(usize::MAX);

    let filters = FindFilters {
        name: name_pat,
        regex: re,
        kind,
        min_size,
        max_size,
        newer_than,
        older_than,
    };

    let mut out = Vec::new();
    let mut truncated = false;
    find_walk(&root, 0, max_depth, &filters, cap, &mut out, &mut truncated);

    if out.is_empty() {
        return Ok(format!("no matches in {path}"));
    }
    if truncated {
        out.push(format!("[truncated at {cap} matches]"));
    }
    Ok(out.join("\n"))
}

/// Recursive walk for find_tool. Uses symlink_metadata so symlinks are
/// classified as symlinks and never followed (also avoids symlink cycles).
/// read_dir errors on a subdir skip that subtree (walk_tree idiom).
fn find_walk(
    dir: &std::path::Path,
    depth: usize,
    max_depth: usize,
    filters: &FindFilters,
    cap: usize,
    out: &mut Vec<String>,
    truncated: &mut bool,
) {
    if *truncated {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.filter_map(|r| r.ok()) {
        if out.len() >= cap {
            *truncated = true;
            return;
        }
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let ft = meta.file_type();

        if find_matches(&path, &meta, filters) {
            out.push(path.display().to_string());
            if out.len() >= cap {
                *truncated = true;
                return;
            }
        }

        // Recurse into real directories only (never into symlinks).
        if ft.is_dir() && depth + 1 < max_depth {
            find_walk(&path, depth + 1, max_depth, filters, cap, out, truncated);
            if *truncated {
                return;
            }
        }
    }
}

/// Evaluate all predicates for a single entry. `meta` is from
/// symlink_metadata so the entry is classified without following links.
fn find_matches(path: &std::path::Path, meta: &std::fs::Metadata, filters: &FindFilters) -> bool {
    let ft = meta.file_type();

    if let Some(k) = &filters.kind {
        let ok = match k.as_str() {
            "file" => ft.is_file(),
            "dir" => ft.is_dir(),
            "symlink" => ft.is_symlink(),
            _ => false,
        };
        if !ok {
            return false;
        }
    }

    // Size predicates apply to regular files only.
    if filters.min_size.is_some() || filters.max_size.is_some() {
        if !ft.is_file() {
            return false;
        }
        let len = meta.len();
        if let Some(min) = filters.min_size {
            if len < min {
                return false;
            }
        }
        if let Some(max) = filters.max_size {
            if len > max {
                return false;
            }
        }
    }

    if let Some(pat) = &filters.name {
        let fname = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !pat.matches(&fname) {
            return false;
        }
    }

    if let Some(re) = &filters.regex {
        // Match against forward-slash-separated paths on every platform, so a
        // regex written with `/` (the natural form) works on Windows too.
        let hay = path.to_string_lossy().replace('\\', "/");
        if !re.is_match(&hay) {
            return false;
        }
    }

    if filters.newer_than.is_some() || filters.older_than.is_some() {
        // elapsed() returns Err for future-dated mtimes (clock skew /
        // freshly touched) — treat future as age 0, never `?`.
        let age = meta
            .modified()
            .ok()
            .and_then(|mt| mt.elapsed().ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Some(n) = filters.newer_than {
            if age > n {
                return false;
            }
        }
        if let Some(n) = filters.older_than {
            if age < n {
                return false;
            }
        }
    }

    true
}

#[derive(Deserialize)]
struct ChmodArgs {
    path: String,
    mode: String,
}

async fn chmod_tool(args: Value) -> Result<String> {
    let ChmodArgs { path, mode } = serde_json::from_value(args)?;
    let bits = u32::from_str_radix(mode.trim().trim_start_matches("0o"), 8)
        .map_err(|_| anyhow!("invalid octal mode: {mode}"))?;
    if bits > 0o7777 {
        return Err(anyhow!("mode out of range: {mode}"));
    }
    chmod_set_mode(&path, bits)
}

#[cfg(unix)]
fn chmod_set_mode(path: &str, bits: u32) -> Result<String> {
    use std::os::unix::fs::PermissionsExt;
    // std::fs::metadata (not symlink_metadata) so we follow symlinks like
    // real chmod does.
    let mut perms = std::fs::metadata(path)
        .with_context(|| format!("stat {path}"))?
        .permissions();
    perms.set_mode(bits);
    std::fs::set_permissions(path, perms).with_context(|| format!("chmod {path}"))?;
    Ok(format!("chmod {:o} {path}", bits & 0o7777))
}

#[cfg(windows)]
fn chmod_set_mode(path: &str, bits: u32) -> Result<String> {
    // Windows has no Unix mode bits; the only permission knob std exposes is
    // the read-only attribute. Map the owner-write bit onto it and report
    // honestly that nothing else was applied.
    let mut perms = std::fs::metadata(path)
        .with_context(|| format!("stat {path}"))?
        .permissions();
    let writable = bits & 0o200 != 0;
    perms.set_readonly(!writable);
    std::fs::set_permissions(path, perms).with_context(|| format!("chmod {path}"))?;
    Ok(format!(
        "chmod (windows: read-only attribute {}) {path}",
        if writable { "cleared" } else { "set" }
    ))
}

async fn readlink_tool(args: Value) -> Result<String> {
    let PathArgs { path } = serde_json::from_value(args)?;
    let target =
        std::fs::read_link(&path).with_context(|| format!("readlink {path} (not a symlink?)"))?;
    Ok(target.to_string_lossy().into_owned())
}

async fn hardlink_tool(args: Value) -> Result<String> {
    let MoveArgs { src, dst } = serde_json::from_value(args)?;
    if std::fs::symlink_metadata(&dst).is_ok() {
        return Err(anyhow!("destination already exists: {dst}"));
    }
    std::fs::hard_link(&src, &dst).with_context(|| format!("hardlink {dst} -> {src}"))?;
    Ok(format!("hardlinked {dst} -> {src}"))
}

async fn pathinfo_tool(args: Value) -> Result<String> {
    let PathArgs { path } = serde_json::from_value(args)?;
    let p = std::path::Path::new(&path);
    // to_string_lossy (not to_str) so a non-UTF-8 component degrades to
    // lossy text rather than silently becoming null and dropping a field.
    let f = |o: Option<&std::ffi::OsStr>| o.map(|s| s.to_string_lossy().into_owned());
    let out = json!({
        "parent": p.parent().map(|x| x.to_string_lossy().into_owned()),
        "file_name": f(p.file_name()),
        "file_stem": f(p.file_stem()),
        "extension": f(p.extension()),
    });
    Ok(out.to_string())
}

#[derive(Deserialize)]
struct MktempArgs {
    #[serde(default)]
    dir: bool,
    #[serde(default = "mktemp_default_prefix")]
    prefix: String,
    #[serde(default)]
    suffix: String,
}

fn mktemp_default_prefix() -> String {
    "teleia-".to_string()
}

async fn mktemp_tool(args: Value) -> Result<String> {
    static MKTEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let MktempArgs {
        dir,
        prefix,
        suffix,
    } = serde_json::from_value(args)?;
    let base = std::env::temp_dir();
    for _ in 0..1000 {
        let n = MKTEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Re-read the clock inside the loop: if a create_new race is lost,
        // a stale nanos value would spin forever on the same name.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let token = (std::process::id() as u64) ^ n ^ nanos;
        let name = format!("{prefix}{token:016x}{suffix}");
        let path = base.join(&name);
        let res = if dir {
            let r = std::fs::create_dir(&path);
            // Match mkdtemp(3): keep the dir private to the owner (0700).
            #[cfg(unix)]
            if r.is_ok() {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
            }
            r
        } else {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            // Match mkstemp(3): create 0600 atomically rather than honoring
            // umask (usually 0644, world-readable) in a shared temp dir.
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            // Drop the handle so the file persists, closed, on disk.
            opts.open(&path).map(|_| ())
        };
        match res {
            Ok(()) => return Ok(path.display().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(anyhow::Error::from(e)).with_context(|| {
                    format!(
                        "creating temp {} at {}",
                        if dir { "dir" } else { "file" },
                        path.display()
                    )
                });
            }
        }
    }
    Err(anyhow!(
        "mktemp: exhausted retries finding a free name in {}",
        base.display()
    ))
}

#[derive(Deserialize)]
struct TruncateArgs {
    path: String,
    size: u64,
}

async fn truncate_tool(args: Value) -> Result<String> {
    let TruncateArgs { path, size } = serde_json::from_value(args)?;
    // write(true) only: no create(true) (would defeat the intentional
    // "error if absent" guarantee) and no truncate(true) (that flag zeroes
    // the file to 0 and would ignore `size`). set_len maps to ftruncate on
    // unix and SetEndOfFile on Windows — portable, no cfg split.
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .with_context(|| format!("truncate {path}"))?;
    f.set_len(size).with_context(|| format!("set_len {path}"))?;
    Ok(format!("truncated {path} to {size} bytes"))
}

#[derive(Deserialize)]
struct SliceArgs {
    path: String,
    start: usize,
    #[serde(default)]
    end: Option<usize>,
    #[serde(default)]
    number: bool,
}

async fn slice_tool(args: Value) -> Result<String> {
    let SliceArgs {
        path,
        start,
        end,
        number,
    } = serde_json::from_value(args)?;
    if start == 0 {
        return Err(anyhow!("start must be >= 1"));
    }
    if let Some(e) = end {
        if e < start {
            return Err(anyhow!("end must be >= start"));
        }
    }
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("read_to_string {path}"))?;
    let out: Vec<String> = content
        .lines()
        .enumerate()
        .skip(start - 1)
        .take_while(|(i, _)| end.is_none_or(|e| *i < e))
        .take(MAX_LINES)
        .map(|(i, l)| {
            if number {
                format!("{}\t{}", i + 1, l)
            } else {
                l.to_string()
            }
        })
        .collect();
    Ok(out.join("\n"))
}

#[derive(Deserialize)]
struct SortArgs {
    path: String,
    #[serde(default)]
    reverse: bool,
    #[serde(default)]
    numeric: bool,
    #[serde(default)]
    unique: bool,
    #[serde(default)]
    ignore_case: bool,
    #[serde(default)]
    field: Option<usize>,
    #[serde(default)]
    delimiter: Option<String>,
}

/// Extract the comparison key for one line: pick the 1-based `field`
/// (splitting on `delimiter`, or whitespace when it's None/empty), then
/// lowercase it when case-insensitive. Out-of-range fields yield "" so
/// nothing panics.
fn sort_key(
    line: &str,
    field: Option<usize>,
    delimiter: &Option<String>,
    ignore_case: bool,
) -> String {
    let raw = match field {
        Some(f) if f >= 1 => {
            let idx = f - 1;
            let picked = match delimiter {
                Some(d) if !d.is_empty() => line.split(d.as_str()).nth(idx),
                _ => line.split_whitespace().nth(idx),
            };
            picked.unwrap_or("").to_string()
        }
        _ => line.to_string(),
    };
    if ignore_case {
        raw.to_lowercase()
    } else {
        raw
    }
}

/// Parse a leading number from `s` for numeric sorts, defaulting to 0.0
/// when there's no parseable prefix (matches `sort -n` on non-numeric
/// input).
fn sort_numeric_key(s: &str) -> f64 {
    let t = s.trim_start();
    let end = t
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+'))
        .map(|(i, _)| i)
        .unwrap_or(t.len());
    t[..end].parse::<f64>().unwrap_or(0.0)
}

async fn sort_tool(args: Value) -> Result<String> {
    let SortArgs {
        path,
        reverse,
        numeric,
        unique,
        ignore_case,
        field,
        delimiter,
    } = serde_json::from_value(args)?;
    let content = std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?;
    let mut lines: Vec<&str> = content.lines().collect();
    if numeric {
        lines.sort_by(|a, b| {
            let ka = sort_numeric_key(&sort_key(a, field, &delimiter, ignore_case));
            let kb = sort_numeric_key(&sort_key(b, field, &delimiter, ignore_case));
            ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        lines.sort_by(|a, b| {
            let ka = sort_key(a, field, &delimiter, ignore_case);
            let kb = sort_key(b, field, &delimiter, ignore_case);
            ka.cmp(&kb)
        });
    }
    if unique {
        lines.dedup();
    }
    if reverse {
        lines.reverse();
    }
    lines.truncate(MAX_LINES);
    Ok(lines.join("\n"))
}

#[derive(Deserialize)]
struct CutArgs {
    path: String,
    #[serde(default)]
    delimiter: Option<String>,
    #[serde(default)]
    fields: Option<String>,
    #[serde(default)]
    chars: Option<String>,
    #[serde(default)]
    complement: bool,
}

/// Parse a 1-based cut spec ("1,3-5", "2-", "-3") into (lo, hi) ranges
/// where `hi == None` means "to end of line". Rejects 0 (positions are
/// 1-based) and reversed ranges.
fn cut_parse_spec(s: &str) -> Result<Vec<(usize, Option<usize>)>> {
    let mut ranges = Vec::new();
    for tok in s.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            return Err(anyhow!("empty field/char spec"));
        }
        if let Some((lo, hi)) = tok.split_once('-') {
            let lo = if lo.is_empty() {
                1
            } else {
                lo.parse::<usize>()
                    .map_err(|_| anyhow!("invalid range start: {tok:?}"))?
            };
            let hi = if hi.is_empty() {
                None
            } else {
                Some(
                    hi.parse::<usize>()
                        .map_err(|_| anyhow!("invalid range end: {tok:?}"))?,
                )
            };
            if lo == 0 || hi == Some(0) {
                return Err(anyhow!("positions are 1-based, got {tok:?}"));
            }
            if let Some(h) = hi {
                if h < lo {
                    return Err(anyhow!("range end before start: {tok:?}"));
                }
            }
            ranges.push((lo, hi));
        } else {
            let n = tok
                .parse::<usize>()
                .map_err(|_| anyhow!("invalid position: {tok:?}"))?;
            if n == 0 {
                return Err(anyhow!("positions are 1-based, got {tok:?}"));
            }
            ranges.push((n, Some(n)));
        }
    }
    Ok(ranges)
}

/// True when 1-based index `idx1` falls in any range, XOR-ed with the
/// complement flag.
fn cut_selected(idx1: usize, ranges: &[(usize, Option<usize>)], complement: bool) -> bool {
    let hit = ranges
        .iter()
        .any(|&(lo, hi)| idx1 >= lo && hi.is_none_or(|h| idx1 <= h));
    hit ^ complement
}

async fn cut_tool(args: Value) -> Result<String> {
    let CutArgs {
        path,
        delimiter,
        fields,
        chars,
        complement,
    } = serde_json::from_value(args)?;
    let (spec, char_mode) = match (fields, chars) {
        (Some(f), None) => (f, false),
        (None, Some(c)) => (c, true),
        (None, None) => return Err(anyhow!("exactly one of `fields` or `chars` is required")),
        (Some(_), Some(_)) => return Err(anyhow!("`fields` and `chars` are mutually exclusive")),
    };
    let ranges = cut_parse_spec(&spec)?;
    let delim = delimiter.as_deref().unwrap_or("\t");
    if !char_mode && delim.is_empty() {
        return Err(anyhow!("delimiter must be non-empty"));
    }
    let src = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {path}"))?;

    let mut out = String::new();
    for line in src.lines() {
        if char_mode {
            let picked: String = line
                .chars()
                .enumerate()
                .filter(|(i, _)| cut_selected(i + 1, &ranges, complement))
                .map(|(_, c)| c)
                .collect();
            out.push_str(&picked);
        } else {
            let cols: Vec<&str> = line.split(delim).collect();
            if cols.len() == 1 {
                // No delimiter present: GNU cut passes the whole line through.
                out.push_str(line);
            } else {
                let picked: Vec<&str> = cols
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| cut_selected(i + 1, &ranges, complement))
                    .map(|(_, s)| *s)
                    .collect();
                out.push_str(&picked.join(delim));
            }
        }
        out.push('\n');
    }
    Ok(out)
}

#[derive(Deserialize)]
struct CommArgs {
    a: String,
    b: String,
    #[serde(default)]
    only_a: bool,
    #[serde(default)]
    only_b: bool,
    #[serde(default)]
    common: bool,
    #[serde(default)]
    ignore_case: bool,
}

/// Build a sorted key -> first-seen-original map for a file's lines.
/// Using a BTreeMap gives deterministic sorted iteration (no separate
/// sort) and set semantics (duplicate lines collapse). The key applies
/// case folding when requested while the value preserves original casing.
fn comm_line_map(content: &str, ignore_case: bool) -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    for line in content.lines() {
        let key = if ignore_case {
            line.to_lowercase()
        } else {
            line.to_string()
        };
        map.entry(key).or_insert_with(|| line.to_string());
    }
    map
}

async fn comm_tool(args: Value) -> Result<String> {
    let CommArgs {
        a,
        b,
        only_a,
        only_b,
        common,
        ignore_case,
    } = serde_json::from_value(args)?;
    let content_a = tokio::fs::read_to_string(&a)
        .await
        .with_context(|| format!("read {a}"))?;
    let content_b = tokio::fs::read_to_string(&b)
        .await
        .with_context(|| format!("read {b}"))?;
    let map_a = comm_line_map(&content_a, ignore_case);
    let map_b = comm_line_map(&content_b, ignore_case);

    // If the caller requested no specific section, emit all three.
    let all = !(only_a || only_b || common);
    let want_only_a = only_a || all;
    let want_only_b = only_b || all;
    let want_common = common || all;

    let mut sections: Vec<(&str, Vec<String>)> = Vec::new();
    if want_only_a {
        let lines: Vec<String> = map_a
            .iter()
            .filter(|(k, _)| !map_b.contains_key(*k))
            .map(|(_, v)| v.clone())
            .collect();
        sections.push(("only in A", lines));
    }
    if want_only_b {
        let lines: Vec<String> = map_b
            .iter()
            .filter(|(k, _)| !map_a.contains_key(*k))
            .map(|(_, v)| v.clone())
            .collect();
        sections.push(("only in B", lines));
    }
    if want_common {
        // Emit A's stored original (first-seen) for shared keys.
        let lines: Vec<String> = map_a
            .iter()
            .filter(|(k, _)| map_b.contains_key(*k))
            .map(|(_, v)| v.clone())
            .collect();
        sections.push(("common", lines));
    }

    // Single active section: return bare lines (no header noise).
    if sections.len() == 1 {
        let (_, lines) = &sections[0];
        return Ok(lines.join("\n"));
    }
    // Multiple sections: label each block.
    let blocks: Vec<String> = sections
        .iter()
        .map(|(label, lines)| {
            let body = if lines.is_empty() {
                "(none)".to_string()
            } else {
                lines.join("\n")
            };
            format!("== {label} ==\n{body}")
        })
        .collect();
    Ok(blocks.join("\n\n"))
}

#[derive(Deserialize)]
struct StringsArgs {
    path: String,
    #[serde(default)]
    min_len: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn strings_tool(args: Value) -> Result<String> {
    let StringsArgs {
        path,
        min_len,
        limit,
    } = serde_json::from_value(args)?;
    let min = min_len.unwrap_or(4).max(1);
    // Default + hard cap so an unbounded `strings` on a large binary can't
    // flood the context window (matches grep=200 / find's default-cap idiom).
    let cap = limit.unwrap_or(2000).min(10_000);
    let data = tokio::fs::read(&path)
        .await
        .with_context(|| format!("read {path}"))?;

    let mut out = String::new();
    let mut run = String::new();
    let mut emitted = 0usize;
    // Returns false once the emit cap is hit so the caller can stop.
    let flush = |run: &mut String, out: &mut String, emitted: &mut usize| -> bool {
        // `run.len()` == char count here: every pushed char is a single
        // ASCII byte (b <= 0x7e), so byte length and char length coincide.
        if run.len() >= min {
            out.push_str(run);
            out.push('\n');
            *emitted += 1;
        }
        run.clear();
        *emitted < cap
    };

    let mut truncated = false;
    for &b in &data {
        if (0x20..=0x7e).contains(&b) {
            run.push(b as char);
        } else if !flush(&mut run, &mut out, &mut emitted) {
            run.clear();
            truncated = true;
            break;
        }
    }
    // Flush a run that reached EOF without a terminating non-printable byte.
    if !truncated {
        flush(&mut run, &mut out, &mut emitted);
    }

    if truncated {
        out.push_str(&format!("[truncated at {cap} runs]\n"));
    } else if out.is_empty() {
        out.push_str("(no printable runs)\n");
    }
    Ok(out)
}

#[derive(Deserialize)]
struct ColumnArgs {
    path: String,
    #[serde(default)]
    delimiter: Option<String>,
    #[serde(default)]
    output_delimiter: Option<String>,
}

async fn column_tool(args: Value) -> Result<String> {
    let ColumnArgs {
        path,
        delimiter,
        output_delimiter,
    } = serde_json::from_value(args)?;
    if let Some(d) = &delimiter {
        if d.is_empty() {
            return Err(anyhow!("delimiter is empty"));
        }
    }
    let out_delim = output_delimiter.as_deref().unwrap_or("  ");
    let content = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {path}"))?;

    let rows: Vec<Vec<&str>> = content
        .lines()
        .map(|line| column_split(line, delimiter.as_deref()))
        .collect();

    // First pass: per-column widths by char count (not byte len) so UTF-8
    // lines up. Grow the widths vec for ragged rows to avoid index panics.
    let mut widths: Vec<usize> = Vec::new();
    for row in &rows {
        for (i, field) in row.iter().enumerate() {
            let w = field.chars().count();
            if i >= widths.len() {
                widths.push(w);
            } else if w > widths[i] {
                widths[i] = w;
            }
        }
    }

    // Second pass: pad every field except the last in each row, then join.
    let mut out_lines: Vec<String> = Vec::with_capacity(rows.len());
    for row in &rows {
        let last = row.len().saturating_sub(1);
        let mut cells: Vec<String> = Vec::with_capacity(row.len());
        for (i, field) in row.iter().enumerate() {
            if i == last {
                cells.push((*field).to_string());
            } else {
                let pad = widths[i].saturating_sub(field.chars().count());
                cells.push(format!("{field}{}", " ".repeat(pad)));
            }
        }
        out_lines.push(cells.join(out_delim));
    }
    Ok(out_lines.join("\n"))
}

/// Split a line into fields for `column_tool`. With no delimiter, collapse
/// runs of whitespace (like `column -t`); with one, split on that literal.
fn column_split<'a>(line: &'a str, delimiter: Option<&str>) -> Vec<&'a str> {
    match delimiter {
        Some(d) => line.split(d).collect(),
        None => line.split_whitespace().collect(),
    }
}

#[derive(Deserialize)]
struct TrArgs {
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    path: Option<String>,
    from: String,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    delete: bool,
    #[serde(default)]
    squeeze: bool,
}

/// Expand a tr-style char spec into a flat char list. `X-Y` (with X <= Y and
/// the `-` between two chars) becomes the inclusive range X..=Y; a `-` at the
/// very start or end, or otherwise without a valid neighbour, is a literal
/// dash. Uniquely named to avoid colliding with sibling tools.
fn tr_expand(spec: &str) -> Vec<char> {
    let chars: Vec<char> = spec.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        // A range needs a char before the dash and a char after it.
        if i + 2 < chars.len() && chars[i + 1] == '-' && chars[i] <= chars[i + 2] {
            for c in chars[i]..=chars[i + 2] {
                out.push(c);
            }
            i += 3;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

async fn tr_tool(args: Value) -> Result<String> {
    let TrArgs {
        data,
        path,
        from,
        to,
        delete,
        squeeze,
    } = serde_json::from_value(args)?;

    let src = match (data, path) {
        (Some(d), None) => d,
        (None, Some(p)) => tokio::fs::read_to_string(&p)
            .await
            .with_context(|| format!("read {p}"))?,
        (Some(_), Some(_)) => {
            return Err(anyhow!("provide exactly one of `data` or `path`, not both"))
        }
        (None, None) => return Err(anyhow!("provide `data` or `path`")),
    };

    if !delete && !squeeze && to.is_none() {
        return Err(anyhow!(
            "`to` is required unless `delete` or `squeeze` is set"
        ));
    }

    let from_v = tr_expand(&from);
    let from_set: std::collections::HashSet<char> = from_v.iter().copied().collect();

    // Translation map (only when translating: `to` given and not deleting).
    let translating = to.is_some() && !delete;
    let map: std::collections::HashMap<char, char> = if translating {
        let to_v = tr_expand(to.as_deref().unwrap());
        if to_v.is_empty() {
            return Err(anyhow!("`to` set is empty"));
        }
        let last = *to_v.last().unwrap();
        from_v
            .iter()
            .enumerate()
            .map(|(i, &c)| (c, to_v.get(i).copied().unwrap_or(last)))
            .collect()
    } else {
        std::collections::HashMap::new()
    };

    // Squeeze operates over the "last set": the `to` set when translating,
    // otherwise the `from` set (matches GNU tr).
    let squeeze_set: std::collections::HashSet<char> = if !squeeze {
        std::collections::HashSet::new()
    } else if translating {
        tr_expand(to.as_deref().unwrap()).into_iter().collect()
    } else {
        from_set.clone()
    };

    let mut out = String::with_capacity(src.len());
    let mut last_emitted: Option<char> = None;
    for c in src.chars() {
        // delete takes precedence over translate.
        if delete && from_set.contains(&c) {
            continue;
        }
        let mapped = if translating {
            *map.get(&c).unwrap_or(&c)
        } else {
            c
        };
        if squeeze && squeeze_set.contains(&mapped) && last_emitted == Some(mapped) {
            continue;
        }
        out.push(mapped);
        last_emitted = Some(mapped);
    }
    Ok(out)
}

#[derive(Deserialize)]
struct ExpandArgs {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default = "expand_default_tab_width")]
    tab_width: usize,
    #[serde(default)]
    unexpand: bool,
}

fn expand_default_tab_width() -> usize {
    8
}

async fn expand_tool(args: Value) -> Result<String> {
    let ExpandArgs {
        path,
        data,
        tab_width,
        unexpand,
    } = serde_json::from_value(args)?;
    if tab_width == 0 {
        return Err(anyhow!("tab_width must be >= 1"));
    }
    let source = match (path, data) {
        (Some(p), None) => tokio::fs::read_to_string(&p)
            .await
            .with_context(|| format!("read {p}"))?,
        (None, Some(d)) => d,
        _ => return Err(anyhow!("provide exactly one of `path` or `data`")),
    };

    // Transform line-by-line. `lines()` drops a trailing empty segment and
    // strips '\n', so re-join manually to preserve the exact line structure
    // (including a trailing newline and blank lines).
    let mut out = String::with_capacity(source.len());
    let mut first = true;
    for line in source.split('\n') {
        if !first {
            out.push('\n');
        }
        first = false;
        if unexpand {
            out.push_str(&expand_unexpand_line(line, tab_width));
        } else {
            out.push_str(&expand_line(line, tab_width));
        }
    }
    Ok(out)
}

/// Expand tabs to spaces across the whole line, tracking the display column
/// so each tab fills to the next multiple of `tab_width` (not a naive fixed
/// count). Columns are counted in `char`s — a display-cell approximation
/// that is correct for ASCII source indentation.
fn expand_line(line: &str, tab_width: usize) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    for ch in line.chars() {
        if ch == '\t' {
            let n = tab_width - (col % tab_width);
            for _ in 0..n {
                out.push(' ');
            }
            col += n;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

/// Convert the LEADING whitespace run (spaces/tabs) into tabs + trailing
/// spaces, GNU-style: measure the indent's display column, then re-emit it
/// as (col / tab_width) tabs followed by (col % tab_width) spaces. Interior
/// whitespace (from the first non-blank char onward) is copied verbatim, so
/// this is idempotent and never touches spaces inside the line.
fn expand_unexpand_line(line: &str, tab_width: usize) -> String {
    let mut col = 0usize;
    let mut rest_byte = line.len();
    for (i, ch) in line.char_indices() {
        match ch {
            ' ' => col += 1,
            '\t' => col += tab_width - (col % tab_width),
            _ => {
                rest_byte = i;
                break;
            }
        }
    }
    let mut out = String::with_capacity(line.len());
    for _ in 0..(col / tab_width) {
        out.push('\t');
    }
    for _ in 0..(col % tab_width) {
        out.push(' ');
    }
    out.push_str(&line[rest_byte..]);
    out
}

#[derive(Deserialize)]
struct DedentArgs {
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

async fn dedent_tool(args: Value) -> Result<String> {
    let DedentArgs { data, path } = serde_json::from_value(args)?;
    let src = match (data, path) {
        (Some(d), None) => d,
        (None, Some(p)) => tokio::fs::read_to_string(&p)
            .await
            .with_context(|| format!("read {p}"))?,
        _ => return Err(anyhow!("provide exactly one of `data` or `path`")),
    };
    Ok(dedent_text(&src))
}

/// Python `textwrap.dedent` on `&str`: remove the longest common
/// leading-whitespace (spaces/tabs) prefix shared by every non-blank line.
/// Whitespace-only lines are ignored for prefix computation and emitted
/// empty. The prefix is matched literally char-by-char, so lines with
/// mixed tabs/spaces that share no literal prefix strip nothing.
fn dedent_text(text: &str) -> String {
    let is_ws = |c: char| c == ' ' || c == '\t';
    let mut common: Option<&str> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let end = line.find(|c: char| !is_ws(c)).unwrap_or(line.len());
        let indent = &line[..end];
        common = Some(match common {
            None => indent,
            Some(prev) => {
                // Longest common prefix; bytes are ASCII whitespace so
                // this stays on char boundaries.
                let n = prev
                    .bytes()
                    .zip(indent.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                &prev[..n]
            }
        });
    }
    let prefix = common.unwrap_or("");
    if prefix.is_empty() {
        return text.to_string();
    }
    text.lines()
        .map(|l| {
            l.strip_prefix(prefix)
                .unwrap_or(if l.trim().is_empty() { "" } else { l })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Deserialize)]
struct CountMatchesArgs {
    path: String,
    pattern: String,
    #[serde(default)]
    ignore_case: bool,
}

async fn count_matches_tool(args: Value) -> Result<String> {
    let CountMatchesArgs {
        path,
        pattern,
        ignore_case,
    } = serde_json::from_value(args)?;
    let re = regex::RegexBuilder::new(&pattern)
        .case_insensitive(ignore_case)
        .build()
        .with_context(|| format!("invalid regex: {pattern}"))?;
    let content = std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?;
    let mut occurrences = 0usize;
    let mut matching_lines = 0usize;
    for line in content.lines() {
        let n = re.find_iter(line).count();
        if n > 0 {
            occurrences += n;
            matching_lines += 1;
        }
    }
    Ok(format!(
        "occurrences: {occurrences}\nmatching_lines: {matching_lines}"
    ))
}

#[derive(Deserialize)]
struct EpochArgs {
    value: Value,
    to: String,
}

async fn epoch_tool(args: Value) -> Result<String> {
    let EpochArgs { value, to } = serde_json::from_value(args)?;
    match to.as_str() {
        "iso" => {
            // Accept a JSON number or a numeric string. i64 throughout so
            // negative / pre-1970 epochs work.
            let secs = value
                .as_i64()
                .or_else(|| value.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
                .ok_or_else(|| {
                    anyhow!("`value` must be epoch seconds (integer or numeric string) when to=iso")
                })?;
            Ok(epoch_secs_to_iso(secs))
        }
        "epoch" => {
            let s = value
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| value.as_i64().map(|n| n.to_string()))
                .ok_or_else(|| {
                    anyhow!("`value` must be a 'YYYY-MM-DD[ HH:MM:SS]' string when to=epoch")
                })?;
            let secs = epoch_iso_to_secs(&s)?;
            Ok(secs.to_string())
        }
        other => Err(anyhow!("`to` must be `iso` or `epoch`, got `{other}`")),
    }
}

/// Unix seconds -> "YYYY-MM-DD HH:MM:SS" (UTC), via Howard Hinnant's
/// civil-from-days. Signed + euclidean div/rem so negative epochs render
/// correctly (e.g. -1 -> 1969-12-31 23:59:59).
fn epoch_secs_to_iso(value: i64) -> String {
    let days = value.div_euclid(86_400);
    let sod = value.rem_euclid(86_400);
    let (hh, mm, ss) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
}

/// "YYYY-MM-DD[ HH:MM:SS]" (UTC) -> Unix seconds, via the inverse Hinnant
/// days_from_civil. A trailing " UTC" / "Z" is tolerated so the to=iso
/// output round-trips. Time defaults to 00:00:00 when omitted.
fn epoch_iso_to_secs(input: &str) -> Result<i64> {
    let s = input.trim();
    let s = s
        .strip_suffix(" UTC")
        .or_else(|| s.strip_suffix('Z'))
        .unwrap_or(s)
        .trim();
    let (date_part, time_part) = match s.split_once([' ', 'T']) {
        Some((d, t)) => (d, t.trim()),
        None => (s, ""),
    };
    let mut dp = date_part.split('-');
    let year = epoch_field(dp.next(), "year")?;
    let m = epoch_field(dp.next(), "month")?;
    let d = epoch_field(dp.next(), "day")?;
    if dp.next().is_some() {
        return Err(anyhow!(
            "malformed date `{date_part}` (expected YYYY-MM-DD)"
        ));
    }
    let (hh, mm, ss) = if time_part.is_empty() {
        (0, 0, 0)
    } else {
        let mut tp = time_part.split(':');
        let hh = epoch_field(tp.next(), "hour")?;
        let mm = epoch_field(tp.next(), "minute")?;
        // Seconds optional so "HH:MM" is accepted.
        let ss = match tp.next() {
            Some(v) => v
                .trim()
                .parse::<i64>()
                .map_err(|_| anyhow!("malformed second `{v}`"))?,
            None => 0,
        };
        if tp.next().is_some() {
            return Err(anyhow!(
                "malformed time `{time_part}` (expected HH:MM[:SS])"
            ));
        }
        (hh, mm, ss)
    };
    if !(1..=12).contains(&m) {
        return Err(anyhow!("month out of range: {m}"));
    }
    if !(1..=31).contains(&d) {
        return Err(anyhow!("day out of range: {d}"));
    }
    // Reject impossible days-of-month (e.g. Feb 30) — otherwise the civil-days
    // formula silently rolls them into the next month. Leap rule uses the real
    // calendar `year`, not the Jan/Feb-shifted `yy` computed below.
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if leap {
                29
            } else {
                28
            }
        }
    };
    if d > days_in_month {
        return Err(anyhow!("day out of range for month {m}: {d}"));
    }
    if hh >= 24 || mm >= 60 || ss >= 60 {
        return Err(anyhow!("time out of range: {hh:02}:{mm:02}:{ss:02}"));
    }
    let yy = if m <= 2 { year - 1 } else { year };
    let era = (if yy >= 0 { yy } else { yy - 399 }) / 400;
    let yoe = yy - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Ok(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

fn epoch_field(v: Option<&str>, label: &str) -> Result<i64> {
    let v = v.ok_or_else(|| anyhow!("missing {label}"))?;
    v.trim()
        .parse::<i64>()
        .map_err(|_| anyhow!("malformed {label} `{v}`"))
}

#[derive(Deserialize)]
struct CalcArgs {
    expr: String,
}

async fn calc_tool(args: Value) -> Result<String> {
    let CalcArgs { expr } = serde_json::from_value(args)?;
    let tokens = calc_tokenize(&expr)?;
    let mut parser = CalcParser {
        tokens: &tokens,
        pos: 0,
    };
    let value = parser.parse_expr(0)?;
    if parser.pos != parser.tokens.len() {
        return Err(anyhow!("unexpected trailing input in `{expr}`"));
    }
    Ok(calc_format(value))
}

#[derive(Clone, Copy)]
enum CalcNum {
    Int(i128),
    Float(f64),
}

#[derive(Clone)]
enum CalcTok {
    Num(CalcNum),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Pow,
    Shl,
    Shr,
    Amp,
    Pipe,
    Caret,
    Tilde,
    LParen,
    RParen,
}

fn calc_tokenize(s: &str) -> Result<Vec<CalcTok>> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit())
        {
            let start = i;
            let mut is_float = false;
            while i < bytes.len() {
                let d = bytes[i];
                if d.is_ascii_digit() {
                    i += 1;
                } else if d == b'.' {
                    is_float = true;
                    i += 1;
                } else if d == b'e' || d == b'E' {
                    is_float = true;
                    i += 1;
                    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            let text = &s[start..i];
            if is_float {
                let v: f64 = text
                    .parse()
                    .map_err(|_| anyhow!("invalid number: {text}"))?;
                out.push(CalcTok::Num(CalcNum::Float(v)));
            } else {
                let v: i128 = text
                    .parse()
                    .map_err(|_| anyhow!("integer literal out of range: {text}"))?;
                out.push(CalcTok::Num(CalcNum::Int(v)));
            }
            continue;
        }
        match c {
            b'+' => out.push(CalcTok::Plus),
            b'-' => out.push(CalcTok::Minus),
            b'*' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                    out.push(CalcTok::Pow);
                    i += 1;
                } else {
                    out.push(CalcTok::Star);
                }
            }
            b'/' => out.push(CalcTok::Slash),
            b'%' => out.push(CalcTok::Percent),
            b'<' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'<' {
                    out.push(CalcTok::Shl);
                    i += 1;
                } else {
                    return Err(anyhow!("unexpected `<` (did you mean `<<`?)"));
                }
            }
            b'>' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                    out.push(CalcTok::Shr);
                    i += 1;
                } else {
                    return Err(anyhow!("unexpected `>` (did you mean `>>`?)"));
                }
            }
            b'&' => out.push(CalcTok::Amp),
            b'|' => out.push(CalcTok::Pipe),
            b'^' => out.push(CalcTok::Caret),
            b'~' => out.push(CalcTok::Tilde),
            b'(' => out.push(CalcTok::LParen),
            b')' => out.push(CalcTok::RParen),
            _ => return Err(anyhow!("unexpected character `{}`", c as char)),
        }
        i += 1;
    }
    Ok(out)
}

struct CalcParser<'a> {
    tokens: &'a [CalcTok],
    pos: usize,
}

impl CalcParser<'_> {
    fn peek(&self) -> Option<&CalcTok> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<CalcTok> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    // Precedence-climbing. `min_bp` is the minimum binding power a binary
    // operator must have to be consumed at this level. Precedence high→low:
    // unary(~,-) > ** > * / % > + - > << >> > & > ^ > |.
    fn parse_expr(&mut self, min_bp: u8) -> Result<CalcNum> {
        let mut lhs = self.parse_prefix()?;
        loop {
            let (lbp, rbp) = match self.peek() {
                Some(CalcTok::Pipe) => (1, 2),
                Some(CalcTok::Caret) => (3, 4),
                Some(CalcTok::Amp) => (5, 6),
                Some(CalcTok::Shl) | Some(CalcTok::Shr) => (7, 8),
                Some(CalcTok::Plus) | Some(CalcTok::Minus) => (9, 10),
                Some(CalcTok::Star) | Some(CalcTok::Slash) | Some(CalcTok::Percent) => (11, 12),
                // `**` is right-associative: right bp < left bp.
                Some(CalcTok::Pow) => (15, 14),
                _ => break,
            };
            if lbp < min_bp {
                break;
            }
            let op = self.bump().expect("peeked");
            let rhs = self.parse_expr(rbp)?;
            lhs = calc_apply(&op, lhs, rhs)?;
        }
        Ok(lhs)
    }

    fn parse_prefix(&mut self) -> Result<CalcNum> {
        match self.peek() {
            Some(CalcTok::Minus) => {
                self.bump();
                let v = self.parse_prefix()?;
                match v {
                    CalcNum::Int(n) => n
                        .checked_neg()
                        .map(CalcNum::Int)
                        .ok_or_else(|| anyhow!("negation overflow")),
                    CalcNum::Float(f) => Ok(CalcNum::Float(-f)),
                }
            }
            Some(CalcTok::Tilde) => {
                self.bump();
                let v = self.parse_prefix()?;
                match v {
                    CalcNum::Int(n) => Ok(CalcNum::Int(!n)),
                    CalcNum::Float(_) => Err(anyhow!("bitwise `~` requires an integer operand")),
                }
            }
            Some(CalcTok::Plus) => {
                self.bump();
                self.parse_prefix()
            }
            _ => self.parse_atom(),
        }
    }

    fn parse_atom(&mut self) -> Result<CalcNum> {
        match self.bump() {
            Some(CalcTok::Num(n)) => Ok(n),
            Some(CalcTok::LParen) => {
                let v = self.parse_expr(0)?;
                match self.bump() {
                    Some(CalcTok::RParen) => Ok(v),
                    _ => Err(anyhow!("expected `)`")),
                }
            }
            Some(_) => Err(anyhow!("expected a number or `(`")),
            None => Err(anyhow!("unexpected end of expression")),
        }
    }
}

fn calc_int_pair(a: CalcNum, b: CalcNum) -> Option<(i128, i128)> {
    match (a, b) {
        (CalcNum::Int(x), CalcNum::Int(y)) => Some((x, y)),
        _ => None,
    }
}

fn calc_as_f64(n: CalcNum) -> f64 {
    match n {
        CalcNum::Int(x) => x as f64,
        CalcNum::Float(f) => f,
    }
}

fn calc_apply(op: &CalcTok, a: CalcNum, b: CalcNum) -> Result<CalcNum> {
    match op {
        CalcTok::Plus => match calc_int_pair(a, b) {
            Some((x, y)) => x
                .checked_add(y)
                .map(CalcNum::Int)
                .ok_or_else(|| anyhow!("integer overflow in `+`")),
            None => Ok(CalcNum::Float(calc_as_f64(a) + calc_as_f64(b))),
        },
        CalcTok::Minus => match calc_int_pair(a, b) {
            Some((x, y)) => x
                .checked_sub(y)
                .map(CalcNum::Int)
                .ok_or_else(|| anyhow!("integer overflow in `-`")),
            None => Ok(CalcNum::Float(calc_as_f64(a) - calc_as_f64(b))),
        },
        CalcTok::Star => match calc_int_pair(a, b) {
            Some((x, y)) => x
                .checked_mul(y)
                .map(CalcNum::Int)
                .ok_or_else(|| anyhow!("integer overflow in `*`")),
            None => Ok(CalcNum::Float(calc_as_f64(a) * calc_as_f64(b))),
        },
        CalcTok::Slash => {
            // Division is always f64 (guarding zero) so `7/2 == 3.5`.
            let d = calc_as_f64(b);
            if d == 0.0 {
                return Err(anyhow!("division by zero"));
            }
            Ok(CalcNum::Float(calc_as_f64(a) / d))
        }
        CalcTok::Percent => {
            let (x, y) =
                calc_int_pair(a, b).ok_or_else(|| anyhow!("`%` requires integer operands"))?;
            if y == 0 {
                return Err(anyhow!("modulo by zero"));
            }
            Ok(CalcNum::Int(x.wrapping_rem(y)))
        }
        CalcTok::Pow => {
            let (base, exp) =
                calc_int_pair(a, b).ok_or_else(|| anyhow!("`**` requires integer operands"))?;
            if exp < 0 {
                return Err(anyhow!("`**` requires a non-negative exponent"));
            }
            let exp: u32 = exp
                .try_into()
                .map_err(|_| anyhow!("`**` exponent too large"))?;
            base.checked_pow(exp)
                .map(CalcNum::Int)
                .ok_or_else(|| anyhow!("integer overflow in `**`"))
        }
        CalcTok::Shl | CalcTok::Shr => {
            let (x, y) =
                calc_int_pair(a, b).ok_or_else(|| anyhow!("shift requires integer operands"))?;
            if !(0..128).contains(&y) {
                return Err(anyhow!("shift amount out of range (0..128): {y}"));
            }
            let y = y as u32;
            Ok(CalcNum::Int(if matches!(op, CalcTok::Shl) {
                x.wrapping_shl(y)
            } else {
                x.wrapping_shr(y)
            }))
        }
        CalcTok::Amp => {
            let (x, y) =
                calc_int_pair(a, b).ok_or_else(|| anyhow!("`&` requires integer operands"))?;
            Ok(CalcNum::Int(x & y))
        }
        CalcTok::Pipe => {
            let (x, y) =
                calc_int_pair(a, b).ok_or_else(|| anyhow!("`|` requires integer operands"))?;
            Ok(CalcNum::Int(x | y))
        }
        CalcTok::Caret => {
            let (x, y) =
                calc_int_pair(a, b).ok_or_else(|| anyhow!("`^` requires integer operands"))?;
            Ok(CalcNum::Int(x ^ y))
        }
        _ => Err(anyhow!("not a binary operator")),
    }
}

fn calc_format(n: CalcNum) -> String {
    match n {
        CalcNum::Int(x) => x.to_string(),
        CalcNum::Float(f) => {
            if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
                // Render whole-valued floats without a trailing `.0` so
                // `1024.0` prints as `1024`.
                format!("{}", f as i128)
            } else {
                format!("{f}")
            }
        }
    }
}

async fn nproc_tool(_args: Value) -> Result<String> {
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    Ok(n.to_string())
}

async fn os_release_tool(_args: Value) -> Result<String> {
    let mut obj = serde_json::Map::new();
    obj.insert("os".to_string(), json!(std::env::consts::OS));
    obj.insert("arch".to_string(), json!(std::env::consts::ARCH));
    obj.insert("family".to_string(), json!(std::env::consts::FAMILY));
    if let Some(pretty) = os_release_pretty_name() {
        obj.insert("pretty".to_string(), json!(pretty));
    }
    Ok(serde_json::to_string_pretty(&Value::Object(obj))?)
}

/// Best-effort human-readable distro/version string. On Linux this reads
/// `/etc/os-release` and returns the unquoted `PRETTY_NAME` value; any IO
/// error (missing file, unreadable) collapses to `None`. On every other
/// platform there is no cheap equivalent, so it honestly returns `None`
/// rather than faking a value. Gated on `target_os = "linux"` (not `unix`)
/// so macOS/BSD take the honest `None` arm.
#[cfg(target_os = "linux")]
fn os_release_pretty_name() -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
            let trimmed = rest.trim().trim_matches('"');
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn os_release_pretty_name() -> Option<String> {
    None
}

#[derive(Deserialize)]
struct KillArgs {
    pid: i32,
    #[serde(default)]
    signal: Option<Value>,
    #[serde(default)]
    group: bool,
}

/// Resolve a signal name to its number. Returns `None` for unknown names.
fn kill_signal_by_name(name: &str) -> Option<i32> {
    match name.to_ascii_uppercase().as_str() {
        "TERM" | "SIGTERM" => Some(libc_sigterm()),
        "KILL" | "SIGKILL" => Some(libc_sigkill()),
        "INT" | "SIGINT" => Some(libc_sigint()),
        "HUP" | "SIGHUP" => Some(libc_sighup()),
        "QUIT" | "SIGQUIT" => Some(libc_sigquit()),
        _ => None,
    }
}

// Signal numbers. On Unix these come from libc; on Windows we only ever
// compare names (TERM/KILL) so the concrete values are placeholders that
// keep the shared name-table compiling.
#[cfg(unix)]
fn libc_sigterm() -> i32 {
    libc::SIGTERM
}
#[cfg(unix)]
fn libc_sigkill() -> i32 {
    libc::SIGKILL
}
#[cfg(unix)]
fn libc_sigint() -> i32 {
    libc::SIGINT
}
#[cfg(unix)]
fn libc_sighup() -> i32 {
    libc::SIGHUP
}
#[cfg(unix)]
fn libc_sigquit() -> i32 {
    libc::SIGQUIT
}
#[cfg(not(unix))]
fn libc_sigterm() -> i32 {
    15
}
#[cfg(not(unix))]
fn libc_sigkill() -> i32 {
    9
}
#[cfg(not(unix))]
fn libc_sigint() -> i32 {
    2
}
#[cfg(not(unix))]
fn libc_sighup() -> i32 {
    1
}
#[cfg(not(unix))]
fn libc_sigquit() -> i32 {
    3
}

/// Parse the `signal` arg into `(number, display_name)`. A string is looked
/// up in the name table; a number is accepted as-is (Unix only). Missing
/// defaults to TERM.
fn kill_parse_signal(sig: &Option<Value>) -> Result<(i32, String)> {
    match sig {
        None => Ok((libc_sigterm(), "TERM".to_string())),
        Some(Value::String(s)) => match kill_signal_by_name(s) {
            Some(n) => Ok((n, s.to_ascii_uppercase())),
            None => Err(anyhow!(
                "unknown signal name `{s}` (allowed: TERM, KILL, INT, HUP, QUIT)"
            )),
        },
        Some(Value::Number(n)) => {
            let num = n
                .as_i64()
                .ok_or_else(|| anyhow!("signal number is not an integer: {n}"))?;
            if num <= 0 || num > i64::from(i32::MAX) {
                return Err(anyhow!("signal number out of range: {num}"));
            }
            Ok((num as i32, num.to_string()))
        }
        Some(other) => Err(anyhow!("signal must be a name or a number, got {other}")),
    }
}

async fn kill_tool(args: Value) -> Result<String> {
    let KillArgs { pid, signal, group } = serde_json::from_value(args)?;
    // Guardrail: pid must be a real, individual process. pid <= 1 would let
    // libc::kill turn into a broadcast (0 = caller's group, -1 = every
    // process the user may signal) or hit init, and group-negation of a
    // small pid could nuke the caller's own group.
    if pid <= 1 {
        return Err(anyhow!(
            "pid must be > 1 (got {pid}); refusing to signal 0/1/negative"
        ));
    }
    let (sig, name) = kill_parse_signal(&signal)?;
    kill_send(pid, sig, &name, group).await
}

#[cfg(unix)]
async fn kill_send(pid: i32, sig: i32, name: &str, group: bool) -> Result<String> {
    // pid > 1 already checked, so -pid <= -2 — never -0 (caller's group) or
    // -1 (broadcast). SAFETY: kill is a thin syscall wrapper with no memory
    // effects; on -1 we read errno to surface ESRCH/EPERM honestly.
    let target = if group { -pid } else { pid };
    let r = unsafe { libc::kill(target, sig) };
    if r == -1 {
        let e = std::io::Error::last_os_error();
        return Err(anyhow!("kill({target}, {name}) failed: {e}"));
    }
    if group {
        Ok(format!("sent {name} to process group {pid}"))
    } else {
        Ok(format!(
            "sent {name} to pid {pid} (signal accepted; process may still be exiting)"
        ))
    }
}

#[cfg(windows)]
async fn kill_send(pid: i32, _sig: i32, name: &str, group: bool) -> Result<String> {
    if group {
        return Err(anyhow!("group signalling is not supported on Windows"));
    }
    // taskkill only offers graceful (TERM-like) and forced (/F, KILL-like)
    // termination. Anything else is rejected rather than lied about.
    let force = match name {
        "TERM" => false,
        "KILL" => true,
        _ => {
            return Err(anyhow!(
                "signal {name} is not supported on Windows; use TERM or KILL"
            ))
        }
    };
    let pid_s = pid.to_string();
    let mut argv = vec!["/PID", pid_s.as_str()];
    if force {
        argv.push("/F");
    }
    run_command("taskkill", &argv).await
}

#[derive(Deserialize)]
struct TcpCheckArgs {
    host: String,
    port: u16,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

async fn tcp_check_tool(args: Value) -> Result<String> {
    let TcpCheckArgs {
        host,
        port,
        timeout_ms,
    } = serde_json::from_value(args)?;
    let timeout_ms = timeout_ms.unwrap_or(3000).clamp(1, 30_000);
    // Resolution and connect_timeout are both blocking (DNS can stall),
    // so run the whole probe off the async runtime.
    tokio::task::spawn_blocking(move || tcp_probe(&host, port, timeout_ms))
        .await
        .context("tcp_check probe task panicked")?
}

/// Blocking TCP reachability probe: resolve `host:port`, connect with a
/// timeout, and classify the outcome (open / closed / timed-out / other).
/// Only DNS resolution failure is surfaced as an error; refused and
/// timed-out connects are normal, expected results returned as text.
fn tcp_probe(host: &str, port: u16, timeout_ms: u64) -> Result<String> {
    use std::net::ToSocketAddrs;
    let addr = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("resolving {host}:{port}"))?
        .next()
        .ok_or_else(|| anyhow!("could not resolve {host}"))?;
    let start = std::time::Instant::now();
    match std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(timeout_ms)) {
        Ok(_) => Ok(format!("open ({}ms) — {addr}", start.elapsed().as_millis())),
        Err(e) => Ok(match e.kind() {
            std::io::ErrorKind::TimedOut => {
                format!("timed-out after {timeout_ms}ms — {addr}")
            }
            std::io::ErrorKind::ConnectionRefused => {
                format!("closed (connection refused) — {addr}")
            }
            other => format!("unreachable ({other}) — {addr}"),
        }),
    }
}

#[derive(Deserialize)]
struct DnsResolveArgs {
    host: String,
}

async fn dns_resolve_tool(args: Value) -> Result<String> {
    let DnsResolveArgs { host } = serde_json::from_value(args)?;
    // ToSocketAddrs needs a port; 0 is fine and we strip it back off. Run on
    // the blocking pool since std's resolver is synchronous.
    let hostname = host.clone();
    let addrs = tokio::task::spawn_blocking(move || {
        use std::net::ToSocketAddrs;
        (hostname.as_str(), 0u16)
            .to_socket_addrs()
            .map(|it| it.collect::<Vec<_>>())
    })
    .await
    .map_err(|e| anyhow!("resolver task panicked: {e}"))?
    .with_context(|| format!("resolving `{host}`"))?;

    // Dedup IPs, preserve first-seen order.
    let mut ips: Vec<String> = Vec::new();
    for a in addrs {
        let ip = a.ip().to_string();
        if !ips.contains(&ip) {
            ips.push(ip);
        }
    }
    if ips.is_empty() {
        return Err(anyhow!("`{host}` resolved to no addresses"));
    }
    Ok(ips.join("\n"))
}

#[derive(Deserialize)]
struct DownloadArgs {
    url: String,
    dest: String,
    #[serde(default)]
    overwrite: bool,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

async fn download_tool(args: Value) -> Result<String> {
    use tokio::io::AsyncWriteExt;
    let DownloadArgs {
        url,
        dest,
        overwrite,
        timeout_secs,
    } = serde_json::from_value(args)?;
    // Guard BEFORE any network I/O so the offline test never hits the wire.
    if !overwrite && std::fs::symlink_metadata(&dest).is_ok() {
        return Err(anyhow!(
            "destination already exists: {dest} (pass overwrite:true to replace)"
        ));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs.unwrap_or(30)))
        .build()?;
    let mut resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("GET {url} -> HTTP {}", status.as_u16()));
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if let Some(parent) = PathBuf::from(&dest).parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
    }
    let mut file = tokio::fs::File::create(&dest)
        .await
        .with_context(|| format!("create {dest}"))?;
    let mut written: u64 = 0;
    // Inherent Response::chunk() — no futures-util import, no Cargo.toml change.
    while let Some(chunk) = resp
        .chunk()
        .await
        .with_context(|| format!("reading body from {url}"))?
    {
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
    }
    file.flush().await?;
    Ok(format!(
        "downloaded {written} bytes to {dest} (content-type: {})",
        if content_type.is_empty() {
            "unknown"
        } else {
            &content_type
        }
    ))
}

#[derive(Deserialize)]
struct HttpRequestArgs {
    url: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: Option<std::collections::HashMap<String, String>>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    json: Option<Value>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    max_bytes: Option<usize>,
}

/// Parse an HTTP method name (case-insensitive) into a `reqwest::Method`.
/// Split out so the validation seam can be unit-tested without a network
/// round-trip. Rejects invalid tokens (spaces, control chars, etc.).
fn http_request_parse_method(m: &str) -> Result<reqwest::Method> {
    reqwest::Method::from_bytes(m.to_ascii_uppercase().as_bytes())
        .map_err(|_| anyhow!("invalid HTTP method: {m}"))
}

async fn http_request_tool(args: Value) -> Result<String> {
    let HttpRequestArgs {
        url,
        method,
        headers,
        body,
        json,
        timeout_secs,
        max_bytes,
    } = serde_json::from_value(args)?;

    if body.is_some() && json.is_some() {
        return Err(anyhow!("body and json are mutually exclusive"));
    }

    let method_str = method.as_deref().unwrap_or("GET").to_string();
    let method = http_request_parse_method(&method_str)?;

    let timeout = timeout_secs.unwrap_or(10).clamp(1, 120);
    // Cap the body cap at 8 MiB so a huge response can't blow the context window.
    const HARD_CAP: usize = 8 * 1024 * 1024;
    let cap = max_bytes.unwrap_or(1024 * 1024).clamp(1, HARD_CAP);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout))
        .build()?;

    let mut req = client.request(method, url.as_str());
    if let Some(hs) = &headers {
        for (k, v) in hs {
            req = req.header(k, v);
        }
    }
    if let Some(j) = &json {
        req = req.json(j);
    } else if let Some(b) = body {
        req = req.body(b);
    }

    let mut resp = req
        .send()
        .await
        .with_context(|| format!("{method_str} {url}"))?;

    let status = resp.status();
    let mut header_block = String::new();
    for (name, value) in resp.headers().iter() {
        let v = value.to_str().unwrap_or("<non-utf8>");
        header_block.push_str(&format!("{name}: {v}\n"));
    }

    // Stream the body and stop one byte past the cap, so a multi-GB (or
    // slow-infinite) response can't be fully buffered into memory. The true
    // size is intentionally never known once we stop early.
    let mut buf: Vec<u8> = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = resp
        .chunk()
        .await
        .with_context(|| format!("reading body from {url}"))?
    {
        buf.extend_from_slice(&chunk);
        if buf.len() > cap {
            truncated = true;
            break;
        }
    }
    let slice = if truncated { &buf[..cap] } else { &buf[..] };
    let body_text = String::from_utf8_lossy(slice);

    let mut out = format!("HTTP {}\n{header_block}\n{body_text}", status.as_u16());
    if truncated {
        out.push_str(&format!("\n\n[truncated at {cap} bytes]"));
    }
    Ok(out)
}

#[derive(Deserialize)]
struct CargoMetadataArgs {
    #[serde(default)]
    manifest_path: Option<String>,
    #[serde(default)]
    no_deps: bool,
}

async fn cargo_metadata_tool(args: Value) -> Result<String> {
    let CargoMetadataArgs {
        manifest_path,
        no_deps,
    } = serde_json::from_value(args)?;
    let mut argv: Vec<&str> = vec!["metadata", "--format-version", "1"];
    if no_deps {
        argv.push("--no-deps");
    }
    if let Some(p) = manifest_path.as_deref() {
        argv.push("--manifest-path");
        argv.push(p);
    }
    run_command("cargo", &argv).await
}

#[derive(Deserialize)]
struct CargoTreeArgs {
    #[serde(default)]
    package: Option<String>,
    #[serde(default)]
    invert: Option<String>,
    #[serde(default)]
    duplicates: Option<bool>,
}

async fn cargo_tree_tool(args: Value) -> Result<String> {
    let CargoTreeArgs {
        package,
        invert,
        duplicates,
    } = serde_json::from_value(args)?;
    let mut argv: Vec<String> = vec!["tree".into()];
    if let Some(p) = package {
        argv.push("-p".into());
        argv.push(p);
    }
    if let Some(i) = invert {
        argv.push("-i".into());
        argv.push(i);
    }
    if duplicates.unwrap_or(false) {
        argv.push("--duplicates".into());
    }
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    run_command("cargo", &refs).await
}

#[derive(Deserialize)]
struct TestOneArgs {
    path: String,
    name: String,
}

async fn test_one_tool(args: Value) -> Result<String> {
    let TestOneArgs { path, name } = serde_json::from_value(args)?;
    let ext = ext_of(&path);
    let (runner, argv) = test_one_argv(ext.as_deref(), &path, &name)?;
    // run_command takes &[&str]; borrow the owned Vec<String>.
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    // NOTE: on Windows `npm` is `npm.cmd`, so Command::new("npm") fails to
    // spawn. This gap is inherited unchanged from the existing `test` tool
    // and is deliberately not introduced as a new regression here.
    run_command(runner, &refs).await
}

/// Pure arg-selection for `test_one`: pick the runner and argv from the
/// path's extension. Split out so the filter wiring can be unit-tested
/// offline without spawning a process or requiring a toolchain.
fn test_one_argv(ext: Option<&str>, path: &str, name: &str) -> Result<(&'static str, Vec<String>)> {
    match ext {
        // cargo test operates on the whole workspace; NAME is a substring
        // filter that is valid alongside --all-targets.
        Some("rs") => Ok((
            "cargo",
            vec![
                "test".to_string(),
                "--all-targets".to_string(),
                name.to_string(),
            ],
        )),
        // pytest: `-k NAME` is the robust substring filter. If the caller
        // passed a `file::node` node-id (contains `::`), honour it verbatim
        // as a single positional so exact node selection still works.
        Some("py") => {
            if name.contains("::") {
                Ok(("pytest", vec![name.to_string()]))
            } else {
                Ok((
                    "pytest",
                    vec![path.to_string(), "-k".to_string(), name.to_string()],
                ))
            }
        }
        // go: `-run` is a regex; passed as-is (use `^Name$` for exact).
        Some("go") => Ok((
            "go",
            vec![
                "test".to_string(),
                "-run".to_string(),
                name.to_string(),
                "./...".to_string(),
            ],
        )),
        // npm/Jest style; non-Jest runners (vitest/mocha) differ.
        Some("js") | Some("jsx") | Some("ts") | Some("tsx") => Ok((
            "npm",
            vec![
                "test".to_string(),
                "--".to_string(),
                "-t".to_string(),
                name.to_string(),
            ],
        )),
        Some(other) => Err(anyhow!("no test runner known for .{other} files")),
        None => Err(anyhow!("no extension on {path} — can't pick a test runner")),
    }
}

#[derive(Deserialize)]
struct ClocArgs {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

/// Per-language accumulator: (files, blank, comment, code).
#[derive(Default, Clone, Copy)]
struct ClocStats {
    files: u64,
    blank: u64,
    comment: u64,
    code: u64,
}

async fn cloc_tool(args: Value) -> Result<String> {
    let ClocArgs { path, exclude } = serde_json::from_value(args)?;
    let path = path.unwrap_or_else(|| ".".to_string());

    let patterns: Vec<glob::Pattern> = exclude
        .iter()
        .map(|p| glob::Pattern::new(p).with_context(|| format!("invalid exclude glob: {p}")))
        .collect::<Result<Vec<_>>>()?;

    let root = PathBuf::from(&path);
    let mut files: Vec<PathBuf> = Vec::new();
    if root.is_file() {
        // Honour excludes even for an explicit single file.
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned());
        let excluded = name
            .as_deref()
            .map(|n| patterns.iter().any(|p| p.matches(n)))
            .unwrap_or(false);
        if !excluded {
            files.push(root);
        }
    } else if root.is_dir() {
        cloc_walk(&root, &patterns, &mut files, 50_000);
    } else {
        return Err(anyhow!("not found: {path}"));
    }

    let mut totals: std::collections::BTreeMap<&'static str, ClocStats> =
        std::collections::BTreeMap::new();
    for file in &files {
        let ext = file
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());
        let Some((lang, marker)) = cloc_lang(ext.as_deref()) else {
            continue;
        };
        // Binary / non-UTF8 files: skip, never propagate.
        let Ok(bytes) = std::fs::read(file) else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let entry = totals.entry(lang).or_default();
        entry.files += 1;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                entry.blank += 1;
            } else if marker.is_some_and(|m| trimmed.starts_with(m)) {
                entry.comment += 1;
            } else {
                entry.code += 1;
            }
        }
    }

    if totals.is_empty() {
        return Ok(format!("no recognized source files under {path}"));
    }

    let mut total = ClocStats::default();
    let mut rows: Vec<(String, ClocStats)> = Vec::new();
    for (lang, s) in &totals {
        total.files += s.files;
        total.blank += s.blank;
        total.comment += s.comment;
        total.code += s.code;
        rows.push((lang.to_string(), *s));
    }
    // Sort by code descending, then language name for stability.
    rows.sort_by(|a, b| b.1.code.cmp(&a.1.code).then_with(|| a.0.cmp(&b.0)));

    let mut out = String::new();
    out.push_str(&format!(
        "{:<14}{:>8}{:>8}{:>9}{:>8}\n",
        "Language", "files", "blank", "comment", "code"
    ));
    out.push_str(&"-".repeat(47));
    out.push('\n');
    for (lang, s) in &rows {
        out.push_str(&format!(
            "{:<14}{:>8}{:>8}{:>9}{:>8}\n",
            lang, s.files, s.blank, s.comment, s.code
        ));
    }
    out.push_str(&"-".repeat(47));
    out.push('\n');
    out.push_str(&format!(
        "{:<14}{:>8}{:>8}{:>9}{:>8}",
        "TOTAL", total.files, total.blank, total.comment, total.code
    ));
    Ok(out)
}

/// Recursive walk for cloc: skips hidden entries and the usual heavy
/// build/install dirs, applies `exclude` globs against each entry's file
/// name, and stops once `cap` files are collected so `cloc /` is bounded.
/// Sub-walk errors are non-fatal (the subtree is skipped).
fn cloc_walk(
    root: &std::path::Path,
    exclude: &[glob::Pattern],
    out: &mut Vec<PathBuf>,
    cap: usize,
) {
    if out.len() >= cap {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= cap {
            break;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || matches!(&*name, "target" | "node_modules" | "dist" | "build") {
            continue;
        }
        if exclude.iter().any(|p| p.matches(&name)) {
            continue;
        }
        // symlink_metadata so we never traverse a symlinked directory
        // (avoids cycles) and don't misclassify a dangling link.
        let Ok(md) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let ft = md.file_type();
        if ft.is_dir() {
            cloc_walk(&entry.path(), exclude, out, cap);
        } else if ft.is_file() {
            out.push(entry.path());
        }
    }
}

/// Map a lowercased file extension to (language label, optional
/// line-comment marker). `None` marker means "count no comments" (e.g.
/// JSON has no line comments). Returns `None` for unrecognized extensions
/// so the file is skipped entirely.
fn cloc_lang(ext: Option<&str>) -> Option<(&'static str, Option<&'static str>)> {
    let e = ext?;
    let m = match e {
        "rs" => ("Rust", Some("//")),
        "py" => ("Python", Some("#")),
        "go" => ("Go", Some("//")),
        "js" | "jsx" | "mjs" | "cjs" => ("JavaScript", Some("//")),
        "ts" | "tsx" => ("TypeScript", Some("//")),
        "c" | "h" => ("C", Some("//")),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => ("C++", Some("//")),
        "java" => ("Java", Some("//")),
        "sh" | "bash" | "zsh" => ("Shell", Some("#")),
        "lua" => ("Lua", Some("--")),
        "rb" => ("Ruby", Some("#")),
        "md" | "markdown" => ("Markdown", None),
        "toml" => ("TOML", Some("#")),
        "yaml" | "yml" => ("YAML", Some("#")),
        "json" => ("JSON", None),
        "html" | "htm" => ("HTML", None),
        "css" => ("CSS", None),
        "sql" => ("SQL", Some("--")),
        _ => return None,
    };
    Some(m)
}

#[derive(Deserialize)]
struct JsonDiffArgs {
    a: String,
    b: String,
}

async fn json_diff_tool(args: Value) -> Result<String> {
    let JsonDiffArgs { a, b } = serde_json::from_value(args)?;
    let text_a = tokio::fs::read_to_string(&a)
        .await
        .with_context(|| format!("read {a}"))?;
    let text_b = tokio::fs::read_to_string(&b)
        .await
        .with_context(|| format!("read {b}"))?;
    let va: Value =
        serde_json::from_str(&text_a).with_context(|| format!("parsing JSON from {a}"))?;
    let vb: Value =
        serde_json::from_str(&text_b).with_context(|| format!("parsing JSON from {b}"))?;
    let mut changes = Vec::new();
    json_diff_walk("", &va, &vb, &mut changes);
    if changes.is_empty() {
        return Ok("(no differences)".to_string());
    }
    changes.sort();
    Ok(changes.join("\n"))
}

/// Recursive structural diff of two JSON values, pushing one `added` /
/// `removed` / `changed` line per difference into `out`. Objects are
/// compared key-by-key (order-independent, via serde_json's own map
/// semantics); arrays are compared positionally by index. Split out as a
/// pure sync fn so it can be unit-tested without touching the filesystem.
fn json_diff_walk(path: &str, a: &Value, b: &Value, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(ma), Value::Object(mb)) => {
            for (k, va) in ma {
                let p = json_diff_child(path, k);
                match mb.get(k) {
                    Some(vb) => json_diff_walk(&p, va, vb, out),
                    None => out.push(format!("removed {p}: {va}")),
                }
            }
            for (k, vb) in mb {
                if !ma.contains_key(k) {
                    out.push(format!("added {}: {vb}", json_diff_child(path, k)));
                }
            }
        }
        (Value::Array(aa), Value::Array(ab)) => {
            let n = aa.len().max(ab.len());
            for i in 0..n {
                let p = format!("{path}[{i}]");
                match (aa.get(i), ab.get(i)) {
                    (Some(x), Some(y)) => json_diff_walk(&p, x, y, out),
                    (Some(x), None) => out.push(format!("removed {p}: {x}")),
                    (None, Some(y)) => out.push(format!("added {p}: {y}")),
                    (None, None) => {}
                }
            }
        }
        // Fires on scalar!=scalar and any type mismatch (object-vs-array,
        // scalar-vs-object, …). Value's PartialEq is key-order-independent
        // for objects, so this only reports genuine differences.
        _ if a != b => out.push(format!(
            "changed {}: {a} -> {b}",
            if path.is_empty() { "(root)" } else { path }
        )),
        _ => {}
    }
}

fn json_diff_child(path: &str, k: &str) -> String {
    if path.is_empty() {
        k.to_string()
    } else {
        format!("{path}.{k}")
    }
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum JsonMergeArrayMode {
    Replace,
    Concat,
}

fn json_merge_default_array_mode() -> JsonMergeArrayMode {
    JsonMergeArrayMode::Replace
}

#[derive(Deserialize)]
struct JsonMergeArgs {
    paths: Vec<String>,
    #[serde(default = "json_merge_default_array_mode")]
    array_mode: JsonMergeArrayMode,
}

async fn json_merge_tool(args: Value) -> Result<String> {
    let JsonMergeArgs { paths, array_mode } = serde_json::from_value(args)?;
    anyhow::ensure!(!paths.is_empty(), "paths must be non-empty");

    let mut acc: Option<Value> = None;
    for p in &paths {
        let text = tokio::fs::read_to_string(p)
            .await
            .with_context(|| format!("read {p}"))?;
        let doc: Value =
            serde_json::from_str(&text).with_context(|| format!("parsing JSON from {p}"))?;
        match acc.as_mut() {
            Some(a) => json_merge_deep(a, doc, array_mode),
            None => acc = Some(doc),
        }
    }
    // Safe: `paths` is non-empty, so the loop set `acc` at least once.
    let merged = acc.expect("non-empty paths yields a merged document");
    Ok(serde_json::to_string_pretty(&merged)?)
}

/// Recursively deep-merge `b` into `a`. Objects union their keys (nested
/// objects merged); on any type mismatch or scalar collision the later
/// value (`b`) wins wholesale. Arrays are concatenated when `mode` is
/// Concat, otherwise replaced by `b`.
fn json_merge_deep(a: &mut Value, b: Value, mode: JsonMergeArrayMode) {
    match (a, b) {
        (Value::Object(am), Value::Object(bm)) => {
            for (k, bv) in bm {
                match am.get_mut(&k) {
                    Some(av) => json_merge_deep(av, bv, mode),
                    None => {
                        am.insert(k, bv);
                    }
                }
            }
        }
        (Value::Array(av), Value::Array(bv)) if matches!(mode, JsonMergeArrayMode::Concat) => {
            av.extend(bv);
        }
        (a, b) => *a = b,
    }
}

#[derive(Deserialize)]
struct JsonlArgs {
    path: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    pointer: Option<String>,
}

async fn jsonl_tool(args: Value) -> Result<String> {
    let JsonlArgs {
        path,
        mode,
        pointer,
    } = serde_json::from_value(args)?;
    let text = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {path}"))?;
    // `mode` only governs whole-record reflow; the pointer path always
    // emits compact JSON (a scalar's pretty form equals its compact one,
    // and it keeps `1\n2`-style output clean).
    let pretty = mode.as_deref() == Some("pretty");
    let mut out: Vec<String> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        // Skip blank lines / trailing newline so a well-formed file with a
        // final `\n` doesn't trip the parser on an empty final record.
        if line.trim().is_empty() {
            continue;
        }
        let doc: Value = serde_json::from_str(line)
            .with_context(|| format!("parsing JSON on line {} of {path}", i + 1))?;
        let rendered = match &pointer {
            Some(p) if !p.is_empty() => {
                let v = doc
                    .pointer(p)
                    .ok_or_else(|| anyhow!("no value at pointer `{p}` on line {}", i + 1))?;
                serde_json::to_string(v)?
            }
            _ => {
                if pretty {
                    serde_json::to_string_pretty(&doc)?
                } else {
                    serde_json::to_string(&doc)?
                }
            }
        };
        out.push(rendered);
    }
    Ok(out.join("\n"))
}

async fn dotenv_parse_tool(args: Value) -> Result<String> {
    let PathArgs { path } = serde_json::from_value(args)?;
    let text = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {path}"))?;
    let mut map = serde_json::Map::new();
    for raw in text.lines() {
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim();
        if key.is_empty() {
            continue;
        }
        let v = v.trim();
        let val = if let Some(inner) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            dotenv_unescape_double(inner)
        } else if let Some(inner) = v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
            inner.to_string()
        } else {
            v.to_string()
        };
        map.insert(key.to_string(), Value::String(val));
    }
    Ok(serde_json::to_string_pretty(&Value::Object(map))?)
}

/// Expand backslash escapes inside a double-quoted .env value: `\n` `\t`
/// `\r` `\\` `\"` map to their literal characters; any other escaped
/// character is passed through verbatim (dropping the backslash).
fn dotenv_unescape_double(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Deserialize)]
struct IniArgs {
    path: String,
}

async fn ini_to_json_tool(args: Value) -> Result<String> {
    let IniArgs { path } = serde_json::from_value(args)?;
    let text = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {path}"))?;
    let mut root = serde_json::Map::new();
    let mut section: Option<String> = None;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        // Full-line comments and blanks only; inline `;` / `#` stay in values.
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim().to_string();
            // A bare top-level key can already hold this name as a String;
            // a section header wins (last-writer), so force it to an object
            // rather than leaving a String that as_object_mut() would panic on.
            let e = root
                .entry(name.clone())
                .or_insert_with(|| Value::Object(Default::default()));
            if !e.is_object() {
                *e = Value::Object(Default::default());
            }
            section = Some(name);
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| anyhow!("{path}:{}: not key=value: {line}", i + 1))?;
        let k = k.trim().to_string();
        let v = Value::String(v.trim().to_string());
        match &section {
            None => {
                root.insert(k, v);
            }
            Some(s) => {
                // Section object is guaranteed present from the header branch.
                root.get_mut(s)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert(k, v);
            }
        }
    }
    Ok(serde_json::to_string_pretty(&Value::Object(root))?)
}

#[derive(Deserialize)]
struct NdjsonArgs {
    path: String,
    to: String,
}

async fn ndjson_to_json_tool(args: Value) -> Result<String> {
    let NdjsonArgs { path, to } = serde_json::from_value(args)?;
    let text = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {path}"))?;
    match to.as_str() {
        "array" => {
            // str::lines() strips both \n and \r\n, so CRLF checkouts on
            // Windows are covered without manual splitting. Skip lines that
            // are empty after trimming ASCII whitespace.
            let mut items: Vec<Value> = Vec::new();
            for (i, raw) in text.lines().enumerate() {
                let line = raw.trim();
                if line.is_empty() {
                    continue;
                }
                let v: Value = serde_json::from_str(line)
                    .with_context(|| format!("parsing JSON on line {} of {path}", i + 1))?;
                items.push(v);
            }
            Ok(serde_json::to_string_pretty(&Value::Array(items))?)
        }
        "lines" => {
            let doc: Value =
                serde_json::from_str(&text).with_context(|| format!("parsing JSON from {path}"))?;
            let arr = doc.as_array().ok_or_else(|| {
                anyhow!("to=lines expects a JSON array at the top level of {path}")
            })?;
            let mut out = String::new();
            for v in arr {
                out.push_str(&serde_json::to_string(v)?);
                out.push('\n');
            }
            Ok(out)
        }
        other => Err(anyhow!("to must be `array` or `lines`, got `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp_path(name: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "teleia-tools-test-{}-{}-{}",
            std::process::id(),
            n,
            name
        ))
    }

    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[tokio::test]
    async fn read_returns_file_contents() {
        let path = tmp_path("read.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "hello world").unwrap();
        let args = json!({ "path": path.to_str().unwrap() }).to_string();
        assert_eq!(dispatch("read", &args).await.unwrap(), "hello world");
    }

    #[tokio::test]
    async fn read_errors_on_missing_file() {
        let path = tmp_path("does-not-exist.txt");
        let args = json!({ "path": path.to_str().unwrap() }).to_string();
        assert!(dispatch("read", &args).await.is_err());
    }

    #[tokio::test]
    async fn write_creates_file_and_reports_bytes() {
        let path = tmp_path("write.txt");
        let _c = Cleanup(path.clone());
        let args = json!({ "path": path.to_str().unwrap(), "content": "data" }).to_string();
        let result = dispatch("write", &args).await.unwrap();
        assert!(result.contains("wrote 4 bytes"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "data");
    }

    #[tokio::test]
    async fn edit_replaces_unique_substring() {
        let path = tmp_path("edit.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "hello world").unwrap();
        let args = json!({
            "path": path.to_str().unwrap(),
            "old_string": "world",
            "new_string": "rust"
        })
        .to_string();
        let result = dispatch("edit", &args).await.unwrap();
        assert!(result.contains("edited"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello rust");
    }

    #[tokio::test]
    async fn edit_errors_when_old_string_missing() {
        let path = tmp_path("edit-missing.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "hello world").unwrap();
        let args = json!({
            "path": path.to_str().unwrap(),
            "old_string": "nope",
            "new_string": "x"
        })
        .to_string();
        let err = dispatch("edit", &args).await.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[tokio::test]
    async fn edit_errors_when_old_string_not_unique() {
        let path = tmp_path("edit-dup.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "abc abc").unwrap();
        let args = json!({
            "path": path.to_str().unwrap(),
            "old_string": "abc",
            "new_string": "x"
        })
        .to_string();
        let err = dispatch("edit", &args).await.unwrap_err().to_string();
        assert!(err.contains("unique"));
    }

    #[tokio::test]
    async fn edit_replace_all_substitutes_every_occurrence() {
        let path = tmp_path("edit-all.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "abc abc abc").unwrap();
        let args = json!({
            "path": path.to_str().unwrap(),
            "old_string": "abc",
            "new_string": "x",
            "replace_all": true
        })
        .to_string();
        let result = dispatch("edit", &args).await.unwrap();
        assert!(result.contains("3 replacements"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x x x");
    }

    #[tokio::test]
    async fn edit_rejects_empty_old_string_without_touching_file() {
        let path = tmp_path("edit-empty.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "hi").unwrap();
        let args = json!({
            "path": path.to_str().unwrap(),
            "old_string": "",
            "new_string": "X",
            "replace_all": true
        })
        .to_string();
        let err = dispatch("edit", &args).await.unwrap_err().to_string();
        assert!(err.contains("empty"));
        // File must be untouched — an empty needle would otherwise shred
        // it to "XhXiX".
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi");
    }

    #[tokio::test]
    async fn multi_edit_applies_edits_in_sequence() {
        let path = tmp_path("multi-edit.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "alpha beta gamma").unwrap();
        let args = json!({
            "path": path.to_str().unwrap(),
            "edits": [
                { "old_string": "alpha", "new_string": "ALPHA" },
                { "old_string": "gamma", "new_string": "GAMMA" }
            ]
        })
        .to_string();
        let result = dispatch("multi_edit", &args).await.unwrap();
        assert!(result.contains("2 steps"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ALPHA beta GAMMA");
    }

    #[tokio::test]
    async fn multi_edit_is_atomic_on_failure() {
        let path = tmp_path("multi-edit-atomic.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "alpha beta").unwrap();
        let args = json!({
            "path": path.to_str().unwrap(),
            "edits": [
                { "old_string": "alpha", "new_string": "ALPHA" },
                { "old_string": "nope", "new_string": "x" }
            ]
        })
        .to_string();
        assert!(dispatch("multi_edit", &args).await.is_err());
        // File must be untouched — first step shouldn't have landed on disk.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha beta");
    }

    #[tokio::test]
    async fn rm_removes_a_file() {
        let path = tmp_path("rm-file.txt");
        std::fs::write(&path, "x").unwrap();
        let args = json!({ "path": path.to_str().unwrap() }).to_string();
        let result = dispatch("rm", &args).await.unwrap();
        assert!(result.contains("removed"));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn rm_refuses_directory_without_recursive() {
        let dir = tmp_path("rm-dir-guard");
        std::fs::create_dir_all(&dir).unwrap();
        let _c = DirCleanup(dir.clone());
        let args = json!({ "path": dir.to_str().unwrap() }).to_string();
        let err = dispatch("rm", &args).await.unwrap_err().to_string();
        assert!(err.contains("recursive"));
        assert!(dir.exists());
    }

    #[tokio::test]
    async fn rm_removes_directory_recursively() {
        let dir = tmp_path("rm-dir");
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested/a.txt"), "x").unwrap();
        let args = json!({ "path": dir.to_str().unwrap(), "recursive": true }).to_string();
        let result = dispatch("rm", &args).await.unwrap();
        assert!(result.contains("removed directory"));
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn rm_refuses_root() {
        let args = json!({ "path": "/", "recursive": true }).to_string();
        let err = dispatch("rm", &args).await.unwrap_err().to_string();
        assert!(err.contains("refusing"));
    }

    // resolves_to_root compares the canonicalised path to `/`, which is
    // only the root on unix — on Windows the root is `C:\` (or similar)
    // and `canonicalize("/")` never equals `Path::new("/")`. Gating the
    // test matches the pattern from PR #1 (the bash + which-sh suite)
    // and keeps Windows CI green. A follow-up could broaden
    // resolves_to_root to also catch Windows drive roots; for now the
    // production code is unix-aware, so the test should be too.
    #[cfg(unix)]
    #[test]
    fn resolves_to_root_catches_root_aliases() {
        use std::path::Path;
        assert!(super::resolves_to_root(Path::new("/")));
        assert!(super::resolves_to_root(Path::new("/.")));
        assert!(super::resolves_to_root(Path::new("/..")));
        assert!(!super::resolves_to_root(Path::new("/tmp")));
        assert!(!super::resolves_to_root(Path::new(
            "/this-path-should-not-exist-xyz-12345"
        )));
    }

    #[tokio::test]
    async fn todo_write_sets_and_renders_list() {
        let args = json!({
            "todos": [
                { "content": "first thing",  "status": "in_progress" },
                { "content": "second thing", "status": "pending" }
            ]
        })
        .to_string();
        let result = dispatch("todo_write", &args).await.unwrap();
        assert!(result.contains("[~] first thing"));
        assert!(result.contains("[ ] second thing"));
    }

    #[tokio::test]
    async fn todo_write_clears_with_empty_array() {
        // Seed the state first so the clear is observable independent of test order.
        let seed = json!({
            "todos": [{ "content": "stale", "status": "pending" }]
        })
        .to_string();
        dispatch("todo_write", &seed).await.unwrap();
        let clear = json!({ "todos": [] }).to_string();
        let result = dispatch("todo_write", &clear).await.unwrap();
        assert!(result.contains("no todos"));
    }

    #[tokio::test]
    async fn dispatch_rejects_unknown_tool() {
        assert!(dispatch("nonsense", "{}").await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_returns_stdout() {
        let args = json!({ "command": "echo hello" }).to_string();
        let result = dispatch("bash", &args).await.unwrap();
        assert!(result.contains("hello"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_reports_nonzero_exit() {
        let args = json!({ "command": "exit 7" }).to_string();
        let result = dispatch("bash", &args).await.unwrap();
        assert!(result.contains("[exit 7]"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_merges_stderr_into_stdout() {
        let args = json!({ "command": "echo out; echo err >&2" }).to_string();
        let result = dispatch("bash", &args).await.unwrap();
        assert!(result.contains("out"));
        assert!(result.contains("err"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kill_child_tree_reaps_a_running_sleep() {
        use std::process::Stdio;
        // Spawn a long-running sleep in its own process group, then verify
        // kill_child_tree wakes it within a small window — proves the
        // SIGKILL actually fires rather than letting the timeout elapse.
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("60").stdout(Stdio::null()).stderr(Stdio::null());
        unsafe {
            cmd.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().expect("spawn sleep");
        super::kill_child_tree(&mut child);
        let status = tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .expect("kill_child_tree should reap within 2s")
            .expect("child.wait succeeds after SIGKILL");
        assert!(!status.success());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_does_not_hang_when_command_backgrounds_a_job() {
        // A backgrounded job inherits bash's stdout pipe. Before reaping
        // the process group on the normal-exit path, read_to_end never saw
        // EOF until the job died, hanging the tool far past its 30s budget.
        // It must now return promptly with the foreground output.
        let args = json!({ "command": "sleep 30 & echo started" }).to_string();
        let result = tokio::time::timeout(Duration::from_secs(5), dispatch("bash", &args))
            .await
            .expect("bash must not hang on a backgrounded job")
            .unwrap();
        assert!(result.contains("started"));
    }

    struct DirCleanup(PathBuf);
    impl Drop for DirCleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn list_returns_entries_with_dir_suffix() {
        let dir = tmp_path("list-dir");
        std::fs::create_dir_all(&dir).unwrap();
        let _c = DirCleanup(dir.clone());
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let args = json!({ "path": dir.to_str().unwrap() }).to_string();
        let out = dispatch("list", &args).await.unwrap();
        assert!(out.contains("a.txt"));
        assert!(out.contains("sub/"));
    }

    #[tokio::test]
    async fn list_reports_empty_directory() {
        let dir = tmp_path("list-empty");
        std::fs::create_dir_all(&dir).unwrap();
        let _c = DirCleanup(dir.clone());
        let args = json!({ "path": dir.to_str().unwrap() }).to_string();
        let out = dispatch("list", &args).await.unwrap();
        assert!(out.contains("(empty)"));
    }

    #[tokio::test]
    async fn glob_finds_matching_files() {
        let dir = tmp_path("glob-dir");
        std::fs::create_dir_all(&dir).unwrap();
        let _c = DirCleanup(dir.clone());
        std::fs::write(dir.join("one.rs"), "").unwrap();
        std::fs::write(dir.join("two.rs"), "").unwrap();
        std::fs::write(dir.join("nope.txt"), "").unwrap();
        let pattern = format!("{}/*.rs", dir.display());
        let args = json!({ "pattern": pattern }).to_string();
        let out = dispatch("glob", &args).await.unwrap();
        assert!(out.contains("one.rs"));
        assert!(out.contains("two.rs"));
        assert!(!out.contains("nope.txt"));
    }

    #[tokio::test]
    async fn glob_reports_no_matches() {
        let pattern = format!("{}/glob-miss-*", tmp_path("glob-miss").display());
        let args = json!({ "pattern": pattern }).to_string();
        let out = dispatch("glob", &args).await.unwrap();
        assert!(out.contains("no matches"));
    }

    #[tokio::test]
    async fn grep_matches_lines_in_file() {
        let path = tmp_path("grep.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "alpha\nbeta gamma\ndelta\n").unwrap();
        let args = json!({ "pattern": "gamm[a]", "path": path.to_str().unwrap() }).to_string();
        let out = dispatch("grep", &args).await.unwrap();
        assert!(out.contains(":2:"));
        assert!(out.contains("beta gamma"));
        assert!(!out.contains("alpha"));
    }

    #[tokio::test]
    async fn grep_walks_directory() {
        let dir = tmp_path("grep-dir");
        std::fs::create_dir_all(&dir).unwrap();
        let _c = DirCleanup(dir.clone());
        std::fs::write(dir.join("a.txt"), "needle here\n").unwrap();
        std::fs::write(dir.join("b.txt"), "haystack only\n").unwrap();
        let args = json!({ "pattern": "needle", "path": dir.to_str().unwrap() }).to_string();
        let out = dispatch("grep", &args).await.unwrap();
        assert!(out.contains("a.txt:1:needle here"));
        assert!(!out.contains("b.txt"));
    }

    #[tokio::test]
    async fn grep_rejects_invalid_regex() {
        let path = tmp_path("grep-bad-re.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "x").unwrap();
        let args = json!({ "pattern": "(", "path": path.to_str().unwrap() }).to_string();
        assert!(dispatch("grep", &args).await.is_err());
    }

    #[tokio::test]
    async fn head_returns_first_n_lines() {
        let path = tmp_path("head.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "1\n2\n3\n4\n5\n").unwrap();
        let args = json!({ "path": path.to_str().unwrap(), "lines": 2 }).to_string();
        assert_eq!(dispatch("head", &args).await.unwrap(), "1\n2");
    }

    #[tokio::test]
    async fn tail_returns_last_n_lines() {
        let path = tmp_path("tail.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "1\n2\n3\n4\n5\n").unwrap();
        let args = json!({ "path": path.to_str().unwrap(), "lines": 2 }).to_string();
        assert_eq!(dispatch("tail", &args).await.unwrap(), "4\n5");
    }

    #[tokio::test]
    async fn tree_walks_and_skips_target() {
        let dir = tmp_path("tree-dir");
        std::fs::create_dir_all(&dir).unwrap();
        let _c = DirCleanup(dir.clone());
        std::fs::write(dir.join("a.txt"), "").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/b.txt"), "").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/skip.txt"), "").unwrap();
        let args = json!({ "path": dir.to_str().unwrap() }).to_string();
        let out = dispatch("tree", &args).await.unwrap();
        assert!(out.contains("a.txt"));
        assert!(out.contains("sub/"));
        assert!(out.contains("b.txt"));
        assert!(!out.contains("target"));
        assert!(!out.contains("skip.txt"));
    }

    #[tokio::test]
    async fn stat_reports_size_and_type() {
        let path = tmp_path("stat.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "hello").unwrap();
        let args = json!({ "path": path.to_str().unwrap() }).to_string();
        let out = dispatch("stat", &args).await.unwrap();
        assert!(out.contains("type:  file"));
        assert!(out.contains("size:  5 bytes"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn which_finds_sh_on_path() {
        // Every Unix-like host has /bin/sh on $PATH.
        let args = json!({ "name": "sh" }).to_string();
        let out = dispatch("which", &args).await.unwrap();
        assert!(out.ends_with("/sh") || out.contains("/sh"));
    }

    #[tokio::test]
    async fn which_errors_on_missing() {
        let args = json!({ "name": "definitely-not-a-real-binary-xyzzy" }).to_string();
        assert!(dispatch("which", &args).await.is_err());
    }

    #[tokio::test]
    async fn mkdir_is_idempotent() {
        let dir = tmp_path("mkdir-dir");
        let _c = DirCleanup(dir.clone());
        let args = json!({ "path": dir.to_str().unwrap() }).to_string();
        assert!(dispatch("mkdir", &args).await.is_ok());
        // Second call still succeeds (idempotent).
        assert!(dispatch("mkdir", &args).await.is_ok());
        assert!(dir.is_dir());
    }

    #[tokio::test]
    async fn mv_renames_and_refuses_to_clobber() {
        let src = tmp_path("mv-src.txt");
        let dst = tmp_path("mv-dst.txt");
        let _c1 = Cleanup(src.clone());
        let _c2 = Cleanup(dst.clone());
        std::fs::write(&src, "hi").unwrap();
        let args =
            json!({ "src": src.to_str().unwrap(), "dst": dst.to_str().unwrap() }).to_string();
        assert!(dispatch("mv", &args).await.is_ok());
        assert!(dst.is_file());
        assert!(!src.exists());
        // Now src is gone, recreate; mv to existing dst should refuse.
        std::fs::write(&src, "again").unwrap();
        assert!(dispatch("mv", &args).await.is_err());
    }

    #[tokio::test]
    async fn cp_copies_and_refuses_to_clobber() {
        let src = tmp_path("cp-src.txt");
        let dst = tmp_path("cp-dst.txt");
        let _c1 = Cleanup(src.clone());
        let _c2 = Cleanup(dst.clone());
        std::fs::write(&src, "data").unwrap();
        let args =
            json!({ "src": src.to_str().unwrap(), "dst": dst.to_str().unwrap() }).to_string();
        assert!(dispatch("cp", &args).await.is_ok());
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "data");
        assert!(src.is_file());
        // Second call should refuse to clobber.
        assert!(dispatch("cp", &args).await.is_err());
    }

    #[tokio::test]
    async fn test_tool_errors_on_unknown_extension() {
        let args = json!({ "path": "foo.cobol" }).to_string();
        assert!(dispatch("test", &args).await.is_err());
    }

    #[tokio::test]
    async fn git_add_requires_paths_and_commit_requires_message() {
        let add = json!({ "subcommand": "add" }).to_string();
        assert!(dispatch("git", &add).await.is_err());
        let commit = json!({ "subcommand": "commit" }).to_string();
        assert!(dispatch("git", &commit).await.is_err());
    }

    #[tokio::test]
    async fn git_new_subcommands_validate_args() {
        // show needs ref; blame needs path; blame line-range must be paired + ordered.
        assert!(
            dispatch("git", &json!({ "subcommand": "show" }).to_string())
                .await
                .is_err()
        );
        assert!(
            dispatch("git", &json!({ "subcommand": "blame" }).to_string())
                .await
                .is_err()
        );
        let bad_range =
            json!({ "subcommand": "blame", "path": "x", "start_line": 5, "end_line": 2 })
                .to_string();
        assert!(dispatch("git", &bad_range).await.is_err());
        let lone = json!({ "subcommand": "blame", "path": "x", "start_line": 5 }).to_string();
        assert!(dispatch("git", &lone).await.is_err());
        // ref that looks like a flag is rejected.
        let flag_ref = json!({ "subcommand": "show", "ref": "--help" }).to_string();
        assert!(dispatch("git", &flag_ref).await.is_err());
    }

    #[tokio::test]
    async fn symlink_creates_link_and_refuses_to_clobber() {
        let target = tmp_path("symlink-target.txt");
        let link = tmp_path("symlink-link.txt");
        let _c1 = Cleanup(target.clone());
        let _c2 = Cleanup(link.clone());
        std::fs::write(&target, "data").unwrap();
        let args =
            json!({ "src": target.to_str().unwrap(), "dst": link.to_str().unwrap() }).to_string();
        assert!(dispatch("symlink", &args).await.is_ok());
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "data");
        // Second call should refuse to clobber the existing link.
        assert!(dispatch("symlink", &args).await.is_err());
    }

    #[tokio::test]
    async fn env_reads_named_var_and_errors_when_unset() {
        std::env::set_var("TELEIA_ENV_TEST_VAR", "present");
        let hit = json!({ "name": "TELEIA_ENV_TEST_VAR" }).to_string();
        assert_eq!(dispatch("env", &hit).await.unwrap(), "present");
        let miss = json!({ "name": "TELEIA_DEFINITELY_UNSET_XYZZY" }).to_string();
        assert!(dispatch("env", &miss).await.is_err());
    }

    #[tokio::test]
    async fn replace_substitutes_regex_and_counts() {
        let path = tmp_path("replace.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "foo bar foo baz foo").unwrap();
        let all = json!({ "path": path.to_str().unwrap(), "pattern": "foo", "replacement": "X" })
            .to_string();
        let out = dispatch("replace", &all).await.unwrap();
        assert!(out.contains("replaced 3 occurrence(s)"), "{out}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "X bar X baz X");
        // all=false touches only the first match; capture groups expand.
        std::fs::write(&path, "a1 a2").unwrap();
        let first = json!({ "path": path.to_str().unwrap(), "pattern": "a(\\d)", "replacement": "[$1]", "all": false })
            .to_string();
        dispatch("replace", &first).await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[1] a2");
    }

    #[tokio::test]
    async fn json_extracts_by_pointer_and_errors_on_miss() {
        let path = tmp_path("data.json");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, r#"{"a":{"b":[10,20]}}"#).unwrap();
        let hit = json!({ "path": path.to_str().unwrap(), "pointer": "/a/b/1" }).to_string();
        assert_eq!(dispatch("json", &hit).await.unwrap(), "20");
        let root = json!({ "path": path.to_str().unwrap(), "pointer": "" }).to_string();
        assert!(dispatch("json", &root).await.unwrap().contains("\"b\""));
        let miss = json!({ "path": path.to_str().unwrap(), "pointer": "/a/z" }).to_string();
        assert!(dispatch("json", &miss).await.is_err());
    }

    #[tokio::test]
    async fn base64_round_trips_and_matches_known_vector() {
        let enc = json!({ "data": "hello" }).to_string();
        assert_eq!(dispatch("base64", &enc).await.unwrap(), "aGVsbG8=");
        let dec = json!({ "data": "aGVsbG8=", "decode": true }).to_string();
        assert_eq!(dispatch("base64", &dec).await.unwrap(), "hello");
        // Two-pad boundary.
        let enc2 = json!({ "data": "hi" }).to_string();
        assert_eq!(dispatch("base64", &enc2).await.unwrap(), "aGk=");
        let bad = json!({ "data": "not*base64", "decode": true }).to_string();
        assert!(dispatch("base64", &bad).await.is_err());
    }

    #[tokio::test]
    async fn hexdump_renders_offsets_hex_and_ascii() {
        let path = tmp_path("hex.bin");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, b"AB\x00").unwrap();
        let args = json!({ "path": path.to_str().unwrap() }).to_string();
        let out = dispatch("hexdump", &args).await.unwrap();
        assert!(out.contains("00000000"), "{out}");
        assert!(out.contains("41 42 00"), "{out}");
        assert!(out.contains("|AB.|"), "{out}");
    }

    #[tokio::test]
    async fn du_sums_a_directory_tree() {
        let dir = tmp_path("du-dir");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "12345").unwrap(); // 5 bytes
        std::fs::write(dir.join("sub/b.txt"), "678").unwrap(); // 3 bytes
        let args = json!({ "path": dir.to_str().unwrap() }).to_string();
        let out = dispatch("du", &args).await.unwrap();
        assert!(out.starts_with("8 bytes"), "{out}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn realpath_canonicalizes_existing_path() {
        let path = tmp_path("real.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "x").unwrap();
        let args = json!({ "path": path.to_str().unwrap() }).to_string();
        let out = dispatch("realpath", &args).await.unwrap();
        assert!(std::path::Path::new(&out).is_absolute(), "{out}");
        assert!(out.ends_with("real.txt"), "{out}");
        let missing = json!({ "path": "/definitely/not/here/xyzzy" }).to_string();
        assert!(dispatch("realpath", &missing).await.is_err());
    }

    #[test]
    fn md5_matches_rfc1321_vectors() {
        assert_eq!(md5(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn sha1_matches_fips_vectors() {
        assert_eq!(sha1(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn sha256_core_refactor_is_byte_identical() {
        // Guards the sha256() -> sha256_core() refactor.
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[tokio::test]
    async fn crc32_matches_canonical_check_value() {
        let out = dispatch("crc32", &json!({ "data": "123456789" }).to_string())
            .await
            .unwrap();
        assert!(out.starts_with("0xcbf43926"), "{out}");
        let both = dispatch("crc32", &json!({ "data": "x", "path": "x" }).to_string()).await;
        assert!(both.is_err());
    }

    #[tokio::test]
    async fn hash_tool_matches_sha2_vectors() {
        let d = |a: &str, s: &str| json!({ "algo": a, "data": s }).to_string();
        assert_eq!(
            dispatch("hash", &d("sha224", "")).await.unwrap(),
            "d14a028c2a3a2bc9476102bb288234c415a2b01f828ea62ac5b3e42f"
        );
        assert_eq!(
            dispatch("hash", &d("sha512", "abc")).await.unwrap(),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(
            dispatch("hash", &d("sha384", "abc")).await.unwrap(),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
    }

    #[tokio::test]
    async fn hmac_sha256_matches_vector_and_key_hex() {
        let a = json!({ "data": "The quick brown fox jumps over the lazy dog", "key": "key" });
        assert_eq!(
            dispatch("hmac_sha256", &a.to_string()).await.unwrap(),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
        // key="6b6579" (hex of "key") with key_hex must give the same MAC.
        let b = json!({ "data": "The quick brown fox jumps over the lazy dog", "key": "6b6579", "key_hex": true });
        assert_eq!(
            dispatch("hmac_sha256", &b.to_string()).await.unwrap(),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[tokio::test]
    async fn hex_round_trips_and_rejects_bad_input() {
        assert_eq!(
            dispatch("hex", &json!({ "data": "hi" }).to_string())
                .await
                .unwrap(),
            "6869"
        );
        assert_eq!(
            dispatch(
                "hex",
                &json!({ "data": "6869", "decode": true }).to_string()
            )
            .await
            .unwrap(),
            "hi"
        );
        assert!(
            dispatch("hex", &json!({ "data": "abc", "decode": true }).to_string())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn base32_matches_rfc4648_vectors() {
        assert_eq!(
            dispatch("base32", &json!({ "data": "foobar" }).to_string())
                .await
                .unwrap(),
            "MZXW6YTBOI======"
        );
        assert_eq!(
            dispatch(
                "base32",
                &json!({ "data": "MZXW6YTBOI======", "decode": true }).to_string()
            )
            .await
            .unwrap(),
            "foobar"
        );
        assert_eq!(
            dispatch(
                "base32",
                &json!({ "data": "foobar", "hex_variant": true }).to_string()
            )
            .await
            .unwrap(),
            "CPNMUOJ1E8======"
        );
        assert!(dispatch(
            "base32",
            &json!({ "data": "0189", "decode": true }).to_string()
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn url_encode_respects_component_and_decode() {
        assert_eq!(
            dispatch(
                "url_encode",
                &json!({ "data": "a b&c", "component": true }).to_string()
            )
            .await
            .unwrap(),
            "a%20b%26c"
        );
        assert_eq!(
            dispatch("url_encode", &json!({ "data": "a b&c" }).to_string())
                .await
                .unwrap(),
            "a%20b&c"
        );
        // decode must NOT turn '+' into space.
        assert_eq!(
            dispatch(
                "url_encode",
                &json!({ "data": "a+b", "decode": true }).to_string()
            )
            .await
            .unwrap(),
            "a+b"
        );
        assert!(dispatch(
            "url_encode",
            &json!({ "data": "%2", "decode": true }).to_string()
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn hash_verify_reports_match_case_insensitively() {
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let hit = json!({ "data": "abc", "expected": abc.to_uppercase() }).to_string();
        assert!(dispatch("hash_verify", &hit)
            .await
            .unwrap()
            .contains("\"match\": true"));
        let miss = json!({ "data": "abc", "expected": "0".repeat(64) }).to_string();
        assert!(dispatch("hash_verify", &miss)
            .await
            .unwrap()
            .contains("\"match\": false"));
    }

    #[tokio::test]
    async fn jwt_decode_splits_header_and_payload() {
        // header {"alg":"HS256","typ":"JWT"}, payload {"sub":"1","admin":true}, base64url.
        let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxIiwiYWRtaW4iOnRydWV9.sig";
        let out = dispatch("jwt_decode", &json!({ "token": token }).to_string())
            .await
            .unwrap();
        assert!(out.contains("\"alg\": \"HS256\""), "{out}");
        assert!(out.contains("\"admin\": true"), "{out}");
        assert!(
            dispatch("jwt_decode", &json!({ "token": "a.b" }).to_string())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn find_filters_by_type_name_size_depth_and_age() {
        let dir = tmp_path("find-dir");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "0123456789").unwrap(); // 10 bytes
        std::fs::write(dir.join("sub/b.log"), vec![b'x'; 5000]).unwrap();
        struct DirCleanup(PathBuf);
        impl Drop for DirCleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _c = DirCleanup(dir.clone());
        let root = dir.to_str().unwrap();

        // name matches the file name only, not the full path.
        let args = json!({ "path": root, "type": "file", "name": "*.txt" }).to_string();
        let out = dispatch("find", &args).await.unwrap();
        assert!(out.contains("a.txt"));
        assert!(!out.contains("b.log"));

        // min_size picks only the large file.
        let args = json!({ "path": root, "min_size": 1000 }).to_string();
        let out = dispatch("find", &args).await.unwrap();
        assert!(out.contains("b.log"));
        assert!(!out.contains("a.txt"));

        // max_depth=1 stays out of sub/.
        let args = json!({ "path": root, "max_depth": 1, "type": "file" }).to_string();
        let out = dispatch("find", &args).await.unwrap();
        assert!(out.contains("a.txt"));
        assert!(!out.contains("b.log"));

        // type=dir returns the subdir.
        let args = json!({ "path": root, "type": "dir" }).to_string();
        let out = dispatch("find", &args).await.unwrap();
        assert!(out.contains("sub"));

        // Freshly-created file: newer_than must not panic on a near-now mtime.
        let args = json!({ "path": root, "newer_than": 1, "type": "file" }).to_string();
        assert!(dispatch("find", &args).await.is_ok());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link = dir.join("link.txt");
            symlink(dir.join("a.txt"), &link).unwrap();
            let args = json!({ "path": root, "type": "symlink" }).to_string();
            let out = dispatch("find", &args).await.unwrap();
            assert!(out.contains("link.txt"));
        }
    }

    #[tokio::test]
    async fn chmod_sets_unix_mode_and_rejects_bad_mode() {
        let path = tmp_path("chmod.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "x").unwrap();
        let p = path.to_str().unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            dispatch("chmod", &json!({ "path": p, "mode": "600" }).to_string())
                .await
                .unwrap();
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            dispatch("chmod", &json!({ "path": p, "mode": "755" }).to_string())
                .await
                .unwrap();
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }

        #[cfg(windows)]
        {
            dispatch("chmod", &json!({ "path": p, "mode": "444" }).to_string())
                .await
                .unwrap();
            assert!(std::fs::metadata(&path).unwrap().permissions().readonly());
            dispatch("chmod", &json!({ "path": p, "mode": "644" }).to_string())
                .await
                .unwrap();
            assert!(!std::fs::metadata(&path).unwrap().permissions().readonly());
        }

        assert!(
            dispatch("chmod", &json!({ "path": p, "mode": "9z9" }).to_string())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn readlink_reads_target_and_rejects_non_symlink() {
        // Symlink creation is unix-only; the non-symlink assertion stays ungated.
        #[cfg(unix)]
        {
            let link = tmp_path("readlink-link");
            let _cl = Cleanup(link.clone());
            std::os::unix::fs::symlink("ghost-target", &link).unwrap();
            let args = json!({ "path": link.to_str().unwrap() }).to_string();
            assert_eq!(dispatch("readlink", &args).await.unwrap(), "ghost-target");
        }

        let regular = tmp_path("readlink-regular.txt");
        let _cr = Cleanup(regular.clone());
        std::fs::write(&regular, "data").unwrap();
        let args = json!({ "path": regular.to_str().unwrap() }).to_string();
        assert!(dispatch("readlink", &args).await.is_err());

        let missing = tmp_path("readlink-missing");
        let args = json!({ "path": missing.to_str().unwrap() }).to_string();
        assert!(dispatch("readlink", &args).await.is_err());
    }

    #[tokio::test]
    async fn hardlink_shares_inode_and_refuses_to_clobber() {
        let src = tmp_path("hardlink-src.txt");
        let dst = tmp_path("hardlink-dst.txt");
        let _c1 = Cleanup(src.clone());
        let _c2 = Cleanup(dst.clone());
        std::fs::write(&src, "A").unwrap();
        let args =
            json!({ "src": src.to_str().unwrap(), "dst": dst.to_str().unwrap() }).to_string();
        assert!(dispatch("hardlink", &args).await.is_ok());
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "A");
        // Prove the shared-inode property (not mere content equality): a
        // rewrite of src's inode data must be visible through dst.
        std::fs::write(&src, "B").unwrap();
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "B");
        // Second call should refuse to clobber the existing link.
        assert!(dispatch("hardlink", &args).await.is_err());
    }

    #[tokio::test]
    async fn pathinfo_decomposes_parts() {
        let out = dispatch("pathinfo", &json!({ "path": "/a/b/c.tar.gz" }).to_string())
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["parent"], "/a/b");
        assert_eq!(v["file_name"], "c.tar.gz");
        assert_eq!(v["file_stem"], "c.tar");
        assert_eq!(v["extension"], "gz");

        let out = dispatch("pathinfo", &json!({ "path": "/a" }).to_string())
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["parent"], "/");
        assert_eq!(v["file_name"], "a");
        assert!(v["extension"].is_null());
    }

    #[tokio::test]
    async fn mktemp_creates_unique_files_and_dir() {
        let temp_root = std::env::temp_dir();

        let a = mktemp_tool(json!({ "prefix": "tvfy-" })).await.unwrap();
        let b = mktemp_tool(json!({ "prefix": "tvfy-" })).await.unwrap();
        let _ca = Cleanup(PathBuf::from(&a));
        let _cb = Cleanup(PathBuf::from(&b));

        assert_ne!(a, b);
        for p in [&a, &b] {
            let pb = PathBuf::from(p);
            assert!(pb.is_file(), "{p} should be a file");
            assert!(pb.starts_with(&temp_root), "{p} should be under temp dir");
            assert!(
                pb.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("tvfy-"),
                "{p} should carry the prefix"
            );
        }

        let d = mktemp_tool(json!({ "dir": true, "prefix": "tvfy-" }))
            .await
            .unwrap();
        let dpb = PathBuf::from(&d);
        struct DirGuard(PathBuf);
        impl Drop for DirGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cd = DirGuard(dpb.clone());
        assert!(dpb.is_dir(), "{d} should be a directory");
    }

    #[tokio::test]
    async fn truncate_shrinks_grows_and_requires_existing_file() {
        let path = tmp_path("truncate.bin");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, b"hello").unwrap();

        // Shrink to 2 bytes.
        let args = json!({ "path": path.to_str().unwrap(), "size": 2 }).to_string();
        let result = dispatch("truncate", &args).await.unwrap();
        assert!(result.contains("2 bytes"));
        assert_eq!(std::fs::read(&path).unwrap(), b"he");

        // Grow to 4 bytes — new space is zero-filled.
        let args = json!({ "path": path.to_str().unwrap(), "size": 4 }).to_string();
        dispatch("truncate", &args).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), vec![b'h', b'e', 0, 0]);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 4);

        // Nonexistent path errors and is not created.
        let missing = tmp_path("truncate-missing.bin");
        let args = json!({ "path": missing.to_str().unwrap(), "size": 0 }).to_string();
        assert!(dispatch("truncate", &args).await.is_err());
        assert!(!missing.exists());
    }

    #[tokio::test]
    async fn slice_ranges_and_validation() {
        let path = tmp_path("slice.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "1\n2\n3\n4\n5").unwrap();
        let p = path.to_str().unwrap();

        let args = json!({ "path": p, "start": 2, "end": 4 }).to_string();
        assert_eq!(dispatch("slice", &args).await.unwrap(), "2\n3\n4");

        let args = json!({ "path": p, "start": 3 }).to_string();
        assert_eq!(dispatch("slice", &args).await.unwrap(), "3\n4\n5");

        let args = json!({ "path": p, "start": 2, "end": 2, "number": true }).to_string();
        assert_eq!(dispatch("slice", &args).await.unwrap(), "2\t2");

        let args = json!({ "path": p, "start": 0, "end": 2 }).to_string();
        assert!(dispatch("slice", &args).await.is_err());

        let args = json!({ "path": p, "start": 4, "end": 2 }).to_string();
        assert!(dispatch("slice", &args).await.is_err());
    }

    #[tokio::test]
    async fn sort_handles_typed_options() {
        // numeric + unique
        let path = tmp_path("sort-num.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "3\n1\n2\n1\n").unwrap();
        let args =
            json!({ "path": path.to_str().unwrap(), "numeric": true, "unique": true }).to_string();
        assert_eq!(dispatch("sort", &args).await.unwrap(), "1\n2\n3");

        // reverse lexical
        let path2 = tmp_path("sort-rev.txt");
        let _c2 = Cleanup(path2.clone());
        std::fs::write(&path2, "b\na\nc\n").unwrap();
        let args2 = json!({ "path": path2.to_str().unwrap(), "reverse": true }).to_string();
        assert_eq!(dispatch("sort", &args2).await.unwrap(), "c\nb\na");

        // field 2 numeric (whitespace delimiter)
        let path3 = tmp_path("sort-field.txt");
        let _c3 = Cleanup(path3.clone());
        std::fs::write(&path3, "x 30\ny 10\nz 20\n").unwrap();
        let args3 =
            json!({ "path": path3.to_str().unwrap(), "field": 2, "numeric": true }).to_string();
        assert_eq!(dispatch("sort", &args3).await.unwrap(), "y 10\nz 20\nx 30");

        // field out of range does not panic
        let args4 = json!({ "path": path3.to_str().unwrap(), "field": 9 }).to_string();
        assert!(dispatch("sort", &args4).await.is_ok());
    }

    #[tokio::test]
    async fn cut_fields_chars_and_errors() {
        let path = tmp_path("cut.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "a,b,c,d").unwrap();
        let p = path.to_str().unwrap();

        let run = |extra: Value| {
            let mut obj = serde_json::Map::new();
            obj.insert("path".into(), json!(p));
            if let Value::Object(m) = extra {
                for (k, v) in m {
                    obj.insert(k, v);
                }
            }
            Value::Object(obj).to_string()
        };

        // fields="1,3" -> "a,c"
        assert_eq!(
            dispatch("cut", &run(json!({ "delimiter": ",", "fields": "1,3" })))
                .await
                .unwrap(),
            "a,c\n"
        );
        // fields="2-4" -> "b,c,d"
        assert_eq!(
            dispatch("cut", &run(json!({ "delimiter": ",", "fields": "2-4" })))
                .await
                .unwrap(),
            "b,c,d\n"
        );
        // open range fields="2-" -> "b,c,d"
        assert_eq!(
            dispatch("cut", &run(json!({ "delimiter": ",", "fields": "2-" })))
                .await
                .unwrap(),
            "b,c,d\n"
        );
        // open start fields="-2" -> "a,b"
        assert_eq!(
            dispatch("cut", &run(json!({ "delimiter": ",", "fields": "-2" })))
                .await
                .unwrap(),
            "a,b\n"
        );
        // complement of field 1 -> "b,c,d"
        assert_eq!(
            dispatch(
                "cut",
                &run(json!({ "delimiter": ",", "fields": "1", "complement": true }))
            )
            .await
            .unwrap(),
            "b,c,d\n"
        );

        // No delimiter in line -> passed through unchanged.
        std::fs::write(&path, "noDelimHere").unwrap();
        assert_eq!(
            dispatch("cut", &run(json!({ "delimiter": ",", "fields": "2" })))
                .await
                .unwrap(),
            "noDelimHere\n"
        );

        // char mode is Unicode-scalar based, concatenated.
        std::fs::write(&path, "áбc").unwrap();
        assert_eq!(
            dispatch("cut", &run(json!({ "chars": "1" })))
                .await
                .unwrap(),
            "á\n"
        );
        std::fs::write(&path, "hello").unwrap();
        assert_eq!(
            dispatch("cut", &run(json!({ "chars": "1-3" })))
                .await
                .unwrap(),
            "hel\n"
        );

        // Error cases.
        assert!(dispatch("cut", &run(json!({}))).await.is_err());
        assert!(
            dispatch("cut", &run(json!({ "fields": "1", "chars": "1" })))
                .await
                .is_err()
        );
        assert!(
            dispatch("cut", &run(json!({ "delimiter": ",", "fields": "0" })))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn comm_compares_line_sets() {
        let path_a = tmp_path("comm-a.txt");
        let path_b = tmp_path("comm-b.txt");
        let _ca = Cleanup(path_a.clone());
        let _cb = Cleanup(path_b.clone());
        std::fs::write(&path_a, "x\ny\nY").unwrap();
        std::fs::write(&path_b, "y\nz").unwrap();
        let a = path_a.to_str().unwrap();
        let b = path_b.to_str().unwrap();

        // Single active selector => bare lines, sorted via BTreeMap.
        let common = dispatch(
            "comm",
            &json!({ "a": a, "b": b, "common": true }).to_string(),
        )
        .await
        .unwrap();
        assert_eq!(common, "y");

        let only_a = dispatch(
            "comm",
            &json!({ "a": a, "b": b, "only_a": true }).to_string(),
        )
        .await
        .unwrap();
        assert_eq!(only_a, "Y\nx");

        let only_b = dispatch(
            "comm",
            &json!({ "a": a, "b": b, "only_b": true }).to_string(),
        )
        .await
        .unwrap();
        assert_eq!(only_b, "z");

        // ignore_case folds Y -> y, so both are common; emitted text keeps
        // A's first-seen original casing.
        let ci = dispatch(
            "comm",
            &json!({ "a": a, "b": b, "common": true, "ignore_case": true }).to_string(),
        )
        .await
        .unwrap();
        assert_eq!(ci, "y");
    }

    #[tokio::test]
    async fn strings_extracts_printable_runs_and_flushes_eof() {
        let path = tmp_path("strings.bin");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, b"\x00hello\x01\x02hi\x00world").unwrap();
        let args = json!({ "path": path.to_str().unwrap(), "min_len": 4 }).to_string();
        let out = dispatch("strings", &args).await.unwrap();
        assert!(out.contains("hello"), "{out}");
        assert!(out.contains("world"), "{out}");
        // "hi" is only 2 chars — below min_len, so it must be dropped.
        assert!(!out.contains("hi"), "{out}");

        // EOF-flush path: a run that reaches end-of-file with no trailing
        // non-printable byte must still be emitted.
        let path2 = tmp_path("strings-eof.bin");
        let _c2 = Cleanup(path2.clone());
        std::fs::write(&path2, b"\x00trailing").unwrap();
        let args2 = json!({ "path": path2.to_str().unwrap(), "min_len": 4 }).to_string();
        let out2 = dispatch("strings", &args2).await.unwrap();
        assert!(out2.contains("trailing"), "{out2}");
    }

    #[tokio::test]
    async fn column_aligns_and_no_trailing_whitespace() {
        let path = tmp_path("column.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "a,bb\nccc,d").unwrap();
        let args = json!({ "path": path.to_str().unwrap(), "delimiter": "," }).to_string();
        let out = dispatch("column", &args).await.unwrap();
        let lines: Vec<&str> = out.lines().collect();
        // Column 0 width is 3 ("ccc"): "a" gets 2 pad spaces + 2 output-delim
        // spaces before "bb"; the last field is never padded.
        assert_eq!(lines[0], "a    bb");
        assert_eq!(lines[1], "ccc  d");
        for line in &lines {
            assert!(
                !line.ends_with(' '),
                "line has trailing whitespace: {line:?}"
            );
        }

        // Ragged rows: a short row must not panic on the widths lookup.
        std::fs::write(&path, "a,b,c\nx").unwrap();
        let args = json!({ "path": path.to_str().unwrap(), "delimiter": "," }).to_string();
        let out = dispatch("column", &args).await.unwrap();
        assert_eq!(out.lines().next().unwrap(), "a  b  c");
        assert_eq!(out.lines().nth(1).unwrap(), "x");
    }

    #[tokio::test]
    async fn tr_translate_delete_squeeze_and_errors() {
        // (a) translate uppercase via ranges
        let args = json!({ "data": "Hello", "from": "a-z", "to": "A-Z" }).to_string();
        assert_eq!(dispatch("tr", &args).await.unwrap(), "HELLO");

        // (b) delete ignores `to`
        let args = json!({ "data": "aabbcc", "from": "b", "delete": true }).to_string();
        assert_eq!(dispatch("tr", &args).await.unwrap(), "aacc");

        // (c) squeeze over the from-set (no translation)
        let args = json!({ "data": "aaabbb", "from": "a-z", "squeeze": true }).to_string();
        assert_eq!(dispatch("tr", &args).await.unwrap(), "ab");

        // (d) short `to` set pads with its last char (GNU behavior)
        let args = json!({ "data": "abc", "from": "abc", "to": "x" }).to_string();
        assert_eq!(dispatch("tr", &args).await.unwrap(), "xxx");

        // (e) literal trailing dash must not panic
        let args = json!({ "data": "a-b", "from": "a-", "to": "X" }).to_string();
        assert_eq!(dispatch("tr", &args).await.unwrap(), "XXb");

        // (f) error paths
        let args = json!({ "data": "x", "path": "y", "from": "a", "to": "b" }).to_string();
        assert!(dispatch("tr", &args).await.is_err());
        let args = json!({ "from": "a", "to": "b" }).to_string();
        assert!(dispatch("tr", &args).await.is_err());
        let args = json!({ "data": "x", "from": "a" }).to_string();
        assert!(dispatch("tr", &args).await.is_err());
    }

    #[tokio::test]
    async fn expand_converts_and_unexpands_with_tab_stops() {
        // (a) leading tab expands to a full tab stop.
        let a = json!({ "data": "\tx", "tab_width": 4 }).to_string();
        assert_eq!(dispatch("expand", &a).await.unwrap(), "    x");

        // (b) load-bearing: mid-line tab fills only to the next stop.
        let b = json!({ "data": "ab\tc", "tab_width": 4 }).to_string();
        assert_eq!(dispatch("expand", &b).await.unwrap(), "ab  c");

        // (c) unexpand collapses a full stop of spaces into a tab.
        let c = json!({ "data": "    x", "tab_width": 4, "unexpand": true }).to_string();
        assert_eq!(dispatch("expand", &c).await.unwrap(), "\tx");

        // (d) unexpand leaves interior spaces untouched (round-trip stable).
        let d = json!({ "data": "\ta  b", "tab_width": 4, "unexpand": true }).to_string();
        assert_eq!(dispatch("expand", &d).await.unwrap(), "\ta  b");

        // (e) tab_width == 0 is rejected.
        let e = json!({ "data": "x", "tab_width": 0 }).to_string();
        assert!(dispatch("expand", &e).await.is_err());

        // (f) neither path nor data is rejected.
        let f = json!({ "tab_width": 4 }).to_string();
        assert!(dispatch("expand", &f).await.is_err());
    }

    #[tokio::test]
    async fn dedent_semantics_and_source_selection() {
        // (a) common two-space prefix stripped.
        let out = dispatch("dedent", &json!({ "data": "    a\n      b" }).to_string())
            .await
            .unwrap();
        assert_eq!(out, "a\n  b");

        // (b) whitespace-only line ignored for prefix, emitted empty.
        let out = dispatch("dedent", &json!({ "data": "    a\n\n    b" }).to_string())
            .await
            .unwrap();
        assert_eq!(out, "a\n\nb");

        // (c) mixed tab/space share no literal prefix -> unchanged.
        let out = dispatch("dedent", &json!({ "data": "\ta\n  b" }).to_string())
            .await
            .unwrap();
        assert_eq!(out, "\ta\n  b");

        // (d) XOR guard: neither, and both, are errors.
        assert!(dispatch("dedent", &json!({}).to_string()).await.is_err());
        assert!(
            dispatch("dedent", &json!({ "data": "x", "path": "y" }).to_string())
                .await
                .is_err()
        );

        // (e) file arm reads the file and leaves it untouched.
        let path = tmp_path("dedent.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "    a\n      b").unwrap();
        let out = dispatch(
            "dedent",
            &json!({ "path": path.to_str().unwrap() }).to_string(),
        )
        .await
        .unwrap();
        assert_eq!(out, "a\n  b");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "    a\n      b");
    }

    #[tokio::test]
    async fn count_matches_counts_occurrences_and_lines() {
        let path = tmp_path("count-matches.txt");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "foo foo\nbar\nfoo").unwrap();
        let p = path.to_str().unwrap();

        let args = json!({ "path": p, "pattern": "foo" }).to_string();
        let out = dispatch("count_matches", &args).await.unwrap();
        assert!(out.contains("occurrences: 3"), "{out}");
        assert!(out.contains("matching_lines: 2"), "{out}");

        // ignore_case pins the flag the original sketch dropped.
        let args = json!({ "path": p, "pattern": "FOO", "ignore_case": true }).to_string();
        let out = dispatch("count_matches", &args).await.unwrap();
        assert!(out.contains("occurrences: 3"), "{out}");
        assert!(out.contains("matching_lines: 2"), "{out}");

        let args = json!({ "path": p, "pattern": "FOO" }).to_string();
        let out = dispatch("count_matches", &args).await.unwrap();
        assert!(out.contains("occurrences: 0"), "{out}");
        assert!(out.contains("matching_lines: 0"), "{out}");
    }

    #[tokio::test]
    async fn epoch_roundtrip_and_edges() {
        // epoch -> iso
        let args = json!({ "value": 0, "to": "iso" }).to_string();
        assert_eq!(
            dispatch("epoch", &args).await.unwrap(),
            "1970-01-01 00:00:00"
        );
        let args = json!({ "value": 1_609_462_861_i64, "to": "iso" }).to_string();
        assert_eq!(
            dispatch("epoch", &args).await.unwrap(),
            "2021-01-01 01:01:01"
        );
        // leap day
        let args = json!({ "value": 951_782_400_i64, "to": "iso" }).to_string();
        assert_eq!(
            dispatch("epoch", &args).await.unwrap(),
            "2000-02-29 00:00:00"
        );
        // negative epoch
        let args = json!({ "value": -1, "to": "iso" }).to_string();
        assert_eq!(
            dispatch("epoch", &args).await.unwrap(),
            "1969-12-31 23:59:59"
        );
        // numeric string input
        let args = json!({ "value": "0", "to": "iso" }).to_string();
        assert_eq!(
            dispatch("epoch", &args).await.unwrap(),
            "1970-01-01 00:00:00"
        );
        // iso -> epoch
        let args = json!({ "value": "2000-02-29 00:00:00", "to": "epoch" }).to_string();
        assert_eq!(dispatch("epoch", &args).await.unwrap(), "951782400");
        // date-only defaults to 00:00:00
        let args = json!({ "value": "1970-01-01", "to": "epoch" }).to_string();
        assert_eq!(dispatch("epoch", &args).await.unwrap(), "0");
        // round-trip: iso output feeds back into epoch
        let iso = dispatch(
            "epoch",
            &json!({ "value": 1_609_462_861_i64, "to": "iso" }).to_string(),
        )
        .await
        .unwrap();
        let back = dispatch("epoch", &json!({ "value": iso, "to": "epoch" }).to_string())
            .await
            .unwrap();
        assert_eq!(back, "1609462861");
        // error: malformed date
        let args = json!({ "value": "not-a-date", "to": "epoch" }).to_string();
        assert!(dispatch("epoch", &args).await.is_err());
    }

    #[tokio::test]
    async fn calc_evaluates_arithmetic_bitwise_and_error_paths() {
        // Value paths: precedence, unary, bit ops, right-assoc `**`, and the
        // trailing-`.0` strip on whole-valued floats.
        let cases = [
            ("(1<<20)/1024", "1024"),
            ("2**10", "1024"),
            ("7%3", "1"),
            ("1+2*3", "7"),
            ("-5+2", "-3"),
            ("~0", "-1"),
            ("1|2", "3"),
            ("6&3", "2"),
            ("5^1", "4"),
            ("1.5+1.5", "3"),
            ("3.0/2.0", "1.5"),
            ("2**3**2", "512"),
        ];
        for (expr, want) in cases {
            let args = json!({ "expr": expr }).to_string();
            assert_eq!(dispatch("calc", &args).await.unwrap(), want, "expr {expr}");
        }
        // Error paths: divide/mod by zero, float operand to shift/pow,
        // negative exponent, malformed input, and pow overflow.
        for bad in ["1/0", "1%0", "2.5<<1", "2**-1", "1+", "(1+2", "2**200"] {
            let args = json!({ "expr": bad }).to_string();
            assert!(
                dispatch("calc", &args).await.is_err(),
                "expected err: {bad}"
            );
        }
    }

    #[tokio::test]
    async fn nproc_reports_a_positive_integer() {
        // Offline, no key. Don't assert equality against the host's real
        // core count — it varies across CI runners and cgroups. Only that
        // the tool returns something that parses to an int >= 1.
        let out = dispatch("nproc", "{}").await.unwrap();
        let n: usize = out.trim().parse().expect("nproc returns an integer");
        assert!(n >= 1, "{out}");
    }

    #[tokio::test]
    async fn os_release_reports_target_constants() {
        let out = dispatch("os_release", &json!({}).to_string())
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["os"], json!(std::env::consts::OS));
        assert_eq!(v["arch"], json!(std::env::consts::ARCH));
        assert_eq!(v["family"], json!(std::env::consts::FAMILY));
        assert!(!std::env::consts::OS.is_empty());
        assert!(!std::env::consts::ARCH.is_empty());
        #[cfg(target_os = "linux")]
        {
            // /etc/os-release is present on essentially every Linux distro
            // and CI runner, so `pretty` should be populated there.
            assert!(v.get("pretty").and_then(Value::as_str).is_some());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kill_terminates_a_running_sleep() {
        use std::process::Stdio;
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id().expect("child has a pid");
        let args = json!({ "pid": pid, "signal": "KILL" }).to_string();
        assert!(dispatch("kill", &args).await.is_ok());
        let status = tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .expect("kill should reap within 2s")
            .expect("child.wait succeeds after SIGKILL");
        assert!(!status.success());
    }

    #[tokio::test]
    async fn kill_rejects_unknown_signal_and_low_pid() {
        // Bogus signal name is refused before any syscall.
        let bogus = json!({ "pid": 424242, "signal": "BOGUS" }).to_string();
        assert!(dispatch("kill", &bogus).await.is_err());
        // pid <= 1 is a hard guardrail (blocks broadcast/init).
        let low = json!({ "pid": 1, "signal": "TERM" }).to_string();
        assert!(dispatch("kill", &low).await.is_err());
        // group defaults off; a numeric signal parses on Unix, is rejected
        // on Windows — either way this maps to TERM's number cleanly.
        assert!(super::kill_parse_signal(&Some(json!("term"))).unwrap().1 == "TERM");
    }

    #[tokio::test]
    async fn tcp_check_open_then_closed_on_loopback() {
        // Bind an ephemeral loopback port; while the listener is alive the
        // connect succeeds, and after dropping it the OS refuses the connect.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let args = json!({ "host": "127.0.0.1", "port": port, "timeout_ms": 2000 }).to_string();
        let open = dispatch("tcp_check", &args).await.unwrap();
        assert!(open.contains("open"), "expected open, got: {open}");

        drop(listener);
        let closed = dispatch("tcp_check", &args).await.unwrap();
        // The meaningful, portable invariant is "no longer accepting". The
        // exact classification of a refused loopback connect differs by OS
        // (Windows often reports timed-out/reset/unreachable rather than
        // ECONNREFUSED), so assert not-open rather than a specific word.
        assert!(
            !closed.contains("open ("),
            "expected not-open after drop, got: {closed}"
        );
    }

    #[tokio::test]
    async fn dns_resolve_localhost_is_offline_safe() {
        // `localhost` resolves from the hosts file — no network required.
        let args = json!({ "host": "localhost" }).to_string();
        let out = dispatch("dns_resolve", &args).await.unwrap();
        assert!(
            out.lines().any(|ip| ip == "127.0.0.1" || ip == "::1"),
            "expected loopback in {out:?}"
        );
    }

    #[tokio::test]
    async fn download_refuses_to_overwrite_existing_dest() {
        // The overwrite guard runs before any network call, so this stays
        // fully offline: no socket is opened for the bogus URL below.
        let path = tmp_path("download-guard.bin");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "existing").unwrap();
        let args = json!({
            "url": "http://127.0.0.1:0/x",
            "dest": path.to_str().unwrap(),
            "overwrite": false
        })
        .to_string();
        let err = dispatch("download", &args).await.unwrap_err().to_string();
        assert!(err.contains("already exists"));
        // File must be untouched by the failed call.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "existing");
    }

    #[tokio::test]
    async fn http_request_validation_errors() {
        // Invalid method token (contains a space) is rejected before any
        // network access — safe to run offline in CI.
        let args = json!({ "url": "http://example.invalid", "method": "NOT A METHOD" });
        let err = dispatch("http_request", &args.to_string())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid HTTP method"), "got: {err}");

        // body + json are mutually exclusive — also rejected pre-network.
        let args = json!({
            "url": "http://example.invalid",
            "body": "raw",
            "json": { "k": "v" }
        });
        let err = dispatch("http_request", &args.to_string())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("mutually exclusive"), "got: {err}");

        // Lowercase method normalizes fine at the parse seam.
        assert!(super::http_request_parse_method("get").is_ok());
    }

    #[tokio::test]
    async fn cargo_metadata_no_deps_offline() {
        // `no_deps` skips dependency resolution, so this stays fully offline
        // (no registry access). Runs against this very workspace.
        let out = dispatch("cargo_metadata", &json!({ "no_deps": true }).to_string())
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&out).expect("cargo metadata emitted valid JSON");
        assert!(v.is_object());
        let packages = v.get("packages").and_then(|p| p.as_array()).unwrap();
        assert!(!packages.is_empty());
        assert!(packages[0].get("name").and_then(|n| n.as_str()).is_some());
        assert!(v
            .get("workspace_members")
            .and_then(|m| m.as_array())
            .is_some());
    }

    #[tokio::test]
    async fn cargo_tree_invert_lists_reverse_deps() {
        // Offline: the registry is already vendored under target/ from the
        // build, so `cargo tree` needs no network. Invert on a known dep and
        // check both the crate and a workspace member surface.
        let out = dispatch("cargo_tree", &json!({ "invert": "reqwest" }).to_string())
            .await
            .expect("cargo tree -i reqwest");
        assert!(out.contains("reqwest"), "missing reqwest in: {out}");
        assert!(out.contains("teleia-"), "missing workspace crate in: {out}");
    }

    #[test]
    fn test_one_argv_selects_runner_per_extension() {
        // Rust: cargo substring filter alongside --all-targets.
        let (runner, argv) = test_one_argv(Some("rs"), "src/lib.rs", "my_test").unwrap();
        assert_eq!(runner, "cargo");
        assert_eq!(argv, vec!["test", "--all-targets", "my_test"]);

        // Python plain name -> pytest PATH -k NAME.
        let (runner, argv) = test_one_argv(Some("py"), "tests/", "test_thing").unwrap();
        assert_eq!(runner, "pytest");
        assert_eq!(argv, vec!["tests/", "-k", "test_thing"]);

        // Python node-id (contains ::) -> passed verbatim.
        let (runner, argv) =
            test_one_argv(Some("py"), "tests/foo.py", "tests/foo.py::test_x").unwrap();
        assert_eq!(runner, "pytest");
        assert_eq!(argv, vec!["tests/foo.py::test_x"]);

        // Go: -run regex + ./...
        let (runner, argv) = test_one_argv(Some("go"), "main.go", "^TestX$").unwrap();
        assert_eq!(runner, "go");
        assert_eq!(argv, vec!["test", "-run", "^TestX$", "./..."]);

        // JS/TS: npm test -- -t NAME
        let (runner, argv) = test_one_argv(Some("ts"), "app.ts", "renders").unwrap();
        assert_eq!(runner, "npm");
        assert_eq!(argv, vec!["test", "--", "-t", "renders"]);

        // Unknown extension and missing extension both error.
        assert!(test_one_argv(Some("txt"), "a.txt", "x").is_err());
        assert!(test_one_argv(None, "Makefile", "x").is_err());
    }

    #[tokio::test]
    async fn cloc_counts_by_language_and_honors_exclude() {
        // Build a scratch directory tree.
        let dir = tmp_path("cloc-dir");
        std::fs::create_dir_all(&dir).unwrap();
        struct DirCleanup(PathBuf);
        impl Drop for DirCleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _c = DirCleanup(dir.clone());

        // a.rs: 2 code lines, 1 blank line, 1 comment-only line.
        std::fs::write(dir.join("a.rs"), "let x = 1;\n\n// a comment\nlet y = 2;\n").unwrap();
        // b.py: 1 code line.
        std::fs::write(dir.join("b.py"), "print(1)\n").unwrap();
        // A binary / non-UTF8 file with a source-like extension: must be
        // skipped without failing the walk.
        std::fs::write(dir.join("bad.rs"), [0xff, 0xfe, 0x00, 0x01]).unwrap();

        let args = json!({ "path": dir.to_str().unwrap() }).to_string();
        let out = dispatch("cloc", &args).await.unwrap();
        assert!(out.contains("Rust"), "output missing Rust: {out}");
        assert!(out.contains("Python"), "output missing Python: {out}");
        // Rust row: files=1 (bad.rs skipped), blank=1, comment=1, code=2.
        let rust_line = out
            .lines()
            .find(|l| l.starts_with("Rust"))
            .expect("no Rust row");
        let nums: Vec<u64> = rust_line
            .split_whitespace()
            .skip(1)
            .filter_map(|t| t.parse().ok())
            .collect();
        assert_eq!(nums, vec![1, 1, 1, 2], "rust stats wrong: {rust_line}");

        // Excluding *.py drops the Python bucket entirely.
        let args = json!({ "path": dir.to_str().unwrap(), "exclude": ["*.py"] }).to_string();
        let out = dispatch("cloc", &args).await.unwrap();
        assert!(out.contains("Rust"), "output missing Rust: {out}");
        assert!(!out.contains("Python"), "Python not excluded: {out}");
    }

    #[test]
    fn json_diff_reports_changed_added_ignores_reorder() {
        let a = json!({ "x": 1, "y": 2 });
        let b = json!({ "y": 2, "x": 3, "z": 4 });
        let mut out = Vec::new();
        json_diff_walk("", &a, &b, &mut out);
        assert!(out.contains(&"changed x: 1 -> 3".to_string()), "{out:?}");
        assert!(out.contains(&"added z: 4".to_string()), "{out:?}");
        assert!(!out.iter().any(|l| l.contains(" y")), "{out:?}");
        assert_eq!(out.len(), 2, "{out:?}");

        let mut nested = Vec::new();
        let na = json!({ "o": { "k": 1 } });
        let nb = json!({ "o": 5 });
        json_diff_walk("", &na, &nb, &mut nested);
        assert_eq!(nested, vec!["changed o: {\"k\":1} -> 5".to_string()]);
    }

    #[tokio::test]
    async fn json_merge_deep_merges_and_honors_array_mode() {
        let pa = tmp_path("merge-a.json");
        let pb = tmp_path("merge-b.json");
        let _ca = Cleanup(pa.clone());
        let _cb = Cleanup(pb.clone());

        // (a) object recursion + (b) scalar later-wins in one pair.
        std::fs::write(&pa, r#"{"a":{"x":1},"n":1}"#).unwrap();
        std::fs::write(&pb, r#"{"a":{"y":2},"n":2}"#).unwrap();
        let args = json!({ "paths": [pa.to_str().unwrap(), pb.to_str().unwrap()] }).to_string();
        let out = dispatch("json_merge", &args).await.unwrap();
        let got: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(got, json!({ "a": { "x": 1, "y": 2 }, "n": 2 }));

        // (c) type mismatch: object overwritten by scalar.
        std::fs::write(&pa, r#"{"a":{"x":1}}"#).unwrap();
        std::fs::write(&pb, r#"{"a":5}"#).unwrap();
        let out = dispatch("json_merge", &args).await.unwrap();
        let got: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(got, json!({ "a": 5 }));

        // (d) array default replace vs (e) concat.
        std::fs::write(&pa, r#"{"l":[1]}"#).unwrap();
        std::fs::write(&pb, r#"{"l":[2]}"#).unwrap();
        let out = dispatch("json_merge", &args).await.unwrap();
        let got: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(got, json!({ "l": [2] }));
        let concat = json!({
            "paths": [pa.to_str().unwrap(), pb.to_str().unwrap()],
            "array_mode": "concat"
        })
        .to_string();
        let out = dispatch("json_merge", &concat).await.unwrap();
        let got: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(got, json!({ "l": [1, 2] }));

        // (g) single path re-emits that doc.
        let single = json!({ "paths": [pa.to_str().unwrap()] }).to_string();
        let out = dispatch("json_merge", &single).await.unwrap();
        let got: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(got, json!({ "l": [1] }));

        // (f) empty paths errors.
        let empty = json!({ "paths": [] }).to_string();
        assert!(dispatch("json_merge", &empty).await.is_err());
    }

    #[tokio::test]
    async fn jsonl_extracts_pointer_reflows_and_reports_line_errors() {
        let path = tmp_path("data.jsonl");
        let _c = Cleanup(path.clone());
        // Trailing newline proves blank-line skipping.
        std::fs::write(&path, "{\"a\":1}\n{\"a\":2}\n").unwrap();
        let p = path.to_str().unwrap();

        // Pointer extraction across every record.
        let args = json!({ "path": p, "pointer": "/a" }).to_string();
        assert_eq!(dispatch("jsonl", &args).await.unwrap(), "1\n2");

        // Compact reflow (default).
        let args = json!({ "path": p, "mode": "compact" }).to_string();
        assert_eq!(
            dispatch("jsonl", &args).await.unwrap(),
            "{\"a\":1}\n{\"a\":2}"
        );

        // Pretty reflow.
        let args = json!({ "path": p, "mode": "pretty" }).to_string();
        let pretty = dispatch("jsonl", &args).await.unwrap();
        assert!(pretty.contains("\n  \"a\": 1"), "got: {pretty}");

        // Missing pointer surfaces the line number.
        let args = json!({ "path": p, "pointer": "/missing" }).to_string();
        let err = dispatch("jsonl", &args).await.unwrap_err().to_string();
        assert!(err.contains("line 1"), "got: {err}");

        // Malformed line surfaces the line number.
        let bad = tmp_path("bad.jsonl");
        let _cb = Cleanup(bad.clone());
        std::fs::write(&bad, "{\"a\":1}\nnot json\n").unwrap();
        let args = json!({ "path": bad.to_str().unwrap() }).to_string();
        let err = dispatch("jsonl", &args).await.unwrap_err().to_string();
        assert!(err.contains("line 2"), "got: {err}");
    }

    #[tokio::test]
    async fn dotenv_parse_handles_quotes_comments_and_duplicates() {
        let path = tmp_path("parse.env");
        let _c = Cleanup(path.clone());
        std::fs::write(
            &path,
            "export A=1\nB=\"two words\"\n# c\nC=3\nD='raw\\n'\nE=\"line\\nbreak\"\nA=override",
        )
        .unwrap();
        let args = json!({ "path": path.to_str().unwrap() }).to_string();
        let out = dispatch("dotenv_parse", &args).await.unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["A"], "override");
        assert_eq!(v["B"], "two words");
        assert_eq!(v["C"], "3");
        assert_eq!(v["D"], "raw\\n");
        assert_eq!(v["E"], "line\nbreak");

        let missing = tmp_path("no-such.env");
        let args = json!({ "path": missing.to_str().unwrap() }).to_string();
        assert!(dispatch("dotenv_parse", &args).await.is_err());
    }

    #[tokio::test]
    async fn ini_to_json_parses_sections_and_bare_keys() {
        let path = tmp_path("config.ini");
        let _c = Cleanup(path.clone());
        std::fs::write(
            &path,
            "; a comment\ntop = 1\n\n[server]\n# full-line comment\nhost = localhost\nport = 8080 ; inline stays\n[server]\nport = 9090\n",
        )
        .unwrap();
        let args = json!({ "path": path.to_str().unwrap() }).to_string();
        let out = dispatch("ini_to_json", &args).await.unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["top"], json!("1"));
        assert_eq!(v["server"]["host"], json!("localhost"));
        // Inline `;` is preserved, not stripped.
        assert_eq!(v["server"]["port"], json!("9090"));
        // Bad line (no `=`) is a hard error.
        let bad = tmp_path("bad.ini");
        let _c2 = Cleanup(bad.clone());
        std::fs::write(&bad, "[x]\nnot a pair\n").unwrap();
        let bad_args = json!({ "path": bad.to_str().unwrap() }).to_string();
        assert!(dispatch("ini_to_json", &bad_args).await.is_err());
    }

    #[tokio::test]
    async fn ndjson_to_json_round_trips_and_errors() {
        // NDJSON -> array
        let path = tmp_path("ndjson.jsonl");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "{\"a\":1}\n{\"a\":2}\n").unwrap();
        let args = json!({ "path": path.to_str().unwrap(), "to": "array" }).to_string();
        let out = dispatch("ndjson_to_json", &args).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed, json!([{ "a": 1 }, { "a": 2 }]));

        // array -> NDJSON lines (exact string, trailing newline)
        let apath = tmp_path("ndjson-arr.json");
        let _ac = Cleanup(apath.clone());
        std::fs::write(&apath, "[{\"a\":1},{\"a\":2}]").unwrap();
        let largs = json!({ "path": apath.to_str().unwrap(), "to": "lines" }).to_string();
        let lout = dispatch("ndjson_to_json", &largs).await.unwrap();
        assert_eq!(lout, "{\"a\":1}\n{\"a\":2}\n");

        // to=lines on a non-array is an error
        let opath = tmp_path("ndjson-obj.json");
        let _oc = Cleanup(opath.clone());
        std::fs::write(&opath, "{}").unwrap();
        let oargs = json!({ "path": opath.to_str().unwrap(), "to": "lines" }).to_string();
        assert!(dispatch("ndjson_to_json", &oargs).await.is_err());

        // malformed NDJSON line names the line number
        let bpath = tmp_path("ndjson-bad.jsonl");
        let _bc = Cleanup(bpath.clone());
        std::fs::write(&bpath, "{\"a\":1}\nnot json\n").unwrap();
        let bargs = json!({ "path": bpath.to_str().unwrap(), "to": "array" }).to_string();
        let err = dispatch("ndjson_to_json", &bargs).await.unwrap_err();
        assert!(err.to_string().contains("line 2"), "err was: {err}");
    }

    // --- regression tests for review findings (fix/tools-review-findings) ---

    #[tokio::test]
    async fn ini_to_json_survives_key_section_name_collision() {
        // A bare key colliding with a later same-named section used to panic
        // (unwrap on a String via as_object_mut).
        let path = tmp_path("collide.ini");
        let _c = Cleanup(path.clone());
        std::fs::write(&path, "foo=1\n[foo]\nk=v\n").unwrap();
        let out = dispatch(
            "ini_to_json",
            &json!({ "path": path.to_str().unwrap() }).to_string(),
        )
        .await
        .unwrap();
        // Section wins (last-writer); no panic.
        assert!(out.contains("\"k\": \"v\""), "{out}");
    }

    #[tokio::test]
    async fn epoch_rejects_impossible_calendar_days() {
        for bad in ["2001-02-29", "2000-02-30", "2000-04-31"] {
            let args = json!({ "value": bad, "to": "epoch" }).to_string();
            assert!(
                dispatch("epoch", &args).await.is_err(),
                "{bad} should be rejected"
            );
        }
        // A real leap day still works.
        assert!(dispatch(
            "epoch",
            &json!({ "value": "2000-02-29", "to": "epoch" }).to_string()
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn base32_rejects_invalid_tail_length() {
        // "MYA" is a 3-char tail — impossible for RFC 4648; must error not
        // silently decode.
        assert!(dispatch(
            "base32",
            &json!({ "data": "MYA", "decode": true }).to_string()
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn strings_caps_output_and_marks_truncation() {
        let path = tmp_path("strings-cap.bin");
        let _c = Cleanup(path.clone());
        // Five printable runs separated by NULs; cap at 2.
        std::fs::write(&path, b"aaaa\x00bbbb\x00cccc\x00dddd\x00eeee").unwrap();
        let args = json!({ "path": path.to_str().unwrap(), "limit": 2 }).to_string();
        let out = dispatch("strings", &args).await.unwrap();
        assert!(out.contains("[truncated at 2 runs]"), "{out}");
        assert!(!out.contains("cccc"), "{out}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mktemp_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let out = dispatch("mktemp", &json!({ "prefix": "teleia-perm-" }).to_string())
            .await
            .unwrap();
        let path = out.trim();
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let _ = std::fs::remove_file(path);
        assert_eq!(mode, 0o600, "temp file mode was {mode:o}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cloc_does_not_follow_symlinked_dirs() {
        // A self-referential symlink must not be traversed (no hang / no
        // external files counted).
        let dir = tmp_path("cloc-sym");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), "fn main() {}\n").unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("loop")).unwrap();
        let args = json!({ "path": dir.to_str().unwrap() }).to_string();
        let out = dispatch("cloc", &args).await.unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        // Completed (didn't spin on the cycle) and counted the one real file.
        assert!(out.contains("Rust") || out.contains("rs"), "{out}");
    }
}
