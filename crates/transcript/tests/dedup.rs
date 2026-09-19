// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use transcript::dedup::{dedup_session, DedupParams};
use transcript::{ChunkTranscript, Segment, SessionTranscript, Track, TrackTranscript};

fn write_silent_wav(path: &Path, duration_seconds: u32) {
    let samples = vec![0_i16; (16_000 * duration_seconds) as usize];
    transcript::wav_io::write_wav_mono_i16(path, &samples, 16_000).unwrap();
}

fn write_loud_wav(path: &Path, duration_seconds: u32) {
    let samples: Vec<i16> = (0..(16_000 * duration_seconds))
        .map(|i| {
            let phase = (i as f32 / 16_000.0) * 2.0 * std::f32::consts::PI * 440.0;
            (i16::MAX as f32 * phase.sin()) as i16
        })
        .collect();
    transcript::wav_io::write_wav_mono_i16(path, &samples, 16_000).unwrap();
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
                            text: "I'll talk to Lennart about adding Wren as approver"
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
                        text: "I'll talk to Lennart about adding Wren as approver"
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

fn sys_seg(start_ms: u32, end_ms: u32, text: &str) -> Segment {
    Segment { start_ms, end_ms, text: text.into(), confidence: None, speaker_id: None }
}

fn session_with_system_segments(td: &TempDir, segments: Vec<Segment>) -> (PathBuf, SessionTranscript) {
    let root = td.path().to_path_buf();
    let chunk_dir = root.join("chunks/0001");
    std::fs::create_dir_all(&chunk_dir).unwrap();
    write_loud_wav(&chunk_dir.join("mic.wav"), 30);
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
                    track: Track::Mic,
                    source_wav_relative: PathBuf::from("chunks/0001/mic.wav"),
                    segments: vec![sys_seg(20_000, 21_000, "unrelated mic words here")],
                },
                TrackTranscript {
                    track: Track::System,
                    source_wav_relative: PathBuf::from("chunks/0001/system.wav"),
                    segments,
                },
            ],
        }],
    };
    (root, st)
}

fn system_texts(result: &transcript::dedup::DedupResult) -> Vec<String> {
    result.deduped.chunks[0]
        .tracks
        .iter()
        .find(|t| t.track == Track::System)
        .unwrap()
        .segments
        .iter()
        .map(|s| s.text.clone())
        .collect()
}

// The live window re-decodes retained audio after a guarded trim and can
// re-commit words it already emitted; consecutive same-track segments then
// share a word run at their time-overlapping boundary.
#[test]
fn boundary_repeat_between_consecutive_segments_is_trimmed() {
    let td = TempDir::new().unwrap();
    let (root, st) = session_with_system_segments(
        &td,
        vec![
            sys_seg(47_700, 50_000, "please note that this call will be"),
            sys_seg(
                49_800,
                53_600,
                "this call will be monitored or recorded for quality and training purposes.",
            ),
        ],
    );
    let result = dedup_session(&st, &root, &DedupParams::default()).unwrap();
    let texts = system_texts(&result);
    assert_eq!(texts.len(), 2);
    assert!(
        texts[1].starts_with("monitored"),
        "leading repeat must be trimmed: {:?}",
        texts[1]
    );
}

#[test]
fn boundary_repeat_behind_one_variant_word_is_trimmed() {
    // "…or recorded for quality" re-decoded as "are recorded for quality…"
    // — one variant word in front of the repeat.
    let td = TempDir::new().unwrap();
    let (root, st) = session_with_system_segments(
        &td,
        vec![
            sys_seg(40_326, 50_918, "Please note that this call will be monitored or recorded for quality"),
            sys_seg(50_768, 57_768, "are recorded for quality and training purposes."),
        ],
    );
    let result = dedup_session(&st, &root, &DedupParams::default()).unwrap();
    let texts = system_texts(&result);
    assert_eq!(texts.len(), 2);
    assert!(
        texts[1].starts_with("and training"),
        "variant word plus repeat run must be trimmed: {:?}",
        texts[1]
    );
}

#[test]
fn a_fully_repeated_segment_is_dropped() {
    let td = TempDir::new().unwrap();
    let (root, st) = session_with_system_segments(
        &td,
        vec![
            sys_seg(81_400, 87_200, "Thank you for calling. I'm Maren speaking. How may I help you?"),
            sys_seg(86_900, 88_000, "How may I help you?"),
        ],
    );
    let result = dedup_session(&st, &root, &DedupParams::default()).unwrap();
    let texts = system_texts(&result);
    assert_eq!(texts.len(), 1, "a segment that is all repeat must be dropped: {texts:?}");
}

#[test]
fn short_and_distant_repeats_survive() {
    let td = TempDir::new().unwrap();
    let (root, st) = session_with_system_segments(
        &td,
        vec![
            // Genuine short repeat at the boundary — under the 3-word floor,
            // must survive.
            sys_seg(10_000, 11_000, "next Tuesday"),
            sys_seg(11_200, 12_000, "next Tuesday"),
            // Verbatim repeat far past the overlap slack (hold-music loop) —
            // must survive.
            sys_seg(57_000, 63_200, "we are a trusted name in logistics"),
            sys_seg(187_100, 192_700, "we are a trusted name in logistics"),
        ],
    );
    let result = dedup_session(&st, &root, &DedupParams::default()).unwrap();
    let texts = system_texts(&result);
    assert_eq!(texts.len(), 4, "no trim without a ≥3-word overlapping run: {texts:?}");
}

fn write_burst_wav(path: &Path, duration_s: u32, bursts: &[(f32, f32)], amp: f32) {
    let n = (16_000 * duration_s) as usize;
    let mut samples = vec![0_i16; n];
    for &(a, b) in bursts {
        let (s, e) = ((a * 16_000.0) as usize, ((b * 16_000.0) as usize).min(n));
        for (i, v) in samples[s..e].iter_mut().enumerate() {
            let phase = i as f32 / 16_000.0 * 2.0 * std::f32::consts::PI * 440.0;
            *v = (amp * phase.sin()) as i16;
        }
    }
    transcript::wav_io::write_wav_mono_i16(path, &samples, 16_000).unwrap();
}

#[test]
fn padded_span_does_not_dilute_the_mic_quiet_check() {
    // Mic speech is 2s at ~-31 dBFS inside a 10s padded span. RMS over the
    // padded span reads ~-38 dBFS — under the -35 bleed threshold — which
    // would drop the segment as bleed-from-system. Clipped to its audible
    // 2s, it reads ~-31 dBFS and survives.
    let td = TempDir::new().unwrap();
    let root = td.path().to_path_buf();
    let chunk_dir = root.join("chunks/0001");
    std::fs::create_dir_all(&chunk_dir).unwrap();
    write_burst_wav(&chunk_dir.join("mic.wav"), 12, &[(0.0, 2.0)], 1_300.0);
    write_loud_wav(&chunk_dir.join("system.wav"), 12);

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
                    track: Track::Mic,
                    source_wav_relative: PathBuf::from("chunks/0001/mic.wav"),
                    segments: vec![sys_seg(0, 10_000, "I'll check the deployment logs after lunch")],
                },
                TrackTranscript {
                    track: Track::System,
                    source_wav_relative: PathBuf::from("chunks/0001/system.wav"),
                    segments: vec![sys_seg(0, 10_000, "I'll check the deployment logs after lunch")],
                },
            ],
        }],
    };
    let result = dedup_session(&st, &root, &DedupParams::default()).unwrap();
    let mic = result.deduped.chunks[0]
        .tracks
        .iter()
        .find(|t| t.track == Track::Mic)
        .unwrap();
    assert_eq!(
        mic.segments.len(),
        1,
        "audible mic speech must not be dropped as bleed because padding diluted its RMS"
    );
    assert!(
        mic.segments[0].end_ms <= 2_500,
        "span must be clipped to the audible extent, got end {}",
        mic.segments[0].end_ms
    );
}

#[test]
fn boundary_repeat_still_trims_when_clipped_spans_no_longer_overlap() {
    // Raw stamps overlap (the re-decode evidence); the audible bursts do not.
    // The trim must key on the raw stamps — clipping first would hide the
    // repeat.
    let td = TempDir::new().unwrap();
    let root = td.path().to_path_buf();
    let chunk_dir = root.join("chunks/0001");
    std::fs::create_dir_all(&chunk_dir).unwrap();
    write_loud_wav(&chunk_dir.join("mic.wav"), 50);
    write_burst_wav(
        &chunk_dir.join("system.wav"),
        50,
        &[(38.0, 40.0), (41.0, 44.5)],
        8_000.0,
    );

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
                    track: Track::Mic,
                    source_wav_relative: PathBuf::from("chunks/0001/mic.wav"),
                    segments: vec![sys_seg(20_000, 21_000, "unrelated mic words here")],
                },
                TrackTranscript {
                    track: Track::System,
                    source_wav_relative: PathBuf::from("chunks/0001/system.wav"),
                    segments: vec![
                        sys_seg(37_800, 41_000, "Yes, correct. So if you don't"),
                        sys_seg(40_800, 44_900, "so if you don't mind filling that out"),
                    ],
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
    assert_eq!(sys.segments.len(), 2);
    assert!(
        sys.segments[1].text.starts_with("mind filling"),
        "repeat must trim on raw stamps even when clipped spans separate: {:?}",
        sys.segments[1].text
    );
}
