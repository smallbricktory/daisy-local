// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::SigningKey;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tauri_app_core::commands::lifecycle::init_vault_impl;
use tauri_app_core::commands::session::delete_session_impl;
use tauri_app_core::commands::sync::*;
use tauri_app_core::now_unix;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, VaultState};
use tauri_app_core::sync::pairing::{
    confirm, desktop_claim, pair_mac, parse_key, phone_proof, transcript, PairError, PhoneClaim,
};
use tauri_app_core::sync::signing::accept_nonce;
use x25519_dalek::{PublicKey, StaticSecret};

const PASS: &str = "lantern-copper-walrus-orbit-2026";
/// The certificate this synthetic phone presents. Nothing here is real TLS:
/// `sync_pairing_start_impl`/`sync_pairing_accept_impl` are called directly
/// with whatever bytes the pairing body and `peer_cert` agree on.
const CERT: [u8; 91] = [5u8; 91];

fn unlocked() -> (AppState, VaultState, tempfile::TempDir) {
    let td = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(td.path().join("daisy")).unwrap());
    let vs = VaultState::new();
    init_vault_impl(&app, &vs, PASS).unwrap();
    (app, vs, td)
}

/// A phone: an identity to pair, with no networking underneath it.
struct Phone {
    device_id: String,
    key: SigningKey,
}

/// A fresh phone under the fixed device id most tests use.
fn a_phone() -> Phone {
    phone_with("4a68f72f5a22110b2bfbe71b6588833c")
}

/// A fresh phone claiming `device_id`, so two `Phone`s can claim the same id
/// under different signing keys.
fn phone_with(device_id: &str) -> Phone {
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    Phone { device_id: device_id.into(), key: SigningKey::from_bytes(&seed) }
}

/// The 16 bytes of a 32-character lowercase hex device id.
fn hex16(hex: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

impl Phone {
    fn claim(&self) -> PhoneClaim {
        PhoneClaim { device_id: hex16(&self.device_id), ed25519_pub: self.key.verifying_key().to_bytes(), cert_sha256: Sha256::digest(CERT).into() }
    }
}

/// Runs the whole v2 round for `key` against this desktop's live window: the
/// `/pair/start` exchange, checking `cD` the way a phone would, then `/pair`
/// under the derived secret. Returns what `/pair` answered.
fn pair(app: &AppState, vs: &VaultState, now: i64, key: &[u8; 10], phone: &Phone) -> std::result::Result<(Vec<u8>, String), PairError> {
    let mut rng = rand::rng();
    let mut eph_bytes = [0u8; 32];
    rng.fill_bytes(&mut eph_bytes);
    let eph = StaticSecret::from(eph_bytes);
    let eph_pub = PublicKey::from(&eph).to_bytes();
    let mut nonce = [0u8; 32];
    rng.fill_bytes(&mut nonce);

    let claim = phone.claim();
    let cp = phone_proof(key, &claim.bytes(), &eph_pub, &nonce);
    let start_body = serde_json::to_vec(&serde_json::json!({
        "v": 2,
        "device_id": phone.device_id,
        "ed25519_pub_b64": B64.encode(phone.key.verifying_key().to_bytes()),
        "tls_cert_der_b64": B64.encode(CERT),
        "eph_pub_b64": B64.encode(eph_pub),
        "nonce_b64": B64.encode(nonce),
        "cP_b64": B64.encode(cp),
    }))
    .unwrap();
    let start_reply = sync_pairing_start_impl(app, vs, now, &start_body, Some(&CERT))?;

    let r: serde_json::Value = serde_json::from_slice(&start_reply).unwrap();
    let arr = |f: &str| -> [u8; 32] { B64.decode(r[f].as_str().unwrap()).unwrap().try_into().unwrap() };
    let b = desktop_claim(
        &hex16(r["device_id"].as_str().unwrap()),
        &arr("ed25519_pub_b64"),
        &arr("x25519_pub_b64"),
        &arr("tls_spki_sha256_b64"),
        r["name"].as_str().unwrap(),
        r["host"].as_str().unwrap(),
        r["port"].as_u64().unwrap() as u16,
    );
    let eph_desktop = arr("eph_pub_b64");
    let t = transcript(&claim.bytes(), &b, &eph_pub, &eph_desktop, &nonce, &arr("nonce_b64"));
    let z = eph.diffie_hellman(&PublicKey::from(eph_desktop)).to_bytes();
    let (cd, secret) = confirm(key, &z, &t);
    assert_eq!(cd, arr("cD_b64"), "the desktop proved it holds the key");
    let session_id = r["session_id"].as_str().unwrap().to_string();

    let pair_body = serde_json::to_vec(&serde_json::json!({
        "v": 1,
        "device_id": phone.device_id,
        "name": "Sable's phone",
        "platform": "ios",
        "ed25519_pub_b64": B64.encode(phone.key.verifying_key().to_bytes()),
        "tls_cert_der_b64": B64.encode(CERT),
    }))
    .unwrap();
    let mac = B64.encode(pair_mac(&secret, &pair_body));
    sync_pairing_accept_impl(app, vs, now, &pair_body, &mac, &session_id, Some(&CERT))
}

#[test]
fn status_mints_an_identity_and_lists_nothing() {
    let (app, vs, _td) = unlocked();
    let s = sync_status_impl(&app, &vs, true, "on").unwrap();
    assert_eq!(s.device_id.len(), 32);
    assert!(s.paired.is_empty());
    assert!(s.window.is_none());
    assert!(!s.machine_name.is_empty());
}

#[test]
fn begin_opens_a_window_that_status_reports_and_cancel_closes() {
    let (app, vs, _td) = unlocked();
    let now = now_unix();
    let w = sync_pairing_begin_impl(&app, &vs, "studio-mac.local".into(), now).unwrap();
    assert_eq!(w.expires_unix, now + tauri_app_core::sync::pairing::KEY_SECS);
    assert!(w.qr_text.starts_with("daisy-pair:2:"));
    assert!(w.qr_text.ends_with("@studio-mac.local:7845"), "{}", w.qr_text);
    assert_eq!(w.key_text.len(), 19, "four groups of four separated by three dashes");
    assert_eq!(w.key_text.matches('-').count(), 3);
    assert!(parse_key(&w.key_text).is_some(), "{}", w.key_text);
    assert!(sync_status_impl(&app, &vs, true, "on").unwrap().window.is_some());
    sync_pairing_cancel_impl(&app).unwrap();
    assert!(sync_status_impl(&app, &vs, true, "on").unwrap().window.is_none());
}

#[test]
fn accept_pairs_persists_and_closes_the_window() {
    let (app, vs, _td) = unlocked();
    let w = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 1000).unwrap();
    let key = parse_key(&w.key_text).unwrap();
    let phone = a_phone();
    let (reply, sig_b64) = pair(&app, &vs, 1005, &key, &phone).unwrap();
    let reply: serde_json::Value = serde_json::from_slice(&reply).unwrap();
    assert_eq!(reply["phone_ed25519_pub_b64"], B64.encode(phone.key.verifying_key().to_bytes()));
    assert_eq!(reply["port"], SYNC_PORT);
    assert_eq!(B64.decode(&sig_b64).unwrap().len(), 64);
    let s = sync_status_impl(&app, &vs, true, "on").unwrap();
    assert_eq!(s.paired.len(), 1);
    assert_eq!(s.paired[0].name, "Sable's phone");
    assert!(s.window.is_none(), "the key is single-use");
    assert_eq!(
        pair(&app, &vs, 1006, &key, &phone).unwrap_err(),
        PairError::Closed,
        "the same key pairs nothing twice"
    );
    // Persisted: a fresh VaultState sees it.
    let vs2 = VaultState::new();
    tauri_app_core::commands::lifecycle::unlock_vault_impl(&app, &vs2, PASS).unwrap();
    assert_eq!(sync_status_impl(&app, &vs2, true, "on").unwrap().paired.len(), 1);
}

#[test]
fn accept_without_a_window_is_refused() {
    let (app, vs, _td) = unlocked();
    sync_pairing_begin_impl(&app, &vs, "host.local".into(), 1000).unwrap();
    sync_pairing_cancel_impl(&app).unwrap();
    assert_eq!(
        sync_pairing_accept_impl(&app, &vs, 1005, b"{}", "", "bogus-session", None).unwrap_err(),
        PairError::Closed
    );
}

#[test]
fn unpair_removes_the_phone_and_lets_it_pair_again() {
    let (app, vs, _td) = unlocked();
    let phone = a_phone();
    let w = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 1000).unwrap();
    let key = parse_key(&w.key_text).unwrap();
    pair(&app, &vs, 1005, &key, &phone).unwrap();
    let inbox = tauri_app_core::sync::server::inbox_of(&app);
    inbox.store(&phone.device_id, "daisy-1700000000", "manifest.json", &tauri_app_core::sync::signing::body_hash(b"m"), b"m").unwrap();
    let s = sync_unpair_impl(&app, &vs, &phone.device_id, true, "on").unwrap();
    assert!(s.paired.is_empty());
    assert!(inbox.staged_sessions().is_empty(), "what it had staged goes; the recording stays on the phone");
    let w2 = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 2000).unwrap();
    let key2 = parse_key(&w2.key_text).unwrap();
    pair(&app, &vs, 2005, &key2, &phone).expect("a fresh key pairs it again");
    assert_eq!(sync_status_impl(&app, &vs, true, "on").unwrap().paired.len(), 1);
}

/// A device id is what this desktop authorizes by; a second key claiming a paired id is refused.
#[test]
fn a_second_key_cannot_claim_a_paired_device_id() {
    let (app, vs, _td) = unlocked();
    let phone = a_phone();
    let w = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 1000).unwrap();
    let key = parse_key(&w.key_text).unwrap();
    pair(&app, &vs, 1005, &key, &phone).unwrap();

    let w2 = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 2000).unwrap();
    let key2 = parse_key(&w2.key_text).unwrap();
    let stranger = phone_with(&phone.device_id);
    assert_eq!(
        pair(&app, &vs, 2005, &key2, &stranger).unwrap_err(),
        PairError::DeviceTaken,
        "the id is already paired under another key"
    );
    assert_eq!(sync_status_impl(&app, &vs, true, "on").unwrap().paired.len(), 1, "the phone on record keeps its pairing");

    // The phone that holds the id pairs again from any fresh key, which is
    // what an unpair and re-pair or a phone that kept its keys does.
    let w3 = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 3000).unwrap();
    let key3 = parse_key(&w3.key_text).unwrap();
    pair(&app, &vs, 3005, &key3, &phone).expect("its own key pairs it again");
    assert_eq!(sync_status_impl(&app, &vs, true, "on").unwrap().paired.len(), 1);

    // Unpairing lets any phone take the id: nothing of the old pairing is left
    // for it to inherit.
    sync_unpair_impl(&app, &vs, &phone.device_id, true, "on").unwrap();
    let w4 = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 4000).unwrap();
    let key4 = parse_key(&w4.key_text).unwrap();
    pair(&app, &vs, 4005, &key4, &stranger).expect("the id is free once the phone is unpaired");
}

/// The first `/pair` to succeed ends the key; one refused because its device id is taken leaves the key live.
#[test]
fn a_pair_refused_for_a_taken_device_id_leaves_the_key_live() {
    let (app, vs, _td) = unlocked();
    let phone = a_phone();
    let w = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 1000).unwrap();
    pair(&app, &vs, 1005, &parse_key(&w.key_text).unwrap(), &phone).unwrap();

    let w2 = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 2000).unwrap();
    let key2 = parse_key(&w2.key_text).unwrap();
    let stranger = phone_with(&phone.device_id);
    assert_eq!(pair(&app, &vs, 2005, &key2, &stranger).unwrap_err(), PairError::DeviceTaken);
    assert!(pairing_key_live(&app, 2006), "the refused pairing did not end the key");
    let second = phone_with("0b7e2c4d9a1f3e5b6c8d0a2b4c6d8e0f");
    pair(&app, &vs, 2010, &key2, &second).expect("the key still pairs a phone");
    assert_eq!(sync_status_impl(&app, &vs, true, "on").unwrap().paired.len(), 2);
}

/// Past its ten minutes the key is gone from memory; what is left only
/// remembers that one was shown, so the pairing routes answer `expired`.
#[test]
fn an_expired_key_leaves_only_a_marker_and_the_routes_answer_expired() {
    let (app, vs, _td) = unlocked();
    let w = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 1000).unwrap();
    let key = parse_key(&w.key_text).unwrap();
    let expiry = 1000 + tauri_app_core::sync::pairing::KEY_SECS;
    assert!(!expire_pairing_key(&app, expiry - 1), "a live key is not expired");
    assert!(pairing_key_live(&app, expiry - 1));

    assert!(expire_pairing_key(&app, expiry));
    assert!(matches!(*app.sync_window.lock().unwrap(), Some(SyncWindow::Expired)));
    assert!(pairing_key_shown(&app));
    assert!(!pairing_key_live(&app, expiry));
    assert_eq!(pair(&app, &vs, expiry, &key, &a_phone()).unwrap_err(), PairError::Expired);
    assert_eq!(
        sync_pairing_accept_impl(&app, &vs, expiry, b"{}", "", "bogus-session", None).unwrap_err(),
        PairError::Expired
    );
    assert!(sync_status_impl(&app, &vs, true, "on").unwrap().window.is_none());
    sync_pairing_cancel_impl(&app).unwrap();
    assert!(!pairing_key_shown(&app), "cancel clears the marker");
}

/// Several phones announce the same name; a rename here tells them apart. It
/// survives the phone pairing again, a blank name clears it, and it goes with
/// an unpair.
#[test]
fn a_rename_names_the_phone_until_it_is_cleared_or_unpaired() {
    let (app, vs, _td) = unlocked();
    let phone = a_phone();
    let pair_at = |at: i64| {
        let w = sync_pairing_begin_impl(&app, &vs, "host.local".into(), at).unwrap();
        let key = parse_key(&w.key_text).unwrap();
        pair(&app, &vs, at + 5, &key, &phone).unwrap();
    };
    let shown = || sync_status_impl(&app, &vs, true, "on").unwrap().paired[0].name.clone();
    pair_at(1000);

    let s = sync_rename_device_impl(&app, &vs, &phone.device_id, "  Work iPhone  ", true, "on").unwrap();
    assert_eq!(s.paired[0].name, "Work iPhone");
    assert_eq!(s.paired[0].phone_name, "Sable's phone");

    pair_at(2000);
    assert_eq!(shown(), "Work iPhone", "pairing again keeps the rename");

    sync_rename_device_impl(&app, &vs, &phone.device_id, " ", true, "on").unwrap();
    assert_eq!(shown(), "Sable's phone", "a blank name clears it");

    sync_rename_device_impl(&app, &vs, &phone.device_id, "Work iPhone", true, "on").unwrap();
    sync_unpair_impl(&app, &vs, &phone.device_id, true, "on").unwrap();
    pair_at(3000);
    assert_eq!(shown(), "Sable's phone", "an unpair forgets it");

    assert!(sync_rename_device_impl(&app, &vs, "phone-0", "Nobody", true, "on").is_err());
}

#[test]
fn locked_vault_refuses_every_command() {
    let td = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(td.path().join("daisy")).unwrap());
    let vs = VaultState::new();
    assert!(sync_status_impl(&app, &vs, true, "on").is_err());
    assert!(sync_pairing_begin_impl(&app, &vs, "host.local".into(), 1).is_err());
}

#[test]
fn the_pairing_host_is_an_mdns_name_or_an_address() {
    let Some(h) = pairing_host() else { return };
    assert!(is_ldh_host(&h), "{h}");
    assert!(h != "127.0.0.1", "a phone cannot reach a loopback address");
    assert!(h.ends_with(".local") || h.parse::<std::net::Ipv4Addr>().is_ok(), "{h}");
}

/// A phone's nonces are kept until it is unpaired.
#[test]
fn unpairing_forgets_the_phones_nonces() {
    let (app, vs, _td) = unlocked();
    let phone = a_phone();
    let w = sync_pairing_begin_impl(&app, &vs, "studio-mac.local".into(), now_unix()).unwrap();
    let key = parse_key(&w.key_text).unwrap();
    pair(&app, &vs, now_unix(), &key, &phone).unwrap();

    assert!(accept_nonce(&app, &phone.device_id, "bm9uY2Ux"));
    assert!(!accept_nonce(&app, &phone.device_id, "bm9uY2Ux"), "a nonce is used once");

    sync_unpair_impl(&app, &vs, &phone.device_id, true, "on").unwrap();
    assert!(accept_nonce(&app, &phone.device_id, "bm9uY2Ux"), "the set went with the pairing");
}

/// Nonce sets survive concurrent writes.
#[test]
fn nonces_recorded_at_once_are_all_kept() {
    let (app, _vs, _td) = unlocked();
    let nonces: Vec<String> = (0..40).map(|i| B64.encode(format!("nonce-{i}"))).collect();

    std::thread::scope(|s| {
        for n in &nonces {
            let app = &app;
            s.spawn(move || assert!(accept_nonce(app, "phone-9", n), "{n} is new"));
        }
    });

    for n in &nonces {
        assert!(!accept_nonce(&app, "phone-9", n), "{n} was replayed");
    }
}

/// A delete on either device deletes on both. Deleting a phone session here
/// leaves the signed tombstone the phone collects at its next hello.
#[test]
fn deleting_a_phone_session_leaves_a_tombstone_for_the_phone() {
    let (app, vs, _td) = unlocked();
    let dir = app.profile.session_path("daisy-1700000000");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 4,
            "session_id": "daisy-1700000000",
            "created_at_unix_seconds": 1_700_000_000i64,
            "sample_rate": 16000,
            "channels": 1,
            "chunks": [],
            "recording_kind": "phone_room",
            "origin_device": "4a68f72f5a22110b2bfbe71b6588833c",
        }))
        .unwrap(),
    )
    .unwrap();

    delete_session_impl(&app, &vs, "daisy-1700000000").unwrap();
    assert!(!dir.exists());

    let inbox = tauri_app_core::sync::server::inbox_of(&app);
    let phone = "4a68f72f5a22110b2bfbe71b6588833c";
    assert!(inbox.is_entombed("daisy-1700000000", phone), "the phone must not send it back");
    let record: tauri_app_core::sync::exchange::TombstoneRecord =
        serde_json::from_slice(&inbox.tombstone("daisy-1700000000", phone).unwrap()).unwrap();
    let id = tauri_app_core::sync::identity::load_or_create(&app, &vs).unwrap();
    assert_eq!(record.origin, id.device_id);
    assert!(tauri_app_core::sync::exchange::tombstone_is_valid(&record, &id.ed25519_pub_b64()));
}

/// A session this desktop recorded belongs to no phone, so deleting it
/// writes nothing for one.
#[test]
fn deleting_a_desktop_session_leaves_no_tombstone() {
    let (app, vs, _td) = unlocked();
    let dir = app.profile.session_path("daisy-1700000001");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 4,
            "session_id": "daisy-1700000001",
            "created_at_unix_seconds": 1_700_000_001i64,
            "sample_rate": 48000,
            "channels": 2,
            "chunks": [],
        }))
        .unwrap(),
    )
    .unwrap();

    delete_session_impl(&app, &vs, "daisy-1700000001").unwrap();
    assert!(!tauri_app_core::sync::server::inbox_of(&app)
        .is_entombed("daisy-1700000001", "4a68f72f5a22110b2bfbe71b6588833c"));
}

/// Imports a phone session directory the way the server imports one.
/// Opt-in: set `DAISY_PHONE_SAMPLE` to the session directory.
///
/// ```sh
/// DAISY_PHONE_SAMPLE=<session dir> \
///   cargo test -p tauri-app --test sync_commands -- --ignored a_real_phone_session
/// ```
#[test]
#[ignore = "needs DAISY_PHONE_SAMPLE pointing at a session directory"]
fn a_real_phone_session_imports_and_opens() {
    let Ok(sample) = std::env::var("DAISY_PHONE_SAMPLE") else {
        panic!("set DAISY_PHONE_SAMPLE to a phone session directory");
    };
    let sample = std::path::PathBuf::from(shellexpand(&sample));
    let session = sample.file_name().unwrap().to_str().unwrap().to_string();
    let (app, vs, _td) = unlocked();
    let inbox = tauri_app_core::sync::server::inbox_of(&app);

    // Stage it exactly as uploads arrive: content-addressed, one file at a time.
    let manifest = std::fs::read(sample.join("manifest.json")).expect("manifest");
    let phone = "4a68f72f5a22110b2bfbe71b6588833c";
    let stage = |rel: &str, bytes: &[u8]| {
        inbox.store(phone, &session, rel, &tauri_app_core::sync::signing::body_hash(bytes), bytes).unwrap();
    };
    stage("manifest.json", &manifest);
    stage("transcript.json", &std::fs::read(sample.join("transcript.json")).expect("transcript"));
    let value: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    for chunk in value["chunks"].as_array().unwrap() {
        let rel = chunk["mic_wav_relative"].as_str().unwrap();
        stage(rel, &std::fs::read(sample.join(rel)).expect("chunk audio"));
    }

    let id = tauri_app_core::sync::identity::load_or_create(&app, &vs).unwrap();
    let key = id.machine_key();
    assert!(tauri_app_core::sync::import::is_ready(&inbox, phone, &session, &key), "every file is staged");
    tauri_app_core::sync::import::import_session(
        &inbox,
        &app.profile.sessions_dir(),
        phone,
        &session,
        &key,
    )
    .expect("a session the phone recorded imports");

    // It opens on the desktop: the library reads it without edits.
    let opened = tauri_app_core::commands::session::read_session_impl(&app, &session).expect("the desktop opens it");
    assert_eq!(opened.session_id, session);
    let dir = app.profile.session_path(&session);
    for chunk in value["chunks"].as_array().unwrap() {
        assert!(dir.join(chunk["mic_wav_relative"].as_str().unwrap()).is_file(), "its audio came with it");
    }
}

fn shellexpand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => path.to_string(),
    }
}
