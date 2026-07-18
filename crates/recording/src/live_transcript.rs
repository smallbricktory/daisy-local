//! Append-only writer for `<session>/live_transcript.jsonl`.
//!
//! One JSON object per line, written as the live pipeline produces final
//! segments (or, optionally, interim updates). At session stop, the
//! existing dedup pipeline reads this file to build the unified
//! Me/Them transcript.
//!
//! File format (each line is a complete JSON object, RFC 7464-friendly):
//!
//! ```json
//! {"track":"mic","start_ms":0,"end_ms":3500,"text":"Hello, how are you","final":true,"received_at_unix":1778270000}
//! ```

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
    /// What produced this line. Missing values deserialize as `Final`.
    #[serde(default)]
    pub kind: LiveTranscriptKind,
}

impl LiveTranscriptLine {
    /// Convenience constructor that stamps `received_at_unix` to now.
    pub fn now(track: LiveTrack, start_ms: u32, end_ms: u32, text: String, is_final: bool) -> Self {
        let received_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Self {
            track,
            start_ms,
            end_ms,
            text,
            is_final,
            received_at_unix,
            kind: LiveTranscriptKind::Final,
        }
    }
}

/// Append-only writer; flushes on each append.
///
/// The file is opened with `OpenOptions::append(true).create(true)`: on most
/// filesystems each write is appended atomically up to the pipe-buffer size,
/// which is above one JSON line.
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
