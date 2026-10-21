// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Append-only writer for `<session>/live_transcript.jsonl`.
//!
//! One JSON object per line, written as the live pipeline produces final
//! segments and interim updates. At session stop, the existing dedup
//! pipeline reads the final lines to build the unified Me/Them transcript.
//!
//! File format (each line is a complete JSON object, RFC 7464-friendly):
//!
//! ```json
//! {"track":"mic","start_ms":0,"end_ms":3500,"text":"Hello, how are you","final":true,"received_at_unix":1778270000,"received_at_ms":1778270000123,"engine":"nemotron","model":"Nemotron Streaming 0.6B","kind":"final"}
//! ```
//!
//! `start_ms`/`end_ms` are session audio time; `received_at_ms` is the Unix
//! ms the line reached the pipeline; `engine`/`model` name the live engine
//! that produced it.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Which physical audio stream a line came from. Drives the Me/Them
/// attribution downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LiveTrack {
    Mic,
    System,
}

/// What produced a line. `Final` is the streaming-recognizer commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LiveTranscriptKind {
    #[default]
    Final,
    /// The engine's current hypothesis for the open utterance; `final` is
    /// false and a later line replaces it.
    Interim,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveTranscriptLine {
    pub track: LiveTrack,
    pub start_ms: u32,
    pub end_ms: u32,
    pub text: String,
    /// `true` for committed finals, `false` for interim hypotheses.
    /// Interim lines are kept for replay and debugging.
    #[serde(rename = "final")]
    pub is_final: bool,
    pub received_at_unix: i64,
    /// Receive time in Unix ms.
    #[serde(default)]
    pub received_at_ms: i64,
    /// Live engine id (`RealtimeTranscriber::name`).
    #[serde(default)]
    pub engine: String,
    /// Live engine model label (`RealtimeTranscriber::model`).
    #[serde(default)]
    pub model: String,
    /// What produced this line. Missing values deserialize as `Final`.
    #[serde(default)]
    pub kind: LiveTranscriptKind,
    /// Per-word timing on the session clock (see `transcript::Segment::words`).
    /// Absent on interim lines and lines from older builds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<transcript::Word>,
}

impl LiveTranscriptLine {
    /// Interim line: non-final, interim kind.
    pub fn interim(track: LiveTrack, start_ms: u32, end_ms: u32, text: String) -> Self {
        Self { kind: LiveTranscriptKind::Interim, ..Self::now(track, start_ms, end_ms, text, false) }
    }

    /// Convenience constructor that stamps the receive time to now.
    pub fn now(track: LiveTrack, start_ms: u32, end_ms: u32, text: String, is_final: bool) -> Self {
        let received_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Self {
            track,
            start_ms,
            end_ms,
            text,
            is_final,
            received_at_unix: received_at_ms.div_euclid(1000),
            received_at_ms,
            engine: String::new(),
            model: String::new(),
            kind: LiveTranscriptKind::Final,
            words: Vec::new(),
        }
    }

    /// Attaches the per-word timing of a final.
    pub fn words(mut self, words: Vec<transcript::Word>) -> Self {
        self.words = words;
        self
    }

    /// Stamps the receive time to `wall_ms` (Unix ms).
    pub fn received(mut self, wall_ms: i64) -> Self {
        self.received_at_ms = wall_ms;
        self.received_at_unix = wall_ms.div_euclid(1000);
        self
    }

    /// Names the live engine that produced the line.
    pub fn engine(mut self, engine: &str, model: &str) -> Self {
        self.engine = engine.into();
        self.model = model.into();
        self
    }
}

/// Append-only writer; flushes on each append.
///
/// The file is opened with `OpenOptions::append(true).create(true)`.
pub struct LiveTranscriptWriter {
    path: PathBuf,
    file: BufWriter<File>,
}

impl LiveTranscriptWriter {
    /// Open or create the file at the given path. Existing content is
    /// preserved; writes append.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = syncsafe::retry(|| OpenOptions::new().append(true).create(true).open(path))?;
        Ok(Self {
            path: path.to_path_buf(),
            file: BufWriter::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a single line. Flushes immediately; a crash mid-recording
    /// loses at most the last in-flight segment.
    pub fn append(&mut self, line: &LiveTranscriptLine) -> std::io::Result<()> {
        let json = serde_json::to_string(line)?;
        self.file.write_all(json.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        Ok(())
    }
}

/// Read all lines from a live_transcript.jsonl file. Lines that fail to
/// decode (e.g. a partial write at the very end of the file) are skipped
/// with a warning.
pub fn read_all(path: &Path) -> std::io::Result<Vec<LiveTranscriptLine>> {
    use std::io::BufRead;
    let file = File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut out = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<LiveTranscriptLine>(&line) {
            Ok(l) => out.push(l),
            Err(e) => {
                log::warn!("live_transcript line {i} undecodable: {e}");
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_of_a_kind_this_build_does_not_write_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live_transcript.jsonl");
        let line = |kind: &str, text: &str, fin: bool| {
            format!(
                r#"{{"track":"system","start_ms":1000,"end_ms":2000,"text":"{text}","final":{fin},"received_at_unix":1,"kind":"{kind}"}}"#
            )
        };
        let body = [line("final", "hello there", true), line("possiblemissedspeech", "", false), line("interim", "and", false)]
            .join("\n");
        std::fs::write(&path, body).unwrap();
        let lines = read_all(&path).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!((lines[0].kind, lines[0].text.as_str()), (LiveTranscriptKind::Final, "hello there"));
        assert_eq!(lines[1].kind, LiveTranscriptKind::Interim);
    }
}
