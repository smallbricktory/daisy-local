// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! What two transcriptions at once cost in memory.
//!
//! Batch transcription is always on-device: every arm of the provider builder
//! is a local engine, and each one loads its own weights. Two of them running
//! together therefore hold two copies — on the GPU backends, two contexts —
//! while contending for the same cores. This drives the real command with the
//! real model and reads the residency counter while it runs.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri_app_core::commands::pipeline::{transcribe_impl, TranscribeRequest};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::AppState;

/// The residency counters are process-wide, so these tests take turns.
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

fn whisper_model() -> Option<std::path::PathBuf> {
    let p = repo_root().join("models/whisper/ggml-base.en.bin");
    p.is_file().then_some(p)
}

/// Speech-shaped noise: whisper is given something with variance rather than
/// digital silence, which the track skip treats as nothing to decode.
fn noise_pcm(samples: usize) -> Vec<i16> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    (0..samples)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 48) as i16) / 6
        })
        .collect()
}

fn temp_app() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    (Arc::new(AppState::new(profile)), dir)
}

/// A session with one chunk of real audio on both tracks, and the manifest
/// the orchestrator reads to find them.
fn synth_audio_session(app: &AppState, id: &str, seconds: usize) {
    let dir = app.profile.session_path(id);
    let chunk = dir.join("chunks/0000");
    std::fs::create_dir_all(&chunk).unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let pcm = noise_pcm(16_000 * seconds);
    for name in ["mic.wav", "system.wav"] {
        let mut w = hound::WavWriter::create(chunk.join(name), spec).unwrap();
        for s in &pcm {
            w.write_sample(*s).unwrap();
        }
        w.finalize().unwrap();
    }
    let manifest = serde_json::json!({
        "schema_version": 4,
        "session_id": id,
        "created_at_unix_seconds": 1_700_000_000i64,
        "sample_rate": 16_000,
        "channels": 1,
        "finalized_at_unix_seconds": null,
        "chunks": [{
            "index": 0,
            "started_at_unix_seconds": 1_700_000_000i64,
            "ended_at_unix_seconds": 1_700_000_000i64 + seconds as i64,
            "duration_seconds": seconds,
            "mic_wav_relative": "chunks/0000/mic.wav",
            "system_wav_relative": "chunks/0000/system.wav",
            "mic_aec_wav_relative": null
        }],
        "attendees": [],
        "tag_ids": []
    });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

/// Runs both sessions through `transcribe_impl` at the same moment and
/// returns the highest whisper residency seen while they ran.
fn peak_residency_of_two_transcribes(app: &Arc<AppState>, model: &std::path::Path) -> u64 {
    let gate = Arc::new(std::sync::Barrier::new(3));
    let peak = Arc::new(AtomicU64::new(0));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let watcher = {
        let peak = Arc::clone(&peak);
        let done = Arc::clone(&done);
        let gate = Arc::clone(&gate);
        std::thread::spawn(move || {
            gate.wait();
            while !done.load(Ordering::Relaxed) {
                let now = providers_local::whisper_resident();
                peak.fetch_max(now, Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        })
    };

    let workers: Vec<_> = ["daisy-a", "daisy-b"]
        .into_iter()
        .map(|id| {
            let app = Arc::clone(app);
            let gate = Arc::clone(&gate);
            let model = model.to_path_buf();
            std::thread::spawn(move || {
                gate.wait();
                transcribe_impl(
                    &app,
                    TranscribeRequest {
                        session_id: id.to_string(),
                        model: Some(model.to_string_lossy().into_owned()),
                    },
                    None,
                )
            })
        })
        .collect();

    for w in workers {
        w.join().unwrap().expect("the transcribe ran");
    }
    done.store(true, Ordering::Relaxed);
    watcher.join().unwrap();
    peak.load(Ordering::Relaxed)
}

#[test]
fn two_transcriptions_at_once_hold_one_model_between_them() {
    let _alone = alone();
    let Some(model) = whisper_model() else {
        return;
    };
    let (app, _d) = temp_app();
    synth_audio_session(&app, "daisy-a", 2);
    synth_audio_session(&app, "daisy-b", 2);
    assert_eq!(providers_local::whisper_resident(), 0, "nothing is loaded yet");

    let peak = peak_residency_of_two_transcribes(&app, &model);

    assert_eq!(
        peak, 1,
        "two transcriptions must not each load the model: {peak} were resident at once"
    );
    assert_eq!(
        providers_local::whisper_resident(),
        0,
        "and both are freed when the passes end"
    );
}

#[test]
fn both_transcripts_are_written() {
    let _alone = alone();
    let Some(model) = whisper_model() else {
        return;
    };
    let (app, _d) = temp_app();
    synth_audio_session(&app, "daisy-a", 2);
    synth_audio_session(&app, "daisy-b", 2);

    peak_residency_of_two_transcribes(&app, &model);

    for id in ["daisy-a", "daisy-b"] {
        let path = app.profile.session_path(id).join("transcript.json");
        assert!(path.is_file(), "{id} has no transcript");
        // The queue must not have handed one session the other's work.
        let t: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(t["session_id"], id, "{id}'s transcript names another session");
    }
}

// ---- diarization -----------------------------------------------------------

/// Writes a transcript over the synthesized chunk: `turns` system-track
/// segments of two seconds each, long enough for the diarizer to embed.
fn synth_transcript(app: &AppState, id: &str, turns: u32) {
    let dir = app.profile.session_path(id);
    let segments: Vec<serde_json::Value> = (0..turns)
        .map(|i| {
            serde_json::json!({
                "start_ms": i * 2000,
                "end_ms": (i + 1) * 2000,
                "text": format!("turn {i}"),
                "confidence": 0.9,
                "speaker_id": null
            })
        })
        .collect();
    let st = serde_json::json!({
        "schema_version": 1,
        "session_id": id,
        "provider": "whisper-local",
        "model": "ggml-base.en",
        "transcribed_at_unix_seconds": 1_700_000_100i64,
        "chunks": [{
            "chunk_index": 0,
            "tracks": [{
                "track": "system",
                "source_wav_relative": "chunks/0000/system.wav",
                "segments": segments
            }]
        }]
    });
    std::fs::write(
        dir.join("transcript.json"),
        serde_json::to_vec_pretty(&st).unwrap(),
    )
    .unwrap();
}

/// Runs both sessions through `diarize_session_impl` at the same moment and
/// returns the highest speaker-encoder residency seen while they ran, with
/// the segments each run labeled.
fn peak_residency_of_two_diarizations(app: &Arc<AppState>) -> (u64, Vec<u32>) {
    use tauri_app_core::commands::voiceprints::diarize_session_impl;
    let gate = Arc::new(std::sync::Barrier::new(3));
    let peak = Arc::new(AtomicU64::new(0));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let watcher = {
        let peak = Arc::clone(&peak);
        let done = Arc::clone(&done);
        let gate = Arc::clone(&gate);
        std::thread::spawn(move || {
            gate.wait();
            while !done.load(Ordering::Relaxed) {
                peak.fetch_max(voiceprints::resident(), Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };

    let workers: Vec<_> = ["daisy-a", "daisy-b"]
        .into_iter()
        .map(|id| {
            let app = Arc::clone(app);
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.wait();
                diarize_session_impl(&app, id, None, None)
            })
        })
        .collect();

    let labeled = workers
        .into_iter()
        .map(|w| w.join().unwrap().expect("the diarization ran").segments_labeled)
        .collect();
    done.store(true, Ordering::Relaxed);
    watcher.join().unwrap();
    (peak.load(Ordering::Relaxed), labeled)
}

#[test]
fn two_diarizations_at_once_hold_one_speaker_encoder_between_them() {
    let _alone = alone();
    if !repo_root().join("models/voiceprints").is_dir() {
        return;
    }
    std::env::set_var("DAISY_VOICEPRINT_DIR", repo_root().join("models/voiceprints"));
    let (app, _d) = temp_app();
    for id in ["daisy-a", "daisy-b"] {
        synth_audio_session(&app, id, 12);
        synth_transcript(&app, id, 6);
    }
    assert_eq!(voiceprints::resident(), 0, "nothing is loaded yet");

    let (peak, labeled) = peak_residency_of_two_diarizations(&app);

    assert_eq!(labeled, [6, 6], "each run embedded and labeled every turn");

    assert_eq!(
        peak, 1,
        "two diarizations must not each load the speaker model: {peak} were resident at once"
    );
    assert_eq!(voiceprints::resident(), 0, "and both are freed when the runs end");
}
