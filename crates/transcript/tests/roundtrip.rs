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
