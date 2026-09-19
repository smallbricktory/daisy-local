// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Mobile Sync commands: identity status, the pairing window, acceptance,
//! unpairing. The open window and the QR it shows are process state
//! (`AppState::sync_window`), and its lock serializes every read-modify-write
//! of the identity, which persists with the pairings through `sync::identity`.
use serde::Serialize;

use crate::error::{AppError, Result};
use crate::state::{AppState, VaultState};
use crate::sync::identity::{load_or_create, save, SyncIdentity};
use crate::sync::inbox::RefusalReason;
use crate::sync::pairing::{accept, pair_code_text, unpair, PairError, PairingWindow};
use crate::sync::server::inbox_of;

pub const SYNC_PORT: u16 = 7845;

#[derive(Debug, Clone, Serialize)]
pub struct PairedPhoneView {
    pub device_id: String,
    /// What this desktop calls the phone: its rename here, else `phone_name`.
    pub name: String,
    /// The name the phone paired with.
    pub phone_name: String,
    pub platform: String,
    pub paired_at_unix: i64,
    pub last_hello_unix: Option<i64>,
}

/// A phone session this desktop gave up on, for the settings screen to
/// name along with what would let it arrive.
#[derive(Debug, Clone, Serialize)]
pub struct RefusedView {
    pub session: String,
    pub device_id: String,
    pub reason: RefusalReason,
}

#[derive(Debug, Clone, Serialize)]
pub struct PairingView {
    pub qr_text: String,
    pub expires_unix: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncStatus {
    pub device_id: String,
    pub machine_name: String,
    pub paired: Vec<PairedPhoneView>,
    pub window: Option<PairingView>,
    /// The LAN name a phone keeps for this machine, and the port it uses.
    pub host: String,
    pub port: u16,
    /// Whether the server is up. False with no phone paired and no code
    /// showing, which is when nothing should be listening.
    pub listening: bool,
    /// Whether this machine can wake a phone: `on` when the radio is
    /// advertising, `off` when Bluetooth is turned off, `denied` when this
    /// Mac withheld Bluetooth from Daisy, `unsupported` on a desktop with no
    /// peripheral role. Only a Mac has one, so a phone elsewhere syncs
    /// whenever its app is open.
    pub bluetooth: String,
    /// Phone sessions this desktop cannot open, in id order.
    pub refused: Vec<RefusedView>,
    /// `windows`, `macos` or `linux`: the settings screen names the firewall
    /// prompt Windows raises the first time the listener binds.
    pub platform: &'static str,
}

/// The open pairing window with the QR it shows, built once when it opens.
pub struct OpenWindow {
    pub window: PairingWindow,
    pub view: PairingView,
}

/// The host a phone keeps for every hello after pairing: this machine's mDNS
/// name. A phone that pinned an IP is unreachable the day DHCP moves it, and
/// only a Mac can hand out a new one over Bluetooth. Falls back to a LAN IPv4
/// when the machine has no name of its own.
pub fn pairing_host() -> String {
    let name = crate::hardware::machine_name();
    let label = name.split('.').next().unwrap_or_default();
    if label.is_empty() || label == crate::hardware::UNKNOWN_MACHINE {
        return lan_ipv4().unwrap_or_else(|| "127.0.0.1".into());
    }
    format!("{label}.local")
}

/// This machine's first non-loopback IPv4 address.
fn lan_ipv4() -> Option<String> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .find_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some(v4.ip.to_string()),
            _ => None,
        })
}

/// True while a pairing code is on screen. The `/pair` route reads this
/// before it reads a body: it is the one route that answers without a client
/// certificate, and a closed window costs a stranger nothing to reach.
pub fn pairing_window_open(app: &AppState, now: i64) -> bool {
    app.sync_window.lock().unwrap().as_ref().is_some_and(|o| o.window.is_open(now))
}

fn open_view(app: &AppState, now: i64) -> Option<PairingView> {
    let guard = app.sync_window.lock().unwrap();
    guard.as_ref().filter(|o| o.window.is_open(now)).map(|o| o.view.clone())
}

fn status_of(app: &AppState, id: &SyncIdentity, listening: bool, bluetooth: &str) -> SyncStatus {
    let seen = crate::sync::hellos::load(app);
    SyncStatus {
        device_id: id.device_id.clone(),
        machine_name: crate::hardware::machine_name(),
        paired: id
            .paired
            .iter()
            .map(|p| PairedPhoneView {
                device_id: p.device_id.clone(),
                name: p.shown_name().to_string(),
                phone_name: p.name.clone(),
                platform: p.platform.clone(),
                paired_at_unix: p.paired_at_unix,
                last_hello_unix: seen.get(&p.device_id).copied(),
            })
            .collect(),
        window: open_view(app, crate::now_unix()),
        host: pairing_host(),
        port: SYNC_PORT,
        listening,
        bluetooth: bluetooth.to_string(),
        platform: std::env::consts::OS,
        refused: inbox_of(app)
            .refused_sessions()
            .into_iter()
            .map(|(session, r)| RefusedView { session, device_id: r.device_id, reason: r.reason })
            .collect(),
    }
}

/// The status the settings screen reads. `listening` and `bluetooth` are
/// what the caller measured: the Tauri layer knows whether the socket is
/// bound and what the radio is doing.
pub fn sync_status_impl(app: &AppState, vs: &VaultState, listening: bool, bluetooth: &str) -> Result<SyncStatus> {
    Ok(status_of(app, &load_or_create(app, vs)?, listening, bluetooth))
}

pub fn sync_pairing_begin_impl(app: &AppState, vs: &VaultState, host: String, now: i64) -> Result<PairingView> {
    let mut guard = app.sync_window.lock().unwrap();
    let id = load_or_create(app, vs)?;
    let window = PairingWindow::open(now);
    let qr_text = pair_code_text(&id, &window, &crate::hardware::machine_name(), host, SYNC_PORT)?;
    let view = PairingView { qr_text, expires_unix: window.expires_unix };
    *guard = Some(OpenWindow { window, view: view.clone() });
    Ok(view)
}

pub fn sync_pairing_cancel_impl(app: &AppState) -> Result<()> {
    *app.sync_window.lock().unwrap() = None;
    Ok(())
}

/// The `/pair` route. Returns the reply body and its signature; the window
/// closes the moment its secret is accepted, so one code pairs one phone.
pub fn sync_pairing_accept_impl(
    app: &AppState,
    vs: &VaultState,
    now: i64,
    body: &[u8],
    mac_b64: &str,
) -> std::result::Result<(Vec<u8>, String), PairError> {
    let mut guard = app.sync_window.lock().unwrap();
    let mut id = load_or_create(app, vs).map_err(|_| PairError::Internal)?;
    let open = guard.as_ref().ok_or(PairError::Closed)?;
    let (mut paired, reply, sig_b64) = accept(&id, &open.window, now, body, mac_b64, &crate::hardware::machine_name(), SYNC_PORT)?;
    *guard = None;
    // A device id is what this desktop authorizes by: whose tombstones it
    // honours, whose staged files a delete reaches, whose refusals a pairing
    // lifts. A phone claiming an id that is paired here under another signing
    // key would inherit all of it, so the pairing on record stands. The
    // remedy is the owner's: unpair that phone here, then show a fresh code.
    let held = id.paired.iter().find(|p| p.device_id == paired.device_id);
    if held.is_some_and(|p| p.ed25519_pub_b64 != paired.ed25519_pub_b64) {
        log::warn!("sync: {} is paired here under another key; it stays until it is unpaired", paired.device_id);
        return Err(PairError::DeviceTaken);
    }
    // A phone that pairs again may seal to this machine's key from here on;
    // what it offers next is tried afresh. It keeps the name it was given here.
    inbox_of(app).forget_refusals_of(&paired.device_id);
    paired.display_name = id.paired.iter().find(|p| p.device_id == paired.device_id).and_then(|p| p.display_name.clone());
    let again = id.paired.iter().any(|p| p.device_id == paired.device_id);
    let device_id = paired.device_id.clone();
    id.paired.retain(|p| p.device_id != paired.device_id);
    id.paired.push(paired);
    save(app, vs, &id).map_err(|_| PairError::Internal)?;
    log::info!("sync: paired {device_id}{}", if again { " again" } else { "" });
    Ok((reply, sig_b64))
}

/// Names a paired phone on this desktop. A blank name, or the phone's own,
/// clears the rename.
pub fn sync_rename_device_impl(
    app: &AppState,
    vs: &VaultState,
    device_id: &str,
    name: &str,
    listening: bool,
    bluetooth: &str,
) -> Result<SyncStatus> {
    let id = {
        let _guard = app.sync_window.lock().unwrap();
        let mut id = load_or_create(app, vs)?;
        let phone = id
            .paired
            .iter_mut()
            .find(|p| p.device_id == device_id)
            .ok_or_else(|| AppError::Config("That phone is no longer paired with this computer.".into()))?;
        let name: String = name.trim().chars().take(80).collect();
        phone.display_name = (!name.is_empty() && name != phone.name).then_some(name);
        save(app, vs, &id)?;
        id
    };
    Ok(status_of(app, &id, listening, bluetooth))
}

pub fn sync_unpair_impl(
    app: &AppState,
    vs: &VaultState,
    device_id: &str,
    listening: bool,
    bluetooth: &str,
) -> Result<SyncStatus> {
    let id = {
        let _guard = app.sync_window.lock().unwrap();
        let mut id = load_or_create(app, vs)?;
        if unpair(&mut id, device_id) {
            save(app, vs, &id)?;
            let paired: Vec<String> = id.paired.iter().map(|p| p.device_id.clone()).collect();
            crate::sync::signing::prune_nonces(app, &paired);
            crate::sync::hellos::retain_devices(app, &paired);
            inbox_of(app).forget_refusals_of(device_id);
            log::info!("sync: unpaired {device_id}");
        }
        id
    };
    Ok(status_of(app, &id, listening, bluetooth))
}
