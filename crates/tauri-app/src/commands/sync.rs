// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Mobile Sync commands: identity status, the pairing key, the exchange and
//! acceptance, unpairing. The live key and the QR it shows are process state
//! (`AppState::sync_window`), and its lock serializes every read-modify-write
//! of the identity, which persists with the pairings through `sync::identity`.
use serde::Serialize;
use zeroize::{Zeroize, Zeroizing};

use crate::error::{AppError, Result};
use crate::state::{AppState, VaultState};
use crate::sync::identity::{load_or_create, save, SyncIdentity};
use crate::sync::inbox::RefusalReason;
use crate::sync::pairing::{accept, end_session, key_text, qr_text, session_secret, start, unpair, PairError, PairingKey, StartRandom};
use crate::sync::server::inbox_of;

pub const SYNC_PORT: u16 = 7845;

/// The port the sync listener binds and every pairing, advertisement and
/// firewall hint names: [`SYNC_PORT`]. A `ui-test` build takes a non-zero
/// `DAISY_SYNC_PORT` instead, read once per process.
pub fn sync_port() -> u16 {
    #[cfg(feature = "ui-test")]
    {
        static PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
        *PORT.get_or_init(|| {
            std::env::var("DAISY_SYNC_PORT").ok().and_then(|v| v.parse().ok()).filter(|&p| p != 0).unwrap_or(SYNC_PORT)
        })
    }
    #[cfg(not(feature = "ui-test"))]
    SYNC_PORT
}

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
    /// Set while this device's hellos are refused at the version gate: the
    /// side, `desktop` or `phone`, whose protocol version is the lower one.
    pub out_of_date: Option<crate::sync::gate::Outdated>,
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
    /// The key as the user reads it out: four groups of four.
    pub key_text: String,
    pub expires_unix: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncStatus {
    pub device_id: String,
    pub machine_name: String,
    pub paired: Vec<PairedPhoneView>,
    pub window: Option<PairingView>,
    /// The LAN name a phone keeps for this machine, and the port it uses.
    /// `None` when this machine has no name or address a phone accepts.
    pub host: Option<String>,
    pub port: u16,
    /// Whether the server is up. False with no phone paired and no code
    /// showing, which is when nothing should be listening.
    pub listening: bool,
    /// Whether this machine can wake a phone: `on`, `off`, `denied` or `unsupported`.
    pub bluetooth: String,
    /// Phone sessions this desktop cannot open, in id order.
    pub refused: Vec<RefusedView>,
    /// `windows`, `macos` or `linux`: the settings screen names the firewall
    /// prompt Windows raises the first time the listener binds.
    pub platform: &'static str,
}

/// The pairing key on screen with the QR it shows, built once when it is
/// minted, and the network advertisement that lets a typed key find it.
/// Dropping it ends the key, zeroes the key and the text that spells it, and
/// withdraws the advertisement.
pub struct OpenWindow {
    pub key: PairingKey,
    pub view: PairingView,
    pub advert: Option<crate::sync::advert::Advert>,
}

impl Drop for OpenWindow {
    fn drop(&mut self) {
        self.view.qr_text.zeroize();
        self.view.key_text.zeroize();
    }
}

/// What the pairing screen holds: a key, or the fact that one was shown and
/// ran out. The expired state holds no key, and the pairing routes answer it
/// with `expired`.
pub enum SyncWindow {
    Live(OpenWindow),
    Expired,
}

/// The window a pairing route works against: `closed` with no key shown,
/// `expired` once the shown one ran out.
fn live_window(window: &mut Option<SyncWindow>) -> std::result::Result<&mut OpenWindow, PairError> {
    match window {
        Some(SyncWindow::Live(open)) => Ok(open),
        Some(SyncWindow::Expired) => Err(PairError::Expired),
        None => Err(PairError::Closed),
    }
}

/// What `sync_pairing_begin` answers when there is no host to put in a key.
pub const NO_NETWORK: &str = "Connect this computer to Wi-Fi or a network to pair a phone.";

/// The host a phone keeps for every hello after pairing: this machine's mDNS name, else a LAN IPv4 address, else `None`.
pub fn pairing_host() -> Option<String> {
    let name = crate::hardware::machine_name();
    let label = name.split('.').next().unwrap_or_default();
    let local = format!("{label}.local");
    if label != crate::hardware::UNKNOWN_MACHINE && is_ldh_host(&local) {
        return Some(local);
    }
    lan_ipv4()
}

/// True for a host name a phone accepts: dot-separated labels of 1-63
/// ASCII letters, digits and hyphens, none starting or ending with a hyphen,
/// at most 253 characters in all. A host of only digits and dots has to be
/// a dotted-quad IPv4 address.
pub fn is_ldh_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let labels = host.split('.').all(|l| {
        (1..=63).contains(&l.len())
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    });
    if !labels {
        return false;
    }
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return host.parse::<std::net::Ipv4Addr>().is_ok();
    }
    true
}

/// This machine's first non-loopback IPv4 address.
pub(crate) fn lan_ipv4() -> Option<String> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .find_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some(v4.ip.to_string()),
            _ => None,
        })
}

/// True while a pairing key is live. The pairing routes read this before they read a body.
pub fn pairing_key_live(app: &AppState, now: i64) -> bool {
    matches!(&*app.sync_window.lock().unwrap(), Some(SyncWindow::Live(o)) if o.key.is_live(now))
}

/// True while a key is on screen, live or past its ten minutes.
pub fn pairing_key_shown(app: &AppState) -> bool {
    app.sync_window.lock().unwrap().is_some()
}

fn open_view(app: &AppState, now: i64) -> Option<PairingView> {
    match &*app.sync_window.lock().unwrap() {
        Some(SyncWindow::Live(o)) if o.key.is_live(now) => Some(o.view.clone()),
        _ => None,
    }
}

fn status_of(app: &AppState, id: &SyncIdentity, listening: bool, bluetooth: &str) -> SyncStatus {
    let seen = crate::sync::hellos::load(app);
    let gated = crate::sync::gate::load(app);
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
                out_of_date: gated.get(&p.device_id).copied(),
            })
            .collect(),
        window: open_view(app, crate::now_unix()),
        host: pairing_host(),
        port: sync_port(),
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
    load_or_create(app, vs)?;
    let key = PairingKey::mint(host, now);
    let text = Zeroizing::new(key_text(&key.key));
    let view = PairingView {
        qr_text: qr_text(&key.key, &key.host, sync_port()),
        key_text: [&text[..4], &text[4..8], &text[8..12], &text[12..]].join("-"),
        expires_unix: key.expires_unix,
    };
    *guard = Some(SyncWindow::Live(OpenWindow { key, view: view.clone(), advert: None }));
    Ok(view)
}

pub fn sync_pairing_cancel_impl(app: &AppState) -> Result<()> {
    *app.sync_window.lock().unwrap() = None;
    Ok(())
}

/// Announces the live key on the local network so a phone given the typed
/// key can find this machine. The advertisement lives and dies with the key.
pub fn advertise_live_key(app: &AppState, vs: &VaultState) -> Result<()> {
    let device_id = load_or_create(app, vs)?.device_id;
    let Some((key, host)) = (match &*app.sync_window.lock().unwrap() {
        Some(SyncWindow::Live(o)) => Some((Zeroizing::new(o.key.key), o.key.host.clone())),
        _ => None,
    }) else {
        return Ok(());
    };
    let advert = crate::sync::advert::Advert::start(&device_id, &crate::sync::pairing::routing_tag(&key), &host, sync_port())?;
    if let Some(SyncWindow::Live(open)) = app.sync_window.lock().unwrap().as_mut() {
        if open.key.key == *key {
            open.advert = Some(advert);
        }
    }
    Ok(())
}

/// Ends a key that has run out: the key, its sessions and its advertisement
/// go, and only the fact that a key was shown stays, so the routes answer
/// `expired` until another is shown or the user cancels. True when a key
/// ended.
pub fn expire_pairing_key(app: &AppState, now: i64) -> bool {
    let mut guard = app.sync_window.lock().unwrap();
    if !matches!(&*guard, Some(SyncWindow::Live(o)) if !o.key.is_live(now)) {
        return false;
    }
    *guard = Some(SyncWindow::Expired);
    true
}

/// The `/pair/start` route. `peer_cert` is the client certificate on the
/// connection.
pub fn sync_pairing_start_impl(
    app: &AppState,
    vs: &VaultState,
    now: i64,
    body: &[u8],
    peer_cert: Option<&[u8]>,
) -> std::result::Result<Vec<u8>, PairError> {
    let mut guard = app.sync_window.lock().unwrap();
    let open = live_window(&mut guard)?;
    let id = load_or_create(app, vs).map_err(|_| PairError::Internal)?;
    start(&id, &mut open.key, now, body, peer_cert, &crate::hardware::machine_name(), sync_port(), StartRandom::fresh())
}

/// The `/pair` route. Returns the reply body and its signature; the key ends when a phone's pairing is saved.
pub fn sync_pairing_accept_impl(
    app: &AppState,
    vs: &VaultState,
    now: i64,
    body: &[u8],
    mac_b64: &str,
    session_id: &str,
    peer_cert: Option<&[u8]>,
) -> std::result::Result<(Vec<u8>, String), PairError> {
    let mut guard = app.sync_window.lock().unwrap();
    let mut id = load_or_create(app, vs).map_err(|_| PairError::Internal)?;
    let open = live_window(&mut guard)?;
    let secret = Zeroizing::new(session_secret(&open.key, now, session_id, body, peer_cert)?);
    let (mut paired, reply, sig_b64) = match accept(&id, &secret, now, body, mac_b64, &crate::hardware::machine_name(), sync_port()) {
        Ok(accepted) => accepted,
        Err(e) => {
            if e == PairError::BadMac {
                end_session(&mut open.key, session_id);
            }
            return Err(e);
        }
    };
    // A device id already paired here under another signing key keeps its pairing on record.
    let held = id.paired.iter().find(|p| p.device_id == paired.device_id);
    if held.is_some_and(|p| p.ed25519_pub_b64 != paired.ed25519_pub_b64) {
        log::warn!("sync: {} is paired here under another key; it stays until it is unpaired", paired.device_id);
        return Err(PairError::DeviceTaken);
    }
    // A phone that pairs again may seal to this machine's key from here on;
    // what it offers next is tried afresh. It keeps the name it was given here.
    inbox_of(app).forget_refusals_of(&paired.device_id);
    crate::sync::gate::set(app, &paired.device_id, None);
    paired.display_name = id.paired.iter().find(|p| p.device_id == paired.device_id).and_then(|p| p.display_name.clone());
    let again = id.paired.iter().any(|p| p.device_id == paired.device_id);
    let device_id = paired.device_id.clone();
    id.paired.retain(|p| p.device_id != paired.device_id);
    id.paired.push(paired);
    save(app, vs, &id).map_err(|_| PairError::Internal)?;
    *guard = None;
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
            crate::sync::gate::retain_devices(app, &paired);
            let inbox = inbox_of(app);
            inbox.forget_refusals_of(device_id);
            if let Err(e) = inbox.drop_device(device_id) {
                log::warn!("sync: staged files of {device_id} not dropped: {e:?}");
            }
            log::info!("sync: unpaired {device_id}");
        }
        id
    };
    Ok(status_of(app, &id, listening, bluetooth))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_is_ldh_or_a_dotted_quad() {
        assert!(is_ldh_host("studio-mac"));
        assert!(is_ldh_host("studio-mac.local"));
        assert!(is_ldh_host("192.168.1.20"));
        assert!(!is_ldh_host("DESKTOP_PC"));
        assert!(!is_ldh_host("DESKTOP_PC.local"));
        assert!(!is_ldh_host("a..b"));
        assert!(!is_ldh_host(&"a".repeat(64)));
        assert!(is_ldh_host(&"a".repeat(63)));
        assert!(!is_ldh_host("-studio"));
        assert!(!is_ldh_host("studio-"));
        assert!(!is_ldh_host(""));
        assert!(!is_ldh_host("studio.local."));
        assert!(!is_ldh_host("12345"));
        assert!(!is_ldh_host("1.2.3"));
        assert!(!is_ldh_host(&["a".repeat(63).as_str(); 4].join(".")), "255 characters");
    }
}
