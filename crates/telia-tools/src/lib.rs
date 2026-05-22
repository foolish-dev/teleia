use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use telia_llm::ToolDef;
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
            "Replace a unique substring inside a file. Fails if old_string is not found or not unique.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" }
                },
                "required": ["path", "old_string", "new_string"]
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
    ]
}

pub async fn dispatch(name: &str, arguments: &str) -> Result<String> {
    let args: Value = serde_json::from_str(arguments)
        .with_context(|| format!("parsing arguments for tool `{name}`: {arguments}"))?;

    match name {
        "read" => read_tool(args).await,
        "write" => write_tool(args).await,
        "edit" => edit_tool(args).await,
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
}

async fn edit_tool(args: Value) -> Result<String> {
    let EditArgs {
        path,
        old_string,
        new_string,
    } = serde_json::from_value(args)?;
    let original = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {path}"))?;
    let occurrences = original.matches(&old_string).count();
    if occurrences == 0 {
        return Err(anyhow!("old_string not found in {path}"));
    }
    if occurrences > 1 {
        return Err(anyhow!(
            "old_string matches {occurrences} times in {path}; needs to be unique"
        ));
    }
    let updated = original.replacen(&old_string, &new_string, 1);
    tokio::fs::write(&path, &updated)
        .await
        .with_context(|| format!("write {path}"))?;
    Ok(format!("edited {path}"))
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
    // both streams land in one buffer in emit order.
    unsafe {
        cmd.pre_exec(|| {
            if libc::dup2(1, 2) == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn bash for: {command}"))?;
    let mut stdout = child.stdout.take().expect("stdout piped");
    // Drain in a separate task so we keep accumulating output regardless of
    // whether the child exits naturally or we kill it on timeout.
    let drain = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf).await;
        buf
    });

    let mut timed_out = false;
    let exit_status = tokio::select! {
        status = child.wait() => status?,
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            timed_out = true;
            let _ = child.start_kill();
            child.wait().await?
        }
    };

    let buf = drain.await.unwrap_or_default();
    let mut out = String::from_utf8_lossy(&buf).into_owned();
    if timed_out {
        out.push_str("\n[bash timed out after 30s]");
    } else if !exit_status.success() {
        out.push_str(&format!("\n[exit {}]", exit_status.code().unwrap_or(-1)));
    }
    Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp_path(name: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "telia-tools-test-{}-{}-{}",
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
    async fn dispatch_rejects_unknown_tool() {
        assert!(dispatch("nonsense", "{}").await.is_err());
    }

    #[tokio::test]
    async fn bash_returns_stdout() {
        let args = json!({ "command": "echo hello" }).to_string();
        let result = dispatch("bash", &args).await.unwrap();
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn bash_reports_nonzero_exit() {
        let args = json!({ "command": "exit 7" }).to_string();
        let result = dispatch("bash", &args).await.unwrap();
        assert!(result.contains("[exit 7]"));
    }

    #[tokio::test]
    async fn bash_merges_stderr_into_stdout() {
        let args = json!({ "command": "echo out; echo err >&2" }).to_string();
        let result = dispatch("bash", &args).await.unwrap();
        assert!(result.contains("out"));
        assert!(result.contains("err"));
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
}
