// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Tauri commands for bootstrap-layer operations.

use crate::bootstrap::Bootstrap;
use crate::error::{AppError, Result};
use directories::ProjectDirs;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Serialize)]
pub struct BootstrapStatus {
    pub has_bootstrap: bool,
    pub profile_dir: Option<PathBuf>,
    /// The platform-default profile dir.
    pub platform_default: PathBuf,
    /// Set when `DAISY_PROFILE_DIR` is present: the directory actually in
    /// use this session, overriding the saved location above.
    pub env_override: Option<PathBuf>,
}

pub fn bootstrap_status_impl() -> Result<BootstrapStatus> {
    let bootstrap = Bootstrap::load().map_err(|e| AppError::Io(e.to_string()))?;
    let platform_default = ProjectDirs::from("ai", "daisy", "Daisy")
        .map(|pd| pd.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("daisy"));
    let env_override = env_profile_override();
    Ok(BootstrapStatus {
        has_bootstrap: bootstrap.is_some(),
        profile_dir: bootstrap.map(|b| crate::profile::expand_home(&b.profile_dir)),
        platform_default,
        env_override,
    })
}

pub fn bootstrap_set_impl(profile_dir: PathBuf) -> Result<()> {
    let profile_dir = crate::profile::expand_home(&profile_dir);
    syncsafe::create_dir_all(&profile_dir).map_err(|e| {
        AppError::Io(format!("create {}: {e}", profile_dir.display()))
    })?;
    Bootstrap::save(&profile_dir).map_err(|e| AppError::Io(e.to_string()))?;
    Ok(())
}

/// `bootstrap_set` for a frontend-supplied path: accepts the folder picked for
/// a new profile, the platform default, or the `DAISY_PROFILE_DIR` override.
pub fn bootstrap_set_picked_impl(
    picks: &crate::commands::files::DialogPicks,
    profile_dir: PathBuf,
) -> Result<()> {
    let status = bootstrap_status_impl()?;
    let backend_known = profile_dir == status.platform_default
        || status.env_override.as_deref() == Some(profile_dir.as_path());
    if !backend_known {
        picks.check_dir(crate::commands::files::PURPOSE_PROFILE_NEW, &profile_dir)?;
    }
    bootstrap_set_impl(profile_dir)?;
    picks.clear_dir(crate::commands::files::PURPOSE_PROFILE_NEW);
    Ok(())
}

/// The `DAISY_PROFILE_DIR` override, if set non-empty.
fn env_profile_override() -> Option<PathBuf> {
    std::env::var("DAISY_PROFILE_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| crate::profile::expand_home(std::path::Path::new(&s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_set_refuses_a_folder_that_was_not_picked() {
        let picks = crate::commands::files::DialogPicks::default();
        let err = bootstrap_set_picked_impl(&picks, PathBuf::from("/tmp/daisy-not-picked")).unwrap_err();
        assert!(err.to_string().contains("folder picker"), "{err}");
    }

    #[test]
    fn env_override_reflects_the_variable() {
        // Serialized inside one test: the var is process-global.
        std::env::remove_var("DAISY_PROFILE_DIR");
        assert_eq!(env_profile_override(), None);
        std::env::set_var("DAISY_PROFILE_DIR", "");
        assert_eq!(env_profile_override(), None, "empty value is no override");
        std::env::set_var("DAISY_PROFILE_DIR", "/tmp/daisy-prof");
        assert_eq!(env_profile_override(), Some(PathBuf::from("/tmp/daisy-prof")));
        std::env::remove_var("DAISY_PROFILE_DIR");
    }
}
