//! Update check against the GitHub releases API. Cheap, best-effort,
//! cached for the rest of the session — runs once in `main` and the
//! result is plumbed into the TUI so `/update` can re-display it.
//!
//! The startup check does not auto-install — it just reports what's
//! available (the running binary might've been put there by `cargo
//! install`, the install.sh one-liner, or a package manager). Explicit
//! `teleia --self-update` (see [`self_update`]) does the upgrade: download
//! the prebuilt for this platform, verify it against the release's
//! SHA256SUMS, and replace the running binary in place — the Rust half of
//! install.sh.

use serde::Deserialize;
use std::time::Duration;

const RELEASES_URL: &str = "https://api.github.com/repos/foolish-dev/teleia/releases/latest";
const REPO: &str = "https://github.com/foolish-dev/teleia";

/// Cached snapshot of the update-check result. `None` means the check
/// itself failed (offline, rate-limited, repo without releases).
#[derive(Debug, Clone)]
pub struct UpdateCheck {
    pub current: String,
    pub latest: String,
    pub url: String,
    /// True when `latest > current` by simple semver-ish string compare.
    pub newer: bool,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// Hit GitHub once with a 5s budget. Returns `None` on any failure —
/// startup must never block longer than this on an update check.
pub async fn check() -> Option<UpdateCheck> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .user_agent(format!("teleia/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let resp = client.get(RELEASES_URL).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let release: Release = resp.json().await.ok()?;
    if release.draft || release.prerelease {
        return None;
    }
    let current = env!("CARGO_PKG_VERSION").to_string();
    let latest = release.tag_name.trim_start_matches('v').to_string();
    let url = release
        .html_url
        .unwrap_or_else(|| "https://github.com/foolish-dev/teleia/releases".to_string());
    let newer = version_is_newer(&latest, &current);
    Some(UpdateCheck {
        current,
        latest,
        url,
        newer,
    })
}

/// Lightweight semver-ish compare: split on `.`, parse each segment as
/// u64, lexicographic on the resulting tuple. Suffixes after `-` are
/// ignored. Not RFC-2119 compliant but good enough to tell `0.2.0`
/// from `0.1.9`.
fn version_is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.split('-')
            .next()
            .unwrap_or("")
            .split('.')
            .map(|s| s.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let l = parse(latest);
    let c = parse(current);
    let max = l.len().max(c.len());
    for i in 0..max {
        let li = l.get(i).copied().unwrap_or(0);
        let ci = c.get(i).copied().unwrap_or(0);
        if li > ci {
            return true;
        }
        if li < ci {
            return false;
        }
    }
    false
}

/// The release asset name (`teleia-<target>[.exe]`) published for the
/// host, or `None` when we ship no prebuilt for this os/arch — the caller
/// then points the user at a source build. Mirrors install.sh's
/// `detect_target` and the `release.yml` asset matrix. Note `ARCH` is
/// `powerpc64` for both endiannesses; we only publish the little-endian
/// build, which is the one that matters in practice.
fn asset_name() -> Option<String> {
    use std::env::consts::{ARCH, OS};
    let (triple, ext) = match (OS, ARCH) {
        ("linux", "x86_64") => ("x86_64-unknown-linux-musl", ""),
        ("linux", "aarch64") => ("aarch64-unknown-linux-musl", ""),
        ("linux", "arm") => ("armv7-unknown-linux-musleabihf", ""),
        ("linux", "x86") => ("i686-unknown-linux-musl", ""),
        ("linux", "riscv64") => ("riscv64gc-unknown-linux-gnu", ""),
        ("linux", "powerpc64") => ("powerpc64le-unknown-linux-gnu", ""),
        ("linux", "s390x") => ("s390x-unknown-linux-gnu", ""),
        ("macos", "x86_64") => ("x86_64-apple-darwin", ""),
        ("macos", "aarch64") => ("aarch64-apple-darwin", ""),
        ("windows", "x86_64") => ("x86_64-pc-windows-msvc", ".exe"),
        ("windows", "aarch64") => ("aarch64-pc-windows-msvc", ".exe"),
        ("windows", "x86") => ("i686-pc-windows-msvc", ".exe"),
        ("freebsd", "x86_64") => ("x86_64-unknown-freebsd", ""),
        _ => return None,
    };
    Some(format!("teleia-{triple}{ext}"))
}

/// Pull the expected lowercase-hex digest for `asset` out of a
/// `sha256sum`-format manifest (`<hex>  <name>` per line). `None` if the
/// asset isn't listed or the line is malformed.
fn expected_sha(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut it = line.split_whitespace();
        let hex = it.next()?;
        let raw = it.next()?;
        // sha256sum prints a binary-mode `*` marker before the name; strip it.
        let name = raw.strip_prefix('*').unwrap_or(raw);
        (name == asset && hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hex.to_ascii_lowercase())
    })
}

/// Download the latest prebuilt for this platform, verify it against the
/// release's SHA256SUMS, and replace the running executable in place.
/// Backs `teleia --self-update`: upgrade an existing install without
/// re-running the shell bootstrap.
pub async fn self_update() -> anyhow::Result<()> {
    use anyhow::{bail, Context};

    let Some(asset) = asset_name() else {
        bail!(
            "no prebuilt teleia for {}/{} — build from source:\n  cargo install --git {REPO} teleia-cli",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
    };
    let base = format!("{REPO}/releases/latest/download");
    let client = reqwest::Client::builder()
        .user_agent(format!("teleia/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(120))
        .build()?;

    println!("fetching {asset} …");
    let bin: Vec<u8> = client
        .get(format!("{base}/{asset}"))
        .send()
        .await?
        .error_for_status()
        .context("download failed — is there a published release for this platform?")?
        .bytes()
        .await?
        .to_vec();

    // A valid teleia binary is multi-MB; anything tiny is an HTML error
    // page or a truncated download, never something to put on PATH.
    if bin.len() < 4096 {
        bail!(
            "downloaded {asset} is implausibly small ({} bytes) — aborting",
            bin.len()
        );
    }

    // Integrity: abort on a real mismatch; skip only when the release
    // PROVABLY has no SHA256SUMS (HTTP 404, older tags) or doesn't list
    // this asset — mirrors install.sh's verify_prebuilt. Any other
    // failure (network error, 5xx, truncated body) can't prove absence,
    // so it aborts rather than installing unverified.
    match client.get(format!("{base}/SHA256SUMS")).send().await {
        Ok(r) if r.status().is_success() => {
            let sums = r
                .text()
                .await
                .context("read SHA256SUMS — refusing to install unverified")?;
            match expected_sha(&sums, &asset) {
                Some(expected) => {
                    let actual = teleia_tools::sha256(&bin);
                    if actual != expected {
                        bail!(
                            "checksum mismatch for {asset}\n  expected: {expected}\n  actual:   {actual}\n\
                             refusing to install a binary that doesn't match the published SHA256SUMS"
                        );
                    }
                    println!("integrity: sha256 verified");
                }
                None => eprintln!("note: {asset} not in SHA256SUMS — skipping integrity check"),
            }
        }
        Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => {
            eprintln!("note: no SHA256SUMS for this release — skipping integrity check")
        }
        Ok(r) => bail!(
            "SHA256SUMS fetch failed (HTTP {}) — refusing to install unverified",
            r.status()
        ),
        Err(e) => {
            return Err(anyhow::Error::new(e)
                .context("SHA256SUMS fetch failed — refusing to install unverified"))
        }
    }

    // Swap the running executable. `self_replace` handles the platform
    // quirks — notably Windows can't unlink a live `.exe`, so it moves the
    // old one aside first.
    let tmp = std::env::temp_dir().join(format!("{asset}.new"));
    std::fs::write(&tmp, &bin).with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .context("chmod the downloaded binary")?;
    }
    let replaced = self_replace::self_replace(&tmp).context("replace the running teleia binary");
    // Clean up the staged download on every path, then surface any failure —
    // a swap error otherwise leaves a stale `.new` file behind.
    let _ = std::fs::remove_file(&tmp);
    replaced?;

    let where_ = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "teleia".into());
    println!("updated {where_} — run `teleia --version` to confirm");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare_basics() {
        assert!(version_is_newer("0.2.0", "0.1.9"));
        assert!(version_is_newer("1.0.0", "0.99.99"));
        assert!(version_is_newer("0.1.10", "0.1.9"));
        assert!(!version_is_newer("0.1.0", "0.1.0"));
        assert!(!version_is_newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn version_strips_pre_release_suffix() {
        // We only compare the leading numeric part — pre-release tags
        // don't push the version above the released one.
        assert!(!version_is_newer("0.1.0-rc1", "0.1.0"));
    }

    #[test]
    fn version_extra_segments() {
        assert!(version_is_newer("0.1.0.1", "0.1.0"));
    }

    #[test]
    fn asset_name_is_published_for_this_host() {
        // Every CI host (linux/macos/windows on x86_64/aarch64) ships a
        // prebuilt, so the mapping must resolve where the tests run.
        assert!(
            asset_name().is_some(),
            "no release asset mapping for this host"
        );
    }

    const A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    #[test]
    fn expected_sha_extracts_the_right_line() {
        let sums = format!(
            "{A}  teleia-x86_64-unknown-linux-musl\n{B}  teleia-x86_64-pc-windows-msvc.exe\n"
        );
        assert_eq!(
            expected_sha(&sums, "teleia-x86_64-unknown-linux-musl").as_deref(),
            Some(A)
        );
        assert_eq!(
            expected_sha(&sums, "teleia-x86_64-pc-windows-msvc.exe").as_deref(),
            Some(B)
        );
        assert_eq!(expected_sha(&sums, "teleia-not-a-target").as_deref(), None);
    }

    #[test]
    fn expected_sha_rejects_malformed_lines() {
        // Too-short digest, non-hex digest, and empty input all yield None
        // rather than a bogus "expected" checksum.
        assert_eq!(
            expected_sha(
                "deadbeef  teleia-x86_64-unknown-linux-musl",
                "teleia-x86_64-unknown-linux-musl"
            ),
            None
        );
        assert_eq!(
            expected_sha(
                "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz  teleia-x86_64-unknown-linux-musl",
                "teleia-x86_64-unknown-linux-musl"
            ),
            None
        );
        assert_eq!(expected_sha("", "teleia-x86_64-unknown-linux-musl"), None);
    }

    #[test]
    fn expected_sha_strips_binary_mode_marker() {
        // `sha256sum --binary` prints "<hex> *<name>".
        let sums = format!("{A} *teleia-x86_64-unknown-linux-musl\n");
        assert_eq!(
            expected_sha(&sums, "teleia-x86_64-unknown-linux-musl").as_deref(),
            Some(A)
        );
    }
}
