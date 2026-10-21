// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Version gate: a phone and this desktop sync only on the same protocol version; each paired device's last answer is kept for the settings screen.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::state::AppState;
use crate::sync::exchange::PROTOCOL_V;

/// The side whose protocol version is the lower one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outdated {
    Desktop,
    Phone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// The body is no hello from this device.
    Malformed,
    /// The phone speaks another version; nothing of the hello is read.
    Refused(Outdated),
}

#[derive(Deserialize)]
struct Head {
    v: u32,
    device_id: String,
}

/// Reads only `v` and `device_id` from a hello body.
pub fn check(body: &[u8], device_id: &str) -> Result<(), Gate> {
    let head: Head = serde_json::from_slice(body).map_err(|_| Gate::Malformed)?;
    if head.device_id != device_id || head.v == 0 {
        return Err(Gate::Malformed);
    }
    match head.v.cmp(&PROTOCOL_V) {
        std::cmp::Ordering::Equal => Ok(()),
        std::cmp::Ordering::Less => Err(Gate::Refused(Outdated::Phone)),
        std::cmp::Ordering::Greater => Err(Gate::Refused(Outdated::Desktop)),
    }
}

/// The body of a 426: the version this desktop speaks.
pub fn refusal_body() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "v": PROTOCOL_V })).unwrap_or_default()
}

static GATE_FILE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn path(app: &AppState) -> std::path::PathBuf {
    let name = crate::hardware::sanitized_machine_name();
    app.profile.root().join(format!("sync.{name}.gate.json"))
}

fn read(path: &std::path::Path) -> BTreeMap<String, Outdated> {
    let Ok(bytes) = syncsafe::read(path) else { return BTreeMap::new() };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn update(app: &AppState, change: impl FnOnce(&mut BTreeMap<String, Outdated>) -> bool) -> bool {
    let path = path(app);
    let _guard = GATE_FILE.lock().unwrap_or_else(|e| e.into_inner());
    let mut map = read(&path);
    if !change(&mut map) {
        return false;
    }
    let bytes = serde_json::to_vec(&map).expect("a map of sides serializes");
    if let Err(e) = syncsafe::write_atomic(&path, &bytes) {
        log::warn!("sync: version gate not recorded: {e}");
    }
    true
}

/// Every paired device a hello was last refused from, by device id.
pub fn load(app: &AppState) -> BTreeMap<String, Outdated> {
    read(&path(app))
}

/// Records the answer to `device_id`'s last hello. True when it differs from
/// the one before.
pub fn set(app: &AppState, device_id: &str, outdated: Option<Outdated>) -> bool {
    update(app, |map| match outdated {
        Some(o) => map.insert(device_id.to_string(), o) != Some(o),
        None => map.remove(device_id).is_some(),
    })
}

/// Forgets every device that is no longer paired.
pub fn retain_devices(app: &AppState, paired: &[String]) {
    update(app, |map| {
        let before = map.len();
        map.retain(|id, _| paired.contains(id));
        map.len() != before
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHONE: &str = "4a68f72f5a22110b2bfbe71b6588833c";

    fn hello(v: u32, device: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "v": v, "device_id": device, "sessions": [], "tombstones": [] })).unwrap()
    }

    #[test]
    fn only_the_same_version_passes_and_the_lower_side_is_named() {
        assert_eq!(check(&hello(PROTOCOL_V, PHONE), PHONE), Ok(()));
        assert_eq!(check(&hello(PROTOCOL_V - 1, PHONE), PHONE), Err(Gate::Refused(Outdated::Phone)));
        assert_eq!(check(&hello(1, PHONE), PHONE), Err(Gate::Refused(Outdated::Phone)));
        assert_eq!(check(&hello(PROTOCOL_V + 1, PHONE), PHONE), Err(Gate::Refused(Outdated::Desktop)));
    }

    #[test]
    fn the_gate_reads_nothing_past_the_version_and_device() {
        let odd = serde_json::to_vec(&serde_json::json!({ "v": 1, "device_id": PHONE, "sessions": "not a list" })).unwrap();
        assert_eq!(check(&odd, PHONE), Err(Gate::Refused(Outdated::Phone)));
        assert_eq!(check(&hello(PROTOCOL_V, "someone-else"), PHONE), Err(Gate::Malformed));
        assert_eq!(check(&hello(0, PHONE), PHONE), Err(Gate::Malformed));
        assert_eq!(check(b"{}", PHONE), Err(Gate::Malformed));
    }

    #[test]
    fn a_refusal_is_recorded_once_and_cleared_by_a_pass() {
        let td = tempfile::tempdir().unwrap();
        let app = AppState::new(crate::profile::ProfileDir::at(td.path()).unwrap());
        assert!(set(&app, PHONE, Some(Outdated::Phone)));
        assert!(!set(&app, PHONE, Some(Outdated::Phone)), "a retry is not a change");
        assert!(set(&app, PHONE, Some(Outdated::Desktop)));
        assert_eq!(load(&app).get(PHONE), Some(&Outdated::Desktop));
        assert!(set(&app, PHONE, None));
        assert!(!set(&app, PHONE, None));
        assert!(load(&app).is_empty());
    }

    #[test]
    fn the_refusal_body_names_this_desktops_version() {
        let v: serde_json::Value = serde_json::from_slice(&refusal_body()).unwrap();
        assert_eq!(v, serde_json::json!({ "v": PROTOCOL_V }));
    }
}
