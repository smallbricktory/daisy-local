// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Per-machine Bluetooth codec preferences: which headsets need the CVSD
//! downshift. Keyed by normalized MAC stem; single writer (this machine),
//! so shared-profile sync cannot conflict on it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn prefs_path(profile_root: &Path) -> PathBuf {
    let name = crate::hardware::sanitized_machine_name();
    profile_root.join(format!("bt_codec_prefs.{name}.json"))
}

fn load(profile_root: &Path) -> HashMap<String, String> {
    syncsafe::read_to_string(prefs_path(profile_root))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// True when this device previously needed the CVSD downshift here.
pub fn prefers_cvsd(profile_root: &Path, mac_stem: &str) -> bool {
    load(profile_root).get(mac_stem).map(String::as_str) == Some("cvsd")
}

/// [`prefers_cvsd`] keyed by a capture node name; false for non-BT nodes.
/// The one lookup every `prepare_capture` call site needs.
pub fn prefers_cvsd_for(profile_root: &Path, node_name: &str) -> bool {
    audio_engine::bt_profile::bluez_address(node_name)
        .map(|m| prefers_cvsd(profile_root, &m))
        .unwrap_or(false)
}

/// Record that this device needed the CVSD downshift on this machine.
pub fn remember_cvsd(profile_root: &Path, mac_stem: &str) {
    let mut m = load(profile_root);
    if m.insert(mac_stem.to_string(), "cvsd".to_string()) != Some("cvsd".to_string()) {
        if let Ok(s) = serde_json::to_string_pretty(&m) {
            if let Err(e) = syncsafe::write_atomic(prefs_path(profile_root), s) {
                log::warn!("bt prefs: write failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_idempotent_write() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!prefers_cvsd(dir.path(), "1A_2B_3C_4D_5E_6F"));
        remember_cvsd(dir.path(), "1A_2B_3C_4D_5E_6F");
        assert!(prefers_cvsd(dir.path(), "1A_2B_3C_4D_5E_6F"));
        assert!(!prefers_cvsd(dir.path(), "AA_BB_CC_DD_EE_FF"));
        remember_cvsd(dir.path(), "1A_2B_3C_4D_5E_6F");
        assert!(prefers_cvsd(dir.path(), "1A_2B_3C_4D_5E_6F"));
    }
}
