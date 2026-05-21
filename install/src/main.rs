// Build available Teleia impls into $PREFIX (default ~/.local/bin).
// Skips any impl whose toolchain is missing. Idempotent; re-run to rebuild.

use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn repo_root() -> PathBuf {
    // env! is evaluated at compile time, so this path survives `cargo install`
    // moving the binary out of the workspace.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("install/ must have parent")
        .to_path_buf()
}

fn prefix() -> PathBuf {
    if let Some(p) = env::var_os("PREFIX") {
        return PathBuf::from(p);
    }
    let home = env::var_os("HOME").expect("HOME not set");
    PathBuf::from(home).join(".local").join("bin")
}

fn have(cmd: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else { return false };
    env::split_paths(&paths).any(|p| {
        let candidate = p.join(cmd);
        fs::metadata(&candidate)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

fn report(name: &str, status: &str) {
    println!("→ {:<7} {}", name, status);
}

fn run(cmd: &mut Command) -> bool {
    cmd.stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn write_shim(path: &Path, script: &str) -> std::io::Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(script.as_bytes())?;
    f.set_permissions(fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn build_rust(repo: &Path, prefix: &Path) -> bool {
    if !have("cargo") {
        report("rust", "skipped (cargo not in PATH)");
        return false;
    }
    let rust_dir = repo.join("rust");
    let ok = run(Command::new("cargo")
        .args(["build", "--release", "--locked", "--quiet"])
        .current_dir(&rust_dir));
    if !ok {
        report("rust", "✗ build failed");
        return false;
    }
    let src = rust_dir.join("target/release/teleia");
    let dst = prefix.join("teleia-rust");
    if let Err(e) = fs::copy(&src, &dst) {
        report("rust", &format!("✗ copy failed: {e}"));
        return false;
    }
    let _ = fs::set_permissions(&dst, fs::Permissions::from_mode(0o755));
    report("rust", &format!("✓ built {}", dst.display()));
    true
}

fn build_python(repo: &Path, prefix: &Path) -> bool {
    if !have("python3") {
        report("python", "skipped (python3 not in PATH)");
        return false;
    }
    let dst = prefix.join("teleia-python");
    let py = repo.join("python");
    let shim = format!(
        "#!/usr/bin/env bash\n\
         export PYTHONPATH=\"{path}${{PYTHONPATH:+:$PYTHONPATH}}\"\n\
         exec python3 -m teleia \"$@\"\n",
        path = py.display()
    );
    if let Err(e) = write_shim(&dst, &shim) {
        report("python", &format!("✗ write failed: {e}"));
        return false;
    }
    report("python", &format!("✓ installed {}", dst.display()));
    true
}

fn build_go(repo: &Path, prefix: &Path) -> bool {
    if !have("go") {
        report("go", "skipped (go not in PATH)");
        return false;
    }
    let dst = prefix.join("teleia-go");
    let ok = run(Command::new("go")
        .args(["build", "-o"])
        .arg(&dst)
        .arg("./cmd/teleia")
        .current_dir(repo.join("go")));
    if !ok {
        report("go", "✗ build failed");
        return false;
    }
    report("go", &format!("✓ built {}", dst.display()));
    true
}

fn build_lua(repo: &Path, prefix: &Path) -> bool {
    let lua_bin = if have("lua5.4") {
        "lua5.4"
    } else if have("lua") {
        "lua"
    } else {
        report("lua", "skipped (lua5.4 not in PATH)");
        return false;
    };
    let dst = prefix.join("teleia-lua");
    let entry = repo.join("lua/teleia.lua");
    let shim = format!(
        "#!/usr/bin/env bash\nexec {bin} \"{path}\" \"$@\"\n",
        bin = lua_bin,
        path = entry.display()
    );
    if let Err(e) = write_shim(&dst, &shim) {
        report("lua", &format!("✗ write failed: {e}"));
        return false;
    }
    report("lua", &format!("✓ installed {}", dst.display()));
    true
}

fn build_bun(repo: &Path, prefix: &Path) -> bool {
    if !have("bun") {
        report("bun", "skipped (bun not in PATH)");
        return false;
    }
    let ok = run(Command::new("bun")
        .args(["install", "--frozen-lockfile"])
        .current_dir(repo.join("bun"))
        .stdout(Stdio::null())
        .stderr(Stdio::null()));
    if !ok {
        report("bun", "✗ bun install failed");
        return false;
    }
    let dst = prefix.join("teleia-bun");
    let entry = repo.join("bun/src/index.ts");
    let shim = format!(
        "#!/usr/bin/env bash\nexec bun \"{path}\" \"$@\"\n",
        path = entry.display()
    );
    if let Err(e) = write_shim(&dst, &shim) {
        report("bun", &format!("✗ write failed: {e}"));
        return false;
    }
    report("bun", &format!("✓ installed {}", dst.display()));
    true
}

fn main() {
    let repo = repo_root();
    let prefix = prefix();
    fs::create_dir_all(&prefix).expect("mkdir prefix");

    let installed = [
        build_rust(&repo, &prefix),
        build_python(&repo, &prefix),
        build_go(&repo, &prefix),
        build_lua(&repo, &prefix),
        build_bun(&repo, &prefix),
    ]
    .iter()
    .filter(|&&x| x)
    .count();

    println!();
    println!("{installed}/5 impls installed.");

    let path = env::var_os("PATH").unwrap_or_default();
    let on_path = env::split_paths(&path).any(|p| p == prefix);
    if !on_path {
        println!(
            "Note: {} is not on PATH. Add it to your shell init.",
            prefix.display()
        );
    }
}
