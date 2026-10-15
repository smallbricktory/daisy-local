// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! A phone talking to the sync server over loopback TLS: pairing, the hello
//! round, an upload, a replayed nonce, a tombstone.
mod seam;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use seam::*;
use tauri_app_core::commands::sync::{sync_pairing_begin_impl, sync_status_impl, SYNC_PORT};
use tauri_app_core::now_unix;
use tauri_app_core::sync::signing::body_hash;

/// The settings screen reads `sync_status` and refreshes on a notification.
#[test]
fn pairing_and_hello_reach_the_settings_screen() {
    let _alone = alone();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let server_cert = desktop_certificate(&states);
        let qr = sync_pairing_begin_impl(&states.app, &states.vault, "studio-mac.local".into(), now_unix())
            .unwrap()
            .qr_text;
        let code = read_qr(&qr);
        let phone = Phone::new();

        pair(port, &server_cert, &phone, &code).await;
        assert!(states.sync_changes() >= 1, "the screen is told a phone paired");

        let hello_body = serde_json::to_vec(&serde_json::json!({
            "v": 5,
            "device_id": phone.device_id,
            "sessions": [],
            "tombstones": [],
        }))
        .unwrap();
        let nonce = B64.encode([7u8; 16]);
        let reply = request(
            port,
            &server_cert,
            Some(&phone),
            "POST",
            "/hello",
            phone.headers("POST", "/hello", &nonce, &hello_body),
            hello_body,
        )
        .await;
        assert_eq!(reply.status, 200);

        let status = sync_status_impl(&states.app, &states.vault, true, "off").unwrap();
        let paired = status.paired.first().expect("the phone is paired");
        assert_eq!(paired.device_id, phone.device_id);
        assert!(paired.last_hello_unix.is_some(), "the screen shows when it was last seen");
        assert!(states.sync_changes() >= 2, "the screen is told the phone said hello");
    });
}

#[test]
fn a_phone_pairs_uploads_and_deletes_over_mutual_tls() {
    let _alone = alone();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let phone = Phone::new();

        // The user opens the pairing screen; the phone reads the key.
        let view = sync_pairing_begin_impl(&states.app, &states.vault, "studio-mac.local".into(), now_unix()).unwrap();
        let key = read_qr(&view.qr_text);
        let server_cert = desktop_certificate(&states);
        assert_eq!(key.port, SYNC_PORT, "the QR names the port the server listens on");

        // Section 6.3: the phone proves it holds the key and confirms cD.
        let started = pair_start(port, &server_cert, &phone, &key).await;

        // Section 6.4: the phone posts its identity under the session's MAC.
        let (pair_body, mac) = pair_request(&phone, &started.secret);
        let headers = vec![("X-Daisy-Pair-Mac".into(), mac), ("X-Daisy-Pair-Session".into(), started.session_id.clone())];
        let reply = request(port, &server_cert, Some(&phone), "POST", "/pair", headers.clone(), pair_body.clone()).await;
        assert_eq!(reply.status, 200, "pairing accepted");
        let sig = reply.header("X-Daisy-Signature").expect("the reply is signed");
        ed25519_dalek::VerifyingKey::from_bytes(&started.desktop_ed25519_pub)
            .unwrap()
            .verify_strict(
                &reply.body,
                &ed25519_dalek::Signature::from_bytes(&B64.decode(sig).unwrap().try_into().unwrap()),
            )
            .expect("signed by the key /pair/start proved");
        assert_eq!(reply.json()["phone_ed25519_pub_b64"], phone.pub_b64());

        // The key ended with that acceptance: the same session pairs nobody else.
        let again = request(port, &server_cert, Some(&phone), "POST", "/pair", headers, pair_body).await;
        assert_eq!(again.status, 409, "the pairing screen is no longer open");

        // Section 5: the first hello wants everything.
        let manifest = serde_json::to_vec(&serde_json::json!({
            "chunks": [{ "mic_wav_relative": "chunks/0001/mic.m4a" }]
        }))
        .unwrap();
        let hello_body = serde_json::to_vec(&serde_json::json!({
            "v": 5,
            "device_id": phone.device_id,
            "sessions": [{
                "id": "daisy-1700000000",
                "inputs": { "manifest": body_hash(&manifest) },
                "artifacts": [],
            }],
            "tombstones": [],
        }))
        .unwrap();
        let nonce = B64.encode([1u8; 16]);
        let reply = request(
            port,
            &server_cert,
            Some(&phone),
            "POST",
            "/hello",
            phone.headers("POST", "/hello", &nonce, &hello_body),
            hello_body.clone(),
        )
        .await;
        assert_eq!(reply.status, 200);
        assert_eq!(reply.json()["wants"], serde_json::json!([{ "session": "daisy-1700000000", "file": "manifest.json" }]));
        let sig = reply.header("X-Daisy-Signature").expect("replies are signed too");
        let signed = tauri_app_core::sync::signing::reply_signing_string(200, "/hello", &nonce, &reply.body);
        ed25519_dalek::VerifyingKey::from_bytes(&started.desktop_ed25519_pub)
            .unwrap()
            .verify_strict(
                signed.as_bytes(),
                &ed25519_dalek::Signature::from_bytes(&B64.decode(sig).unwrap().try_into().unwrap()),
            )
            .expect("the reply signature covers status, path, nonce and body");

        // The same nonce twice is a replay.
        let replay = request(
            port,
            &server_cert,
            Some(&phone),
            "POST",
            "/hello",
            phone.headers("POST", "/hello", &nonce, &hello_body),
            hello_body.clone(),
        )
        .await;
        assert_eq!(replay.status, 401, "a nonce is accepted once");

        // Section 6: the manifest arrives.
        let hash = body_hash(&manifest);
        let path = format!("/file/{hash}");
        let mut headers = phone.headers("PUT", &path, &B64.encode([2u8; 16]), &manifest);
        headers.push(("X-Daisy-Session".into(), "daisy-1700000000".into()));
        headers.push(("X-Daisy-File".into(), "manifest.json".into()));
        let reply = request(port, &server_cert, Some(&phone), "PUT", &path, headers, manifest.clone()).await;
        assert_eq!(reply.status, 200);

        // Now the manifest is here, its chunk is what is missing.
        let reply = request(
            port,
            &server_cert,
            Some(&phone),
            "POST",
            "/hello",
            phone.headers("POST", "/hello", &B64.encode([3u8; 16]), &hello_body),
            hello_body.clone(),
        )
        .await;
        assert_eq!(
            reply.json()["wants"],
            serde_json::json!([{ "session": "daisy-1700000000", "file": "chunks/0001/mic.m4a" }])
        );

        // Section 7: nothing has been built for this session yet.
        let path = "/artifact/sha256:2e2cbe12ad6b2aaf5003b32705b230b93cd01352871a1d35874ecdfb7deb2806";
        let reply = request(
            port,
            &server_cert,
            Some(&phone),
            "GET",
            path,
            phone.headers("GET", path, &B64.encode([4u8; 16]), b""),
            Vec::new(),
        )
        .await;
        assert_eq!(reply.status, 404);

        // Section 8: the phone deletes the session, so the desktop does too.
        let stone = serde_json::to_vec(&tauri_app_core::sync::exchange::tombstone_record(
            "daisy-1700000000",
            1,
            &phone.device_id,
            &phone.key,
        ))
        .unwrap();
        let reply = request(
            port,
            &server_cert,
            Some(&phone),
            "POST",
            "/tombstone",
            phone.headers("POST", "/tombstone", &B64.encode([5u8; 16]), &stone),
            stone,
        )
        .await;
        assert_eq!(reply.status, 200);

        // The session is gone and its files are refused from here on.
        let hash = body_hash(&manifest);
        let path = format!("/file/{hash}");
        let mut headers = phone.headers("PUT", &path, &B64.encode([6u8; 16]), &manifest);
        headers.push(("X-Daisy-Session".into(), "daisy-1700000000".into()));
        headers.push(("X-Daisy-File".into(), "manifest.json".into()));
        let reply = request(port, &server_cert, Some(&phone), "PUT", &path, headers, manifest).await;
        assert_eq!(reply.status, 409, "a tombstoned session takes no more uploads");
    });
}

/// The mothership loop as a phone sees it: upload a whole session, watch it
/// leave the wants, take back what the desktop built, then delete it.
#[test]
fn a_whole_session_arrives_is_imported_and_comes_back_as_an_artifact() {
    let _alone = alone();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let phone = Phone::new();
        let view = sync_pairing_begin_impl(&states.app, &states.vault, "studio-mac.local".into(), now_unix()).unwrap();
        let code = read_qr(&view.qr_text);
        let server_cert = desktop_certificate(&states);
        pair(port, &server_cert, &phone, &code).await;

        let session = "daisy-1700000000";
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema_version": 4,
            "session_id": session,
            "created_at_unix_seconds": 1_700_000_000i64,
            "sample_rate": 16000,
            "channels": 1,
            "language": "fr-CA",
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
        let audio = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../recording/tests/fixtures/burst-16k.m4a")).unwrap();
        let notes = br#"{"v":1,"runs":[{"text":"Harbour budget question\n","stamp_ms":174000}],"annotations":[]}"#.to_vec();

        let hello_at = |v: u32, artifacts: serde_json::Value| {
            serde_json::to_vec(&serde_json::json!({
                "v": v,
                "device_id": phone.device_id,
                "sessions": [{
                    "id": session,
                    "inputs": {
                        "manifest": body_hash(&manifest),
                        "transcript": body_hash(&transcript),
                        "notes": body_hash(&notes),
                    },
                    "artifacts": artifacts,
                }],
                "tombstones": [],
            }))
            .unwrap()
        };
        let hello_body = |artifacts: serde_json::Value| hello_at(5, artifacts);

        // The desktop asks for the three inputs, then for the chunk the
        // manifest named, then for nothing at all.
        let mut nonce = 10u8;
        let mut next_nonce = || {
            nonce += 1;
            B64.encode([nonce; 16])
        };
        let wants = |reply: &Reply| -> Vec<String> {
            reply.json()["wants"].as_array().unwrap().iter().map(|w| w["file"].as_str().unwrap().to_string()).collect()
        };

        let body = hello_body(serde_json::json!([]));
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "POST", "/hello", phone.headers("POST", "/hello", &n, &body), body.clone()).await;
        assert_eq!(wants(&reply), ["manifest.json", "transcript.json", "notes.json"]);

        for (file, bytes) in [
            ("manifest.json", &manifest),
            ("transcript.json", &transcript),
            ("notes.json", &notes),
            ("chunks/0001/mic.m4a", &audio),
        ] {
            let hash = body_hash(bytes);
            let path = format!("/file/{hash}");
            let n = next_nonce();
            let mut headers = phone.headers("PUT", &path, &n, bytes);
            headers.push(("X-Daisy-Session".into(), session.into()));
            headers.push(("X-Daisy-File".into(), file.into()));
            let reply = request(port, &server_cert, Some(&phone), "PUT", &path, headers, bytes.to_vec()).await;
            assert_eq!(reply.status, 200, "{file} accepted");
        }

        // Everything arrived, so the session was imported: it sits in the
        // library as a phone recording and nothing is wanted for it.
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "POST", "/hello", phone.headers("POST", "/hello", &n, &body), body.clone()).await;
        assert!(wants(&reply).is_empty(), "the desktop has the session now");
        let dir = states.app.profile.session_path(session);
        // The import decodes the audio off the connection; wait for it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !dir.join("manifest.json").is_file() {
            assert!(std::time::Instant::now() < deadline, "the session was never imported");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let written: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(written["recording_kind"], "phone_room");
        assert_eq!(written["origin_device"], phone.device_id);
        assert!(dir.join("chunks/0001/mic.wav").is_file(), "the chunk was decoded to the WAV the pipeline reads");
        assert!(!dir.join("chunks/0001/mic.m4a").exists());
        assert_eq!(written["chunks"][0]["mic_wav_relative"], "chunks/0001/mic.wav");
        assert_eq!(written["single_local_speaker"], false);
        assert_eq!(std::fs::read(dir.join("notes.json")).unwrap(), notes, "the phone's notes land beside the transcript");
        // The rest of the app hears about it, so the session shows up in the
        // library without a restart.
        assert!(
            states.library_events().contains(&("Added".to_string(), session.to_string())),
            "the library was told: {:?}",
            states.library_events()
        );

        // What the pipeline writes is offered back, and downloads by its hash.
        let summary = summarize::SessionSummary::hand_written(session, "test", "# md", 1).encode();
        std::fs::write(dir.join("summary.json"), &summary).unwrap();
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "POST", "/hello", phone.headers("POST", "/hello", &n, &body), body.clone()).await;
        let offers = reply.json()["offers"].as_array().unwrap().clone();
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0]["name"], "summary.json");
        let artifact_id = offers[0]["artifact_id"].as_str().unwrap().to_string();
        assert_eq!(artifact_id, body_hash(&summary));
        let header: serde_json::Value =
            serde_json::from_slice(&B64.decode(offers[0]["header_b64"].as_str().unwrap()).unwrap()).unwrap();
        assert_eq!(header["built_from"]["transcript"], body_hash(&transcript));
        assert!(header["built_from"].get("notes").is_none(), "a note edit leaves the summary current");

        let path = format!("/artifact/{artifact_id}");
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "GET", &path, phone.headers("GET", &path, &n, b""), Vec::new()).await;
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, summary);
        assert_eq!(header["v"], 5, "the header travels at the protocol version");

        // The phone names a meta.json this desktop has never seen, and is
        // asked for it by bare session id.
        let meta_hello = |meta: Option<String>| {
            let mut entry = serde_json::json!({
                "id": session,
                "inputs": {
                    "manifest": body_hash(&manifest),
                    "transcript": body_hash(&transcript),
                    "notes": body_hash(&notes),
                },
                "artifacts": [body_hash(&summary)],
            });
            if let Some(h) = meta {
                entry["meta"] = serde_json::json!(h);
            }
            serde_json::to_vec(&serde_json::json!({
                "v": 5, "device_id": phone.device_id, "sessions": [entry], "tombstones": [],
            }))
            .unwrap()
        };
        let phone_meta = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "title": { "value": "Harbour budget", "updated_at_unix_seconds": 1_700_000_042i64 },
            "tag_ids": { "t-1": { "on": true, "updated_at_unix_seconds": 1_700_000_042i64 } },
            "dismissed_suggested_tag_ids": {},
            "speaker_maps": {},
        }))
        .unwrap();
        let phone_meta_hash = body_hash(&phone_meta);

        let b = meta_hello(Some(phone_meta_hash.clone()));
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "POST", "/hello", phone.headers("POST", "/hello", &n, &b), b).await;
        assert_eq!(
            reply.json()["meta_wants"].as_array().unwrap(),
            &vec![serde_json::json!(session)],
            "a bare session id, the file name being the same every time"
        );

        // It uploads the plaintext under the session's real id.
        let path = format!("/file/{phone_meta_hash}");
        let n = next_nonce();
        let mut headers = phone.headers("PUT", &path, &n, &phone_meta);
        headers.push(("X-Daisy-Session".into(), session.into()));
        headers.push(("X-Daisy-File".into(), "meta.json".into()));
        let reply = request(port, &server_cert, Some(&phone), "PUT", &path, headers, phone_meta.clone()).await;
        assert_eq!(reply.status, 200);

        // The next hello merges it, writes it through to the manifest the
        // rest of the app reads, and offers the settled file back.
        let b = meta_hello(Some(phone_meta_hash.clone()));
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "POST", "/hello", phone.headers("POST", "/hello", &n, &b), b).await;
        assert!(reply.json()["meta_wants"].as_array().unwrap().is_empty(), "it arrived");
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(written["title"], "Harbour budget", "the phone's title reached the manifest the UI reads");
        assert_eq!(written["tag_ids"], serde_json::json!(["t-1"]));

        // Both devices write the same values the same way, so the merge lands on bytes the phone already holds.
        assert!(
            reply.json()["meta_offers"].as_array().unwrap().is_empty(),
            "a merge that changed nothing offers nothing"
        );
        assert_eq!(
            std::fs::read(dir.join("meta.json")).unwrap(),
            phone_meta,
            "byte for byte what the phone sent"
        );

        // Now this desktop holds something the phone does not: a title typed
        // here after the phone's. The merge moves, and the result goes back
        // named by the revision it merged.
        recording::session_meta::update(&dir, |m| m.set_title("Renamed here", 1_700_000_141)).unwrap();
        let b = meta_hello(Some(phone_meta_hash.clone()));
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "POST", "/hello", phone.headers("POST", "/hello", &n, &b), b).await;
        let meta_offers = reply.json()["meta_offers"].as_array().unwrap().clone();
        assert_eq!(meta_offers.len(), 1);
        assert_eq!(meta_offers[0]["name"], "meta.json");
        assert_eq!(meta_offers[0]["session"], session, "a real session id, not the profile's");
        let header: serde_json::Value =
            serde_json::from_slice(&B64.decode(meta_offers[0]["header_b64"].as_str().unwrap()).unwrap()).unwrap();
        assert_eq!(header["merged_from"], phone_meta_hash, "so the phone installs it whole");
        assert!(header.get("ids").is_none(), "a metadata merge retires nothing");

        // And it downloads as plaintext by its hash, through section 7.
        let meta_id = meta_offers[0]["artifact_id"].as_str().unwrap().to_string();
        let path = format!("/artifact/{meta_id}");
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "GET", &path, phone.headers("GET", &path, &n, b""), Vec::new()).await;
        assert_eq!(reply.status, 200);
        assert_eq!(body_hash(&reply.body), meta_id);
        let settled: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(settled["title"]["value"], "Renamed here", "the later title won");
        assert_eq!(settled["tag_ids"]["t-1"]["on"], true, "and the phone's tagging survived it");

        // A phone carrying no meta.json for a session this desktop has none
        // of either is asked for nothing: both read the same manifest.
        let b = meta_hello(None);
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "POST", "/hello", phone.headers("POST", "/hello", &n, &b), b).await;
        assert!(reply.json()["meta_wants"].as_array().unwrap().is_empty());

        // A phone on another version is refused before anything is read,
        // whichever side is older, and the screen is told which one.
        let seen = tauri_app_core::sync::hellos::load(&states.app);
        let changes = states.sync_changes();
        for (v, older) in [(4, "Phone"), (6, "Desktop")] {
            let other = hello_at(v, serde_json::json!([]));
            let n = next_nonce();
            let reply =
                request(port, &server_cert, Some(&phone), "POST", "/hello", phone.headers("POST", "/hello", &n, &other), other).await;
            assert_eq!(reply.status, 426, "v{v}");
            assert_eq!(reply.json(), serde_json::json!({ "v": 5 }));
            assert!(reply.header("X-Daisy-Signature").is_some_and(|s| !s.is_empty()), "a 426 is signed like every reply");
            assert_eq!(tauri_app_core::sync::gate::load(&states.app).len(), 1);
            assert_eq!(states.gated.lock().unwrap().last().unwrap().1, older);
        }
        assert_eq!(tauri_app_core::sync::hellos::load(&states.app), seen, "a refused hello is not a sighting");
        assert_eq!(states.sync_changes(), changes + 2, "once per change of side");
        let again = hello_at(5, serde_json::json!([]));
        let n = next_nonce();
        request(port, &server_cert, Some(&phone), "POST", "/hello", phone.headers("POST", "/hello", &n, &again), again).await;
        assert_eq!(states.gated.lock().unwrap().len(), 2, "a retry is not told again");

        // The phone lists it as held, so it is not offered a second time.
        let held = hello_body(serde_json::json!([artifact_id]));
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "POST", "/hello", phone.headers("POST", "/hello", &n, &held), held).await;
        assert!(reply.json()["offers"].as_array().unwrap().is_empty());

        // Deleting on the phone deletes here, library copy and all.
        let stone = serde_json::to_vec(&tauri_app_core::sync::exchange::tombstone_record(session, 1, &phone.device_id, &phone.key)).unwrap();
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "POST", "/tombstone", phone.headers("POST", "/tombstone", &n, &stone), stone).await;
        assert_eq!(reply.status, 200);
        assert!(!dir.exists(), "the session left the library");
        assert!(states.library_events().contains(&("Deleted".to_string(), session.to_string())));

        // And the desktop keeps saying so while the phone still lists it.
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "POST", "/hello", phone.headers("POST", "/hello", &n, &body), body).await;
        let stones = reply.json()["tombstones"].as_array().unwrap().clone();
        assert_eq!(stones.len(), 1);
        assert_eq!(stones[0]["session"], session);
    });
}

/// A phone's recording reports progress from the first file that lands to the last step that runs.
#[test]
fn an_arriving_session_is_reported_from_the_first_file_to_the_last_step() {
    let _alone = alone();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let server_cert = desktop_certificate(&states);
        let qr = sync_pairing_begin_impl(&states.app, &states.vault, "studio-mac.local".into(), now_unix())
            .unwrap()
            .qr_text;
        let code = read_qr(&qr);
        let phone = Phone::new();
        pair(port, &server_cert, &phone, &code).await;

        let session = "daisy-1700000000";
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema_version": 4, "session_id": session, "created_at_unix_seconds": 1_700_000_000i64,
            "sample_rate": 16000, "channels": 1,
            "mic_source_id": 1, "mic_source_node_name": "phone", "mic_source_description": "iPhone mic",
            "system_source_id": 0, "system_source_node_name": "", "system_source_description": "",
            "aec_mode": "off", "title": "Harbour budget",
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
        let audio = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../recording/tests/fixtures/burst-16k.m4a")).unwrap();

        let mut nonce = 40u8;
        let mut next_nonce = || {
            nonce += 1;
            B64.encode([nonce; 16])
        };
        let put = |file: &str, bytes: &[u8], n: String| {
            let hash = body_hash(bytes);
            let path = format!("/file/{hash}");
            let mut headers = phone.headers("PUT", &path, &n, bytes);
            headers.push(("X-Daisy-Session".into(), session.into()));
            headers.push(("X-Daisy-File".into(), file.into()));
            (path, headers, bytes.to_vec())
        };
        let send = |(path, headers, body): (String, Vec<(String, String)>, Vec<u8>)| {
            let phone = &phone;
            let server_cert = &server_cert;
            async move { request(port, server_cert, Some(phone), "PUT", &path, headers, body).await }
        };

        // The manifest names one chunk, and none of it is here yet.
        assert_eq!(send(put("manifest.json", &manifest, next_nonce())).await.status, 200);
        let after_manifest = states.progress_events();
        let last = after_manifest.last().expect("the manifest landing was reported");
        assert_eq!((last.stage.as_str(), last.session.as_str()), ("Receiving", session));
        assert_eq!((last.received, last.total), (Some(0), Some(1)), "the manifest says how much audio to expect");
        assert_eq!(last.device_name, "Rowan's phone", "the phone is named the way its owner named it");

        assert_eq!(send(put("transcript.json", &transcript, next_nonce())).await.status, 200);
        assert_eq!(send(put("chunks/0001/mic.m4a", &audio, next_nonce())).await.status, 200);

        // Every stage of the import runs off the connection; wait for the last one.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !states.progress_stages(session).iter().any(|s| s == "Done") {
            assert!(
                std::time::Instant::now() < deadline,
                "the import never reported it had finished: {:?}",
                states.progress_stages(session)
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let stages = states.progress_stages(session);
        assert!(
            stages.contains(&"Receiving".to_string())
                && stages.contains(&"Importing".to_string())
                && stages.contains(&"Audio".to_string())
                && stages.contains(&"Transcript".to_string()),
            "every step of the way was reported: {stages:?}"
        );
        let receiving = states.progress_events();
        let all_audio = receiving
            .iter()
            .filter(|r| r.stage == "Receiving" && r.session == session)
            .any(|r| (r.received, r.total) == (Some(1), Some(1)));
        assert!(all_audio, "the last chunk to land was counted: {receiving:?}");
        assert_eq!(stages.last().map(String::as_str), Some("Done"), "{stages:?}");
        let done = receiving.iter().rfind(|r| r.stage == "Done").unwrap();
        assert_eq!(done.title.as_deref(), Some("Harbour budget"), "the meeting is named when it lands");
    });
}

/// A hello answers from what is on disk and never waits for work; this one arrives with a large session staged.
#[test]
fn a_hello_answers_fast_even_with_a_session_waiting_to_be_imported() {
    let _alone = alone();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let phone = Phone::new();
        let view = sync_pairing_begin_impl(&states.app, &states.vault, "studio-mac.local".into(), now_unix()).unwrap();
        let code = read_qr(&view.qr_text);
        let server_cert = desktop_certificate(&states);
        pair(port, &server_cert, &phone, &code).await;

        // Two hundred chunks of a megabyte each, all staged.
        let session = "daisy-1700000100";
        let chunks: Vec<String> = (1..=200).map(|i| format!("chunks/{i:04}/mic.m4a")).collect();
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema_version": 4,
            "session_id": session,
            "created_at_unix_seconds": 1_700_000_042i64,
            "sample_rate": 16000,
            "channels": 1,
            "chunks": chunks.iter().enumerate().map(|(n, path)| serde_json::json!({
                "index": n as u32 + 1,
                "started_at_unix_seconds": 1_700_000_042i64,
                "ended_at_unix_seconds": 1_700_000_052i64,
                "duration_seconds": 10.0,
                "mic_samples": 160_000,
                "mic_wav_relative": path,
            })).collect::<Vec<_>>(),
        }))
        .unwrap();
        let transcript = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "session_id": session, "provider": "phone",
            "model": "on-device", "transcribed_at_unix_seconds": 1_700_000_062i64, "chunks": [],
        }))
        .unwrap();
        let inbox = tauri_app_core::sync::server::inbox_of(&states.app);
        let audio = vec![7u8; 1024 * 1024];
        inbox.store(&phone.device_id, session, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        inbox.store(&phone.device_id, session, "transcript.json", &body_hash(&transcript), &transcript).unwrap();
        for path in &chunks {
            inbox.store(&phone.device_id, session, path, &body_hash(&audio), &audio).unwrap();
        }

        let body = serde_json::to_vec(&serde_json::json!({
            "v": 5,
            "device_id": phone.device_id,
            "sessions": [{
                "id": session,
                "inputs": { "manifest": body_hash(&manifest), "transcript": body_hash(&transcript) },
                "artifacts": [],
            }],
            "tombstones": [],
        }))
        .unwrap();
        let nonce = B64.encode([42u8; 16]);
        let started = std::time::Instant::now();
        let reply = request(
            port,
            &server_cert,
            Some(&phone),
            "POST",
            "/hello",
            phone.headers("POST", "/hello", &nonce, &body),
            body,
        )
        .await;
        let took = started.elapsed();
        assert_eq!(reply.status, 200);
        assert!(reply.json()["wants"].as_array().unwrap().is_empty(), "every file is already staged");
        assert!(took < std::time::Duration::from_secs(1), "a hello took {took:?}");
        eprintln!("hello answered in {took:?}");
    });
}

/// Any hello runs the import of whatever is staged and ready, so the device
/// on the connection is rarely the device whose files are being imported.
/// The recording lands under the device that sent it.
#[test]
fn a_staged_session_imports_under_the_device_that_sent_it() {
    let _alone = alone();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let server_cert = desktop_certificate(&states);
        let sender = Phone::with("11111111111111111111111111111111", 31);
        let caller = Phone::with("22222222222222222222222222222222", 32);
        for phone in [&sender, &caller] {
            let view =
                sync_pairing_begin_impl(&states.app, &states.vault, "studio-mac.local".into(), now_unix()).unwrap();
            let code = read_qr(&view.qr_text);
            pair(port, &server_cert, phone, &code).await;
        }

        let session = "daisy-1700000130";
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema_version": 4,
            "session_id": session,
            "created_at_unix_seconds": 1_700_000_072i64,
            "sample_rate": 16000,
            "channels": 1,
            "chunks": [{
                "index": 1,
                "started_at_unix_seconds": 1_700_000_072i64,
                "ended_at_unix_seconds": 1_700_000_082i64,
                "duration_seconds": 9.4,
                "mic_samples": 150_393,
                "mic_wav_relative": "chunks/0001/mic.m4a",
            }],
        }))
        .unwrap();
        let transcript = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "session_id": session, "provider": "phone",
            "model": "on-device", "transcribed_at_unix_seconds": 1_700_000_092i64, "chunks": [],
        }))
        .unwrap();
        let audio =
            std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../recording/tests/fixtures/burst-16k.m4a")).unwrap();
        let inbox = tauri_app_core::sync::server::inbox_of(&states.app);
        let sent = &sender.device_id;
        inbox.store(sent, session, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        inbox.store(sent, session, "transcript.json", &body_hash(&transcript), &transcript).unwrap();
        inbox.store(sent, session, "chunks/0001/mic.m4a", &body_hash(&audio), &audio).unwrap();

        let body = serde_json::to_vec(&serde_json::json!({
            "v": 5, "device_id": caller.device_id, "sessions": [], "tombstones": [],
        }))
        .unwrap();
        let nonce = B64.encode([43u8; 16]);
        let reply = request(
            port,
            &server_cert,
            Some(&caller),
            "POST",
            "/hello",
            caller.headers("POST", "/hello", &nonce, &body),
            body,
        )
        .await;
        assert_eq!(reply.status, 200);

        let dir = states.app.profile.session_path(session);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !(dir.join("manifest.json").is_file() && inbox.imported(sent, session).is_some()) {
            assert!(std::time::Instant::now() < deadline, "the staged session was never imported");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let written: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(written["origin_device"], *sent, "the recording is the sender's, not the caller's");
        assert!(inbox.imported(sent, session).is_some(), "and it is held for the sender");
        assert!(inbox.imported(&caller.device_id, session).is_none());
    });
}

#[test]
fn an_unpaired_phone_gets_nowhere_past_the_pairing_route() {
    let _alone = alone();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let stranger = Phone::new();
        sync_pairing_begin_impl(&states.app, &states.vault, "studio-mac.local".into(), now_unix()).unwrap();
        let server_cert = desktop_certificate(&states);

        let body = b"{}".to_vec();
        let reply = request(
            port,
            &server_cert,
            Some(&stranger),
            "POST",
            "/hello",
            stranger.headers("POST", "/hello", &B64.encode([7u8; 16]), &body),
            body.clone(),
        )
        .await;
        assert_eq!(reply.status, 401, "its certificate is in no pairing");

        // A phone with no client certificate at all cannot reach a route either.
        let reply = request(port, &server_cert, None, "POST", "/hello", Vec::new(), body).await;
        assert_eq!(reply.status, 401);
    });
}

/// A phone that does not hold the key gets nowhere: `/pair/start` refuses
/// before anything is kept, and the key stays live for whoever does.
#[test]
fn a_pair_start_under_the_wrong_key_is_refused_and_the_key_stays_live() {
    let _alone = alone();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let phone = Phone::new();
        let view = sync_pairing_begin_impl(&states.app, &states.vault, "studio-mac.local".into(), now_unix()).unwrap();
        let key = read_qr(&view.qr_text);
        let server_cert = desktop_certificate(&states);

        let mut wrong = key.clone();
        wrong.key[9] ^= 1;
        let body = start_body(&phone, &wrong.key);
        let reply = request(port, &server_cert, Some(&phone), "POST", "/pair/start", Vec::new(), body).await;
        assert_eq!(reply.status, 403);
        assert_eq!(reply.json()["error"], "bad_key");

        // Nothing was kept: the real key still opens it on the first try.
        pair(port, &server_cert, &phone, &key).await;
    });
}

/// The body's certificate must match the one presented on this connection.
#[test]
fn pair_start_refuses_a_body_certificate_that_is_not_the_connections() {
    let _alone = alone();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let phone = Phone::new();
        let other = Phone::with("11111111111111111111111111111111", 90);
        let view = sync_pairing_begin_impl(&states.app, &states.vault, "studio-mac.local".into(), now_unix()).unwrap();
        let key = read_qr(&view.qr_text);
        let server_cert = desktop_certificate(&states);

        let body = start_body_with_cert(&phone, &key.key, &other.tls_cert_der);
        let reply = request(port, &server_cert, Some(&phone), "POST", "/pair/start", Vec::new(), body).await;
        assert_eq!(reply.status, 403);
        assert_eq!(reply.json()["error"], "cert_mismatch");
    });
}

/// A `/pair` whose connection presents a different certificate than the one
/// that started the session is refused, even when the body still names the
/// phone that started it.
#[test]
fn pair_is_refused_when_the_connections_certificate_is_not_the_sessions() {
    let _alone = alone();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let phone = Phone::new();
        let impostor = Phone::with("11111111111111111111111111111111", 91);
        let view = sync_pairing_begin_impl(&states.app, &states.vault, "studio-mac.local".into(), now_unix()).unwrap();
        let key = read_qr(&view.qr_text);
        let server_cert = desktop_certificate(&states);
        let started = pair_start(port, &server_cert, &phone, &key).await;

        let (pair_body, mac) = pair_request(&phone, &started.secret);
        let headers = vec![("X-Daisy-Pair-Mac".into(), mac), ("X-Daisy-Pair-Session".into(), started.session_id)];
        let reply = request(port, &server_cert, Some(&impostor), "POST", "/pair", headers, pair_body).await;
        assert_eq!(reply.status, 403);
        assert_eq!(reply.json()["error"], "cert_mismatch");
    });
}

/// The tag catalog and the contacts travel both ways (section 5a): the
/// phone's copy is merged into this desktop's by name and by email, and the
/// merged file goes back with the ids the phone has to rewrite.
#[test]
fn the_phones_tags_and_contacts_merge_here_and_the_merged_files_go_back() {
    let _alone = alone();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let phone = Phone::new();
        let view = sync_pairing_begin_impl(&states.app, &states.vault, "studio-mac.local".into(), now_unix()).unwrap();
        let code = read_qr(&view.qr_text);
        let server_cert = desktop_certificate(&states);
        pair(port, &server_cert, &phone, &code).await;

        // This desktop: a "phsa" and a contact under one email.
        let root = states.app.profile.root().to_path_buf();
        std::fs::write(root.join("tags.json"), serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "tags": [{ "id": "d-phsa", "name": "phsa", "color_hex": "#3B4B9B", "prompt_md": "## Names\n* Brewhaven",
                       "vocab_md": null, "created_at_unix_seconds": 100, "use_count": 2 }],
        })).unwrap()).unwrap();
        std::fs::write(root.join("contacts.json"), serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "contacts": [{ "id": "d-alex", "display_name": "Alexandra Chen", "emails": ["achen@example.org"], "created_at_unix_seconds": 100 }],
        })).unwrap()).unwrap();
        // The phone: "PHSA" with nothing else, a tag of its own, and the same
        // person under two emails.
        let tags = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "tags": [
                { "id": "p-phsa", "name": "PHSA", "color_hex": "#3B4B9B", "prompt_md": null, "vocab_md": null, "created_at_unix_seconds": 200, "use_count": 1 },
                { "id": "p-ops", "name": "Ops", "color_hex": "#A03B3B", "prompt_md": null, "vocab_md": null, "created_at_unix_seconds": 210, "use_count": 1 },
            ],
        })).unwrap();
        let contacts = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "contacts": [{ "id": "p-alex", "display_name": "Alex Chen", "emails": ["ACHEN@example.org", "alex@home.example"], "created_at_unix_seconds": 200 }],
        })).unwrap();

        let mut nonce = 50u8;
        let mut next_nonce = || {
            nonce += 1;
            B64.encode([nonce; 16])
        };
        let hello_with = |profile: serde_json::Value| {
            serde_json::to_vec(&serde_json::json!({
                "v": 5, "device_id": phone.device_id, "sessions": [], "tombstones": [], "profile": profile,
            }))
            .unwrap()
        };

        let body = hello_with(serde_json::json!({ "tags": body_hash(&tags), "contacts": body_hash(&contacts) }));
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "POST", "/hello", phone.headers("POST", "/hello", &n, &body), body.clone()).await;
        assert_eq!(reply.json()["profile_wants"], serde_json::json!(["tags.json", "contacts.json"]));

        for (file, bytes) in [("tags.json", &tags), ("contacts.json", &contacts)] {
            let hash = body_hash(bytes);
            let path = format!("/file/{hash}");
            let n = next_nonce();
            let mut headers = phone.headers("PUT", &path, &n, bytes);
            headers.push(("X-Daisy-Session".into(), "profile".into()));
            headers.push(("X-Daisy-File".into(), file.into()));
            let reply = request(port, &server_cert, Some(&phone), "PUT", &path, headers, bytes.to_vec()).await;
            assert_eq!(reply.status, 200, "{file} accepted");
        }

        // The merges run off the connection; wait for both catalogs to change.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let merged = || {
            std::fs::read_to_string(root.join("tags.json")).unwrap().contains("Ops")
                && std::fs::read_to_string(root.join("contacts.json")).unwrap().contains("alex@home.example")
        };
        while !merged() {
            assert!(std::time::Instant::now() < deadline, "the phone's profile files were never merged");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let merged_tags: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("tags.json")).unwrap()).unwrap();
        let names: Vec<&str> = merged_tags["tags"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["phsa", "Ops"], "one PHSA; with no edit on either side the desktop's row stands");
        assert_eq!(merged_tags["tags"][0]["id"], "d-phsa");
        assert!(
            merged_tags["tags"].as_array().unwrap().iter().all(|t| t["use_count"] == 0),
            "a count is this library's own and travels as 0"
        );
        let merged_contacts: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("contacts.json")).unwrap()).unwrap();
        assert_eq!(merged_contacts["contacts"].as_array().unwrap().len(), 1);
        assert_eq!(merged_contacts["contacts"][0]["id"], "d-alex", "with no edit on either side the desktop's row stands");
        assert_eq!(merged_contacts["contacts"][0]["emails"], serde_json::json!(["achen@example.org", "alex@home.example"]));

        // The next hello offers both merged files, each with the ids the
        // phone has to rewrite.
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "POST", "/hello", phone.headers("POST", "/hello", &n, &body), body.clone()).await;
        assert!(reply.json()["profile_wants"].as_array().unwrap().is_empty());
        let offers = reply.json()["profile_offers"].as_array().unwrap().clone();
        assert_eq!(offers.len(), 2);
        let header = |o: &serde_json::Value| -> serde_json::Value {
            serde_json::from_slice(&B64.decode(o["header_b64"].as_str().unwrap()).unwrap()).unwrap()
        };
        let tags_offer = offers.iter().find(|o| o["name"] == "tags.json").unwrap();
        let contacts_offer = offers.iter().find(|o| o["name"] == "contacts.json").unwrap();
        assert_eq!(header(tags_offer)["ids"], serde_json::json!({ "p-phsa": "d-phsa" }));
        assert_eq!(header(tags_offer)["merged_from"], body_hash(&tags));
        assert_eq!(header(contacts_offer)["ids"], serde_json::json!({ "p-alex": "d-alex" }));

        let artifact_id = tags_offer["artifact_id"].as_str().unwrap().to_string();
        let path = format!("/artifact/{artifact_id}");
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "GET", &path, phone.headers("GET", &path, &n, b""), Vec::new()).await;
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, std::fs::read(root.join("tags.json")).unwrap());

        // Installed on the phone: its hashes are this desktop's, and the
        // exchange has nothing left to move.
        let body = hello_with(serde_json::json!({
            "tags": artifact_id,
            "contacts": contacts_offer["artifact_id"],
        }));
        let n = next_nonce();
        let reply = request(port, &server_cert, Some(&phone), "POST", "/hello", phone.headers("POST", "/hello", &n, &body), body).await;
        assert!(reply.json()["profile_wants"].as_array().unwrap().is_empty());
        assert!(reply.json()["profile_offers"].as_array().unwrap().is_empty());
    });
}

/// A hello held up on this desktop does not block other requests.
#[test]
fn a_hello_held_up_here_leaves_the_server_answering_other_requests() {
    let _alone = alone();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let phone = std::sync::Arc::new(Phone::new());
        let view = sync_pairing_begin_impl(&states.app, &states.vault, "studio-mac.local".into(), now_unix()).unwrap();
        let code = read_qr(&view.qr_text);
        let server_cert = desktop_certificate(&states);
        pair(port, &server_cert, &phone, &code).await;

        states.sync_changed_takes_ms.store(3_000, std::sync::atomic::Ordering::SeqCst);
        let held = {
            let (phone, server_cert) = (phone.clone(), server_cert.clone());
            let body = serde_json::to_vec(&serde_json::json!({
                "v": 5, "device_id": phone.device_id, "sessions": [], "tombstones": [],
            }))
            .unwrap();
            let nonce = B64.encode([51u8; 16]);
            tokio::spawn(async move {
                let headers = phone.headers("POST", "/hello", &nonce, &body);
                request(port, &server_cert, Some(&*phone), "POST", "/hello", headers, body).await.status
            })
        };
        let started = std::time::Instant::now();
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let path = format!("/artifact/{}", body_hash(b"nothing here"));
        let nonce = B64.encode([52u8; 16]);
        let reply = request(port, &server_cert, Some(&*phone), "GET", &path, phone.headers("GET", &path, &nonce, b""), Vec::new()).await;
        let took = started.elapsed();
        assert_eq!(reply.status, 404);
        assert!(took < std::time::Duration::from_millis(2_000), "the other request waited {took:?}");
        assert_eq!(held.await.unwrap(), 200);
    });
}

/// A hello or a tombstone past what either can hold is refused before it is
/// read to the end.
#[test]
fn a_hello_or_tombstone_past_its_cap_is_refused() {
    let _alone = alone();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (states, _td) = unlocked();
        let port = start(states.clone()).await;
        let phone = Phone::new();
        let view = sync_pairing_begin_impl(&states.app, &states.vault, "studio-mac.local".into(), now_unix()).unwrap();
        let code = read_qr(&view.qr_text);
        let server_cert = desktop_certificate(&states);
        pair(port, &server_cert, &phone, &code).await;

        for (path, len, n) in [("/hello", 4 * 1024 * 1024 + 1, 61u8), ("/tombstone", 64 * 1024 + 1, 62u8)] {
            let body = vec![b' '; len];
            let nonce = B64.encode([n; 16]);
            let headers = phone.headers("POST", path, &nonce, &body);
            let reply = request_declaring(port, &server_cert, Some(&phone), "POST", path, headers, len as u64).await;
            assert_eq!(reply.status, 413, "{path}");
        }
    });
}
