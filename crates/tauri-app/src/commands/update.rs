//! Notify-only update check. Fetches a small `latest.json` manifest from the
//! Daisy website and reports whether a newer build is published. Never
//! downloads or installs anything.

use crate::error::{AppError, Result};
use serde::Serialize;

/// Where per-OS manifests live. Each is the newest published build for that OS;
/// shape: `{ "version": "...", "notes": "...", "url": "https://.../download" }`.
/// Extra fields are ignored.
const UPDATE_BASE: &str = "https://daisy.smbr.app/updates";

fn os_segment() -> &'static str {
    if cfg!(target_os = "windows") {
        "win"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

#[derive(Debug, Serialize)]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    pub update_available: bool,
    pub notes: String,
    /// Where to send the user to download (changelog or direct link).
    pub url: Option<String>,
}

/// Blocking HTTP fetch + parse of the update manifest.
pub fn check_for_update_impl() -> Result<UpdateInfo> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let url = format!("{}/{}/latest.json", UPDATE_BASE, os_segment());

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("daisy/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| AppError::Provider(format!("update client: {e}")))?;
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| AppError::Provider(format!("update check: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Provider(format!(
            "update check: HTTP {}",
            resp.status().as_u16()
        )));
    }
    let v: serde_json::Value = resp
        .json()
        .map_err(|e| AppError::Provider(format!("update manifest parse: {e}")))?;

    let latest = v
        .get("version")
        .and_then(|x| x.as_str())
        .ok_or_else(|| AppError::Provider("update manifest missing `version`".into()))?
        .trim()
        .to_string();
    let notes = v
        .get("notes")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    // Prefer a top-level `url`; fall back to the first Tauri-style platform url.
    let download_url = v
        .get("url")
        .and_then(|x| x.as_str())
        .map(String::from)
        .or_else(|| {
            v.get("platforms")
                .and_then(|p| p.as_object())
                .and_then(|m| m.values().next())
                .and_then(|plat| plat.get("url"))
                .and_then(|u| u.as_str())
                .map(String::from)
        });

    let released_at_unix = v.get("released_at_unix").and_then(|x| x.as_i64());
    let built_at_unix: i64 = env!("DAISY_BUILD_UNIX").parse().unwrap_or(0);
    let update_available =
        !latest.is_empty() && is_newer(released_at_unix, built_at_unix, &latest, &current);

    Ok(UpdateInfo {
        current,
        latest,
        update_available,
        notes,
        url: download_url,
    })
}

/// Update decision. With a manifest `released_at_unix`, a release is newer
/// when it was published after this binary was built and carries a different
/// version string. Without the field, falls back to semver comparison.
fn is_newer(released_at_unix: Option<i64>, built_at_unix: i64, latest: &str, current: &str) -> bool {
    match released_at_unix {
        Some(r) => r > built_at_unix && latest != current,
        None => parse_version(latest) > parse_version(current),
    }
}

/// Parse a semver `MAJOR.MINOR.PATCH` version into a sortable tuple. On parse
/// failure returns `(0, 0, 0)`, which compares as older than any valid
/// version.
fn parse_version(s: &str) -> (u32, u32, u32) {
    let mut parts = s.split('.');
    let a = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let b = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let c = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (a, b, c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_handles_semver() {
        assert_eq!(parse_version("2.0.0"), (2, 0, 0));
        assert_eq!(parse_version("2.1.0"), (2, 1, 0));
        assert_eq!(parse_version("2.0.19"), (2, 0, 19));
    }

    #[test]
    fn ordering_is_integer_not_string() {
        // 19 > 9 as integers even though "19" < "9" lexically.
        assert!(parse_version("2.0.19") > parse_version("2.0.9"));
        assert!(parse_version("2.1.0") > parse_version("2.0.99"));
        assert!(parse_version("3.0.0") > parse_version("2.9.9"));
    }

    #[test]
    fn malformed_compares_as_zero() {
        assert!(parse_version("2.0.0") > parse_version("garbage"));
    }

    #[test]
    fn date_field_decides_when_present() {
        // Re-baseline case: semver went backwards but the release is newer.
        assert!(is_newer(Some(2_000), 1_000, "2.0.0", "2.4.3"));
        // Older-dated release never prompts.
        assert!(!is_newer(Some(500), 1_000, "9.9.9", "2.0.0"));
        // Same version string never prompts, whatever the dates say
        // (prevents a prompt loop on rebuilds of the shipped version).
        assert!(!is_newer(Some(2_000), 1_000, "2.0.0", "2.0.0"));
    }

    #[test]
    fn missing_date_falls_back_to_semver() {
        assert!(is_newer(None, 0, "2.0.19", "2.0.9"));
        assert!(!is_newer(None, 0, "2.0.0", "2.4.3"));
        assert!(!is_newer(None, 0, "2.0.0", "2.0.0"));
    }
}
