// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The clip and the caption behind "listen to this voice" on the Speakers
//! screen. Both read the session's own audio and transcript and pick the
//! windows belonging to one cluster, so the session here is real WAVs and a
//! real transcript rather than a stub.
use std::fs;

use tauri_app_core::commands::session::{
    session_speaker_sample_audio_impl, session_speaker_sample_text_impl,
};
use tauri_app_core::error::AppError;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::AppState;

const SESSION: &str = "daisy-1788200000";
const RATE: u32 = 16_000;

/// Speech-shaped noise, loud enough to clear the sample floor. A tone would
/// be rejected as unvoiced.
fn speech(samples: usize, seed: u64) -> Vec<i16> {
    let mut state = seed | 1;
    (0..samples)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 48) as i16) / 5
        })
        .collect()
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

/// A finished session: sixty seconds of mic audio, and a transcript whose
/// segments alternate between two clusters.
///
/// `segments` are `(start_ms, seconds, cluster, text)`.
fn session(app: &AppState, segments: &[(u32, u32, u32, &str)]) {
    let root = app.profile.session_path(SESSION);
    let chunk = root.join("chunks/0000");
    fs::create_dir_all(&chunk).unwrap();
    wav(&chunk.join("mic.wav"), &speech(RATE as usize * 60, 0x2545_F491));
    wav(&chunk.join("system.wav"), &speech(RATE as usize * 60, 0x9E37_79B9));

    let manifest = serde_json::json!({
        "schema_version": 4,
        "session_id": SESSION,
        "created_at_unix_seconds": 1_788_200_000i64,
        "sample_rate": RATE,
        "channels": 1,
        "finalized_at_unix_seconds": 1_788_200_060i64,
        "chunks": [{
            "index": 0,
            "started_at_unix_seconds": 1_788_200_000i64,
            "ended_at_unix_seconds": 1_788_200_060i64,
            "duration_seconds": 60,
            "mic_wav_relative": "chunks/0000/mic.wav",
            "system_wav_relative": "chunks/0000/system.wav",
            "mic_aec_wav_relative": null,
        }],
        "attendees": [],
        "tag_ids": [],
    });
    fs::write(root.join("manifest.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();

    let segs: Vec<serde_json::Value> = segments
        .iter()
        .map(|&(start, secs, cluster, text)| {
            serde_json::json!({
                "start_ms": start,
                "end_ms": start + secs * 1_000,
                "text": text,
                "confidence": null,
                "speaker_id": cluster,
            })
        })
        .collect();
    let transcript = serde_json::json!({
        "schema_version": 1,
        "session_id": SESSION,
        "provider": "whisper",
        "model": "base.en",
        "transcribed_at_unix_seconds": 1_788_200_070i64,
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

/// Two speakers taking eight-second turns, well clear of each other.
const TWO_SPEAKERS: &[(u32, u32, u32, &str)] = &[
    (0, 8, 0, "so the rebate import is the first thing"),
    (12_000, 8, 1, "the bank sends a fixed width file"),
    (24_000, 8, 0, "and the portal certificate is second"),
    (36_000, 8, 1, "the rotation takes about a day"),
    (48_000, 8, 0, "then we escalate rather than fall back"),
];

fn app() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(dir.path().join("daisy")).unwrap());
    (app, dir)
}

/// Reads the WAV the command returns, checking the envelope on the way.
fn decode(bytes: &[u8]) -> (hound::WavSpec, Vec<i16>) {
    let mut r = hound::WavReader::new(std::io::Cursor::new(bytes)).expect("a readable WAV");
    let spec = r.spec();
    let pcm = r.samples::<i16>().map(|s| s.unwrap()).collect();
    (spec, pcm)
}

// ---- the session has to be there -------------------------------------------

#[test]
fn a_session_that_is_not_here_is_named_by_both() {
    let (app, _d) = app();
    let text = session_speaker_sample_text_impl(&app, "daisy-nothing", 0).unwrap_err();
    assert!(matches!(text, AppError::SessionNotFound(s) if s == "daisy-nothing"));
    let audio = session_speaker_sample_audio_impl(&app, "daisy-nothing", 0).unwrap_err();
    assert!(matches!(audio, AppError::SessionNotFound(s) if s == "daisy-nothing"));
}

// ---- the caption -----------------------------------------------------------

#[test]
fn the_caption_is_this_clusters_own_words() {
    let (app, _d) = app();
    session(&app, TWO_SPEAKERS);
    let text = session_speaker_sample_text_impl(&app, SESSION, 0).unwrap();
    assert!(!text.is_empty(), "cluster 0 said something");
    assert!(
        text.contains("rebate import"),
        "the caption is drawn from the cluster's segments: {text}"
    );
    assert!(
        !text.contains("fixed width file") && !text.contains("rotation takes"),
        "and never from the other speaker's: {text}"
    );
}

#[test]
fn each_cluster_gets_its_own_caption() {
    let (app, _d) = app();
    session(&app, TWO_SPEAKERS);
    let zero = session_speaker_sample_text_impl(&app, SESSION, 0).unwrap();
    let one = session_speaker_sample_text_impl(&app, SESSION, 1).unwrap();
    assert!(!one.is_empty());
    assert_ne!(zero, one, "two speakers do not share a caption");
    assert!(one.contains("fixed width file") || one.contains("rotation takes"), "{one}");
}

#[test]
fn a_cluster_nobody_was_assigned_to_has_no_caption() {
    let (app, _d) = app();
    session(&app, TWO_SPEAKERS);
    assert_eq!(
        session_speaker_sample_text_impl(&app, SESSION, 7).unwrap(),
        "",
        "a cluster id with no segments reads as nothing to show"
    );
}

#[test]
fn the_caption_stays_short_enough_for_the_row() {
    let (app, _d) = app();
    // One speaker talking for the whole session: the caption must not grow
    // with the meeting.
    let long: Vec<(u32, u32, u32, &str)> = (0..6)
        .map(|i| (i * 9_000, 8u32, 0u32, "the same speaker keeps talking for a long time here"))
        .collect();
    session(&app, &long);
    let text = session_speaker_sample_text_impl(&app, SESSION, 0).unwrap();
    assert!(!text.is_empty(), "the length is what is being measured, so there has to be a caption");
    assert!(
        text.chars().count() < 400,
        "the caption is a preview, not a transcript: {} chars",
        text.chars().count()
    );
}

// ---- the clip --------------------------------------------------------------

#[test]
fn the_clip_is_a_wav_the_player_can_open() {
    let (app, _d) = app();
    session(&app, TWO_SPEAKERS);
    let bytes = session_speaker_sample_audio_impl(&app, SESSION, 0).unwrap();
    let (spec, pcm) = decode(&bytes);
    assert_eq!(spec.channels, 1);
    assert_eq!(spec.sample_rate, RATE);
    assert_eq!(spec.bits_per_sample, 16);
    assert!(!pcm.is_empty(), "the clip carries audio");
    assert!(
        pcm.iter().any(|&s| s.abs() > 200),
        "and it is not silence"
    );
}

#[test]
fn the_clip_is_capped_at_five_seconds_however_long_the_speaker_talked() {
    let (app, _d) = app();
    let long: Vec<(u32, u32, u32, &str)> = (0..6)
        .map(|i| (i * 9_000, 8u32, 0u32, "one speaker for the whole meeting"))
        .collect();
    session(&app, &long);
    let (_, pcm) = decode(&session_speaker_sample_audio_impl(&app, SESSION, 0).unwrap());
    let seconds = pcm.len() as f32 / RATE as f32;
    assert!(
        seconds > 1.0,
        "the cap is what is being measured, so there has to be a clip: {seconds:.1}s"
    );
    assert!(
        seconds <= 5.05,
        "a preview that plays for {seconds:.1}s is not a preview"
    );
}

#[test]
fn a_cluster_with_no_audio_says_so_rather_than_playing_nothing() {
    let (app, _d) = app();
    session(&app, TWO_SPEAKERS);
    let e = session_speaker_sample_audio_impl(&app, SESSION, 7).unwrap_err();
    assert!(
        format!("{e}").contains("No clear audio"),
        "the screen needs something to show: {e}"
    );
}

#[test]
fn a_session_whose_audio_was_purged_says_so_rather_than_failing_oddly() {
    let (app, _d) = app();
    session(&app, TWO_SPEAKERS);
    fs::remove_file(app.profile.session_path(SESSION).join("chunks/0000/mic.wav")).unwrap();
    let e = session_speaker_sample_audio_impl(&app, SESSION, 0).unwrap_err();
    assert!(
        format!("{e}").contains("No clear audio"),
        "a deleted recording is the same answer as a silent one: {e}"
    );
    assert_eq!(
        session_speaker_sample_text_impl(&app, SESSION, 0).unwrap(),
        "",
        "and the caption goes with it"
    );
}
