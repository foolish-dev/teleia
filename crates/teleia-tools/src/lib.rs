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
}
