// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! What the import pipeline leaves in a session that arrived from a phone:
//! the same finished transcript a desktop recording gets, so nothing is left
//! for the startup integrity audit to repair.
mod keys;
use tauri_app_core::commands::integrity::audit_session;
use tauri_app_core::commands::lifecycle::init_vault_impl;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, VaultState};

const PASS: &str = "correct-horse-battery-staple-extended-22";
const SESSION: &str = "daisy-1700000000";

fn unlocked() -> (AppState, VaultState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let app = AppState::new(profile);
    let vs = VaultState::new();
    init_vault_impl(&app, &vs, PASS).unwrap();
    (app, vs, dir)
}

/// One second of 16 kHz mono silence as a WAV file.
fn silent_wav() -> Vec<u8> {
    let samples = 16_000u32;
    let data_len = samples * 2;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&16_000u32.to_le_bytes());
    b.extend_from_slice(&32_000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    b.resize(44 + data_len as usize, 0);
    b
}

/// A phone recording as the import lands it: a mic chunk decoded to WAV, the
/// phone's transcript, and a finalized manifest naming the phone.
fn landed_phone_session(app: &AppState, text: &str) -> std::path::PathBuf {
    let dir = app.profile.session_path(SESSION);
    std::fs::create_dir_all(dir.join("chunks/0001")).unwrap();
    std::fs::write(dir.join("chunks/0001/mic.wav"), silent_wav()).unwrap();
    let manifest = serde_json::json!({
        "schema_version": 4,
        "session_id": SESSION,
        "created_at_unix_seconds": 1_700_000_000i64,
        "sample_rate": 16000,
        "channels": 1,
        "chunks": [{
            "index": 1,
            "started_at_unix_seconds": 1_700_000_000i64,
            "ended_at_unix_seconds": 1_700_000_001i64,
            "duration_seconds": 1.0,
            "mic_samples": 16_000,
            "mic_wav_relative": "chunks/0001/mic.wav",
        }],
        "finalized_at_unix_seconds": 1_700_000_002i64,
        "recording_kind": "phone_room",
        "origin_device": "9e0e2893b2dfa75593a633e92892a5d8",
        "single_local_speaker": false,
    });
    std::fs::write(dir.join("manifest.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    let transcript = serde_json::json!({
        "schema_version": 1,
        "session_id": SESSION,
        "provider": "phone",
        "model": "on-device",
        "transcribed_at_unix_seconds": 1_700_000_002i64,
        "chunks": [{
            "chunk_index": 1,
            "tracks": [{
                "track": "mic",
                "source_wav_relative": "chunks/0001/mic.wav",
                "segments": [{
                    "start_ms": 0,
                    "end_ms": 900,
                    "text": text,
                    "confidence": null,
                }],
            }],
        }],
    });
    std::fs::write(dir.join("transcript.json"), serde_json::to_vec_pretty(&transcript).unwrap()).unwrap();
    dir
}

#[test]
fn a_phone_session_leaves_the_pipeline_with_its_finished_transcript() {
    let _k = keys::unlocked();
    let (app, vs, _d) = unlocked();
    let dir = landed_phone_session(&app, "the harbour shit is late again");

    tauri_app_core::sync::pipeline::run(&app, &vs, SESSION, &|_| {});

    assert!(dir.join("transcript.dedup.json").is_file(), "the finished transcript was written");
    let md = String::from_utf8(sealfile::fs::read(dir.join("transcript.md")).unwrap()).unwrap();
    assert!(md.contains("harbour s*** is late again"), "the rendered transcript is masked: {md}");
    assert!(!md.contains("shit"), "the rendered transcript is masked: {md}");
    assert!(audit_session(&dir, false).is_empty(), "nothing is left for the startup audit to repair");
}
