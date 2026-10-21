// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The two-device sync matrix and hostile inputs: unpairing while a phone is mid-conversation,
//! re-pairing after a reset, a second phone on the same desktop, a phone
//! killed part-way through an upload, and every way a request can be wrong
//! that must answer rather than crash.
//!
//! `sync_transport.rs` walks the happy path. Nothing is repeated here.
mod seam;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use seam::*;
use tauri_app_core::commands::sync::{
    sync_pairing_begin_impl, sync_status_impl, sync_unpair_impl,
};
use tauri_app_core::now_unix;
use tauri_app_core::sync::signing::body_hash;

/// Opens a pairing window and returns the key a phone would read.
fn open_key(states: &States) -> Key {
    read_qr(
        &sync_pairing_begin_impl(
            &states.app,
            &states.vault,
            "studio-mac.local".into(),
            now_unix(),
        )
        .unwrap()
        .qr_text,
    )
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Nonces have to differ per request; a counter is what a phone would keep.
struct Nonces(u8);

impl Nonces {
    fn next(&mut self) -> String {
        self.0 = self.0.wrapping_add(1);
        B64.encode([self.0; 16])
    }
}

fn hello_body(phone: &Phone, sessions: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "v": 5,
        "device_id": phone.device_id,
        "sessions": sessions,
        "tombstones": [],
    }))
    .unwrap()
}

async fn hello(port: u16, cert: &[u8], phone: &Phone, n: &mut Nonces, body: &[u8]) -> Reply {
    let nonce = n.next();
    request(
        port,
        cert,
        Some(phone),
        "POST",
        "/hello",
        phone.headers("POST", "/hello", &nonce, body),
        body.to_vec(),
    )
    .await
}

async fn put_file(
    port: u16,
    cert: &[u8],
    phone: &Phone,
    n: &mut Nonces,
    session: &str,
    file: &str,
    bytes: &[u8],
) -> Reply {
    put_file_named(port, cert, phone, n, session, file, &body_hash(bytes), bytes).await
}

/// The upload with the hash in the path chosen by the caller, so a mismatch
/// can be sent deliberately.
async fn put_file_named(
    port: u16,
    cert: &[u8],
    phone: &Phone,
    n: &mut Nonces,
    session: &str,
    file: &str,
    hash: &str,
    bytes: &[u8],
) -> Reply {
    let path = format!("/file/{hash}");
    let nonce = n.next();
    let mut headers = phone.headers("PUT", &path, &nonce, bytes);
    headers.push(("X-Daisy-Session".into(), session.into()));
    headers.push(("X-Daisy-File".into(), file.into()));
    request(port, cert, Some(phone), "PUT", &path, headers, bytes.to_vec()).await
}

fn wants(reply: &Reply) -> Vec<String> {
    reply.json()["wants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["file"].as_str().unwrap().to_string())
        .collect()
}

/// A one-chunk session's three inputs.
fn session_files(session: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schema_version": 4,
        "session_id": session,
        "created_at_unix_seconds": 1_700_000_000i64,
        "sample_rate": 16000,
        "channels": 1,
        "chunks": [{
            "index": 1,
            "started_at_unix_seconds": 1_700_000_000i64,
            "ended_at_unix_seconds": 1_700_000_010i64,
            "duration_seconds": 9.4,
            "mic_samples": 150_393,
            "mic_wav_relative": "chunks/0001/mic.m4a",
        }],
    }))
    .unwrap();
    let transcript = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1, "session_id": session, "provider": "phone",
        "model": "on-device", "transcribed_at_unix_seconds": 1_700_000_012i64, "chunks": [],
    }))
    .unwrap();
    let audio = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../recording/tests/fixtures/burst-16k.m4a"
    ))
    .unwrap();
    (manifest, transcript, audio)
}

// ---- unpairing and re-pairing ---------------------------------------------

/// After unpairing, the very next request from the phone is refused.
#[test]
fn a_phone_unpaired_mid_conversation_is_refused_at_once() {
    let _alone = alone();
    rt().block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let cert = desktop_certificate(&states);
        let phone = Phone::new();
        let code = open_key(&states);
        pair(port, &cert, &phone, &code).await;
        let mut n = Nonces(0);

        let body = hello_body(&phone, serde_json::json!([]));
        assert_eq!(hello(port, &cert, &phone, &mut n, &body).await.status, 200);

        let status = sync_unpair_impl(&states.app, &states.vault, &phone.device_id, true, "off")
            .unwrap();
        assert!(status.paired.is_empty(), "the screen shows no phone");

        let after = hello(port, &cert, &phone, &mut n, &body).await;
        assert_eq!(after.status, 401, "the unpaired phone gets nowhere");

        // Its uploads and downloads go the same way.
        let put = put_file(port, &cert, &phone, &mut n, "daisy-1700000000", "notes.json", b"{}").await;
        assert_eq!(put.status, 401);
        let nonce = n.next();
        let art = request(
            port,
            &cert,
            Some(&phone),
            "GET",
            "/artifact/abc",
            phone.headers("GET", "/artifact/abc", &nonce, b""),
            Vec::new(),
        )
        .await;
        assert_eq!(art.status, 401);
    });
}

/// A phone reset and paired again is a new key under the same device id. The old key stops working when the new one is accepted.
#[test]
fn re_pairing_replaces_the_key_and_the_old_one_stops_working() {
    let _alone = alone();
    rt().block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let cert = desktop_certificate(&states);
        let old = Phone::new();
        pair(port, &cert, &old, &open_key(&states)).await;
        let mut n = Nonces(0);
        let old_body = hello_body(&old, serde_json::json!([]));
        assert_eq!(hello(port, &cert, &old, &mut n, &old_body).await.status, 200);

        // Same device id, new signing key and new client certificate.
        let fresh = Phone::with(&old.device_id, 99);
        assert_ne!(fresh.pub_b64(), old.pub_b64());
        sync_unpair_impl(&states.app, &states.vault, &old.device_id, true, "off").unwrap();
        pair(port, &cert, &fresh, &open_key(&states)).await;

        let fresh_body = hello_body(&fresh, serde_json::json!([]));
        assert_eq!(
            hello(port, &cert, &fresh, &mut n, &fresh_body).await.status,
            200,
            "the phone that just paired is let in"
        );
        assert_eq!(
            hello(port, &cert, &old, &mut n, &old_body).await.status,
            401,
            "the identity it replaced is not"
        );
        let status = sync_status_impl(&states.app, &states.vault, true, "off").unwrap();
        assert_eq!(status.paired.len(), 1, "one phone, not two rows for one device");
        assert_eq!(status.paired[0].device_id, fresh.device_id);
    });
}

/// Nonces are spent per phone and per device. Unpairing forgets them.
#[test]
fn a_re_paired_phone_may_reuse_the_nonces_the_old_one_spent() {
    let _alone = alone();
    rt().block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let cert = desktop_certificate(&states);
        let old = Phone::new();
        pair(port, &cert, &old, &open_key(&states)).await;

        let old_body = hello_body(&old, serde_json::json!([]));
        let nonce = B64.encode([3u8; 16]);
        let first = request(
            port,
            &cert,
            Some(&old),
            "POST",
            "/hello",
            old.headers("POST", "/hello", &nonce, &old_body),
            old_body.clone(),
        )
        .await;
        assert_eq!(first.status, 200);
        let replay = request(
            port,
            &cert,
            Some(&old),
            "POST",
            "/hello",
            old.headers("POST", "/hello", &nonce, &old_body),
            old_body.clone(),
        )
        .await;
        assert_eq!(replay.status, 401, "the same nonce twice is a replay");

        sync_unpair_impl(&states.app, &states.vault, &old.device_id, true, "off").unwrap();
        let fresh = Phone::with(&old.device_id, 77);
        pair(port, &cert, &fresh, &open_key(&states)).await;
        let fresh_body = hello_body(&fresh, serde_json::json!([]));
        let reply = request(
            port,
            &cert,
            Some(&fresh),
            "POST",
            "/hello",
            fresh.headers("POST", "/hello", &nonce, &fresh_body),
            fresh_body,
        )
        .await;
        assert_eq!(
            reply.status, 200,
            "the phone that paired again starts its nonces over and is not refused"
        );
    });
}

// ---- two phones ------------------------------------------------------------

/// A household with two phones. Each pairs on its own code, each is answered
/// for its own sessions, and unpairing one leaves the other alone.
#[test]
fn two_phones_pair_separately_and_do_not_see_each_others_work() {
    let _alone = alone();
    rt().block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let cert = desktop_certificate(&states);
        let rowan = Phone::with("4a68f72f5a22110b2bfbe71b6588833c", 21);
        let sam = Phone::with("b1c2d3e4f5061728394a5b6c7d8e9f00", 42);

        pair(port, &cert, &rowan, &open_key(&states)).await;
        pair(port, &cert, &sam, &open_key(&states)).await;
        let status = sync_status_impl(&states.app, &states.vault, true, "off").unwrap();
        assert_eq!(status.paired.len(), 2, "both phones are listed");

        let mut n = Nonces(0);
        // Rowan's phone has a session; Sam's has none.
        let (manifest, transcript, _audio) = session_files("daisy-1700000000");
        let rowan_body = hello_body(
            &rowan,
            serde_json::json!([{
                "id": "daisy-1700000000",
                "inputs": { "manifest": body_hash(&manifest), "transcript": body_hash(&transcript) },
                "artifacts": [],
            }]),
        );
        let sam_body = hello_body(&sam, serde_json::json!([]));

        let r = hello(port, &cert, &rowan, &mut n, &rowan_body).await;
        assert_eq!(wants(&r), ["manifest.json", "transcript.json"]);
        let s = hello(port, &cert, &sam, &mut n, &sam_body).await;
        assert!(wants(&s).is_empty(), "Sam's phone is asked for nothing it does not have");

        // One phone's signature does not carry the other's device header.
        let nonce = n.next();
        let mut headers = sam.headers("POST", "/hello", &nonce, &rowan_body);
        headers.retain(|(k, _)| k != "X-Daisy-Device");
        headers.push(("X-Daisy-Device".into(), rowan.device_id.clone()));
        let crossed = request(port, &cert, Some(&sam), "POST", "/hello", headers, rowan_body.clone()).await;
        assert_eq!(
            crossed.status, 401,
            "Sam's certificate signing as Rowan's device is refused"
        );

        sync_unpair_impl(&states.app, &states.vault, &sam.device_id, true, "off").unwrap();
        assert_eq!(
            hello(port, &cert, &sam, &mut n, &sam_body).await.status,
            401
        );
        assert_eq!(
            hello(port, &cert, &rowan, &mut n, &rowan_body).await.status,
            200,
            "unpairing one phone leaves the other paired"
        );
    });
}

// ---- an interrupted upload -------------------------------------------------

/// The phone's app is killed part-way through an upload. When it comes back the desktop asks for what is missing and nothing else.
#[test]
fn a_phone_killed_part_way_through_resumes_where_it_stopped() {
    let _alone = alone();
    rt().block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let cert = desktop_certificate(&states);
        let phone = Phone::new();
        pair(port, &cert, &phone, &open_key(&states)).await;
        let mut n = Nonces(0);

        let session = "daisy-1700000000";
        let (manifest, transcript, audio) = session_files(session);
        let body = hello_body(
            &phone,
            serde_json::json!([{
                "id": session,
                "inputs": { "manifest": body_hash(&manifest), "transcript": body_hash(&transcript) },
                "artifacts": [],
            }]),
        );

        let first = hello(port, &cert, &phone, &mut n, &body).await;
        assert_eq!(wants(&first), ["manifest.json", "transcript.json"]);

        // It gets the manifest up and then the app is killed.
        assert_eq!(
            put_file(port, &cert, &phone, &mut n, session, "manifest.json", &manifest).await.status,
            200
        );

        // Back on the next wake: the transcript is still wanted, the manifest
        // is not, and the chunk the manifest named has joined the list.
        let second = hello(port, &cert, &phone, &mut n, &body).await;
        let asked = wants(&second);
        assert!(
            !asked.contains(&"manifest.json".to_string()),
            "the manifest that arrived is not asked for again: {asked:?}"
        );
        assert!(asked.contains(&"transcript.json".to_string()), "{asked:?}");
        assert!(
            asked.contains(&"chunks/0001/mic.m4a".to_string()),
            "the chunk the manifest named is asked for once it can be: {asked:?}"
        );

        // It finishes on the third wake and the session imports.
        assert_eq!(
            put_file(port, &cert, &phone, &mut n, session, "transcript.json", &transcript).await.status,
            200
        );
        assert_eq!(
            put_file(port, &cert, &phone, &mut n, session, "chunks/0001/mic.m4a", &audio).await.status,
            200
        );
        let third = hello(port, &cert, &phone, &mut n, &body).await;
        assert!(wants(&third).is_empty(), "nothing is left to send: {:?}", wants(&third));

        let dir = states.app.profile.session_path(session);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !dir.join("manifest.json").is_file() {
            assert!(std::time::Instant::now() < deadline, "the resumed session never imported");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(dir.join("chunks/0001/mic.wav").is_file());
    });
}

/// An upload interrupted by the connection dying leaves nothing half-written:
/// the file is named by its own hash, so bytes that do not hash to it are
/// refused and the session stays as it was.
#[test]
fn bytes_that_do_not_match_the_hash_in_the_path_are_refused() {
    let _alone = alone();
    rt().block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let cert = desktop_certificate(&states);
        let phone = Phone::new();
        pair(port, &cert, &phone, &open_key(&states)).await;
        let mut n = Nonces(0);

        let session = "daisy-1700000000";
        let (manifest, transcript, _audio) = session_files(session);
        let body = hello_body(
            &phone,
            serde_json::json!([{
                "id": session,
                "inputs": { "manifest": body_hash(&manifest), "transcript": body_hash(&transcript) },
                "artifacts": [],
            }]),
        );

        // A truncated manifest under the whole manifest's hash: what a
        // connection dying mid-body would deliver.
        let truncated = &manifest[..manifest.len() / 2];
        let reply = put_file_named(
            port,
            &cert,
            &phone,
            &mut n,
            session,
            "manifest.json",
            &body_hash(&manifest),
            truncated,
        )
        .await;
        assert_eq!(reply.status, 400, "the desktop checks what it was sent");

        // And the desktop still wants it, so the phone sends it again.
        let after = hello(port, &cert, &phone, &mut n, &body).await;
        assert!(
            wants(&after).contains(&"manifest.json".to_string()),
            "a refused upload leaves the file wanted: {:?}",
            wants(&after)
        );
        assert_eq!(
            put_file(port, &cert, &phone, &mut n, session, "manifest.json", &manifest).await.status,
            200,
            "and the whole file is taken"
        );
    });
}

/// A file named something the desktop does not accept must not reach the
/// filesystem, whatever it hashes to.
#[test]
fn a_file_named_outside_the_session_is_refused() {
    let _alone = alone();
    rt().block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let cert = desktop_certificate(&states);
        let phone = Phone::new();
        pair(port, &cert, &phone, &open_key(&states)).await;
        let mut n = Nonces(0);
        let payload = b"not a manifest".to_vec();

        for name in [
            "../../../../etc/daisy-owned",
            "chunks/../../escape.json",
            "/absolute.json",
            "",
        ] {
            let reply = put_file(
                port,
                &cert,
                &phone,
                &mut n,
                "daisy-1700000000",
                name,
                &payload,
            )
            .await;
            assert_eq!(reply.status, 400, "{name:?} should be refused");
        }
        for name in ["../../../../etc/daisy-owned", "chunks/../../escape.json"] {
            let escaped = states.app.profile.root().join(name);
            assert!(!escaped.exists(), "{name:?} must not have been written");
        }
    });
}

// ---- malformed and hostile requests ---------------------------------------

/// Every one of these has to answer.
#[test]
fn a_malformed_hello_is_answered_rather_than_fatal() {
    let _alone = alone();
    rt().block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let cert = desktop_certificate(&states);
        let phone = Phone::new();
        pair(port, &cert, &phone, &open_key(&states)).await;
        let mut n = Nonces(0);

        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("not JSON at all", b"<html>".to_vec()),
            ("an empty body", Vec::new()),
            (
                "version zero",
                serde_json::to_vec(&serde_json::json!({
                    "v": 0, "device_id": phone.device_id, "sessions": [], "tombstones": [],
                }))
                .unwrap(),
            ),
            (
                "a device id that is not the signer",
                serde_json::to_vec(&serde_json::json!({
                    "v": 5, "device_id": "0000000000000000000000000000dead",
                    "sessions": [], "tombstones": [],
                }))
                .unwrap(),
            ),
            (
                "sessions that are not a list",
                serde_json::to_vec(&serde_json::json!({
                    "v": 5, "device_id": phone.device_id, "sessions": {}, "tombstones": [],
                }))
                .unwrap(),
            ),
        ];
        for (what, body) in cases {
            let reply = hello(port, &cert, &phone, &mut n, &body).await;
            assert_eq!(reply.status, 400, "{what} should be a bad request");
        }

        // A session id that is not one is skipped; the rest of the hello is served.
        let mixed = hello_body(
            &phone,
            serde_json::json!([
                { "id": "../../etc/passwd", "inputs": { "manifest": "abc" }, "artifacts": [] },
                { "id": "daisy-1700000000", "inputs": { "manifest": "def" }, "artifacts": [] },
            ]),
        );
        let reply = hello(port, &cert, &phone, &mut n, &mixed).await;
        assert_eq!(reply.status, 200);
        let asked = reply.json()["wants"].as_array().unwrap().clone();
        assert_eq!(asked.len(), 1, "only the real session is asked about");
        assert_eq!(asked[0]["session"], "daisy-1700000000");

        // The server is still serving after all of it.
        let good = hello_body(&phone, serde_json::json!([]));
        assert_eq!(hello(port, &cert, &phone, &mut n, &good).await.status, 200);
    });
}

/// A signed request needs all three headers, a signature that verifies, and a
/// route that exists. Each failure is its own answer.
#[test]
fn requests_that_are_not_signed_properly_get_nowhere() {
    let _alone = alone();
    rt().block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let cert = desktop_certificate(&states);
        let phone = Phone::new();
        pair(port, &cert, &phone, &open_key(&states)).await;
        let mut n = Nonces(0);
        let body = hello_body(&phone, serde_json::json!([]));

        // Each header removed in turn.
        for missing in ["X-Daisy-Device", "X-Daisy-Nonce", "X-Daisy-Signature"] {
            let nonce = n.next();
            let mut headers = phone.headers("POST", "/hello", &nonce, &body);
            headers.retain(|(k, _)| k != missing);
            let reply = request(port, &cert, Some(&phone), "POST", "/hello", headers, body.clone()).await;
            assert_eq!(reply.status, 401, "a request with no {missing} is refused");
        }

        // A signature over a different body.
        let nonce = n.next();
        let headers = phone.headers("POST", "/hello", &nonce, b"something else");
        let reply = request(port, &cert, Some(&phone), "POST", "/hello", headers, body.clone()).await;
        assert_eq!(reply.status, 401, "the signature covers the body");

        // A signature over a different path.
        let nonce = n.next();
        let headers = phone.headers("POST", "/tombstone", &nonce, &body);
        let reply = request(port, &cert, Some(&phone), "POST", "/hello", headers, body.clone()).await;
        assert_eq!(reply.status, 401, "the signature covers the path");

        // A signature that is not base64, and one that is the wrong length.
        for bad in ["not base64 at all", "AAAA"] {
            let nonce = n.next();
            let mut headers = phone.headers("POST", "/hello", &nonce, &body);
            headers.retain(|(k, _)| k != "X-Daisy-Signature");
            headers.push(("X-Daisy-Signature".into(), bad.into()));
            let reply = request(port, &cert, Some(&phone), "POST", "/hello", headers, body.clone()).await;
            assert_eq!(reply.status, 401, "{bad:?} is not a signature");
        }

        // A route that does not exist, properly signed.
        let nonce = n.next();
        let reply = request(
            port,
            &cert,
            Some(&phone),
            "POST",
            "/nope",
            phone.headers("POST", "/nope", &nonce, b""),
            Vec::new(),
        )
        .await;
        assert_eq!(reply.status, 404);

        // An artifact that was never offered.
        let path = "/artifact/0000000000000000000000000000000000000000000000000000000000000000";
        let nonce = n.next();
        let reply = request(
            port,
            &cert,
            Some(&phone),
            "GET",
            path,
            phone.headers("GET", path, &nonce, b""),
            Vec::new(),
        )
        .await;
        assert_eq!(reply.status, 404);

        // And after all of that the phone still works.
        assert_eq!(hello(port, &cert, &phone, &mut n, &body).await.status, 200);
    });
}

/// A tombstone is a deletion. Only the phone that owns the session can sign one.
#[test]
fn a_tombstone_signed_by_another_phone_is_refused() {
    let _alone = alone();
    rt().block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let cert = desktop_certificate(&states);
        let rowan = Phone::with("4a68f72f5a22110b2bfbe71b6588833c", 21);
        let sam = Phone::with("b1c2d3e4f5061728394a5b6c7d8e9f00", 42);
        pair(port, &cert, &rowan, &open_key(&states)).await;
        pair(port, &cert, &sam, &open_key(&states)).await;
        let mut n = Nonces(0);

        let session = "daisy-1700000000";
        // Sam's phone signs a tombstone with Rowan's key inside the record but
        // sends it under its own identity.
        let forged = serde_json::to_vec(&tauri_app_core::sync::exchange::tombstone_record(
            session,
            1,
            &rowan.device_id,
            &rowan.key,
        ))
        .unwrap();
        let nonce = n.next();
        let reply = request(
            port,
            &cert,
            Some(&sam),
            "POST",
            "/tombstone",
            sam.headers("POST", "/tombstone", &nonce, &forged),
            forged,
        )
        .await;
        assert_eq!(
            reply.status, 403,
            "a deletion has to be signed by the phone that sent it"
        );

        // Garbage in the same place is a bad request, not a deletion.
        let nonce = n.next();
        let junk = b"{}".to_vec();
        let reply = request(
            port,
            &cert,
            Some(&sam),
            "POST",
            "/tombstone",
            sam.headers("POST", "/tombstone", &nonce, &junk),
            junk,
        )
        .await;
        assert_eq!(reply.status, 400);
    });
}

/// A session the user deleted stays deleted when the phone re-sends its files.
#[test]
fn an_entombed_session_will_not_take_uploads_again() {
    let _alone = alone();
    rt().block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let cert = desktop_certificate(&states);
        let phone = Phone::new();
        pair(port, &cert, &phone, &open_key(&states)).await;
        let mut n = Nonces(0);

        let session = "daisy-1700000000";
        let (manifest, _transcript, _audio) = session_files(session);
        let stone = serde_json::to_vec(&tauri_app_core::sync::exchange::tombstone_record(
            session,
            1,
            &phone.device_id,
            &phone.key,
        ))
        .unwrap();
        let nonce = n.next();
        let reply = request(
            port,
            &cert,
            Some(&phone),
            "POST",
            "/tombstone",
            phone.headers("POST", "/tombstone", &nonce, &stone),
            stone,
        )
        .await;
        assert_eq!(reply.status, 200);

        let put = put_file(port, &cert, &phone, &mut n, session, "manifest.json", &manifest).await;
        assert_eq!(put.status, 409, "the deleted session takes nothing back");

        // And the hello says so.
        let body = hello_body(
            &phone,
            serde_json::json!([{
                "id": session,
                "inputs": { "manifest": body_hash(&manifest) },
                "artifacts": [],
            }]),
        );
        let reply = hello(port, &cert, &phone, &mut n, &body).await;
        assert!(wants(&reply).is_empty(), "nothing is asked for a deleted session");
        assert_eq!(reply.json()["tombstones"].as_array().unwrap().len(), 1);
    });
}

/// The pairing window is one key for one phone until it is used, cancelled or
/// ten minutes pass. Cancelling it closes it, and a key from a window that
/// was cancelled pairs nobody.
#[test]
fn a_cancelled_pairing_key_pairs_nobody() {
    let _alone = alone();
    rt().block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let cert = desktop_certificate(&states);
        let phone = Phone::new();
        let key = open_key(&states);
        tauri_app_core::commands::sync::sync_pairing_cancel_impl(&states.app).unwrap();

        let body = start_body(&phone, &key.key);
        let reply = request(port, &cert, Some(&phone), "POST", "/pair/start", Vec::new(), body).await;
        assert_eq!(reply.status, 409, "the pairing screen is closed");
        assert_eq!(reply.json()["error"], "closed");
        assert!(
            sync_status_impl(&states.app, &states.vault, true, "off")
                .unwrap()
                .paired
                .is_empty()
        );
    });
}

/// A `/pair/start` request that is not shaped like one, and a `/pair` with no
/// MAC or session at all.
#[test]
fn a_pair_request_that_is_not_one_is_answered_rather_than_fatal() {
    let _alone = alone();
    rt().block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let cert = desktop_certificate(&states);
        let phone = Phone::new();
        let key = open_key(&states);

        // Garbage where the start body should be.
        let reply = request(port, &cert, Some(&phone), "POST", "/pair/start", Vec::new(), b"<not json>".to_vec()).await;
        assert_eq!(reply.status, 400);

        // No MAC and no session header on /pair.
        let reply = request(port, &cert, Some(&phone), "POST", "/pair", Vec::new(), b"{}".to_vec()).await;
        assert_eq!(reply.status, 400);

        // The window is still open after both, so the real phone still pairs.
        pair(port, &cert, &phone, &key).await;
        assert_eq!(
            sync_status_impl(&states.app, &states.vault, true, "off")
                .unwrap()
                .paired
                .len(),
            1
        );
    });
}
