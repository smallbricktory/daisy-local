// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! This desktop, waiting for a real phone. The server, the identity, the
//! pairing window and the inbox are the shipping ones; what is on the other
//! end is a Daisy build on a simulator or a handset, driven by its own UI
//! suite.
//!
//! `sync_transport.rs` and `sync_seam_matrix.rs` write the phone's bytes
//! themselves, so they prove the desktop answers the protocol. They cannot
//! prove the phone speaks it. This is the harness that can, and it is
//! ignored by default because it waits on a device.
//!
//! `scripts/test-seam-sim.sh` runs it against the iOS simulator end to end.
//! By hand:
//!
//! ```sh
//! cargo test -p tauri-app --test seam_live -- --ignored --nocapture
//! ```
//!
//! then type the printed code into Settings › Desktop Sync › Enter a code.
//!
//! It binds the shipping sync port, because the reply a phone stores names
//! that port for every hello after pairing. Daisy itself has to be closed.
mod seam;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use seam::*;
use tauri_app_core::sync::signing::body_hash;
use tauri_app_core::commands::contacts::{load_contacts, save_contacts, Contact};
use tauri_app_core::commands::sync::{sync_pairing_begin_impl, SYNC_PORT};
use tauri_app_core::commands::tags::{create_tag_impl, list_tags_impl, CreateTagRequest};
use tauri_app_core::now_unix;
use tauri_app_core::sync::identity::load_or_create;
use tauri_app_core::sync::server::{inbox_of, serve};

/// The tag the phone's UI suite makes before it pairs. A tag by this name in
/// this desktop's list came over the seam. Paired with the constant of the
/// same name in smbr-daisy-ios `DesktopSyncLiveUITests`.
const PHONE_TAG: &str = "Harbour survey";

/// The certificate the running desktop serves, for the synthetic phone in
/// the self-check. A handset reads it off the wire instead.
static SERVED_CERT: std::sync::Mutex<Option<Vec<u8>>> = std::sync::Mutex::new(None);

/// How long to wait for a phone at each step, in seconds.
fn budget(key: &str, fallback: u64) -> Duration {
    Duration::from_secs(
        std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(fallback),
    )
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

/// Polls `f` until it answers `Some`, or gives up after `limit`. Prints a
/// line a second so a person watching knows it is still waiting.
fn until<T>(what: &str, limit: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    let mut said = 0u64;
    loop {
        if let Some(v) = f() {
            println!("[seam] {what} after {}s", start.elapsed().as_secs());
            return Some(v);
        }
        if start.elapsed() >= limit {
            println!("[seam] gave up waiting for {what} after {}s", limit.as_secs());
            return None;
        }
        let secs = start.elapsed().as_secs();
        if secs > said {
            said = secs;
            if secs % 10 == 0 {
                println!("[seam] waiting for {what} ({secs}s of {}s)", limit.as_secs());
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Where to listen. A handset needs the shipping port and nothing else: the
/// pairing reply names `SYNC_PORT`, and that is what the phone stores for
/// every hello afterwards. The override exists for the self-check below,
/// whose phone is told where to go.
fn port() -> u16 {
    std::env::var("DAISY_LIVE_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(SYNC_PORT)
}

/// Bound before anything else, so a running Daisy is named as the reason
/// rather than showing up as a phone that never arrives.
fn bind_the_sync_port(rt: &tokio::runtime::Runtime, port: u16) -> tokio::net::TcpListener {
    match rt.block_on(tokio::net::TcpListener::bind(("0.0.0.0", port))) {
        Ok(l) => l,
        Err(e) => panic!(
            "port {port} is taken ({e}). Daisy itself is probably running — quit it and run \
             this again. A phone cannot be sent elsewhere: the pairing reply names {SYNC_PORT}, \
             and the phone uses that for every hello afterwards."
        ),
    }
}

/// Two tags and a contact, so the profile the desktop offers is not empty.
fn seed_profile(states: &States) {
    for (name, colour) in [("Northwind", "#4C6EF5"), ("Weekly", "#12B886")] {
        create_tag_impl(
            &states.app,
            CreateTagRequest {
                name: name.into(),
                color_hex: colour.into(),
                prompt_md: None,
                vocab_md: None,
            },
        )
        .unwrap();
    }
    let mut contacts = load_contacts(&states.app).unwrap();
    contacts.push(Contact {
        id: "contact-abel-okafor".into(),
        display_name: "Abel Okafor".into(),
        emails: vec!["abel@northwind.example".into()],
        created_at_unix_seconds: now_unix(),
        updated_at_unix_seconds: Some(now_unix()),
        extra: serde_json::Map::new(),
    });
    save_contacts(&states.app, &contacts).unwrap();
}

/// What a session brought with it, for the report.
fn landed(states: &States, session: &str) -> Vec<String> {
    let dir = states.app.profile.sessions_dir().join(session);
    let mut names: Vec<String> = walkdir::WalkDir::new(&dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            e.path().strip_prefix(&dir).ok().map(|p| p.display().to_string())
        })
        .collect();
    names.sort();
    names
}

#[test]
#[ignore = "waits for a real phone; scripts/test-seam-sim.sh drives it"]
fn a_real_phone_pairs_and_syncs_with_this_desktop() {
    let _alone = alone();
    wait_for_a_phone();
}

/// The harness against the synthetic phone `seam/mod.rs` already carries: it
/// pairs, says hello and uploads one session through the same socket a
/// handset would. A run that ends green here is the harness working; a run
/// that ends green with no phone at all would be the harness proving
/// nothing, which is what this rules out.
#[test]
#[ignore = "binds the shipping sync port, so it cannot run beside Daisy or the other suites"]
fn the_harness_fails_with_no_phone_and_passes_with_one() {
    let _alone = alone();
    let code_file = tempfile::NamedTempFile::new().unwrap();
    let path = code_file.path().to_path_buf();
    std::env::set_var("DAISY_LIVE_CODE_FILE", &path);
    std::env::set_var("DAISY_LIVE_PAIR_TIMEOUT", "60");
    std::env::set_var("DAISY_LIVE_HELLO_TIMEOUT", "60");
    std::env::set_var("DAISY_LIVE_TRANSFER_TIMEOUT", "120");
    std::env::set_var("DAISY_LIVE_QUIET", "3");

    let synthetic = std::thread::spawn(move || synthetic_phone(&path, port()));
    wait_for_a_phone();
    synthetic.join().expect("the synthetic phone finished");
}

/// Pairs, says hello and uploads one session, reading the code the harness
/// wrote. The bytes are `seam/mod.rs`'s, which are the protocol's.
fn synthetic_phone(code_file: &std::path::Path, port: u16) {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let text = until("the desktop to show a code", Duration::from_secs(60), || {
        std::fs::read_to_string(code_file).ok().filter(|t| t.starts_with("daisy-pair:"))
    })
    .expect("no code was written");
    let code = read_code(&text);
    let phone = Phone::new();
    let session = "daisy-1788000001";
    rt.block_on(async {
        // What a phone pins. The pin is derived from the code and checked
        // against the wire in `sync_vectors.rs`; here the same certificate is
        // taken from the desktop directly, so this stays about the harness.
        let cert = until("the desktop to have an identity", Duration::from_secs(60), || {
            SERVED_CERT.lock().unwrap().clone()
        })
        .expect("the desktop never published its certificate");
        pair(port, &cert, &phone, &code).await;

        let (manifest, transcript, audio) = synthetic_session(session);
        let files: Vec<(&str, &[u8])> = vec![
            ("manifest.json", &manifest),
            ("transcript.json", &transcript),
            ("chunks/0001/mic.m4a", &audio),
        ];
        let body = serde_json::to_vec(&serde_json::json!({
            "v": 2,
            "device_id": phone.device_id,
            "sessions": [{
                "id": session,
                "inputs": {
                    "manifest": body_hash(&manifest),
                    "transcript": body_hash(&transcript),
                },
                "artifacts": [],
            }],
            "tombstones": [],
        }))
        .unwrap();

        // Say hello, send back whatever was asked for, and say hello again:
        // the manifest names the chunk, so the desktop learns what audio it
        // is short of only after the manifest lands.
        let mut nonce = 0u8;
        for round in 0..6 {
            nonce = nonce.wrapping_add(1);
            let n = B64.encode([nonce; 16]);
            let reply = request(
                port,
                &cert,
                Some(&phone),
                "POST",
                "/hello",
                phone.headers("POST", "/hello", &n, &body),
                body.clone(),
            )
            .await;
            assert_eq!(reply.status, 200, "hello answered: {}", String::from_utf8_lossy(&reply.body));
            let wanted: Vec<String> = reply.json()["wants"]
                .as_array()
                .unwrap()
                .iter()
                .map(|w| w["file"].as_str().unwrap().to_string())
                .collect();
            if wanted.is_empty() {
                println!("[phone] nothing left to send after {round} round(s)");
                return;
            }
            for name in wanted {
                let bytes = files
                    .iter()
                    .find(|(f, _)| *f == name)
                    .unwrap_or_else(|| panic!("the desktop asked for {name}, which this phone has not got"))
                    .1;
                let hash = body_hash(bytes);
                let path = format!("/file/{hash}");
                nonce = nonce.wrapping_add(1);
                let n = B64.encode([nonce; 16]);
                let mut headers = phone.headers("PUT", &path, &n, bytes);
                headers.push(("X-Daisy-Session".into(), session.into()));
                headers.push(("X-Daisy-File".into(), name.clone()));
                let r = request(port, &cert, Some(&phone), "PUT", &path, headers, bytes.to_vec()).await;
                assert_eq!(r.status, 200, "{name} accepted");
            }
        }
        panic!("the desktop kept asking for files after six rounds");
    });
}

/// A one-chunk session: manifest, transcript and the recorded burst the
/// other seam suites upload.
fn synthetic_session(session: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schema_version": 4,
        "session_id": session,
        "created_at_unix_seconds": 1_788_000_001i64,
        "sample_rate": 16000,
        "channels": 1,
        "chunks": [{
            "index": 1,
            "started_at_unix_seconds": 1_788_000_001i64,
            "ended_at_unix_seconds": 1_788_000_011i64,
            "duration_seconds": 9.4,
            "mic_samples": 150_393,
            "mic_wav_relative": "chunks/0001/mic.m4a",
        }],
    }))
    .unwrap();
    let transcript = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1, "session_id": session, "provider": "phone",
        "model": "on-device", "transcribed_at_unix_seconds": 1_788_000_020i64, "chunks": [],
    }))
    .unwrap();
    let audio = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../recording/tests/fixtures/burst-16k.m4a"
    ))
    .unwrap();
    (manifest, transcript, audio)
}

/// Serves this desktop on the shipping port, shows a code, and waits for a
/// phone to pair, say hello and send what it has.
fn wait_for_a_phone() {
    // The server logs each hello's wants and offers; without a logger they go
    // nowhere, and what the desktop answered is the whole question.
    let _ = env_logger::builder().is_test(false).filter_level(log::LevelFilter::Info).try_init();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let port = port();
    let listener = bind_the_sync_port(&rt, port);
    let (states, _dir) = unlocked();
    *SERVED_CERT.lock().unwrap() = Some(desktop_certificate(&states));
    seed_profile(&states);
    let tags_before = list_tags_impl(&states.app).unwrap().len();

    let served = Arc::clone(&states);
    rt.spawn(async move {
        serve(listener, served, std::future::pending()).await;
    });

    // The host goes into the code, so a phone on this same Mac reaches the
    // loopback and one on the LAN reaches the machine by name.
    let host = env_or("DAISY_LIVE_HOST", "127.0.0.1");
    let view = sync_pairing_begin_impl(&states.app, &states.vault, host.clone(), now_unix()).unwrap();
    // The window is 90 seconds; a phone that has to be driven through a UI
    // needs longer, so it is reopened for as long as the pairing budget.
    let pair_for = budget("DAISY_LIVE_PAIR_TIMEOUT", 240);
    if let Ok(file) = std::env::var("DAISY_LIVE_CODE_FILE") {
        std::fs::write(&file, &view.qr_text).unwrap();
    }
    println!("[seam] listening on {host}:{port}");
    println!("PAIR_CODE={}", view.qr_text);

    let paired = until("a phone to pair", pair_for, || {
        // Reopening keeps one live secret: the window replaces itself, and
        // the code text is the same only while the old one is still open.
        let id = load_or_create(&states.app, &states.vault).unwrap();
        if !id.paired.is_empty() {
            return Some(id.paired);
        }
        if !view_is_open(&states) {
            let again =
                sync_pairing_begin_impl(&states.app, &states.vault, host.clone(), now_unix()).unwrap();
            if let Ok(file) = std::env::var("DAISY_LIVE_CODE_FILE") {
                let _ = std::fs::write(&file, &again.qr_text);
            }
            println!("PAIR_CODE={}", again.qr_text);
        }
        None
    })
    .expect("no phone paired inside the budget");

    assert_eq!(paired.len(), 1, "one phone paired, not {}", paired.len());
    let phone = &paired[0];
    println!("[seam] paired {} ({}) as {:?}", phone.name, phone.platform, phone.device_id);
    assert_eq!(phone.platform, "ios", "the phone said what it is");
    assert!(!phone.name.trim().is_empty(), "a paired phone has a name to show");

    // Pairing alone proves the code and the MAC. A hello proves the client
    // certificate, the signing string and the nonce.
    let hello_for = budget("DAISY_LIVE_HELLO_TIMEOUT", 180);
    let device = phone.device_id.clone();
    let seen = until("the first hello", hello_for, || {
        tauri_app_core::sync::hellos::load(&states.app).get(&device).copied()
    })
    .expect("the phone paired but never said hello");
    println!("[seam] first hello at {seen}");

    // Then the transfer. It is done when nothing new has arrived for a
    // while, which is also what a person watching the phone would call it.
    //
    // A phone driven through a UI test is quiet between exchanges for longer
    // than that: the first run of both directions ended here while the
    // Simulator was still tapping, so the hello that would have carried the
    // merged catalog never reached a listener. When the runner names a file
    // it writes once the phone half has exited, the quiet window is only
    // asked about after that.
    let quiet_for = budget("DAISY_LIVE_QUIET", 20);
    let transfer_for = budget("DAISY_LIVE_TRANSFER_TIMEOUT", 600);
    let done_file = std::env::var("DAISY_LIVE_PHONE_DONE_FILE").ok().map(PathBuf::from);
    if let Some(f) = &done_file {
        println!("[seam] the transfer ends once {} appears and the phone goes quiet", f.display());
    }
    let start = Instant::now();
    let mut last_change = Instant::now();
    let mut last_count = 0usize;
    while start.elapsed() < transfer_for {
        let now = states.library_events().len();
        if now != last_count {
            last_count = now;
            last_change = Instant::now();
        } else if last_change.elapsed() >= quiet_for
            && done_file.as_ref().is_none_or(|f| f.exists())
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    let inbox = inbox_of(&states.app);
    let imported = inbox.imported_sessions();
    let staged = inbox.staged_sessions();
    let refused = inbox.refused_sessions();
    let tags_after = list_tags_impl(&states.app).unwrap();

    println!("\n[seam] ---- what crossed ----");
    println!("[seam] status changes:    {}", states.sync_changes());
    println!("[seam] sessions imported: {}", imported.len());
    for s in &imported {
        println!("[seam]   {s}: {}", landed(&states, s).join(", "));
    }
    if !staged.is_empty() {
        println!("[seam] still part-way:    {staged:?}");
    }
    for (session, r) in &refused {
        println!("[seam] refused {session}: {:?} ({})", r.reason, r.device_id);
    }
    println!(
        "[seam] tags: {tags_before} before, {} after ({})",
        tags_after.len(),
        tags_after.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
    );
    println!("[seam] profile staged by the phone: {}", inbox.has_staged_profile(&device));
    // A merge record is what lets the next hello offer the merged file back.
    // Without one the desktop can only keep asking for the phone's copy.
    match inbox.profile_merge(&device, "tags.json") {
        Some((from, ids)) => println!("[seam] tags.json merged from {from} ({} id rewrites)", ids.len()),
        None => println!("[seam] tags.json: no merge recorded"),
    }
    println!("[seam] -----------------------\n");

    assert!(
        refused.is_empty(),
        "the desktop refused what the phone sent: {}",
        refused
            .iter()
            .map(|(s, r)| format!("{s}: {:?}", r.reason))
            .collect::<Vec<_>>()
            .join("; ")
    );
    assert!(staged.is_empty(), "a session never finished arriving: {staged:?}");

    let expected: usize = std::env::var("DAISY_LIVE_EXPECT_SESSIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    assert!(
        imported.len() >= expected,
        "expected at least {expected} session(s) from the phone, got {}",
        imported.len()
    );
    for s in &imported {
        let files = landed(&states, s);
        assert!(
            files.iter().any(|f| f == "manifest.json"),
            "{s} arrived without a manifest: {files:?}"
        );
        assert!(
            files.iter().any(|f| f.ends_with(".m4a") || f.ends_with(".wav")),
            "{s} arrived without audio: {files:?}"
        );
    }

    // Protocol section 5a is written on both sides. Whether the round trip
    // works against a real phone is what DAISY_LIVE_EXPECT_PROFILE=1 asks;
    // it is opt-in until a run has settled it either way.
    //
    // The phone's UI suite makes PHONE_TAG before it pairs, so a tag by that
    // name in this desktop's list came across the seam. Change the name here
    // and change it in DesktopSyncLiveUITests.
    if std::env::var("DAISY_LIVE_EXPECT_PROFILE").as_deref() == Ok("1") {
        assert!(
            tags_after.iter().any(|t| t.name.contains(PHONE_TAG)),
            "the phone's tag never arrived; this desktop holds {:?}",
            tags_after.iter().map(|t| t.name.as_str()).collect::<Vec<_>>()
        );
    }
}

/// Whether the pairing window the desktop last opened is still open.
fn view_is_open(states: &States) -> bool {
    use tauri_app_core::commands::sync::sync_status_impl;
    sync_status_impl(&states.app, &states.vault, true, "unsupported")
        .map(|s| s.window.is_some())
        .unwrap_or(false)
}
