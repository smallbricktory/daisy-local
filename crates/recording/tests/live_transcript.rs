// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use recording::live_transcript::{
    read_all, LiveTrack, LiveTranscriptKind, LiveTranscriptLine, LiveTranscriptWriter,
};
use tempfile::TempDir;

#[test]
fn appends_lines_and_reads_them_back() {
    let td = TempDir::new().unwrap();
    let p = td.path().join("live_transcript.jsonl");

    {
        let mut w = LiveTranscriptWriter::open(&p).unwrap();
        w.append(&LiveTranscriptLine {
            track: LiveTrack::Mic,
            start_ms: 0,
            end_ms: 1500,
            text: "Hello".into(),
            is_final: true,
            received_at_unix: 100,
            received_at_ms: 100_000,
            engine: "nemotron".into(),
            model: "Nemotron".into(),
            kind: LiveTranscriptKind::Final,
            words: Vec::new(),
        })
        .unwrap();
        w.append(&LiveTranscriptLine {
            track: LiveTrack::System,
            start_ms: 800,
            end_ms: 2400,
            text: "Hi there".into(),
            is_final: true,
            received_at_unix: 101,
            received_at_ms: 101_000,
            engine: "nemotron".into(),
            model: "Nemotron".into(),
            kind: LiveTranscriptKind::Final,
            words: Vec::new(),
        })
        .unwrap();
    }

    let lines = read_all(&p).unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].track, LiveTrack::Mic);
    assert_eq!(lines[0].text, "Hello");
    assert_eq!(lines[1].track, LiveTrack::System);
    assert_eq!(lines[1].end_ms, 2400);
}

#[test]
fn second_writer_appends_to_existing_file() {
    let td = TempDir::new().unwrap();
    let p = td.path().join("live_transcript.jsonl");
    {
        let mut w1 = LiveTranscriptWriter::open(&p).unwrap();
        w1.append(&LiveTranscriptLine::now(
            LiveTrack::Mic,
            0,
            500,
            "first".into(),
            true,
        ))
        .unwrap();
    }
    {
        let mut w2 = LiveTranscriptWriter::open(&p).unwrap();
        w2.append(&LiveTranscriptLine::now(
            LiveTrack::Mic,
            500,
            1000,
            "second".into(),
            true,
        ))
        .unwrap();
    }
    let lines = read_all(&p).unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "first");
    assert_eq!(lines[1].text, "second");
}

#[test]
fn ignores_undecodable_lines_without_failing() {
    let td = TempDir::new().unwrap();
    let p = td.path().join("live_transcript.jsonl");
    {
        let mut w = LiveTranscriptWriter::open(&p).unwrap();
        w.append(&LiveTranscriptLine::now(LiveTrack::Mic, 0, 500, "ok".into(), true)).unwrap();
    }
    // Append garbage manually.
    use std::io::Write;
    {
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"this is not json\n").unwrap();
    }
    {
        let mut w = LiveTranscriptWriter::open(&p).unwrap();
        w.append(&LiveTranscriptLine::now(LiveTrack::Mic, 500, 1000, "after".into(), true)).unwrap();
    }
    let lines = read_all(&p).unwrap();
    assert_eq!(lines.len(), 2, "garbage line should be skipped, valid lines kept");
    assert_eq!(lines[0].text, "ok");
    assert_eq!(lines[1].text, "after");
}

#[test]
fn final_field_serializes_as_final_not_is_final() {
    let line = LiveTranscriptLine {
        track: LiveTrack::Mic,
        start_ms: 0,
        end_ms: 500,
        text: "x".into(),
        is_final: true,
        received_at_unix: 0,
        received_at_ms: 0,
        engine: String::new(),
        model: String::new(),
        kind: LiveTranscriptKind::Final,
        words: Vec::new(),
    };
    let json = serde_json::to_string(&line).unwrap();
    assert!(json.contains("\"final\":true"), "got: {json}");
    assert!(!json.contains("is_final"), "rust field name leaked: {json}");
}

#[test]
fn old_jsonl_without_kind_defaults_to_final() {
    let raw = r#"{"track":"mic","start_ms":0,"end_ms":100,"text":"hi","final":true,"received_at_unix":0}"#;
    let line: LiveTranscriptLine = serde_json::from_str(raw).unwrap();
    assert_eq!(line.kind, LiveTranscriptKind::Final);
    assert!(line.words.is_empty(), "older lines carry no words");
    assert!(!serde_json::to_string(&line).unwrap().contains("words"));
}

#[test]
fn words_survive_a_write_and_read() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("live_transcript.jsonl");
    let seg = transcript::Segment::from_words([
        ("hello".to_string(), 1_000, 1_300),
        ("there".to_string(), 1_400, 1_800),
    ])
    .unwrap();
    {
        let mut w = LiveTranscriptWriter::open(&p).unwrap();
        w.append(&LiveTranscriptLine::now(LiveTrack::Mic, seg.start_ms, seg.end_ms, seg.text.clone(), true).words(seg.words.clone()))
            .unwrap();
    }
    let lines = read_all(&p).unwrap();
    assert_eq!(lines[0].words, seg.words);
}

#[test]
fn track_serializes_lowercase() {
    let line = LiveTranscriptLine::now(LiveTrack::System, 0, 1, "x".into(), true);
    let json = serde_json::to_string(&line).unwrap();
    assert!(json.contains("\"track\":\"system\""), "got: {json}");
}

#[test]
fn interim_lines_carry_ms_receive_time_and_engine_and_stay_out_of_finals() {
    let td = TempDir::new().unwrap();
    let p = td.path().join("live_transcript.jsonl");
    {
        let mut w = LiveTranscriptWriter::open(&p).unwrap();
        w.append(
            &LiveTranscriptLine::interim(LiveTrack::Mic, 0, 400, "hello wor".into())
                .received(1_778_270_000_123)
                .engine("nemotron", "Nemotron Streaming 0.6B"),
        )
        .unwrap();
        w.append(&LiveTranscriptLine::now(LiveTrack::Mic, 0, 900, "hello world".into(), true).received(1_778_270_000_987))
            .unwrap();
    }
    let raw = std::fs::read_to_string(&p).unwrap();
    let first: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(first["kind"], "interim");
    assert_eq!(first["final"], false);
    assert_eq!(first["received_at_ms"], 1_778_270_000_123i64);
    assert_eq!(first["received_at_unix"], 1_778_270_000);
    assert_eq!(first["engine"], "nemotron");
    assert_eq!(first["model"], "Nemotron Streaming 0.6B");

    let lines = read_all(&p).unwrap();
    assert_eq!(lines[0].kind, LiveTranscriptKind::Interim);
    let finals: Vec<_> = lines.iter().filter(|l| l.is_final).collect();
    assert_eq!(finals.len(), 1);
    assert_eq!(finals[0].text, "hello world");
}

#[test]
fn old_lines_without_ms_or_engine_still_decode() {
    let raw = r#"{"track":"system","start_ms":0,"end_ms":100,"text":"hi","final":true,"received_at_unix":7,"kind":"final"}"#;
    let line: LiveTranscriptLine = serde_json::from_str(raw).unwrap();
    assert_eq!(line.received_at_ms, 0);
    assert!(line.engine.is_empty());
}
