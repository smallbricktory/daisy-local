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
            kind: LiveTranscriptKind::Final,
        })
        .unwrap();
        w.append(&LiveTranscriptLine {
            track: LiveTrack::System,
            start_ms: 800,
            end_ms: 2400,
            text: "Hi there".into(),
            is_final: true,
            received_at_unix: 101,
            kind: LiveTranscriptKind::Final,
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
        kind: LiveTranscriptKind::Final,
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
}

#[test]
fn track_serializes_lowercase() {
    let line = LiveTranscriptLine::now(LiveTrack::System, 0, 1, "x".into(), true);
    let json = serde_json::to_string(&line).unwrap();
    assert!(json.contains("\"track\":\"system\""), "got: {json}");
}
