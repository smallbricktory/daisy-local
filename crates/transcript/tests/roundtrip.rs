// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use std::path::PathBuf;
use transcript::{ChunkTranscript, Segment, SessionTranscript, Track, TrackTranscript};

#[test]
fn roundtrip_full() {
    let s = SessionTranscript {
        schema_version: SessionTranscript::SCHEMA,
        session_id: "lc1".into(),
        provider: "groq".into(),
        model: "whisper-large-v3-turbo".into(),
        transcribed_at_unix_seconds: 1_778_180_000,
        provisional: false,
        edited_at_unix_seconds: None,
        chunks: vec![ChunkTranscript {
            chunk_index: 1,
            tracks: vec![
                TrackTranscript {
                    track: Track::MicAec,
                    source_wav_relative: PathBuf::from("chunks/0001/mic_aec.wav"),
                    segments: vec![Segment {
                        start_ms: 0,
                        end_ms: 3620,
                        text: "Hello".into(),
                        confidence: None,
                        speaker_id: None,
                        words: Vec::new(), split_group: None,
                    }],
                },
                TrackTranscript {
                    track: Track::System,
                    source_wav_relative: PathBuf::from("chunks/0001/system.wav"),
                    segments: vec![],
                },
            ],
        }],
    };
    let json = serde_json::to_string_pretty(&s).unwrap();
    let back: SessionTranscript = serde_json::from_str(&json).unwrap();
    assert_eq!(s, back);
}

#[test]
fn track_serializes_snake_case() {
    let json = serde_json::to_string(&Track::MicAec).unwrap();
    assert_eq!(json, "\"mic_aec\"");
}

#[test]
fn a_transcript_without_the_local_fields_reads_as_transcribed_and_unedited() {
    let json = r#"{"schema_version":1,"session_id":"s","provider":"phone","model":"m","transcribed_at_unix_seconds":1700000100,"chunks":[]}"#;
    let t: SessionTranscript = serde_json::from_str(json).unwrap();
    assert!(!t.provisional);
    assert_eq!(t.edited_at_unix_seconds, None);
    assert_eq!(t.rank(), 1_700_000_100);
    assert_eq!(serde_json::to_string(&t).unwrap(), json, "absent fields stay absent when written back");
}

#[test]
fn a_transcript_ranks_by_its_later_stamp_and_a_provisional_one_by_its_edit_alone() {
    let json = |provisional: bool, edited: Option<i64>| {
        serde_json::from_value::<SessionTranscript>(serde_json::json!({
            "schema_version": 1, "session_id": "s", "provider": "p", "model": "m",
            "transcribed_at_unix_seconds": 1_700_000_500i64, "chunks": [],
            "provisional": provisional, "edited_at_unix_seconds": edited,
        }))
        .unwrap()
    };
    assert_eq!(json(false, Some(1_700_000_900)).rank(), 1_700_000_900);
    assert_eq!(json(false, Some(1_700_000_100)).rank(), 1_700_000_500);
    assert_eq!(json(true, None).rank(), 0, "a provisional transcript was never transcribed");
    assert_eq!(json(true, Some(1_700_000_200)).rank(), 1_700_000_200);
    let written = serde_json::to_value(json(true, Some(1_700_000_200))).unwrap();
    assert_eq!(written["provisional"], true);
    assert_eq!(written["edited_at_unix_seconds"], 1_700_000_200i64);
}
