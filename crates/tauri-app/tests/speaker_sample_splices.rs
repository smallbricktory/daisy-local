// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The waveform of the "Play sample audio" clip, at the joins.
//!
//! The clip is built by concatenating several non-contiguous pieces of the
//! session, so every join is a place where the waveform can jump. A jump is a
//! click. The voice here is the synthetic source-filter speaker, which is
//! smooth sample to sample, so a join stands out against the signal's own
//! slope; white noise would hide it.
use std::fs;

use tauri_app_core::commands::session::session_speaker_sample_audio_impl;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::AppState;

mod voices;
use voices::{room_tone, rowan, RATE};

const SESSION: &str = "daisy-1788200000";
const SECONDS: u32 = 90;

/// Turns far enough apart that each is gathered whole, and short enough that
/// no single one fills the clip's 5 s budget — so the clip has to join several.
/// `(start_ms, seconds)`.
const TURNS: &[(u32, u32)] = &[(0, 2), (20_000, 2), (40_000, 2), (60_000, 2)];

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

fn session(app: &AppState) {
    let root = app.profile.session_path(SESSION);
    let chunk = root.join("chunks/0000");
    fs::create_dir_all(&chunk).unwrap();

    let total = RATE as usize * SECONDS as usize;
    let voice = rowan(total);
    let mut mic = room_tone(total);
    for &(start_ms, secs) in TURNS {
        let a = start_ms as usize * RATE as usize / 1000;
        let b = a + secs as usize * RATE as usize;
        mic[a..b].copy_from_slice(&voice[a..b]);
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
        .map(|&(start_ms, secs)| {
            serde_json::json!({
                "start_ms": start_ms,
                "end_ms": start_ms + secs * 1_000,
                "text": format!("piece{} soundings", start_ms / 1000),
                "confidence": null,
                "speaker_id": 0,
            })
        })
        .collect();
    let transcript = serde_json::json!({
        "schema_version": 1,
        "session_id": SESSION,
        "provider": "whisper",
        "model": "base.en",
        "transcribed_at_unix_seconds": 1_788_200_000i64 + SECONDS as i64,
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

fn decode(bytes: &[u8]) -> Vec<i16> {
    let mut r = hound::WavReader::new(std::io::Cursor::new(bytes)).expect("a readable WAV");
    r.samples::<i16>().map(|s| s.unwrap()).collect()
}

/// The largest sample-to-sample step in a waveform — the fastest it moves.
fn max_step(v: &[i16]) -> i32 {
    v.windows(2).map(|w| (w[1] as i32 - w[0] as i32).abs()).max().unwrap_or(0)
}

/// The largest change *of* that step: curvature. A join can land at a matching
/// amplitude and still break the slope, which the ear hears and a
/// first-difference bound does not see.
fn max_bend(v: &[i16]) -> i32 {
    v.windows(3)
        .map(|w| (w[2] as i32 - 2 * w[1] as i32 + w[0] as i32).abs())
        .max()
        .unwrap_or(0)
}

#[test]
fn the_clip_never_jumps_faster_than_the_voice_itself_does() {
    let dir = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(dir.path().join("daisy")).unwrap());
    session(&app);

    let caption =
        tauri_app_core::commands::session::session_speaker_sample_text_impl(&app, SESSION, 0)
            .unwrap();
    // Without this the test goes vacuous: a clip built from one segment has no
    // join to measure, which is what a 5 s budget and an 8 s per-segment cap
    // produced on the first attempt.
    let pieces = caption.matches("piece").count();
    assert!(pieces >= 2, "the clip must join several pieces to measure a join: {caption}");
    eprintln!("caption ({pieces} joined pieces): {caption}");
    let pcm = decode(&session_speaker_sample_audio_impl(&app, SESSION, 0).unwrap());
    assert!(pcm.len() > RATE as usize, "a clip worth measuring");

    // The whole voice the session was built from, so the bound is what this
    // clip's own samples came out of rather than a shorter sample of it.
    let source = rowan(RATE as usize * SECONDS as usize);
    let limit = max_step(&source);
    let (worst_at, worst) = pcm
        .windows(2)
        .enumerate()
        .map(|(i, w)| (i, (w[1] as i32 - w[0] as i32).abs()))
        .max_by_key(|&(_, d)| d)
        .unwrap();
    eprintln!(
        "clip {} samples, source max step {limit}, clip max step {worst} at {worst_at} ({:.2}s)",
        pcm.len(),
        worst_at as f32 / RATE as f32
    );
    assert!(
        worst <= limit,
        "a join steps {worst} in one sample where the voice never exceeds {limit} — that is a click \
         (at sample {worst_at}, {:.2}s into the clip)",
        worst_at as f32 / RATE as f32
    );

    let bend_limit = max_bend(&source);
    let (bend_at, bend) = pcm
        .windows(3)
        .enumerate()
        .map(|(i, w)| (i + 1, (w[2] as i32 - 2 * w[1] as i32 + w[0] as i32).abs()))
        .max_by_key(|&(_, d)| d)
        .unwrap();
    eprintln!(
        "source max bend {bend_limit}, clip max bend {bend} at {bend_at} ({:.2}s)",
        bend_at as f32 / RATE as f32
    );
    assert!(
        bend <= bend_limit,
        "a join bends {bend} where the voice never exceeds {bend_limit} — the amplitude matches and \
         the slope does not, which is still a click (at sample {bend_at}, {:.2}s into the clip)",
        bend_at as f32 / RATE as f32
    );
}

/// The same measurement over a real session's clip, bounded by that session's
/// own audio. Synthetic speech is smooth by construction; real speech is what
/// the report was heard on. The recording must be at the voiceprint sample
/// rate, so the clip and the bound are the same kind of waveform.
///
///   DAISY_REPRO_PROFILE=/path/to/profile DAISY_REPRO_SESSION=daisy-… \
///     cargo test -p tauri-app --test speaker_sample_splices -- --ignored --nocapture
#[test]
#[ignore]
fn a_real_clip_never_jumps_faster_than_its_own_recording_does() {
    let root = std::env::var("DAISY_REPRO_PROFILE").expect("DAISY_REPRO_PROFILE");
    let sid = std::env::var("DAISY_REPRO_SESSION").expect("DAISY_REPRO_SESSION");
    let app = AppState::new(ProfileDir::at(&root).unwrap());
    let sdir = app.profile.session_path(&sid);
    let m: recording::manifest::SessionManifest =
        serde_json::from_slice(&fs::read(sdir.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(m.sample_rate, RATE, "the bound is only fair at the clip's own rate");

    // The fastest step and sharpest bend anywhere in the recording the clip was
    // cut from: whatever a join does, the untouched audio already does this.
    let (mut step_limit, mut bend_limit) = (0, 0);
    for c in &m.chunks {
        let pcm = decode(&fs::read(sdir.join(&c.mic_wav_relative)).unwrap());
        step_limit = step_limit.max(max_step(&pcm));
        bend_limit = bend_limit.max(max_bend(&pcm));
    }
    eprintln!("recording: max step {step_limit}, max bend {bend_limit}");

    let mut clicked = Vec::new();
    for c in &m.cluster_sides {
        let Ok(bytes) = session_speaker_sample_audio_impl(&app, &sid, c.cluster_id) else {
            eprintln!("cluster {}: no clip", c.cluster_id);
            continue;
        };
        let pcm = decode(&bytes);
        let caption = tauri_app_core::commands::session::session_speaker_sample_text_impl(
            &app,
            &sid,
            c.cluster_id,
        )
        .unwrap_or_default();
        let (step, bend) = (max_step(&pcm), max_bend(&pcm));
        eprintln!(
            "cluster {}: {} samples, step {step} (limit {step_limit}), bend {bend} (limit {bend_limit})\n  caption: {}",
            c.cluster_id,
            pcm.len(),
            caption.chars().take(90).collect::<String>()
        );
        if step > step_limit || bend > bend_limit {
            clicked.push(c.cluster_id);
        }
    }
    assert!(clicked.is_empty(), "clips move faster than their own recording: {clicked:?}");
}
