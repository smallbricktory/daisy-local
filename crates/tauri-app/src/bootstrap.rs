// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Bootstrap record pointing at the user's profile directory. Stored
//! per-machine in the platform config dir.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bootstrap {
    pub schema_version: u32,
    pub profile_dir: PathBuf,
}

impl Bootstrap {
    pub const SCHEMA: u32 = 1;

    /// Path where bootstrap.json lives on this machine.
    pub fn path() -> std::io::Result<PathBuf> {
        Ok(license::config_dir()?.join("bootstrap.json"))
    }

    /// Loads bootstrap.json. Returns Ok(None) when the file is absent.
    pub fn load() -> std::io::Result<Option<Self>> {
        let p = Self::path()?;
        if !p.exists() {
            return Ok(None);
        }
        let bytes = syncsafe::read(&p)?;
        let b: Self = serde_json::from_slice(&bytes).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;
        Ok(Some(b))
    }

    /// Atomically write a bootstrap pointing at `profile_dir`.
    pub fn save(profile_dir: &Path) -> std::io::Result<()> {
        let p = Self::path()?;
        let b = Self {
            schema_version: Self::SCHEMA,
            profile_dir: profile_dir.to_path_buf(),
        };
        let bytes = serde_json::to_vec_pretty(&b).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;
        syncsafe::write_atomic(&p, &bytes)?;
        Ok(())
    }
}

/// Recording-consent acknowledgement, stored per-machine in the config dir.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Consent {
    pub version: u32,
    pub accepted_at_unix_seconds: i64,
}

impl Consent {
    /// A stored version below CURRENT re-prompts the user.
    pub const CURRENT: u32 = 1;

    pub fn path() -> std::io::Result<PathBuf> {
        Ok(license::config_dir()?.join("consent.json"))
    }

    /// True if this machine has accepted the current consent version.
    pub fn is_accepted() -> bool {
        let Ok(p) = Self::path() else { return false };
        let Ok(bytes) = syncsafe::read(&p) else { return false };
        serde_json::from_slice::<Self>(&bytes)
            .map(|c| c.version >= Self::CURRENT)
            .unwrap_or(false)
    }

    /// Record acceptance of the current consent version.
    pub fn accept() -> std::io::Result<()> {
        let p = Self::path()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let c = Self { version: Self::CURRENT, accepted_at_unix_seconds: now };
        let bytes = serde_json::to_vec_pretty(&c).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;
        syncsafe::write_atomic(&p, &bytes)?;
        Ok(())
    }
}

/// Legal-terms acceptance (Terms of Service + Privacy Policy), stored
/// per-machine in the config dir. Tracks each document's version
/// independently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Eula {
    pub tos_version: u32,
    pub privacy_version: u32,
    pub accepted_at_unix_seconds: i64,
}

impl Eula {
    /// A stored version below the corresponding CURRENT re-prompts the user.
    pub const TOS_CURRENT: u32 = 1;
    pub const PRIVACY_CURRENT: u32 = 1;

    pub fn path() -> std::io::Result<PathBuf> {
        Ok(license::config_dir()?.join("eula.json"))
    }

    /// True when both stored versions are current.
    fn versions_current(tos_version: u32, privacy_version: u32) -> bool {
        tos_version >= Self::TOS_CURRENT && privacy_version >= Self::PRIVACY_CURRENT
    }

    /// True only if both the accepted ToS and Privacy versions are current.
    pub fn is_accepted() -> bool {
        let Ok(p) = Self::path() else { return false };
        let Ok(bytes) = syncsafe::read(&p) else { return false };
        serde_json::from_slice::<Self>(&bytes)
            .map(|e| Self::versions_current(e.tos_version, e.privacy_version))
            .unwrap_or(false)
    }

    /// Record acceptance of the current ToS + Privacy versions.
    pub fn accept() -> std::io::Result<()> {
        let p = Self::path()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let e = Self {
            tos_version: Self::TOS_CURRENT,
            privacy_version: Self::PRIVACY_CURRENT,
            accepted_at_unix_seconds: now,
        };
        let bytes = serde_json::to_vec_pretty(&e).map_err(|err| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string())
        })?;
        syncsafe::write_atomic(&p, &bytes)?;
        Ok(())
    }
}

/// First-run wizard completion, stored per-machine in the config dir. Set
/// when the wizard's finish button is pressed. Read by the record hotkey and
/// the meeting-started prompt so neither can act while the wizard is still
/// on screen.
pub struct Onboarding;

impl Onboarding {
    fn path() -> std::io::Result<PathBuf> {
        Ok(license::config_dir()?.join("onboarding_complete"))
    }

    pub fn is_complete() -> bool {
        Self::path().map(|p| p.exists()).unwrap_or(false)
    }

    /// Idempotent: a no-op once the marker is already there.
    pub fn mark_complete() -> std::io::Result<()> {
        let p = Self::path()?;
        if p.exists() {
            return Ok(());
        }
        syncsafe::write_atomic(&p, b"1")
    }
}

/// True once EULA and consent are accepted, first-run setup has finished,
/// and the vault is unlocked — the point at which a feature that assumes a
/// finished install (the record hotkey, the meeting-started prompt) may act.
pub fn first_run_complete(
    eula_accepted: bool,
    consent_accepted: bool,
    onboarding_complete: bool,
    vault_unlocked: bool,
) -> bool {
    eula_accepted && consent_accepted && onboarding_complete && vault_unlocked
}

#[cfg(test)]
mod onboarding_gate_tests {
    use super::first_run_complete;

    #[test]
    fn all_four_conditions_must_hold() {
        assert!(first_run_complete(true, true, true, true));
        assert!(!first_run_complete(false, true, true, true), "eula");
        assert!(!first_run_complete(true, false, true, true), "consent");
        assert!(!first_run_complete(true, true, false, true), "onboarding incomplete");
        assert!(!first_run_complete(true, true, true, false), "vault locked");
    }
}

#[cfg(test)]
mod eula_tests {
    use super::*;

    #[test]
    fn current_versions_accepted() {
        assert!(Eula::versions_current(Eula::TOS_CURRENT, Eula::PRIVACY_CURRENT));
    }

    #[test]
    fn higher_versions_accepted() {
        assert!(Eula::versions_current(Eula::TOS_CURRENT + 5, Eula::PRIVACY_CURRENT + 5));
    }

    #[test]
    fn stale_tos_reprompts() {
        if Eula::TOS_CURRENT > 0 {
            assert!(!Eula::versions_current(Eula::TOS_CURRENT - 1, Eula::PRIVACY_CURRENT));
        }
    }

    #[test]
    fn stale_privacy_reprompts() {
        if Eula::PRIVACY_CURRENT > 0 {
            assert!(!Eula::versions_current(Eula::TOS_CURRENT, Eula::PRIVACY_CURRENT - 1));
        }
    }
}
