// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The one-voice gate a diarization run applies to the clusters it just made.
//!
//! After clustering, the run samples each cluster's enrollment audio, measures
//! how far apart two different people sit in this session, and marks the
//! clusters that do not read as one clear voice. Both land in the manifest —
//! `speaker_separation` and `mixed_clusters` — and the Speakers screen reads
//! them. The session here is real WAVs through the real speaker encoder,
//! because what the embeddings say is the gate's whole subject.
use std::fs;

use recording::manifest::SessionManifest;
use tauri_app_core::commands::voiceprints::diarize_session_impl;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::AppState;

mod voices;
use voices::{room_tone, rowan, zoe, RATE};

const SESSION: &str = "daisy-1788200000";
const SECONDS: u32 = 90;

/// `DAISY_VOICEPRINT_DIR` is process-wide, so these tests take turns.
fn alone() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn wav(path: &std::path::Path, pcm: &[i16]) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for s in pcm {
        w.write_sample(*s).unwrap();
    }
    w.finalize().unwrap();
}

/// Rowan and Zoe taking ten-second turns twelve seconds apart — clear of the
/// isolation margin, so every turn can be sampled. `(start_ms, is_rowan)`.
const TURNS: &[(u32, bool)] = &[
    (0, true),
    (12_000, false),
    (24_000, true),
    (36_000, false),
    (48_000, true),
    (60_000, false),
];
const TURN_SECONDS: usize = 10;

/// A finished mic-only session: every turn carries its own speaker's voice,
/// room tone in between, and no speaker ids at all — the run assigns those.
fn session(app: &AppState) {
    let root = app.profile.session_path(SESSION);
    let chunk = root.join("chunks/0000");
    fs::create_dir_all(&chunk).unwrap();

    let total = RATE as usize * SECONDS as usize;
    let mut mic = room_tone(total);
    for &(start_ms, is_rowan) in TURNS {
        let from = start_ms as usize * RATE as usize / 1000;
        let len = TURN_SECONDS * RATE as usize;
        let pcm = if is_rowan { rowan(len) } else { zoe(len) };
        mic[from..from + len].copy_from_slice(&pcm);
    }
    wav(&chunk.join("mic.wav"), &mic);

    let manifest = serde_json::json!({
        "schema_version": 4,
        "session_id": SESSION,
        "created_at_unix_seconds": 1_788_200_000i64,
        "sample_rate": RATE,
        "channels": 1,
        "finalized_at_unix_seconds": 1_788_200_000i64 + SECONDS as i64,
        "chunks": [{
            "index": 0,
            "started_at_unix_seconds": 1_788_200_000i64,
            "ended_at_unix_seconds": 1_788_200_000i64 + SECONDS as i64,
            "duration_seconds": SECONDS,
            "mic_wav_relative": "chunks/0000/mic.wav",
            "system_wav_relative": "chunks/0000/system.wav",
            "mic_aec_wav_relative": null,
        }],
        "attendees": [],
        "tag_ids": [],
    });
    fs::write(root.join("manifest.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();

    let segs: Vec<serde_json::Value> = TURNS
        .iter()
        .map(|&(start_ms, _)| {
            serde_json::json!({
                "start_ms": start_ms,
                "end_ms": start_ms + TURN_SECONDS as u32 * 1_000,
                "text": "the rebate import is the first thing",
                "confidence": null,
                "speaker_id": null,
            })
        })
        .collect();
    let transcript = serde_json::json!({
        "schema_version": 1,
        "session_id": SESSION,
        "provider": "whisper",
        "model": "base.en",
        "transcribed_at_unix_seconds": 1_788_200_000i64 + SECONDS as i64 + 10,
        "chunks": [{
            "chunk_index": 0,
            "tracks": [{
                "track": "mic",
                "source_wav_relative": "chunks/0000/mic.wav",
                "segments": segs,
            }],
        }],
    });
    fs::write(root.join("transcript.json"), serde_json::to_vec_pretty(&transcript).unwrap())
        .unwrap();
}

fn manifest_of(app: &AppState) -> SessionManifest {
    let p = app.profile.session_path(SESSION).join("manifest.json");
    serde_json::from_slice(&fs::read(p).unwrap()).unwrap()
}

/// A profile holding the session, or `None` when the speaker-encoder weights
/// are not staged in this checkout — the signal to skip rather than assert
/// against a missing model.
fn app() -> Option<(AppState, tempfile::TempDir)> {
    let models = repo_root().join("models/voiceprints");
    if !models.is_dir() {
        return None;
    }
    std::env::set_var("DAISY_VOICEPRINT_DIR", models);
    let dir = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(dir.path().join("daisy")).unwrap());
    // k-means, so the run under test is the one described here rather than
    // whichever backend this machine defaults to.
    app.update_machine_settings(|m| m.diarizer = "kmeans".into()).unwrap();
    session(&app);
    Some((app, dir))
}

#[test]
fn a_mic_only_run_measures_the_session_separation_over_the_clusters_it_made() {
    let _alone = alone();
    let Some((app, _d)) = app() else { return };

    let r = diarize_session_impl(&app, SESSION, Some(2), None).unwrap();
    assert_eq!(r.speakers, 2, "two voices, two clusters");

    let m = manifest_of(&app);
    assert_eq!(m.cluster_sides.len(), 2, "sides: {:?}", m.cluster_sides);
    // A mic-only run leaves no surviving system-track clusters, so a gate that
    // samples only the untouched track measures nothing and this stays None.
    let sep = m
        .speaker_separation
        .expect("the run must measure how far apart two people sit in this session");
    assert!(
        (0.0..=1.0).contains(&sep),
        "separation is a cosine between two clusters' windows: {sep}"
    );
}

#[test]
fn two_voices_forced_into_one_cluster_do_not_read_as_one_clear_voice() {
    let _alone = alone();
    let Some((app, _d)) = app() else { return };

    // Told to expect one speaker, the run has to put both voices in cluster 0.
    let r = diarize_session_impl(&app, SESSION, Some(1), None).unwrap();
    assert_eq!(r.speakers, 1, "one cluster was asked for");

    let m = manifest_of(&app);
    assert_eq!(
        m.mixed_clusters,
        vec![0],
        "the cluster holding both voices must carry the chip"
    );
}
