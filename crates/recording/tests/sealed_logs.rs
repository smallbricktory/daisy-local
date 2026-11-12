// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Append-only session logs (`live_transcript.jsonl`, `telemetry.jsonl`,
//! `metrics.jsonl`) are sealed one record per line; plaintext logs from
//! older builds still read.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use recording::live_transcript::{read_all, LiveTrack, LiveTranscriptLine, LiveTranscriptWriter};
use sealfile::{keyring, ContentSecret};
use tempfile::TempDir;

/// The keyring is process-wide; these tests take turns.
fn alone() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn session(td: &TempDir, id: &str) -> PathBuf {
    keyring::unlock(ContentSecret::generate().0);
    let dir = td.path().join("sessions").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn line(text: &str, start_ms: u32) -> LiveTranscriptLine {
    LiveTranscriptLine::now(LiveTrack::Mic, start_ms, start_ms + 1_000, text.into(), true)
}

fn holds(path: &Path, needle: &str) -> bool {
    std::fs::read(path).unwrap().windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[test]
fn live_transcript_round_trip_sealed() {
    let _g = alone();
    let td = TempDir::new().unwrap();
    let path = session(&td, "s-1").join("live_transcript.jsonl");
    let mut w = LiveTranscriptWriter::open(&path).unwrap();
    w.append(&line("Harbour lights at dusk", 0)).unwrap();
    w.append(&line("Second line", 1_000)).unwrap();
    drop(w);
    assert!(std::fs::read(&path).unwrap().starts_with(b"DSR1 1\n"));
    assert!(!holds(&path, "Harbour"), "plaintext on disk");
    let lines = read_all(&path).unwrap();
    assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["Harbour lights at dusk", "Second line"]);

    // A writer opened again continues the same log.
    LiveTranscriptWriter::open(&path).unwrap().append(&line("Third", 2_000)).unwrap();
    assert_eq!(read_all(&path).unwrap().len(), 3);
}

#[test]
fn live_transcript_torn_last_line_dropped() {
    let _g = alone();
    let td = TempDir::new().unwrap();
    let path = session(&td, "s-2").join("live_transcript.jsonl");
    let mut w = LiveTranscriptWriter::open(&path).unwrap();
    w.append(&line("kept", 0)).unwrap();
    w.append(&line("torn", 1_000)).unwrap();
    drop(w);
    // A crash mid-write leaves the last line cut short.
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() - 20]).unwrap();
    let lines = read_all(&path).unwrap();
    assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["kept"]);

    // The next writer starts a line of its own after the torn one.
    LiveTranscriptWriter::open(&path).unwrap().append(&line("after", 2_000)).unwrap();
    let lines = read_all(&path).unwrap();
    assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["kept", "after"]);
}

#[test]
fn telemetry_append_stage_sealed() {
    let _g = alone();
    let td = TempDir::new().unwrap();
    let dir = session(&td, "s-3");
    let path = dir.join(recording::telemetry::TELEMETRY_FILE);
    recording::telemetry::append_stage(&dir, "finalize_start");
    recording::telemetry::append_stage(&dir, "finalize_done");
    assert!(std::fs::read(&path).unwrap().starts_with(b"DSR1 1\n"));
    assert!(!holds(&path, "finalize_start"), "plaintext on disk");
    let records = sealfile::fs::read_records(&path).unwrap();
    let stages: Vec<String> = records
        .iter()
        .map(|r| serde_json::from_slice::<serde_json::Value>(r).unwrap()["stage"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(stages, ["finalize_start", "finalize_done"]);

    // The recording's writer and the metrics log seal the same way.
    let t = recording::telemetry::Telemetry::open(&dir);
    t.end();
    drop(t);
    assert_eq!(sealfile::fs::read_records(&path).unwrap().len(), 3);
    let metrics = recording::flight_recorder::FlightRecorder::open_if(true, &dir);
    metrics.mic_switch(7, None, false);
    drop(metrics);
    let metrics_path = dir.join(recording::flight_recorder::METRICS_FILE);
    assert!(std::fs::read(&metrics_path).unwrap().starts_with(b"DSR1 1\n"));
    assert_eq!(sealfile::fs::read_records(&metrics_path).unwrap().len(), 1);
}

#[test]
fn legacy_plaintext_jsonl_still_reads() {
    let _g = alone();
    let td = TempDir::new().unwrap();
    let path = session(&td, "s-4").join("live_transcript.jsonl");
    let old = serde_json::to_string(&line("from an older build", 0)).unwrap();
    std::fs::write(&path, format!("{old}\n")).unwrap();
    assert_eq!(read_all(&path).unwrap()[0].text, "from an older build");

    // Appending keeps the file plaintext until the migration seals it whole.
    LiveTranscriptWriter::open(&path).unwrap().append(&line("appended", 1_000)).unwrap();
    assert!(holds(&path, "appended"));
    assert_eq!(read_all(&path).unwrap().len(), 2);
}

/// Appends `line` and a newline to `path` as they are.
fn append_raw(path: &Path, line: &str) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(format!("{line}\n").as_bytes()).unwrap();
}

#[test]
fn telemetry_truncate_drops_a_damaged_line_and_leaves_a_log_with_another_keys_line() {
    use recording::telemetry::{truncate_words, TELEMETRY_FILE};
    let _g = alone();
    let td = TempDir::new().unwrap();
    let dir = session(&td, "s-trunc");
    let path = dir.join(TELEMETRY_FILE);
    let words = |w: &str, ms: u64| {
        format!("{{\"v\":1,\"wall_ms\":1,\"kind\":\"words\",\"track\":\"mic\",\"engine\":\"e\",\"words\":[[\"{w}\",{ms},{},null]]}}", ms + 200)
    };
    sealfile::fs::append_record(&path, words("before", 100).as_bytes()).unwrap();
    append_raw(&path, "#damaged#");
    sealfile::fs::append_record(&path, words("after", 900).as_bytes()).unwrap();
    truncate_words(&dir, 500).unwrap();
    let text = String::from_utf8(sealfile::fs::read(&path).unwrap()).unwrap();
    assert!(text.contains("before") && !text.contains("after"), "{text}");

    // A line sealed under a key this process does not hold: the log stays
    // as it was and the truncate says so.
    let foreign = sealfile::records::seal_record(&sealfile::SessionKey::random(), TELEMETRY_FILE, words("theirs", 50).as_bytes());
    append_raw(&path, &foreign);
    sealfile::fs::append_record(&path, words("later", 1_000).as_bytes()).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(truncate_words(&dir, 500).unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
