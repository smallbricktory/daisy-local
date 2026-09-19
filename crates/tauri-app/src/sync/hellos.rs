// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! When each paired phone was last heard from, which is what the settings
//! screen shows beside it. Kept beside the nonce sets and named for the
//! machine: a hello another desktop took says nothing here.
//!
//! A timestamp is not a secret, and every hello would otherwise re-seal the
//! identity — an Argon2 derivation on the phone's ten-second window.

use std::collections::BTreeMap;

use crate::state::AppState;

/// Held across the read-modify-write, the same way the nonce file is: a phone
/// opens several connections at once and each is served on its own task.
static HELLO_FILE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn path(app: &AppState) -> std::path::PathBuf {
    let name = crate::hardware::sanitized_machine_name();
    app.profile.root().join(format!("sync.{name}.hellos.json"))
}

fn read(path: &std::path::Path) -> BTreeMap<String, i64> {
    let Ok(bytes) = syncsafe::read(path) else { return BTreeMap::new() };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Reads the file, applies `change`, writes it back, all under the lock.
fn update(app: &AppState, change: impl FnOnce(&mut BTreeMap<String, i64>)) -> std::io::Result<()> {
    let path = path(app);
    let _guard = HELLO_FILE.lock().unwrap_or_else(|e| e.into_inner());
    let mut seen = read(&path);
    change(&mut seen);
    let bytes = serde_json::to_vec(&seen).expect("a map of times serializes");
    syncsafe::write_atomic(&path, &bytes)
}

/// Every device this machine has heard from, by device id. A missing or
/// unreadable file reads as none: the screen then shows a phone with no time
/// beside it, which is what it shows before the first hello anyway.
pub fn load(app: &AppState) -> BTreeMap<String, i64> {
    read(&path(app))
}

/// Stamps now against `device_id`.
pub fn record(app: &AppState, device_id: &str) {
    let now = crate::now_unix();
    if let Err(e) = update(app, |seen| {
        seen.insert(device_id.to_string(), now);
    }) {
        log::warn!("sync: hello from {device_id} not recorded: {e}");
    }
}

/// Forgets every device that is no longer paired.
pub fn retain_devices(app: &AppState, paired: &[String]) {
    if let Err(e) = update(app, |seen| seen.retain(|id, _| paired.contains(id))) {
        log::warn!("sync: hello times not pruned: {e}");
    }
}
