use std::path::{Path, PathBuf};
use tempfile::TempDir;
use transcript::dedup::{dedup_session, DedupParams};
use transcript::{ChunkTranscript, Segment, SessionTranscript, Track, TrackTranscript};

fn write_silent_wav(path: &Path, duration_seconds: u32) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for _ in 0..(16_000 * duration_seconds) {
        w.write_sample(0_i16).unwrap();
    }
    w.finalize().unwrap();
}

fn write_loud_wav(path: &Path, duration_seconds: u32) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..(16_000 * duration_seconds) {
        let phase = (i as f32 / 16_000.0) * 2.0 * std::f32::consts::PI * 440.0;
        let s = (i16::MAX as f32 * phase.sin()) as i16;
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
}

fn fixture_session(td: &TempDir, mic_silent: bool) -> (PathBuf, SessionTranscript) {
    let root = td.path().to_path_buf();
    let chunk_dir = root.join("chunks/0001");
    std::fs::create_dir_all(&chunk_dir).unwrap();

    if mic_silent {
        write_silent_wav(&chunk_dir.join("mic_aec.wav"), 30);
    } else {
        write_loud_wav(&chunk_dir.join("mic_aec.wav"), 30);
    }
    write_loud_wav(&chunk_dir.join("system.wav"), 30);

    let st = SessionTranscript {
        schema_version: SessionTranscript::SCHEMA,
        session_id: "test".into(),
        provider: "fake".into(),
        model: "fake-1".into(),
        transcribed_at_unix_seconds: 0,
        chunks: vec![ChunkTranscript {
            chunk_index: 1,
            tracks: vec![
                TrackTranscript {
                    track: Track::MicAec,
                    source_wav_relative: PathBuf::from("chunks/0001/mic_aec.wav"),
                    segments: vec![
                        // Bleed candidate: overlaps in time + text with system seg.
                        Segment {
                            start_ms: 1000,
                            end_ms: 5000,
                            text: "I'll talk to Miles about adding Dana as approver"
                                .into(),
                            confidence: Some(0.5),
                            speaker_id: None,
                        },
                        // No system overlap — must survive.
                        Segment {
                            start_ms: 10000,
                            end_ms: 12000,
                            text: "yeah totally agree".into(),
                            confidence: Some(0.8),
                            speaker_id: None,
                        },
                    ],
                },
                TrackTranscript {
                    track: Track::System,
                    source_wav_relative: PathBuf::from("chunks/0001/system.wav"),
                    segments: vec![Segment {
                        start_ms: 900,
                        end_ms: 5100,
                        text: "I'll talk to Miles about adding Dana as approver"
                            .into(),
                        confidence: Some(0.6),
                        speaker_id: None,
                    }],
                },
            ],
        }],
    };
    (root, st)
}

#[test]
fn drops_duplicate_when_text_matches_and_mic_quiet() {
    let td = TempDir::new().unwrap();
    let (root, st) = fixture_session(&td, /*mic_silent=*/ true);
    let params = DedupParams::default();

    let result = dedup_session(&st, &root, &params).unwrap();
    let mic_segs = &result.deduped.chunks[0].tracks[0].segments;
    assert_eq!(mic_segs.len(), 1, "bleed segment should be dropped");
    assert_eq!(mic_segs[0].text, "yeah totally agree");
    assert_eq!(result.report.dropped, 1);
}

#[test]
fn keeps_segment_when_mic_was_loud_even_if_text_matches() {
    let td = TempDir::new().unwrap();
    let (root, st) = fixture_session(&td, /*mic_silent=*/ false);
    let params = DedupParams::default();
    let result = dedup_session(&st, &root, &params).unwrap();
    let mic_segs = &result.deduped.chunks[0].tracks[0].segments;
    assert_eq!(mic_segs.len(), 2, "both segments survive when mic is loud");
}

#[test]
fn report_serializes_to_json() {
    let td = TempDir::new().unwrap();
    let (root, st) = fixture_session(&td, true);
    let result = dedup_session(&st, &root, &DedupParams::default()).unwrap();
    let json = serde_json::to_string(&result.report).unwrap();
    assert!(json.contains("\"dropped\""));
}

/// Audio imports write a placeholder mic track (zero segments) whose source
/// wav does not exist on disk. Dedup passes such chunks through without
/// loading the wav.
#[test]
fn empty_mic_track_with_missing_wav_passes_through() {
    let td = TempDir::new().unwrap();
    let root = td.path().to_path_buf();
    let chunk_dir = root.join("chunks/0001");
    std::fs::create_dir_all(&chunk_dir).unwrap();
    write_loud_wav(&chunk_dir.join("system.wav"), 5);
    // No mic.wav on disk, mirroring import_audio_meeting_impl.

    let st = SessionTranscript {
        schema_version: SessionTranscript::SCHEMA,
        session_id: "import".into(),
        provider: "fake".into(),
        model: "fake-1".into(),
        transcribed_at_unix_seconds: 0,
        chunks: vec![ChunkTranscript {
            chunk_index: 1,
            tracks: vec![
                TrackTranscript {
                    track: Track::Mic,
                    source_wav_relative: PathBuf::from("chunks/0001/mic.wav"),
                    segments: vec![],
                },
                TrackTranscript {
                    track: Track::System,
                    source_wav_relative: PathBuf::from("chunks/0001/system.wav"),
                    segments: vec![Segment {
                        start_ms: 0,
                        end_ms: 2000,
                        text: "imported speech".into(),
                        confidence: None,
                        speaker_id: None,
                    }],
                },
            ],
        }],
    };

    let result = dedup_session(&st, &root, &DedupParams::default()).unwrap();
    let sys = result.deduped.chunks[0]
        .tracks
        .iter()
        .find(|t| t.track == Track::System)
        .unwrap();
    assert_eq!(sys.segments.len(), 1, "system segments survive untouched");
}
