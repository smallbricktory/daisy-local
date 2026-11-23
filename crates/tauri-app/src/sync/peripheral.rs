// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The Bluetooth side of sync: one advertised service per paired phone. A
//! phone writes a challenge, reads the address this desktop is reachable at
//! sealed to that challenge, and is woken by a notify whose nonce the next
//! address carries as `w`. Section 9a of the sync protocol; the radio itself
//! is the `ble` crate, macOS only.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use zeroize::Zeroizing;

use crate::state::{AppState, VaultState};
use crate::sync::identity::PairedPhone;
use crate::sync::wake::{self, Challenge, WakeNonce};

/// How long a sent wake stays pending.
pub const WAKE_WINDOW: Duration = Duration::from_secs(30);

/// Centrals whose challenge one service holds at once.
pub const MAX_CENTRALS: usize = 8;

/// Challenge writes one service takes in a burst.
pub const WRITE_BURST: f64 = 6.0;

/// Challenge writes per second one service takes once the burst is spent.
pub const WRITE_REFILL_PER_SEC: f64 = 2.0;

/// How long a write holds its slot against newcomers.
pub const ROUND_HOLD: Duration = Duration::from_secs(3);

/// How long a central whose address read succeeded keeps its slot.
pub const READ_HOLD: Duration = WAKE_WINDOW;

/// The running peripheral, or nothing on a desktop with no peripheral radio.
#[derive(Default)]
pub struct PeripheralHandle {
    inner: Mutex<Option<Radio>>,
}

struct Radio {
    peripheral: ble::Peripheral,
    /// Each published service, by the paired phone's device id.
    services: HashMap<String, Published>,
}

struct Published {
    service_uuid: String,
    binding: Arc<Binding>,
}

/// One pairing's service state: the address it answers with, the pending
/// wake, and each central's challenge with the reply sealed to it.
pub struct Binding {
    key: Zeroizing<[u8; 32]>,
    state: Mutex<Bound>,
}

struct Bound {
    host: String,
    port: u16,
    pending: Option<Pending>,
    /// Oldest write first.
    held: VecDeque<Held>,
    /// Challenge writes still available to this service.
    tokens: f64,
    refilled: Option<Instant>,
}

#[derive(Clone, Copy)]
struct Pending {
    n: WakeNonce,
    at: Instant,
}

struct Held {
    central: String,
    written: Instant,
    read: Option<Instant>,
    challenge: Challenge,
    sealed: Vec<u8>,
}

/// A wake whose nonce is pending, with the pending wake it replaced.
pub struct Armed {
    n: WakeNonce,
    replaced: Option<Pending>,
}

impl Held {
    fn held_at(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.written) < ROUND_HOLD
            || self.read.is_some_and(|r| now.saturating_duration_since(r) < READ_HOLD)
    }
}

impl Bound {
    /// Spends one challenge write at `now`, or false when the service's
    /// bucket is empty.
    fn take_write(&mut self, now: Instant) -> bool {
        let earned = self.refilled.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64() * WRITE_REFILL_PER_SEC);
        self.tokens = (self.tokens + earned).min(WRITE_BURST);
        self.refilled = Some(now);
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }

    fn w(&self, now: Instant) -> Option<WakeNonce> {
        self.pending.filter(|p| now.saturating_duration_since(p.at) < WAKE_WINDOW).map(|p| p.n)
    }
}

impl Binding {
    pub fn new(key: [u8; 32], host: String, port: u16) -> Self {
        Self {
            key: Zeroizing::new(key),
            state: Mutex::new(Bound { host, port, pending: None, held: VecDeque::new(), tokens: WRITE_BURST, refilled: None }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Bound> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A central's challenge write at `now`: 16 bytes are kept against the
    /// central with the address sealed to them, replacing its previous
    /// challenge. Writes beyond the service's rate are refused, and a new
    /// central beyond [`MAX_CENTRALS`] takes the oldest slot not held by a
    /// recent write or read, or is refused when every slot is held.
    pub fn challenge_at(&self, central: &str, value: &[u8], now: Instant) -> ble::Written {
        let Ok(challenge) = <Challenge>::try_from(value) else {
            log::debug!("sync: bluetooth challenge of {} bytes refused", value.len());
            return ble::Written::WrongLength;
        };
        let mut bound = self.lock();
        if !bound.take_write(now) {
            log::debug!("sync: bluetooth challenge refused, writes too frequent");
            return ble::Written::Limited;
        }
        let known = bound.held.iter().any(|h| h.central == central);
        let evict = if known || bound.held.len() < MAX_CENTRALS {
            None
        } else {
            match bound.held.iter().position(|h| !h.held_at(now)) {
                Some(i) => Some(i),
                None => {
                    log::debug!("sync: bluetooth challenge refused, every slot is in use");
                    return ble::Written::Limited;
                }
            }
        };
        let w = bound.w(now);
        let Some(sealed) = wake::address_payload(&self.key, &challenge, &bound.host, bound.port, w.as_ref()) else {
            return ble::Written::Failed;
        };
        if let Some(i) = evict {
            bound.held.remove(i);
        }
        bound.held.retain(|h| h.central != central);
        bound.held.push_back(Held { central: central.to_string(), written: now, read: None, challenge, sealed });
        ble::Written::Kept
    }

    /// The bytes sealed for a central's last challenge, or `None` when it
    /// holds none.
    pub fn address_of(&self, central: &str) -> Option<Vec<u8>> {
        self.address_at(central, Instant::now())
    }

    /// [`Self::address_of`] at `now`; a central that gets an answer holds its
    /// slot for [`READ_HOLD`].
    pub fn address_at(&self, central: &str, now: Instant) -> Option<Vec<u8>> {
        let mut bound = self.lock();
        let held = bound.held.iter_mut().find(|h| h.central == central)?;
        held.read = Some(now);
        Some(held.sealed.clone())
    }

    /// Moves the address at `now`, resealing every held central's reply
    /// under its own challenge. False when the address is unchanged.
    pub fn move_to_at(&self, host: &str, port: u16, now: Instant) -> bool {
        let mut bound = self.lock();
        if bound.host == host && bound.port == port {
            return false;
        }
        bound.host = host.to_string();
        bound.port = port;
        let w = bound.w(now);
        let Bound { host, port, held, .. } = &mut *bound;
        held.retain_mut(|h| match wake::address_payload(&self.key, &h.challenge, host, *port, w.as_ref()) {
            Some(sealed) => {
                h.sealed = sealed;
                true
            }
            None => false,
        });
        true
    }

    /// A fresh wake at `now`: its nonce becomes the pending wake, and the
    /// sealed notify is returned to send.
    pub fn arm_at(&self, now: Instant) -> Option<(Armed, Vec<u8>)> {
        let (n, payload) = wake::wake_payload(&self.key)?;
        let mut bound = self.lock();
        let replaced = bound.pending.replace(Pending { n, at: now });
        Some((Armed { n, replaced }, payload))
    }

    /// Takes back a wake that was never sent: the wake it replaced is pending
    /// again, unless a newer one has replaced it since.
    pub fn disarm(&self, armed: Armed) {
        let mut bound = self.lock();
        if bound.pending.is_some_and(|p| p.n == armed.n) {
            bound.pending = armed.replaced;
        }
    }
}

impl ble::Responder for Binding {
    fn challenge(&self, central: &str, value: &[u8]) -> ble::Written {
        self.challenge_at(central, value, Instant::now())
    }

    fn address(&self, central: &str) -> Option<Vec<u8>> {
        self.address_at(central, Instant::now())
    }
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
        return;
    }
    let Some(host) = crate::commands::sync::pairing_host() else {
        log::info!("sync: no network address to hand a phone over bluetooth");
        return;
    };
    let port = crate::commands::sync::sync_port();
    let stale = running.as_ref().is_some_and(|radio| {
        radio.services.iter().any(|(device, p)| {
            !id.paired.iter().any(|phone| &phone.device_id == device && phone.service_uuid == p.service_uuid)
        })
    });
    if stale {
        running.take();
        log::info!("sync: bluetooth restarted for a changed pairing");
    }
    if running.is_none() {
        match ble::Peripheral::start() {
            Ok(peripheral) => *running = Some(Radio { peripheral, services: HashMap::new() }),
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
    let Some(radio) = running.as_mut() else { return };
    for phone in &id.paired {
        if let Some(published) = radio.services.get(&phone.device_id) {
            published.binding.move_to_at(&host, port, Instant::now());
            continue;
        }
        let Some(key) = pairing_key(phone) else {
            log::warn!("sync: {} has no pairing key; no bluetooth for it", phone.device_id);
            continue;
        };
        let challenge_uuid = wake::challenge_uuid(&key);
        let binding = Arc::new(Binding::new(key, host.clone(), port));
        match radio.peripheral.add_service(
            &phone.service_uuid,
            &phone.wake_uuid,
            &phone.address_uuid,
            &challenge_uuid,
            binding.clone(),
        ) {
            Ok(()) => {
                radio
                    .services
                    .insert(phone.device_id.clone(), Published { service_uuid: phone.service_uuid.clone(), binding });
            }
            // A phone left without its service is tried again at the next call.
            Err(e) => log::warn!("sync: bluetooth service for {}: {e}", phone.device_id),
        }
    }
}

/// Wakes one phone. False when this desktop has no radio, no such pairing, or nothing is listening on it.
pub fn wake(handle: &PeripheralHandle, device_id: &str) -> bool {
    let running = handle.inner.lock().unwrap();
    let Some(radio) = running.as_ref() else { return false };
    let Some(published) = radio.services.get(device_id) else { return false };
    if radio.peripheral.subscribers(&published.service_uuid) == 0 {
        log::info!("sync: {device_id} is not listening; it collects at its next hello");
        return false;
    }
    let Some((armed, payload)) = published.binding.arm_at(Instant::now()) else { return false };
    if radio.peripheral.notify(&published.service_uuid, &payload) {
        return true;
    }
    published.binding.disarm(armed);
    false
}

/// What the settings screen shows: `on` while the radio is advertising,
/// `off` when Bluetooth is turned off or the radio is not running, `denied`
/// when this Mac refused Daisy the radio, and `unsupported` where Daisy has
/// no peripheral role at all.
pub fn radio_state(handle: &PeripheralHandle) -> &'static str {
    let running = handle.inner.lock().unwrap();
    match running.as_ref().map(|r| &r.peripheral) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::wake::tests::open;
    use crate::sync::wake::{address_aad, WAKE_AAD};
    use ble::Written;

    const KEY: [u8; 32] = [6u8; 32];

    fn binding() -> Binding {
        Binding::new(KEY, "192.168.1.20".into(), 7845)
    }

    fn challenge(b: u8) -> Challenge {
        [b; 16]
    }

    /// The address a central holds, opened under `c`.
    fn opened(binding: &Binding, central: &str, c: &Challenge) -> Option<serde_json::Value> {
        let wire = binding.address_of(central)?;
        open(&KEY, &address_aad(c), &wire).map(|p| serde_json::from_slice(&p).unwrap())
    }

    fn wake_n(payload: &[u8]) -> String {
        let v: serde_json::Value = serde_json::from_slice(&open(&KEY, WAKE_AAD, payload).unwrap()).unwrap();
        v["n"].as_str().unwrap().to_string()
    }

    #[test]
    fn a_read_before_any_challenge_gets_nothing() {
        assert_eq!(binding().address_of("central-a"), None);
    }

    #[test]
    fn a_challenge_binds_that_centrals_reply_to_it() {
        let b = binding();
        let t = Instant::now();
        assert_eq!(b.challenge_at("central-a", &challenge(1), t), Written::Kept);
        let v = opened(&b, "central-a", &challenge(1)).expect("opens under the central's challenge");
        assert_eq!(v["host"], "192.168.1.20");
        assert_eq!(v["port"], 7845);
        assert!(v.get("w").is_none(), "no wake pending");
        assert!(opened(&b, "central-a", &challenge(2)).is_none(), "another challenge opens nothing");
        assert_eq!(b.address_of("central-b"), None, "another central holds nothing");
    }

    #[test]
    fn every_read_returns_the_bytes_sealed_at_the_write() {
        let b = binding();
        b.challenge_at("central-a", &challenge(1), Instant::now());
        assert_eq!(b.address_of("central-a"), b.address_of("central-a"));
    }

    #[test]
    fn a_challenge_of_any_other_length_is_refused_and_keeps_nothing() {
        let b = binding();
        let t = Instant::now();
        for len in [0, 1, 15, 17, 32] {
            assert_eq!(b.challenge_at("central-a", &vec![1u8; len], t), Written::WrongLength, "{len} bytes");
        }
        assert_eq!(b.address_of("central-a"), None);
    }

    #[test]
    fn a_new_challenge_replaces_the_centrals_previous_one() {
        let b = binding();
        let t = Instant::now();
        b.challenge_at("central-a", &challenge(1), t);
        b.challenge_at("central-a", &challenge(2), t);
        assert!(opened(&b, "central-a", &challenge(1)).is_none());
        assert!(opened(&b, "central-a", &challenge(2)).is_some());
        assert_eq!(b.lock().held.len(), 1);
    }

    fn secs(t: Instant, n: u64) -> Instant {
        t + Duration::from_secs(n)
    }

    #[test]
    fn a_ninth_central_pushes_out_the_oldest_idle_slot() {
        let b = binding();
        let t = Instant::now();
        for i in 0..MAX_CENTRALS as u8 {
            assert_eq!(b.challenge_at(&format!("central-{i}"), &challenge(i), secs(t, i as u64)), Written::Kept);
        }
        // Central 0 writes again, so central 1 is now the oldest.
        b.challenge_at("central-0", &challenge(100), secs(t, 8));
        assert_eq!(b.challenge_at("central-new", &challenge(200), secs(t, 9)), Written::Kept);
        assert_eq!(b.lock().held.len(), MAX_CENTRALS);
        assert_eq!(b.address_of("central-1"), None, "the oldest write went");
        assert!(opened(&b, "central-0", &challenge(100)).is_some());
        assert!(opened(&b, "central-new", &challenge(200)).is_some());
        for i in 2..MAX_CENTRALS as u8 {
            assert!(opened(&b, &format!("central-{i}"), &challenge(i)).is_some(), "central {i} kept");
        }
    }

    #[test]
    fn a_burst_within_the_limit_is_accepted() {
        let b = binding();
        let t = Instant::now();
        for i in 0..WRITE_BURST as u8 {
            assert_eq!(b.challenge_at("central-a", &challenge(i), t), Written::Kept, "write {i}");
        }
    }

    #[test]
    fn a_flood_is_refused_and_keeps_the_established_central() {
        let b = binding();
        let t = Instant::now();
        b.challenge_at("phone", &challenge(1), t);
        assert!(b.address_at("phone", secs(t, 1)).is_some());
        let later = secs(t, 20);
        let mut refused = 0;
        for i in 0..100u8 {
            if b.challenge_at(&format!("stranger-{i}"), &challenge(i), later) == Written::Limited {
                refused += 1;
            }
        }
        assert!(refused >= 90, "{refused} refused");
        assert!(opened(&b, "phone", &challenge(1)).is_some(), "the phone's slot survived");
    }

    #[test]
    fn a_full_store_of_held_slots_refuses_a_newcomer() {
        let b = binding();
        let t = Instant::now();
        for i in 0..MAX_CENTRALS as u8 {
            let at = secs(t, if i < 6 { 0 } else { 1 });
            assert_eq!(b.challenge_at(&format!("central-{i}"), &challenge(i), at), Written::Kept);
        }
        assert_eq!(b.challenge_at("late", &challenge(9), secs(t, 2)), Written::Limited);
        assert_eq!(b.lock().held.len(), MAX_CENTRALS);
        assert!(b.address_at("central-0", secs(t, 2)).is_some(), "no slot was evicted");
    }

    #[test]
    fn writes_are_accepted_again_after_the_window() {
        let b = binding();
        let t = Instant::now();
        for i in 0..WRITE_BURST as u8 {
            b.challenge_at("central-a", &challenge(i), t);
        }
        assert_eq!(b.challenge_at("central-a", &challenge(50), t), Written::Limited);
        assert_eq!(b.challenge_at("central-a", &challenge(51), secs(t, 1)), Written::Kept);
        assert_eq!(b.challenge_at("central-a", &challenge(52), secs(t, 10)), Written::Kept);
    }

    #[test]
    fn a_wake_sent_is_echoed_as_w_in_the_next_bound_reply() {
        let b = binding();
        let t = Instant::now();
        let (_, payload) = b.arm_at(t).unwrap();
        b.challenge_at("central-a", &challenge(1), t + Duration::from_secs(1));
        let v = opened(&b, "central-a", &challenge(1)).unwrap();
        assert_eq!(v["w"], wake_n(&payload));
    }

    #[test]
    fn reads_never_consume_the_pending_wake() {
        let b = binding();
        let t = Instant::now();
        let (_, payload) = b.arm_at(t).unwrap();
        b.challenge_at("stranger", &challenge(9), t);
        for _ in 0..3 {
            assert!(b.address_of("stranger").is_some());
        }
        b.challenge_at("central-a", &challenge(1), t + Duration::from_secs(2));
        assert_eq!(opened(&b, "central-a", &challenge(1)).unwrap()["w"], wake_n(&payload));
    }

    #[test]
    fn a_wake_is_pending_for_thirty_seconds_of_the_monotonic_clock() {
        let b = binding();
        let t = Instant::now();
        let (_, payload) = b.arm_at(t).unwrap();
        b.challenge_at("central-a", &challenge(1), t + WAKE_WINDOW - Duration::from_millis(1));
        assert_eq!(opened(&b, "central-a", &challenge(1)).unwrap()["w"], wake_n(&payload));
        b.challenge_at("central-a", &challenge(2), t + WAKE_WINDOW);
        assert!(opened(&b, "central-a", &challenge(2)).unwrap().get("w").is_none(), "lapsed at 30 s");
    }

    #[test]
    fn a_newer_wake_replaces_the_pending_one() {
        let b = binding();
        let t = Instant::now();
        b.arm_at(t).unwrap();
        let (_, newer) = b.arm_at(t + Duration::from_secs(5)).unwrap();
        b.challenge_at("central-a", &challenge(1), t + Duration::from_secs(6));
        assert_eq!(opened(&b, "central-a", &challenge(1)).unwrap()["w"], wake_n(&newer));
    }

    #[test]
    fn a_wake_never_sent_leaves_the_earlier_one_pending() {
        let b = binding();
        let t = Instant::now();
        let (_, sent) = b.arm_at(t).unwrap();
        let (unsent, _) = b.arm_at(t + Duration::from_secs(1)).unwrap();
        b.disarm(unsent);
        b.challenge_at("central-a", &challenge(1), t + Duration::from_secs(2));
        assert_eq!(opened(&b, "central-a", &challenge(1)).unwrap()["w"], wake_n(&sent));
    }

    #[test]
    fn taking_back_a_replaced_wake_leaves_the_newer_one() {
        let b = binding();
        let t = Instant::now();
        let (older, _) = b.arm_at(t).unwrap();
        let (_, newer) = b.arm_at(t + Duration::from_secs(1)).unwrap();
        b.disarm(older);
        b.challenge_at("central-a", &challenge(1), t + Duration::from_secs(2));
        assert_eq!(opened(&b, "central-a", &challenge(1)).unwrap()["w"], wake_n(&newer));
    }

    #[test]
    fn an_address_change_reseals_every_central_under_its_own_challenge() {
        let b = binding();
        let t = Instant::now();
        for i in 0..3u8 {
            b.challenge_at(&format!("central-{i}"), &challenge(i), t);
        }
        let before: Vec<_> = (0..3u8).map(|i| b.address_of(&format!("central-{i}")).unwrap()).collect();
        assert!(b.move_to_at("10.0.0.7", 7845, t));
        for i in 0..3u8 {
            let central = format!("central-{i}");
            assert_ne!(b.address_of(&central).unwrap(), before[i as usize], "{central} resealed");
            let v = opened(&b, &central, &challenge(i)).expect("still bound to its own challenge");
            assert_eq!(v["host"], "10.0.0.7");
            for other in (0..3u8).filter(|o| *o != i) {
                assert!(opened(&b, &central, &challenge(other)).is_none());
            }
        }
    }

    #[test]
    fn an_unchanged_address_leaves_the_replies_alone() {
        let b = binding();
        let t = Instant::now();
        b.challenge_at("central-a", &challenge(1), t);
        let before = b.address_of("central-a");
        assert!(!b.move_to_at("192.168.1.20", 7845, t));
        assert_eq!(b.address_of("central-a"), before);
    }

    #[test]
    fn a_reseal_carries_the_wake_pending_at_that_moment() {
        let b = binding();
        let t = Instant::now();
        b.challenge_at("central-a", &challenge(1), t);
        let (_, payload) = b.arm_at(t + Duration::from_secs(1)).unwrap();
        b.move_to_at("10.0.0.7", 7845, t + Duration::from_secs(2));
        assert_eq!(opened(&b, "central-a", &challenge(1)).unwrap()["w"], wake_n(&payload));
    }
}
