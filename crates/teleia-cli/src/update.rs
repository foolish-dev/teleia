//! Update check against the GitHub releases API. Cheap, best-effort,
//! cached for the rest of the session — runs once in `main` and the
//! result is plumbed into the TUI so `/update` can re-display it.
//!
//! No auto-install. We just tell the user what's available + how to
//! upgrade; the binary they're running might've been put there by
//! `cargo install`, the install.sh one-liner, or a system package
//! manager, and any of those want a different command.

use serde::Deserialize;
use std::time::Duration;

const RELEASES_URL: &str = "https://api.github.com/repos/foolish-dev/teleia/releases/latest";

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
}
