// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine;
use ed25519_dalek::SigningKey;
use tauri_app_core::commands::lifecycle::init_vault_impl;
use tauri_app_core::commands::session::delete_session_impl;
use tauri_app_core::commands::sync::*;
use tauri_app_core::now_unix;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, VaultState};
use tauri_app_core::sync::pairing::{pair_mac, PairError};
use tauri_app_core::sync::signing::accept_nonce;

const PASS: &str = "lantern-copper-walrus-orbit-2026";

fn unlocked() -> (AppState, VaultState, tempfile::TempDir) {
    let td = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(td.path().join("daisy")).unwrap());
    let vs = VaultState::new();
    init_vault_impl(&app, &vs, PASS).unwrap();
    (app, vs, td)
}

/// The secret inside a pairing code, as a phone reads it.
fn code_secret(qr_text: &str) -> [u8; 32] {
    let json = B64URL.decode(qr_text.strip_prefix("daisy-pair:1:").unwrap()).unwrap();
    let code: serde_json::Value = serde_json::from_slice(&json).unwrap();
    B64.decode(code["secret_b64"].as_str().unwrap()).unwrap().try_into().unwrap()
}

fn a_key() -> SigningKey {
    let mut seed = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut seed);
    SigningKey::from_bytes(&seed)
}

/// What a phone posts to `/pair`: the body and its MAC under the code secret.
fn phone_request(qr_text: &str, key: &SigningKey) -> (Vec<u8>, String) {
    let body = serde_json::to_vec(&serde_json::json!({
        "v": 1,
        "device_id": "phone-9",
        "name": "Sable's phone",
        "platform": "ios",
        "ed25519_pub_b64": B64.encode(key.verifying_key().to_bytes()),
        "tls_cert_der_b64": B64.encode([3u8; 91]),
    }))
    .unwrap();
    let mac = B64.encode(pair_mac(&code_secret(qr_text), &body));
    (body, mac)
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
    assert_eq!(w.expires_unix, now + 90);
    let json = B64URL.decode(w.qr_text.strip_prefix("daisy-pair:1:").unwrap()).unwrap();
    let code: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(code["host"], "studio-mac.local");
    assert_eq!(code["port"], SYNC_PORT);
    assert_eq!(code["v"], 1);
    assert!(sync_status_impl(&app, &vs, true, "on").unwrap().window.is_some());
    sync_pairing_cancel_impl(&app).unwrap();
    assert!(sync_status_impl(&app, &vs, true, "on").unwrap().window.is_none());
}

#[test]
fn accept_pairs_persists_and_closes_the_window() {
    let (app, vs, _td) = unlocked();
    let w = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 1000).unwrap();
    let key = a_key();
    let (body, mac) = phone_request(&w.qr_text, &key);
    let (reply, sig_b64) = sync_pairing_accept_impl(&app, &vs, 1005, &body, &mac).unwrap();
    let reply: serde_json::Value = serde_json::from_slice(&reply).unwrap();
    assert_eq!(reply["phone_ed25519_pub_b64"], B64.encode(key.verifying_key().to_bytes()));
    assert_eq!(reply["port"], SYNC_PORT);
    assert_eq!(B64.decode(&sig_b64).unwrap().len(), 64);
    let s = sync_status_impl(&app, &vs, true, "on").unwrap();
    assert_eq!(s.paired.len(), 1);
    assert_eq!(s.paired[0].name, "Sable's phone");
    assert!(s.window.is_none(), "the secret is single-use");
    assert_eq!(
        sync_pairing_accept_impl(&app, &vs, 1006, &body, &mac).unwrap_err(),
        PairError::Closed,
        "the same code pairs nothing twice"
    );
    // Persisted: a fresh VaultState sees it.
    let vs2 = VaultState::new();
    tauri_app_core::commands::lifecycle::unlock_vault_impl(&app, &vs2, PASS).unwrap();
    assert_eq!(sync_status_impl(&app, &vs2, true, "on").unwrap().paired.len(), 1);
}

#[test]
fn accept_without_a_window_is_refused() {
    let (app, vs, _td) = unlocked();
    let w = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 1000).unwrap();
    let (body, mac) = phone_request(&w.qr_text, &a_key());
    sync_pairing_cancel_impl(&app).unwrap();
    assert_eq!(sync_pairing_accept_impl(&app, &vs, 1005, &body, &mac).unwrap_err(), PairError::Closed);
}

#[test]
fn unpair_removes_the_phone_and_lets_it_pair_again() {
    let (app, vs, _td) = unlocked();
    let w = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 1000).unwrap();
    let key = a_key();
    let (body, mac) = phone_request(&w.qr_text, &key);
    sync_pairing_accept_impl(&app, &vs, 1005, &body, &mac).unwrap();
    let s = sync_unpair_impl(&app, &vs, "phone-9", true, "on").unwrap();
    assert!(s.paired.is_empty());
    let w2 = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 2000).unwrap();
    let (body, mac) = phone_request(&w2.qr_text, &key);
    sync_pairing_accept_impl(&app, &vs, 2005, &body, &mac).expect("a fresh code pairs it again");
    assert_eq!(sync_status_impl(&app, &vs, true, "on").unwrap().paired.len(), 1);
}

/// A device id is what this desktop authorizes by: whose tombstones it
/// honours, whose staged files a delete reaches, whose refusals a pairing
/// lifts. Anyone who sees the screen has 90 seconds, so a second key claiming
/// a paired id would inherit all of it and evict the phone that holds it.
#[test]
fn a_second_key_cannot_claim_a_paired_device_id() {
    let (app, vs, _td) = unlocked();
    let phone = a_key();
    let w = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 1000).unwrap();
    let (body, mac) = phone_request(&w.qr_text, &phone);
    sync_pairing_accept_impl(&app, &vs, 1005, &body, &mac).unwrap();

    let w2 = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 2000).unwrap();
    let (body, mac) = phone_request(&w2.qr_text, &a_key());
    assert_eq!(
        sync_pairing_accept_impl(&app, &vs, 2005, &body, &mac).unwrap_err(),
        PairError::DeviceTaken,
        "the id is already paired under another key"
    );
    assert_eq!(sync_status_impl(&app, &vs, true, "on").unwrap().paired.len(), 1, "the phone on record keeps its pairing");

    // The phone that holds the id pairs again from any fresh code, which is
    // what an unpair and re-pair or a phone that kept its keys does.
    let w3 = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 3000).unwrap();
    let (body, mac) = phone_request(&w3.qr_text, &phone);
    sync_pairing_accept_impl(&app, &vs, 3005, &body, &mac).expect("its own key pairs it again");
    assert_eq!(sync_status_impl(&app, &vs, true, "on").unwrap().paired.len(), 1);

    // Unpairing lets any phone take the id: nothing of the old pairing is left
    // for it to inherit.
    sync_unpair_impl(&app, &vs, "phone-9", true, "on").unwrap();
    let w4 = sync_pairing_begin_impl(&app, &vs, "host.local".into(), 4000).unwrap();
    let (body, mac) = phone_request(&w4.qr_text, &a_key());
    sync_pairing_accept_impl(&app, &vs, 4005, &body, &mac).expect("the id is free once the phone is unpaired");
}

/// Several phones announce the same name; a rename here tells them apart. It
/// survives the phone pairing again, a blank name clears it, and it goes with
/// an unpair.
#[test]
fn a_rename_names_the_phone_until_it_is_cleared_or_unpaired() {
    let (app, vs, _td) = unlocked();
    let key = a_key();
    let pair = |at: i64| {
        let w = sync_pairing_begin_impl(&app, &vs, "host.local".into(), at).unwrap();
        let (body, mac) = phone_request(&w.qr_text, &key);
        sync_pairing_accept_impl(&app, &vs, at + 5, &body, &mac).unwrap();
    };
    let shown = || sync_status_impl(&app, &vs, true, "on").unwrap().paired[0].name.clone();
    pair(1000);

    let s = sync_rename_device_impl(&app, &vs, "phone-9", "  Work iPhone  ", true, "on").unwrap();
    assert_eq!(s.paired[0].name, "Work iPhone");
    assert_eq!(s.paired[0].phone_name, "Sable's phone");

    pair(2000);
    assert_eq!(shown(), "Work iPhone", "pairing again keeps the rename");

    sync_rename_device_impl(&app, &vs, "phone-9", " ", true, "on").unwrap();
    assert_eq!(shown(), "Sable's phone", "a blank name clears it");

    sync_rename_device_impl(&app, &vs, "phone-9", "Work iPhone", true, "on").unwrap();
    sync_unpair_impl(&app, &vs, "phone-9", true, "on").unwrap();
    pair(3000);
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
    let h = pairing_host();
    assert!(!h.is_empty());
    assert!(!h.contains(':'), "the port travels in its own field: {h}");
    assert!(h.ends_with(".local") || h.parse::<std::net::Ipv4Addr>().is_ok(), "{h}");
}

/// A phone's nonces are kept forever so a signed request can never be
/// replayed. Unpairing ends that: the device is gone, its set is dead weight,
/// and a phone that pairs again is a new device to the replay guard.
#[test]
fn unpairing_forgets_the_phones_nonces() {
    let (app, vs, _td) = unlocked();
    let qr = sync_pairing_begin_impl(&app, &vs, "studio-mac.local".into(), now_unix()).unwrap().qr_text;
    let key = a_key();
    let (body, mac) = phone_request(&qr, &key);
    sync_pairing_accept_impl(&app, &vs, now_unix(), &body, &mac).unwrap();

    assert!(accept_nonce(&app, "phone-9", "bm9uY2Ux"));
    assert!(!accept_nonce(&app, "phone-9", "bm9uY2Ux"), "a nonce is used once");

    sync_unpair_impl(&app, &vs, "phone-9", true, "on").unwrap();
    assert!(accept_nonce(&app, "phone-9", "bm9uY2Ux"), "the set went with the pairing");
}

/// A phone opens several connections at once and each one records its nonce,
/// so the sets must survive being written concurrently: a lost record is a
/// nonce that can be replayed.
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
    let dir = app.profile.session_path("daisy-1787249858");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 4,
            "session_id": "daisy-1787249858",
            "created_at_unix_seconds": 1_787_249_858i64,
            "sample_rate": 16000,
            "channels": 1,
            "chunks": [],
            "recording_kind": "phone_room",
            "origin_device": "4a68f72f5a22110b2bfbe71b6588833c",
        }))
        .unwrap(),
    )
    .unwrap();

    delete_session_impl(&app, &vs, "daisy-1787249858").unwrap();
    assert!(!dir.exists());

    let inbox = tauri_app_core::sync::server::inbox_of(&app);
    let phone = "4a68f72f5a22110b2bfbe71b6588833c";
    assert!(inbox.is_entombed("daisy-1787249858", phone), "the phone must not send it back");
    let record: tauri_app_core::sync::exchange::TombstoneRecord =
        serde_json::from_slice(&inbox.tombstone("daisy-1787249858", phone).unwrap()).unwrap();
    let id = tauri_app_core::sync::identity::load_or_create(&app, &vs).unwrap();
    assert_eq!(record.origin, id.device_id);
    assert!(tauri_app_core::sync::exchange::tombstone_is_valid(&record, &id.ed25519_pub_b64()));
}

/// A session this desktop recorded belongs to no phone, so deleting it
/// writes nothing for one.
#[test]
fn deleting_a_desktop_session_leaves_no_tombstone() {
    let (app, vs, _td) = unlocked();
    let dir = app.profile.session_path("daisy-1787249859");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 4,
            "session_id": "daisy-1787249859",
            "created_at_unix_seconds": 1_787_249_859i64,
            "sample_rate": 48000,
            "channels": 2,
            "chunks": [],
        }))
        .unwrap(),
    )
    .unwrap();

    delete_session_impl(&app, &vs, "daisy-1787249859").unwrap();
    assert!(!tauri_app_core::sync::server::inbox_of(&app)
        .is_entombed("daisy-1787249859", "4a68f72f5a22110b2bfbe71b6588833c"));
}

/// A real session pulled off the test phone, imported the way the server
/// imports one. Opt-in, because the samples live in the phone repo:
///
/// ```sh
/// DAISY_PHONE_SAMPLE=~/repos/smbr-daisy-ios/samples/device-sessions/daisy-1787249858 \
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
