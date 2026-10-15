// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The live echo-cancelled mic, written while recording to
//! `chunks/NNNN/mic_live_aec.wav` (16 kHz mono i16) for live readers.
//!
//! The file holds what the live pipeline feeds its mic transcriber, before
//! the captions gain, at the same sample offsets as the chunk's `mic.wav`:
//! each sample lands where its capture frame landed in `mic.wav`, pads and
//! gap fills included, so a span of one file is the same span of the other.
//! The header is rewritten on every flush, so its length is how much audio
//! is safely on disk. Finalize deletes the files; the chunk manifest does
//! not list them.

use std::collections::VecDeque;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use crate::manifest::SessionManifest;

/// File name inside each chunk directory.
pub const FILE_NAME: &str = "mic_live_aec.wav";

/// Longest silence a write fills before its first sample, as for the
/// capture writer's leading pad.
const MAX_FILL_SAMPLES: u64 = 30 * 60 * 16_000;

/// Session-relative path of chunk `index`'s live echo-cancelled mic.
pub fn rel(index: u32) -> PathBuf {
    crate::session::chunk_rel(index, FILE_NAME)
}

/// Deletes every chunk's live echo-cancelled mic under `root`.
pub fn remove_all(root: &Path, manifest: &SessionManifest) {
    for c in &manifest.chunks {
        let p = syncsafe::join_rel(root, rel(c.index));
        if p.exists() {
            if let Err(e) = syncsafe::remove_file(&p) {
                log::warn!("live AEC mic: remove {}: {e}", p.display());
            }
        }
    }
}

/// Writes the live echo-cancelled mic of one recording, one chunk file at a
/// time.
pub struct LiveAecWav {
    root: PathBuf,
    open: Option<(u32, hound::WavWriter<BufWriter<File>>)>,
    dirty: bool,
}

impl LiveAecWav {
    pub fn new(root: PathBuf) -> Self {
        Self { root, open: None, dirty: false }
    }

    /// Writes `samples` starting at sample `offset` of chunk `index`'s file.
    /// Moving to another chunk closes the open file. A file that already
    /// exists is appended to; a span before its end keeps what is there,
    /// and a span past its end is preceded by silence.
    pub fn write(&mut self, index: u32, offset: u64, samples: &[i16]) -> std::io::Result<()> {
        if self.open.as_ref().map(|(i, _)| *i) != Some(index) {
            self.close();
            let path = syncsafe::join_rel(&self.root, rel(index));
            let w = if path.is_file() {
                hound::WavWriter::append(&path)
            } else {
                let spec = hound::WavSpec {
                    channels: 1,
                    sample_rate: 16_000,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                };
                hound::WavWriter::create(&path, spec)
            }
            .map_err(hound_io)?;
            self.open = Some((index, w));
        }
        let (_, w) = self.open.as_mut().expect("opened above");
        let len = w.len() as u64;
        if offset > len {
            let gap = offset - len;
            if gap > MAX_FILL_SAMPLES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("offset {offset} is {gap} samples past the end"),
                ));
            }
            for _ in 0..gap {
                w.write_sample(0i16).map_err(hound_io)?;
            }
        }
        let skip = len.saturating_sub(offset).min(samples.len() as u64) as usize;
        for &s in &samples[skip..] {
            w.write_sample(s).map_err(hound_io)?;
        }
        self.dirty = true;
        Ok(())
    }

    /// Rewrites the open file's header to cover everything written.
    pub fn flush(&mut self) -> std::io::Result<()> {
        if let (Some((_, w)), true) = (self.open.as_mut(), self.dirty) {
            w.flush().map_err(hound_io)?;
            self.dirty = false;
        }
        Ok(())
    }

    /// Closes the open file; the next write opens its chunk's file again.
    pub fn close(&mut self) {
        if let Some((index, w)) = self.open.take() {
            if let Err(e) = w.finalize() {
                log::warn!("live AEC mic: close chunk {index}: {e}");
            }
        }
        self.dirty = false;
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }
}

impl Drop for LiveAecWav {
    fn drop(&mut self) {
        self.close();
    }
}

/// Where the samples of the mic stream sit in the chunk files: runs of
/// stream samples, each tagged with the chunk and file offset its capture
/// frame was written at.
#[derive(Default)]
pub struct StreamTags {
    runs: VecDeque<Run>,
}

struct Run {
    start: u64,
    len: u64,
    chunk: u32,
    offset: u64,
}

impl StreamTags {
    /// Notes that `len` stream samples from `start` were written to chunk
    /// `chunk` at `offset`. Untagged samples belong to no file.
    pub fn push(&mut self, start: u64, len: u64, chunk: Option<(u32, u64)>) {
        if let Some((chunk, offset)) = chunk {
            self.runs.push_back(Run { start, len, chunk, offset });
        }
    }

    pub fn clear(&mut self) {
        self.runs.clear();
    }

    /// Writes `samples`, which sit at stream position `at`, to the chunk
    /// files their capture frames went to, and forgets the runs that end by
    /// the end of `samples`.
    pub fn write(&mut self, at: u64, samples: &[i16], wav: &mut LiveAecWav) -> std::io::Result<()> {
        let end = at + samples.len() as u64;
        let mut result = Ok(());
        for r in self.runs.iter() {
            if r.start >= end {
                break;
            }
            let from = r.start.max(at);
            let to = (r.start + r.len).min(end);
            if from >= to {
                continue;
            }
            let slice = &samples[(from - at) as usize..(to - at) as usize];
            if let Err(e) = wav.write(r.chunk, r.offset + (from - r.start), slice) {
                result = Err(e);
            }
        }
        while self.runs.front().is_some_and(|r| r.start + r.len <= end) {
            self.runs.pop_front();
        }
        result
    }
}

fn hound_io(e: hound::Error) -> std::io::Error {
    match e {
        hound::Error::IoError(e) => e,
        other => std::io::Error::new(std::io::ErrorKind::InvalidData, other.to_string()),
    }
}

/// Lets a repeating warning through at most once per `every`; the next one
/// let through carries how many were held back.
pub struct Throttle {
    every: std::time::Duration,
    last: Option<std::time::Instant>,
    held: u64,
}

impl Throttle {
    pub fn new(every: std::time::Duration) -> Self {
        Self { every, last: None, held: 0 }
    }

    /// `Some(held back since the last one)` when a warning at `now` is let
    /// through.
    pub fn pass(&mut self, now: std::time::Instant) -> Option<u64> {
        if self.last.is_some_and(|t| now.duration_since(t) < self.every) {
            self.held += 1;
            return None;
        }
        self.last = Some(now);
        Some(std::mem::take(&mut self.held))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_len(p: &Path) -> u32 {
        hound::WavReader::open(p).unwrap().duration()
    }

    fn samples(p: &Path) -> Vec<i16> {
        hound::WavReader::open(p).unwrap().samples::<i16>().map(|s| s.unwrap()).collect()
    }

    fn root() -> tempfile::TempDir {
        let td = tempfile::tempdir().unwrap();
        for i in 1..=3 {
            std::fs::create_dir_all(td.path().join(format!("chunks/{i:04}"))).unwrap();
        }
        td
    }

    #[test]
    fn the_header_length_grows_with_each_flush() {
        let td = root();
        let p = td.path().join("chunks/0001/mic_live_aec.wav");
        let mut w = LiveAecWav::new(td.path().to_path_buf());
        w.write(1, 0, &[5; 160]).unwrap();
        w.flush().unwrap();
        assert_eq!(header_len(&p), 160);
        w.write(1, 160, &[6; 320]).unwrap();
        w.flush().unwrap();
        assert_eq!(header_len(&p), 480);
        assert!(w.is_open());
    }

    #[test]
    fn a_new_chunk_closes_the_last_and_a_reopen_appends() {
        let td = root();
        let mut w = LiveAecWav::new(td.path().to_path_buf());
        w.write(1, 0, &[1; 100]).unwrap();
        w.write(2, 0, &[2; 50]).unwrap();
        w.flush().unwrap();
        let one = td.path().join("chunks/0001/mic_live_aec.wav");
        assert_eq!(samples(&one), vec![1; 100]);
        // Closed (a pause), then the same chunk's stragglers arrive.
        w.close();
        assert!(!w.is_open());
        w.write(1, 100, &[3; 10]).unwrap();
        w.close();
        let got = samples(&one);
        assert_eq!(got.len(), 110);
        assert_eq!(&got[100..], &[3; 10]);
        assert_eq!(header_len(&td.path().join("chunks/0002/mic_live_aec.wav")), 50);
    }

    #[test]
    fn offsets_place_audio_as_mic_wav_holds_it() {
        let td = root();
        let mut w = LiveAecWav::new(td.path().to_path_buf());
        // A pad of 40 samples precedes the first frame; an overlap is kept.
        w.write(1, 40, &[7; 20]).unwrap();
        w.write(1, 50, &[8; 20]).unwrap();
        w.close();
        let got = samples(&td.path().join("chunks/0001/mic_live_aec.wav"));
        assert_eq!(got.len(), 70);
        assert_eq!(&got[..40], &[0; 40]);
        assert_eq!(&got[40..60], &[7; 20]);
        assert_eq!(&got[60..], &[8; 10]);
    }

    #[test]
    fn stream_tags_split_output_at_the_chunk_boundary() {
        let td = root();
        let mut w = LiveAecWav::new(td.path().to_path_buf());
        let mut tags = StreamTags::default();
        // Stream samples 0..300 went to chunk 1 at 1000; 300..600 to chunk 2
        // at 0; 600..700 to no file (no chunk open).
        tags.push(0, 300, Some((1, 1_000)));
        tags.push(300, 300, Some((2, 0)));
        tags.push(600, 100, None);
        let out: Vec<i16> = (0..700).map(|i| i as i16).collect();
        for at in (0..700).step_by(160) {
            let end = (at + 160).min(700);
            tags.write(at as u64, &out[at..end], &mut w).unwrap();
        }
        w.close();
        let one = samples(&td.path().join("chunks/0001/mic_live_aec.wav"));
        assert_eq!(one.len(), 1_300);
        assert_eq!(&one[1_000..], &out[..300]);
        let two = samples(&td.path().join("chunks/0002/mic_live_aec.wav"));
        assert_eq!(two, &out[300..600]);
        assert!(tags.runs.is_empty());
    }

    #[test]
    fn finalize_removes_every_chunks_file() {
        let td = root();
        let mut w = LiveAecWav::new(td.path().to_path_buf());
        w.write(1, 0, &[1; 10]).unwrap();
        w.write(2, 0, &[1; 10]).unwrap();
        w.close();
        let chunks: Vec<serde_json::Value> = [1, 2, 3]
            .iter()
            .map(|i| {
                serde_json::json!({
                    "index": i, "started_at_unix_seconds": 1_000,
                    "mic_wav_relative": format!("chunks/{i:04}/mic.wav"),
                    "system_wav_relative": format!("chunks/{i:04}/system.wav"),
                })
            })
            .collect();
        let m: SessionManifest = serde_json::from_value(serde_json::json!({
            "schema_version": 2, "session_id": "daisy-1", "created_at_unix_seconds": 1_000,
            "sample_rate": 16_000, "channels": 1,
            "mic_source_id": 1, "mic_source_node_name": "mic", "mic_source_description": "Mic",
            "system_source_id": 2, "system_source_node_name": "sys", "system_source_description": "System",
            "aec_mode": "disabled", "finalized_at_unix_seconds": null, "title": "t",
            "chunks": chunks,
        }))
        .unwrap();
        remove_all(td.path(), &m);
        for i in 1..=3 {
            assert!(!td.path().join(format!("chunks/{i:04}/mic_live_aec.wav")).exists());
        }
    }

    #[test]
    fn a_throttle_passes_one_warning_a_minute_and_counts_the_rest() {
        use std::time::{Duration, Instant};
        let mut t = Throttle::new(Duration::from_secs(60));
        let t0 = Instant::now();
        assert_eq!(t.pass(t0), Some(0));
        for i in 1..=100 {
            assert_eq!(t.pass(t0 + Duration::from_millis(i * 10)), None);
        }
        assert_eq!(t.pass(t0 + Duration::from_secs(61)), Some(100));
        assert_eq!(t.pass(t0 + Duration::from_secs(62)), None);
    }
}
