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
            "Run a bounded set of git subcommands in the current repo. `subcommand` is one of status / diff / log / add / commit. `paths` scopes diff and is required by add; `message` is required by commit. Returns git's combined output.",
            json!({ "type": "object", "properties": {
                "subcommand": { "type": "string", "enum": ["status", "diff", "log", "add", "commit"] },
                "paths": { "type": "array", "items": { "type": "string" }, "description": "Paths for add (required) or to scope diff (optional)" },
                "message": { "type": "string", "description": "Commit message (required for commit)" }
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

/// Minimal SHA-256 (FIPS 180-4) of `data`, returned as a 64-char hex
/// string. Hand-rolled so we don't drag in a hash crate for one tool.
fn sha256(data: &[u8]) -> String {
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
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
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
    let mut out = String::with_capacity(64);
    for word in h {
        out.push_str(&format!("{word:08x}"));
    }
    out
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
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum GitSub {
    Status,
    Diff,
    Log,
    Add,
    Commit,
}

async fn git_tool(args: Value) -> Result<String> {
    let GitArgs {
        subcommand,
        paths,
        message,
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
}
