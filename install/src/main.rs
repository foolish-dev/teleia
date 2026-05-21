// Build available Teleia impls into $PREFIX (default ~/.local/bin).
// Skips any impl whose toolchain is missing. Idempotent.

use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn have(cmd: &str) -> bool {
    let Some(path) = env::var_os("PATH") else { return false };
    env::split_paths(&path).any(|d| {
        fs::metadata(d.join(cmd))
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

fn run(cmd: &mut Command) -> bool {
    cmd.stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn write_exec(path: &Path, content: &str) -> std::io::Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(content.as_bytes())?;
    f.set_permissions(fs::Permissions::from_mode(0o755))
}

fn try_install<F: FnOnce() -> Result<String, String>>(name: &str, body: F) -> bool {
    let (status, ok) = match body() {
        Ok(msg) => (msg, true),
        Err(msg) => (msg, false),
    };
    println!("→ {name:<7} {status}");
    ok
}

fn main() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("install/ must have parent")
        .to_path_buf();
    let prefix = env::var_os("PREFIX").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env::var_os("HOME").expect("HOME not set")).join(".local/bin")
    });
    fs::create_dir_all(&prefix).expect("mkdir prefix");

    let ok = [
        try_install("rust", || {
            if !have("cargo") { return Err("skipped (cargo not in PATH)".into()); }
            let dir = repo.join("rust");
            if !run(Command::new("cargo")
                .args(["build", "--release", "--locked", "--quiet"])
                .current_dir(&dir))
            {
                return Err("✗ build failed".into());
            }
            let dst = prefix.join("teleia-rust");
            fs::copy(dir.join("target/release/teleia"), &dst)
                .map_err(|e| format!("✗ copy failed: {e}"))?;
            fs::set_permissions(&dst, fs::Permissions::from_mode(0o755)).ok();
            Ok(format!("✓ built {}", dst.display()))
        }),
        try_install("python", || {
            if !have("python3") { return Err("skipped (python3 not in PATH)".into()); }
            let dst = prefix.join("teleia-python");
            write_exec(&dst, &format!(
                "#!/usr/bin/env bash\n\
                 export PYTHONPATH=\"{path}${{PYTHONPATH:+:$PYTHONPATH}}\"\n\
                 exec python3 -m teleia \"$@\"\n",
                path = repo.join("python").display(),
            )).map_err(|e| format!("✗ write failed: {e}"))?;
            Ok(format!("✓ installed {}", dst.display()))
        }),
        try_install("go", || {
            if !have("go") { return Err("skipped (go not in PATH)".into()); }
            let dst = prefix.join("teleia-go");
            if !run(Command::new("go")
                .args(["build", "-o"]).arg(&dst).arg("./cmd/teleia")
                .current_dir(repo.join("go")))
            {
                return Err("✗ build failed".into());
            }
            Ok(format!("✓ built {}", dst.display()))
        }),
        try_install("lua", || {
            let bin = if have("lua5.4") { "lua5.4" }
                else if have("lua")     { "lua" }
                else { return Err("skipped (lua5.4 not in PATH)".into()); };
            let dst = prefix.join("teleia-lua");
            write_exec(&dst, &format!(
                "#!/usr/bin/env bash\nexec {bin} \"{path}\" \"$@\"\n",
                path = repo.join("lua/teleia.lua").display(),
            )).map_err(|e| format!("✗ write failed: {e}"))?;
            Ok(format!("✓ installed {}", dst.display()))
        }),
        try_install("bun", || {
            if !have("bun") { return Err("skipped (bun not in PATH)".into()); }
            if !run(Command::new("bun")
                .args(["install", "--frozen-lockfile"])
                .current_dir(repo.join("bun"))
                .stdout(Stdio::null()).stderr(Stdio::null()))
            {
                return Err("✗ bun install failed".into());
            }
            let dst = prefix.join("teleia-bun");
            write_exec(&dst, &format!(
                "#!/usr/bin/env bash\nexec bun \"{path}\" \"$@\"\n",
                path = repo.join("bun/src/index.ts").display(),
            )).map_err(|e| format!("✗ write failed: {e}"))?;
            Ok(format!("✓ installed {}", dst.display()))
        }),
    ].iter().filter(|&&x| x).count();

    println!("\n{ok}/5 impls installed.");

    let on_path = env::var_os("PATH")
        .map(|p| env::split_paths(&p).any(|d| d == prefix))
        .unwrap_or(false);
    if !on_path {
        println!("Note: {} is not on PATH. Add it to your shell init.", prefix.display());
    }
}
