use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use teleia_llm::ToolDef;
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
    let mut cmd = Command::new("timeout");
    cmd.arg("30").arg("bash").arg("-lc").arg(&command);
    // SAFETY: pre_exec runs in the forked child before exec. dup2(1, 2)
    // redirects the child's stderr fd onto stdout's pipe, so writes from
    // both streams land in one buffer in emit order — matching go's
    // CombinedOutput and lua's outer `> tmp 2>&1` redirect.
    unsafe {
        cmd.pre_exec(|| {
            if libc::dup2(1, 2) == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let output = cmd
        .output()
        .await
        .with_context(|| format!("spawn timeout/bash for: {command}"))?;
    let mut out = String::from_utf8_lossy(&output.stdout).into_owned();
    let exit_code = output.status.code().unwrap_or(-1);
    if exit_code == 124 {
        out.push_str("\n[bash timed out after 30s]");
    } else if !output.status.success() {
        out.push_str(&format!("\n[exit {exit_code}]"));
    }
    Ok(out)
}
