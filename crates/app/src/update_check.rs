//! Lightweight update check.
//!
//! Fetches the `latest.json` published with each release and compares its version to
//! this build's `CARGO_PKG_VERSION`. Cheap and side-effect-free, so it can run in the
//! background at startup.

use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

/// Served from mayorana.ch alongside the builds it describes, so update checks do not
/// depend on the source repository staying publicly readable.
const LATEST_URL: &str = "https://mayorana.ch/downloads/splitter/latest/latest.json";
/// Fallback when `latest.json` has no build for this OS (e.g. an Intel Mac: only Apple
/// Silicon is built), so the button leads somewhere to pick one instead of to a 404.
const RELEASES_URL: &str = "https://mayorana.ch/en/apps";

/// Lets the download logs tell an existing user updating from a new install, and which
/// versions are still in use.
const USER_AGENT: &str = concat!("splitter/", env!("CARGO_PKG_VERSION"), " (updater)");

#[derive(Deserialize)]
struct LatestJson {
    version: String,
    platforms: Platforms,
}

#[derive(Deserialize)]
struct Platforms {
    macos: HashMap<String, Artifact>,
    windows: HashMap<String, Artifact>,
    linux: HashMap<String, Artifact>,
}

#[derive(Deserialize)]
struct Artifact {
    url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpdateInfo {
    pub latest_version: String,
    /// Direct link to this OS's build, so the banner downloads the binary itself. The
    /// download happens in the user's browser, so there is no checksum to verify here.
    pub download_url: String,
}

/// `Some` if a newer release is published. Any network or parse failure is `None`;
/// setting `DISABLE_UPDATE_CHECK` turns the check off.
pub async fn check() -> Option<UpdateInfo> {
    if std::env::var_os("DISABLE_UPDATE_CHECK").is_some() {
        return None;
    }
    let body = tokio::task::spawn_blocking(fetch).await.ok()??;
    let latest: LatestJson = serde_json::from_str(&body).ok()?;
    is_newer(&latest.version, env!("CARGO_PKG_VERSION"))
        .then(|| UpdateInfo { download_url: platform_url(&latest.platforms), latest_version: latest.version })
}

fn fetch() -> Option<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5))).build().into();
    agent.get(LATEST_URL).header("User-Agent", USER_AGENT).call().ok()?.body_mut().read_to_string().ok()
}

/// The artifact published for this OS, or the landing page when there is none.
fn platform_url(platforms: &Platforms) -> String {
    let by_os = match std::env::consts::OS {
        "macos" => &platforms.macos,
        "windows" => &platforms.windows,
        "linux" => &platforms.linux,
        _ => return RELEASES_URL.to_string(),
    };
    // One build per OS today, keyed by format ("dmg", "tarball"…), so any entry will do.
    by_os
        .values()
        .next()
        .map(|a| a.url.as_str())
        .filter(|u| !u.is_empty())
        // Marks the hit as an update in the download logs: the browser fetches the file,
        // not this app, so the User-Agent above doesn't reach that request.
        .map(|u| format!("{u}?src=updater"))
        .unwrap_or_else(|| RELEASES_URL.to_string())
}

fn is_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Option<(u32, u32, u32)> {
        let mut parts = s.trim_start_matches('v').split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.split(['-', '+']).next()?.parse().ok()?;
        Some((major, minor, patch))
    };
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if a > b)
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("v0.2.0", "0.1.2"));
        assert!(!is_newer("0.1.2", "0.1.2"));
        assert!(!is_newer("0.1.1", "0.1.2"));
        assert!(!is_newer("garbage", "0.1.2"));
    }
}
