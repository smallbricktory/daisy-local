use std::path::PathBuf;
use transcript::render::render_markdown;
use transcript::{ChunkTranscript, Segment, SessionTranscript, Track, TrackTranscript};

fn fixture() -> SessionTranscript {
    SessionTranscript {
        schema_version: SessionTranscript::SCHEMA,
        session_id: "demo".into(),
        provider: "openai".into(),
        model: "whisper-1".into(),
        transcribed_at_unix_seconds: 0,
        chunks: vec![ChunkTranscript {
            chunk_index: 1,
            tracks: vec![
                TrackTranscript {
                    track: Track::MicAec,
                    source_wav_relative: PathBuf::from("chunks/0001/mic_aec.wav"),
                    segments: vec![
                        Segment {
                            start_ms: 0,
                            end_ms: 1500,
                            text: "Hello".into(),
                            confidence: None,
                            speaker_id: None,
                        },
                        Segment {
                            start_ms: 5000,
                            end_ms: 6000,
                            text: "That sounds reasonable".into(),
                            confidence: None,
                            speaker_id: None,
                        },
                    ],
                },
                TrackTranscript {
                    track: Track::System,
                    source_wav_relative: PathBuf::from("chunks/0001/system.wav"),
                    segments: vec![Segment {
                        start_ms: 2000,
                        end_ms: 4500,
                        text: "Hi there".into(),
                        confidence: None,
                        speaker_id: None,
                    }],
                },
            ],
        }],
    }
}

#[test]
fn renders_in_chronological_order_with_labels() {
    let md = render_markdown(&fixture());
    assert!(md.contains("# Transcript: demo"));
    assert!(md.contains("openai (whisper-1)"));
    let me_pos = md.find("**Me**: Hello").unwrap();
    let them_pos = md.find("**Them**: Hi there").unwrap();
    let me2_pos = md.find("**Me**: That sounds reasonable").unwrap();
    assert!(me_pos < them_pos && them_pos < me2_pos, "should be in time order");
}

#[test]
fn includes_chunk_separators_for_multi_chunk_sessions() {
    let mut s = fixture();
    s.chunks.push(ChunkTranscript {
        chunk_index: 2,
        tracks: vec![],
    });
    let md = render_markdown(&s);
    assert!(md.contains("## Chunk 1"));
    assert!(md.contains("## Chunk 2"));
}

#[test]
fn timestamps_formatted_as_hms() {
    let mut s = fixture();
    // Push Me[1] to 65s; with a System segment between Me[0] and Me[1] in the
    // fixture, Me[1] starts its own coalesced run and keeps its timestamp.
    s.chunks[0].tracks[0].segments[1].start_ms = 65_000;
    s.chunks[0].tracks[0].segments[1].end_ms = 66_000;
    let md = render_markdown(&s);
    assert!(md.contains("[00:01:05]"), "got: {md}");
}

#[test]
fn coalesces_consecutive_same_speaker_rows() {
    let mut s = fixture();
    // Append a third Me segment AFTER Me[1] with no intervening Them — should
    // merge into the preceding Me run.
    s.chunks[0].tracks[0].segments.push(Segment {
        start_ms: 7000,
        end_ms: 8000,
        text: "And we ship Friday".into(),
        confidence: None,
        speaker_id: None,
    });
    let md = render_markdown(&s);
    assert!(md.contains("**Me**: That sounds reasonable And we ship Friday"), "got: {md}");
    assert!(!md.contains("**Me**: And we ship Friday\n"), "got: {md}");
}

#[test]
fn strips_leading_filler_in_render() {
    let mut s = fixture();
    s.chunks[0].tracks[0].segments[0].text = "Um, yeah, the deadline is Friday.".into();
    let md = render_markdown(&s);
    assert!(md.contains("**Me**: The deadline is Friday."), "got: {md}");
    assert!(!md.contains("Um,"), "got: {md}");
}

fn seg(start_ms: u32, text: &str, speaker_id: Option<u32>) -> Segment {
    Segment { start_ms, end_ms: start_ms + 1000, text: text.into(), confidence: None, speaker_id }
}

fn mic_only(segments: Vec<Segment>) -> SessionTranscript {
    SessionTranscript {
        schema_version: SessionTranscript::SCHEMA,
        session_id: "demo".into(),
        provider: "local".into(),
        model: "whisper".into(),
        transcribed_at_unix_seconds: 0,
        chunks: vec![ChunkTranscript {
            chunk_index: 1,
            tracks: vec![TrackTranscript {
                track: Track::MicAec,
                source_wav_relative: PathBuf::from("chunks/0001/mic_aec.wav"),
                segments,
            }],
        }],
    }
}

#[test]
fn single_unnamed_mic_cluster_stays_me() {
    let st = mic_only(vec![seg(0, "note to self", Some(0)), seg(5000, "second thought", Some(0))]);
    let md = transcript::render::render_markdown_with_speakers(&st, &Default::default(), &Default::default());
    assert!(md.contains("**Me**: Note to self"), "{md}");
    assert!(!md.contains("Person A"), "{md}");
}

#[test]
fn named_mic_cluster_shows_the_name() {
    let st = mic_only(vec![seg(0, "note to self", Some(0))]);
    let speakers = std::collections::HashMap::from([(0u32, "Dana".to_string())]);
    let md = transcript::render::render_markdown_with_speakers(&st, &Default::default(), &speakers);
    assert!(md.contains("**Dana**: Note to self"), "{md}");
}

#[test]
fn multiple_mic_clusters_get_person_letters() {
    let st = mic_only(vec![seg(0, "first voice", Some(0)), seg(5000, "second voice", Some(1))]);
    let md = transcript::render::render_markdown_with_speakers(&st, &Default::default(), &Default::default());
    assert!(md.contains("**Person A**: First voice"), "{md}");
    assert!(md.contains("**Person B**: Second voice"), "{md}");
}

#[test]
fn undiarized_mic_segments_stay_me() {
    let st = mic_only(vec![seg(0, "plain", None)]);
    let md = transcript::render::render_markdown_with_speakers(&st, &Default::default(), &Default::default());
    assert!(md.contains("**Me**: Plain"), "{md}");
}
