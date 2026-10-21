// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The system track's speaker turns, found while the recording runs, hand
//! finalize exactly what it would have found at Stop.
//!
//! Real WAVs through the real segmentation model and speaker encoder: the live pass and the Stop pass agree.
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use tauri_app_core::commands::voiceprints::{diarize_session_impl, diarize_session_with_turns_impl};
use tauri_app_core::live_diarize;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::AppState;

mod voices;
use voices::{room_tone, rowan, zoe, RATE};

const SESSION: &str = "daisy-1788300000";
/// Seconds per chunk.
const CHUNK_S: usize = 45;

fn alone() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("workspace root")
}

fn wav(path: &Path, pcm: &[i16]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
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

/// Chunk `i`'s system track: Rowan and Zoe trading eight-second turns over
/// room tone, a different order per chunk.
fn system_pcm(i: u32) -> Vec<i16> {
    let n = CHUNK_S * RATE as usize;
    let mut pcm = room_tone(n);
    for (k, start_s) in [1usize, 10, 19, 28, 36].into_iter().enumerate() {
        let len = 8 * RATE as usize;
        let from = start_s * RATE as usize;
        let voice = if (k as u32 + i) % 2 == 0 { rowan(len) } else { zoe(len) };
        let len = len.min(n - from);
        pcm[from..from + len].copy_from_slice(&voice[..len]);
    }
    pcm
}

/// The profile with no session yet, or `None` when the speaker models are
/// not staged in this checkout.
fn bare_app() -> Option<(AppState, tempfile::TempDir)> {
    let models = repo_root().join("models/voiceprints");
    if !models.join("segmentation-3.0.onnx").is_file() || !models.join("model.onnx").is_file() {
        return None;
    }
    std::env::set_var("DAISY_VOICEPRINT_DIR", models);
    let dir = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(dir.path().join("daisy")).unwrap());
    Some((app, dir))
}

/// Writes the manifest naming `chunks` (index, ended) and a transcript with
/// one system segment per eight-second turn.
fn write_session(root: &Path, chunks: &[(u32, bool)]) {
    let entries: Vec<serde_json::Value> = chunks
        .iter()
        .map(|&(i, ended)| {
            let start = 1_788_300_000i64 + (i as i64) * CHUNK_S as i64;
            serde_json::json!({
                "index": i,
                "started_at_unix_seconds": start,
                "ended_at_unix_seconds": if ended { Some(start + CHUNK_S as i64) } else { None },
                "duration_seconds": if ended { Some(CHUNK_S) } else { None },
                "mic_wav_relative": format!("chunks/{i:04}/mic.wav"),
                "system_wav_relative": format!("chunks/{i:04}/system.wav"),
                "mic_aec_wav_relative": null,
            })
        })
        .collect();
    let manifest = serde_json::json!({
        "schema_version": 4,
        "session_id": SESSION,
        "created_at_unix_seconds": 1_788_300_000i64,
        "sample_rate": RATE,
        "channels": 1,
        "finalized_at_unix_seconds": null,
        "chunks": entries,
        "attendees": [],
        "tag_ids": [],
    });
    fs::write(root.join("manifest.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    let tchunks: Vec<serde_json::Value> = chunks
        .iter()
        .map(|&(i, _)| {
            let segs: Vec<serde_json::Value> = [1u32, 10, 19, 28, 36]
                .iter()
                .map(|s| serde_json::json!({
                    "start_ms": s * 1000, "end_ms": (s + 8).min(CHUNK_S as u32) * 1000,
                    "text": "the rebate import is the first thing", "confidence": null, "speaker_id": null,
                }))
                .collect();
            serde_json::json!({
                "chunk_index": i,
                "tracks": [{
                    "track": "system",
                    "source_wav_relative": format!("chunks/{i:04}/system.wav"),
                    "segments": segs,
                }],
            })
        })
        .collect();
    let transcript = serde_json::json!({
        "schema_version": 1, "session_id": SESSION, "provider": "whisper", "model": "base.en",
        "transcribed_at_unix_seconds": 1_788_300_500i64, "chunks": tchunks,
    });
    fs::write(root.join("transcript.json"), serde_json::to_vec_pretty(&transcript).unwrap()).unwrap();
}

fn write_chunk(root: &Path, i: u32, seconds: usize) {
    let pcm = system_pcm(i);
    let n = (seconds * RATE as usize).min(pcm.len());
    wav(&root.join(format!("chunks/{i:04}/system.wav")), &pcm[..n]);
    wav(&root.join(format!("chunks/{i:04}/mic.wav")), &room_tone(n));
}

/// `(chunk, start_ms, end_ms, speaker)` of every system segment.
fn labels(app: &AppState) -> Vec<(u32, u32, u32, Option<u32>)> {
    let p = app.profile.session_path(SESSION).join("transcript.json");
    let st: transcript::SessionTranscript = serde_json::from_slice(&fs::read(p).unwrap()).unwrap();
    st.chunks
        .iter()
        .flat_map(|c| {
            c.tracks.iter().flat_map(move |t| t.segments.iter().map(move |s| (c.chunk_index, s.start_ms, s.end_ms, s.speaker_id)))
        })
        .collect()
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for e in fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            fs::copy(e.path(), &to).unwrap();
        }
    }
}

#[test]
fn turns_found_while_recording_give_the_speakers_a_stop_only_run_gives() {
    let _alone = alone();
    let Some((live_app, _d1)) = bare_app() else { return };
    let (stop_app, _d2) = bare_app().unwrap();
    let root = live_app.profile.session_path(SESSION);
    fs::create_dir_all(&root).unwrap();

    // Recording: chunk 1 half written, then whole, then chunk 2 opens.
    write_chunk(&root, 1, 20);
    write_session(&root, &[(1, false)]);
    live_diarize::start(SESSION, root.clone());
    std::thread::sleep(std::time::Duration::from_millis(2_500));
    write_chunk(&root, 1, CHUNK_S);
    write_chunk(&root, 2, 30);
    write_session(&root, &[(1, true), (2, false)]);
    std::thread::sleep(std::time::Duration::from_millis(2_500));
    write_chunk(&root, 2, CHUNK_S);
    write_session(&root, &[(1, true), (2, true)]);
    live_diarize::recording_stopped(SESSION);

    let stop_root = stop_app.profile.session_path(SESSION);
    copy_dir(&root, &stop_root);

    let turns = live_diarize::take(SESSION, &root).expect("live turns for every chunk");
    assert_eq!(turns.system.keys().copied().collect::<Vec<_>>(), vec![1, 2]);
    assert!(!turns.system_embeddings.is_empty(), "the live pass embedded turns");
    let live = diarize_session_with_turns_impl(&live_app, SESSION, None, None, &[], &turns).unwrap();
    let stop = diarize_session_impl(&stop_app, SESSION, None, None, &[]).unwrap();

    assert_eq!(live.speakers, stop.speakers);
    assert_eq!(labels(&live_app), labels(&stop_app));
}

#[test]
fn a_chunk_that_changed_after_the_live_pass_read_it_voids_the_live_turns() {
    let _alone = alone();
    let Some((app, _d)) = bare_app() else { return };
    let root = app.profile.session_path(SESSION);
    fs::create_dir_all(&root).unwrap();
    write_chunk(&root, 1, CHUNK_S);
    write_chunk(&root, 2, CHUNK_S);
    write_session(&root, &[(1, true), (2, true)]);

    let live = live_diarize::run(&root, &AtomicBool::new(true), &AtomicBool::new(false)).expect("live pass");
    assert!(live.matches_disk(&root));

    // A truncate rewrites chunk 2 shorter.
    write_chunk(&root, 2, 30);
    assert!(!live.matches_disk(&root));

    // A chunk the pass never saw.
    write_chunk(&root, 2, CHUNK_S);
    assert!(live.matches_disk(&root));
    write_chunk(&root, 3, 10);
    write_session(&root, &[(1, true), (2, true), (3, true)]);
    assert!(!live.matches_disk(&root));
}

#[test]
fn a_cancelled_pass_hands_nothing_to_finalize() {
    let _alone = alone();
    let Some((app, _d)) = bare_app() else { return };
    let root = app.profile.session_path(SESSION);
    fs::create_dir_all(&root).unwrap();
    write_chunk(&root, 1, CHUNK_S);
    write_session(&root, &[(1, false)]);
    live_diarize::start(SESSION, root.clone());
    live_diarize::cancel(SESSION);
    assert!(live_diarize::take(SESSION, &root).is_none());
    assert!(live_diarize::run(&root, &AtomicBool::new(true), &AtomicBool::new(true)).is_none());
}
