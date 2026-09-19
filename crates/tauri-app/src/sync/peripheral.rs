// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The Bluetooth side of sync: one advertised service per paired phone,
//! carrying the address this desktop is reachable at and the wake that brings
//! a phone back to collect. Section 9 of the sync protocol; the radio itself
//! is the `ble` crate, macOS only.

use std::sync::Mutex;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use crate::state::{AppState, VaultState};
use crate::sync::identity::PairedPhone;

/// The running peripheral, or nothing on a desktop with no peripheral radio.
#[derive(Default)]
pub struct PeripheralHandle {
    inner: Mutex<Option<ble::Peripheral>>,
    /// What the running peripheral was last given: the address every paired
    /// phone reads, and who was paired when it went out. Each address is
    /// sealed under a fresh nonce, so publishing again for an unchanged
    /// address hands every phone a new ciphertext for what it already has.
    published: Mutex<Option<(String, u16, Vec<String>)>>,
}

/// The pairing key a phone's payloads are sealed under.
fn pairing_key(phone: &PairedPhone) -> Option<[u8; 32]> {
    B64.decode(&phone.pairing_key_b64).ok()?.try_into().ok()
}

/// Publishes every pairing and the address they reach this machine at. A
/// desktop with no peripheral radio logs once and serves the LAN alone.
pub fn apply(handle: &PeripheralHandle, app: &AppState, vs: &VaultState) {
    let Ok(id) = crate::sync::identity::load_or_create(app, vs) else { return };
    let mut running = handle.inner.lock().unwrap();
    if id.paired.is_empty() {
        if running.take().is_some() {
            log::info!("sync: bluetooth stopped, no phone paired");
        }
        *handle.published.lock().unwrap() = None;
        return;
    }
    let host = crate::commands::sync::pairing_host();
    let addressed: Vec<String> = id.paired.iter().map(|p| p.device_id.clone()).collect();
    let fresh = running.is_none();
    if !fresh && *handle.published.lock().unwrap() == Some((host.clone(), crate::commands::sync::SYNC_PORT, addressed.clone())) {
        return;
    }
    if fresh {
        match ble::Peripheral::start() {
            Ok(p) => *running = Some(p),
            Err(ble::Error::Unsupported) => {
                log::info!("sync: no bluetooth peripheral here; phones reach this machine over the LAN");
                return;
            }
            Err(e) => {
                log::warn!("sync: bluetooth not started: {e}");
                return;
            }
        }
    }
    let Some(peripheral) = running.as_ref() else { return };
    let mut every_phone = true;
    for phone in &id.paired {
        let Some(key) = pairing_key(phone) else {
            log::warn!("sync: {} has no pairing key; no bluetooth for it", phone.device_id);
            every_phone = false;
            continue;
        };
        let Some(address) = crate::sync::wake::address_payload(&key, &host, crate::commands::sync::SYNC_PORT) else {
            every_phone = false;
            continue;
        };
        if peripheral.set_address(&phone.service_uuid, &address) {
            continue;
        }
        if let Err(e) = peripheral.add_service(&phone.service_uuid, &phone.wake_uuid, &phone.address_uuid, &address) {
            log::warn!("sync: bluetooth service for {}: {e}", phone.device_id);
            every_phone = false;
        }
    }
    // A phone left without its address is tried again at the next call.
    *handle.published.lock().unwrap() = every_phone.then_some((host, crate::commands::sync::SYNC_PORT, addressed));
}

/// Wakes one phone: it has ten seconds of its own once it connects, so this
/// is sent when there is something for it to collect. False when this desktop
/// has no radio, no such pairing, or nothing is listening on it.
pub fn wake(handle: &PeripheralHandle, app: &AppState, vs: &VaultState, device_id: &str) -> bool {
    let Ok(id) = crate::sync::identity::load_or_create(app, vs) else { return false };
    let Some(phone) = id.paired.iter().find(|p| p.device_id == device_id) else { return false };
    let Some(key) = pairing_key(phone) else { return false };
    let Some(payload) = crate::sync::wake::wake_payload(&key) else { return false };
    let running = handle.inner.lock().unwrap();
    let Some(peripheral) = running.as_ref() else { return false };
    if peripheral.subscribers(&phone.service_uuid) == 0 {
        log::info!("sync: {device_id} is not listening; it collects at its next hello");
        return false;
    }
    peripheral.notify(&phone.service_uuid, &payload)
}

/// What the settings screen shows: `on` while the radio is advertising,
/// `off` when Bluetooth is turned off or the radio is not running, `denied`
/// when this Mac refused Daisy the radio, and `unsupported` where Daisy has
/// no peripheral role at all.
pub fn radio_state(handle: &PeripheralHandle) -> &'static str {
    let running = handle.inner.lock().unwrap();
    match running.as_ref() {
        Some(p) if p.is_advertising() => "on",
        Some(p) => match p.state() {
            ble::State::Unauthorized => "denied",
            ble::State::Unsupported => "unsupported",
            _ => "off",
        },
        None if ble::Peripheral::supported() => "off",
        None => "unsupported",
    }
}

/// Stops advertising, which is what a locked vault and a reset both want.
pub fn stop(handle: &PeripheralHandle) {
    if handle.inner.lock().unwrap().take().is_some() {
        log::info!("sync: bluetooth stopped");
    }
    *handle.published.lock().unwrap() = None;
}
