// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! One track of a recording's audio, read from `chunks/NNNN/<track>.wav` while
//! the recorder is still writing them.
//!
//! The capture writer rewrites each WAV header as it appends, so the header's
//! length is how much audio is safely on disk. A reader that falls behind
//! reads more on its next poll; nothing is dropped and nothing waits on it.

use std::path::{Path, PathBuf};

use recording::manifest::{ChunkManifest, SessionManifest};

/// Sample rate of every chunk WAV the feed reads.
pub const SAMPLE_RATE: u32 = 16_000;

/// Most audio one `Samples` event carries.
const MAX_READ_FRAMES: u64 = 60 * SAMPLE_RATE as u64;

/// Which chunk WAV the feed reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedTrack {
    System,
    Mic,
    /// The live echo-cancelled mic (`chunks/NNNN/mic_live_aec.wav`), written
    /// while recording in the room case; sample-aligned with `mic.wav`.
    MicLiveAec,
}

impl FeedTrack {
    fn wav(self, chunk: &ChunkManifest, root: &Path) -> PathBuf {
        match self {
            FeedTrack::System => syncsafe::join_rel(root, &chunk.system_wav_relative),
            FeedTrack::Mic => syncsafe::join_rel(root, &chunk.mic_wav_relative),
            FeedTrack::MicLiveAec => syncsafe::join_rel(root, recording::live_aec_wav::rel(chunk.index)),
        }
    }
}

/// What a poll found, in chunk order.
#[derive(Debug, PartialEq)]
pub enum FeedEvent<'a> {
    /// New mono 16 kHz audio of chunk `chunk`, starting `start_sample` into
    /// the chunk. `offset_ms` is where the chunk starts on the session's
    /// audio timeline.
    Samples { chunk: u32, offset_ms: u32, start_sample: u64, samples: &'a [f32] },
    /// The chunk is complete at `samples`; no more audio of it follows.
    Closed { chunk: u32, offset_ms: u32, samples: u64 },
}

struct Cursor {
    index: u32,
    wav: PathBuf,
    /// Frames already delivered.
    read: u64,
    /// The header length the previous poll saw.
    last_len: Option<u64>,
    closed: bool,
    /// Once closed: the chunk's length on the session clock, the longer of
    /// its two tracks.
    span_samples: Option<u64>,
}

/// Reads one track of a session's chunks as they grow.
pub struct ChunkFeed {
    root: PathBuf,
    track: FeedTrack,
    cursors: Vec<Cursor>,
    buf: Vec<f32>,
    /// The manifest's chunks as last read, and the file's modification time
    /// and length then.
    chunks: Vec<ChunkManifest>,
    manifest_seen: Option<(std::time::SystemTime, u64)>,
}

impl ChunkFeed {
    pub fn new(root: PathBuf, track: FeedTrack) -> Self {
        Self { root, track, cursors: Vec::new(), buf: Vec::new(), chunks: Vec::new(), manifest_seen: None }
    }

    /// Re-reads the manifest when its modification time or length changed.
    fn refresh_chunks(&mut self) -> std::io::Result<()> {
        let path = self.root.join("manifest.json");
        let meta = std::fs::metadata(&path)?;
        let seen = (meta.modified()?, meta.len());
        if self.manifest_seen == Some(seen) {
            return Ok(());
        }
        let manifest: SessionManifest = serde_json::from_slice(&syncsafe::read(&path)?)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.chunks = manifest.chunks;
        self.chunks.sort_by_key(|c| c.index);
        self.manifest_seen = Some(seen);
        Ok(())
    }

    /// Delivers the audio written since the last poll, chunk by chunk, and
    /// closes each chunk the recorder has finished with. A chunk is finished
    /// once a later chunk exists or its end is stamped, and its WAV stopped
    /// growing between two polls; `recording_done` (the recorder has
    /// stopped) closes every chunk at its current length.
    pub fn poll(&mut self, recording_done: bool, on: &mut dyn FnMut(FeedEvent<'_>)) -> std::io::Result<()> {
        self.refresh_chunks()?;
        let chunks: &[ChunkManifest] = &self.chunks;
        for c in chunks {
            if !self.cursors.iter().any(|k| k.index == c.index) {
                self.cursors.push(Cursor {
                    index: c.index,
                    wav: self.track.wav(c, &self.root),
                    read: 0,
                    last_len: None,
                    closed: false,
                    span_samples: None,
                });
            }
        }
        self.cursors.sort_by_key(|k| k.index);
        let mut offset_ms: u64 = 0;
        for i in 0..self.cursors.len() {
            let index = self.cursors[i].index;
            let entry = chunks.iter().find(|c| c.index == index);
            let later = chunks.iter().any(|c| c.index > index);
            let ended = entry.is_some_and(|c| c.ended_at_unix_seconds.is_some());
            let at = offset_ms.min(u32::MAX as u64) as u32;
            // Each chunk starts where the previous ones' audio ends, the
            // longer of its two tracks counting, as the session clock does.
            let span = match self.cursors[i].span_samples {
                Some(n) => n,
                None => entry.and_then(|c| recording::manifest_ops::chunk_wav_samples(&self.root, c)).unwrap_or(0),
            };
            offset_ms += span * 1000 / SAMPLE_RATE as u64;
            let k = &mut self.cursors[i];
            if k.closed {
                k.span_samples.get_or_insert(span);
                continue;
            }
            let len = match wav_frames(&k.wav) {
                Some(n) => n,
                // Not created yet, or a header caught mid-write: next poll.
                None if !recording_done => continue,
                None => k.read,
            };
            while k.read < len {
                let want = (len - k.read).min(MAX_READ_FRAMES);
                self.buf.clear();
                let got = read_mono(&k.wav, k.read, want, &mut self.buf)?;
                if got == 0 {
                    break;
                }
                on(FeedEvent::Samples { chunk: index, offset_ms: at, start_sample: k.read, samples: &self.buf });
                k.read += got;
            }
            let stable = k.last_len == Some(len);
            k.last_len = Some(len);
            if recording_done || ((later || ended) && stable && k.read >= len) {
                k.closed = true;
                k.span_samples = Some(span);
                on(FeedEvent::Closed { chunk: index, offset_ms: at, samples: k.read });
            }
        }
        Ok(())
    }

    /// Every chunk seen so far is closed.
    pub fn all_closed(&self) -> bool {
        self.cursors.iter().all(|k| k.closed)
    }
}

/// Frames the WAV header at `path` announces; `None` when it does not open
/// as a 16 kHz WAV.
pub fn wav_frames(path: &Path) -> Option<u64> {
    let r = hound::WavReader::open(path).ok()?;
    (r.spec().sample_rate == SAMPLE_RATE).then(|| r.duration() as u64)
}

/// Appends up to `frames` frames of the 16 kHz WAV at `path`, starting at
/// frame `from`, to `out` as mono f32 in [-1, 1] (channels averaged).
/// Returns the frames read; a truncated tail ends the read early.
pub fn read_mono(path: &Path, from: u64, frames: u64, out: &mut Vec<f32>) -> std::io::Result<u64> {
    let mut reader = MonoReader::open(path)?;
    reader.seek(from)?;
    reader.read(frames, out)
}

/// Sequential mono reader over one 16 kHz WAV.
pub struct MonoReader {
    reader: hound::WavReader<std::io::BufReader<std::fs::File>>,
    channels: usize,
    format: hound::SampleFormat,
    scale: f32,
}

impl MonoReader {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let reader = hound::WavReader::open(path).map_err(hound_io)?;
        let spec = reader.spec();
        if spec.sample_rate != SAMPLE_RATE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{} is {} Hz", path.display(), spec.sample_rate),
            ));
        }
        Ok(Self {
            reader,
            channels: spec.channels.max(1) as usize,
            format: spec.sample_format,
            scale: 1.0 / (1i64 << (spec.bits_per_sample.max(1) - 1)) as f32,
        })
    }

    /// Frames the header announces.
    pub fn frames(&self) -> u64 {
        self.reader.duration() as u64
    }

    pub fn seek(&mut self, frame: u64) -> std::io::Result<()> {
        self.reader.seek(frame.min(u32::MAX as u64) as u32)
    }

    /// Appends up to `frames` frames to `out`; returns how many were read.
    pub fn read(&mut self, frames: u64, out: &mut Vec<f32>) -> std::io::Result<u64> {
        let channels = self.channels;
        let want = frames as usize * channels;
        let mut got = 0u64;
        let mut push = |vals: &mut dyn Iterator<Item = f32>| {
            loop {
                let mut sum = 0.0;
                for _ in 0..channels {
                    match vals.next() {
                        Some(v) => sum += v,
                        None => return,
                    }
                }
                out.push(sum / channels as f32);
                got += 1;
            }
        };
        match self.format {
            hound::SampleFormat::Int => {
                let scale = self.scale;
                push(&mut self.reader.samples::<i32>().take(want).map_while(|s| s.ok()).map(|v| v as f32 * scale));
            }
            hound::SampleFormat::Float => {
                push(&mut self.reader.samples::<f32>().take(want).map_while(|s| s.ok()));
            }
        }
        Ok(got)
    }
}

fn hound_io(e: hound::Error) -> std::io::Error {
    match e {
        hound::Error::IoError(e) => e,
        other => std::io::Error::new(std::io::ErrorKind::InvalidData, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, samples: &[i16]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &s in samples {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    }

    fn manifest(root: &Path, chunks: &[(u32, bool)]) {
        let chunks: Vec<serde_json::Value> = chunks
            .iter()
            .map(|&(i, ended)| {
                serde_json::json!({
                    "index": i,
                    "started_at_unix_seconds": 1_000 + i as i64 * 300,
                    "ended_at_unix_seconds": if ended { Some(1_000 + i as i64 * 300 + 300) } else { None },
                    "mic_wav_relative": format!("chunks/{i:04}/mic.wav"),
                    "system_wav_relative": format!("chunks/{i:04}/system.wav"),
                })
            })
            .collect();
        let m = serde_json::json!({
            "schema_version": 2, "session_id": "daisy-1", "created_at_unix_seconds": 1_000,
            "sample_rate": 16_000, "channels": 1,
            "mic_source_id": 1, "mic_source_node_name": "mic", "mic_source_description": "Mic",
            "system_source_id": 2, "system_source_node_name": "sys", "system_source_description": "System",
            "aec_mode": "disabled", "chunks": chunks, "finalized_at_unix_seconds": null, "title": "t",
        });
        std::fs::write(root.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    }

    fn drain(feed: &mut ChunkFeed, done: bool) -> Vec<(u32, u64, usize, bool)> {
        let mut seen = Vec::new();
        feed.poll(done, &mut |e| match e {
            FeedEvent::Samples { chunk, start_sample, samples, .. } => {
                seen.push((chunk, start_sample, samples.len(), false))
            }
            FeedEvent::Closed { chunk, samples, .. } => seen.push((chunk, samples, 0, true)),
        })
        .unwrap();
        seen
    }

    #[test]
    fn a_growing_chunk_is_read_in_pieces_and_closes_once_the_next_one_exists() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let sys = root.join("chunks/0001/system.wav");
        write_wav(&sys, &vec![1000; 16_000]);
        manifest(root, &[(1, false)]);
        let mut feed = ChunkFeed::new(root.to_path_buf(), FeedTrack::System);
        assert_eq!(drain(&mut feed, false), vec![(1, 0, 16_000, false)]);

        write_wav(&sys, &vec![1000; 24_000]);
        assert_eq!(drain(&mut feed, false), vec![(1, 16_000, 8_000, false)]);
        // Same length twice, but the chunk is still the open one.
        assert_eq!(drain(&mut feed, false), vec![]);

        write_wav(&root.join("chunks/0002/system.wav"), &vec![1000; 4_000]);
        manifest(root, &[(1, true), (2, false)]);
        assert_eq!(drain(&mut feed, false), vec![(1, 24_000, 0, true), (2, 0, 4_000, false)]);
        assert!(!feed.all_closed());
        assert_eq!(drain(&mut feed, true), vec![(2, 4_000, 0, true)]);
        assert!(feed.all_closed());
    }

    #[test]
    fn a_chunk_still_growing_after_its_end_stamp_stays_open() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let sys = root.join("chunks/0001/system.wav");
        write_wav(&sys, &vec![1000; 8_000]);
        manifest(root, &[(1, true)]);
        let mut feed = ChunkFeed::new(root.to_path_buf(), FeedTrack::System);
        assert_eq!(drain(&mut feed, false), vec![(1, 0, 8_000, false)]);
        write_wav(&sys, &vec![1000; 9_000]);
        assert_eq!(drain(&mut feed, false), vec![(1, 8_000, 1_000, false)]);
        assert_eq!(drain(&mut feed, false), vec![(1, 9_000, 0, true)]);
    }

    #[test]
    fn offsets_follow_the_longer_track_of_each_earlier_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_wav(&root.join("chunks/0001/system.wav"), &vec![0; 16_000]);
        write_wav(&root.join("chunks/0001/mic.wav"), &vec![0; 32_000]);
        write_wav(&root.join("chunks/0002/system.wav"), &vec![0; 1_600]);
        manifest(root, &[(1, true), (2, true)]);
        let mut feed = ChunkFeed::new(root.to_path_buf(), FeedTrack::System);
        let mut offsets = Vec::new();
        feed.poll(true, &mut |e| {
            if let FeedEvent::Closed { chunk, offset_ms, .. } = e {
                offsets.push((chunk, offset_ms));
            }
        })
        .unwrap();
        assert_eq!(offsets, vec![(1, 0), (2, 2_000)]);
    }

    #[test]
    fn the_live_aec_mic_track_reads_each_chunks_fixed_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_wav(&root.join("chunks/0001/mic.wav"), &vec![0; 16_000]);
        write_wav(&root.join("chunks/0001/mic_live_aec.wav"), &vec![700; 8_000]);
        manifest(root, &[(1, false)]);
        let mut feed = ChunkFeed::new(root.to_path_buf(), FeedTrack::MicLiveAec);
        assert_eq!(drain(&mut feed, false), vec![(1, 0, 8_000, false)]);
        write_wav(&root.join("chunks/0001/mic_live_aec.wav"), &vec![700; 12_000]);
        assert_eq!(drain(&mut feed, true), vec![(1, 8_000, 4_000, false), (1, 12_000, 0, true)]);
    }

    #[test]
    fn mono_reads_match_the_whole_file_read_in_one_go() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        let pcm: Vec<i16> = (0..20_000).map(|i| ((i * 37) % 2000 - 1000) as i16).collect();
        write_wav(&p, &pcm);
        let mut whole = Vec::new();
        read_mono(&p, 0, 20_000, &mut whole).unwrap();
        let mut parts = Vec::new();
        for from in (0..20_000).step_by(7_000) {
            read_mono(&p, from, 7_000, &mut parts).unwrap();
        }
        assert_eq!(whole, parts);
        assert_eq!(whole[3], pcm[3] as f32 / 32768.0);
    }

    #[test]
    fn a_closed_chunks_offset_is_read_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_wav(&root.join("chunks/0001/system.wav"), &vec![0; 16_000]);
        write_wav(&root.join("chunks/0002/system.wav"), &vec![0; 1_600]);
        manifest(root, &[(1, true), (2, false)]);
        let mut feed = ChunkFeed::new(root.to_path_buf(), FeedTrack::System);
        let offsets = |feed: &mut ChunkFeed| {
            let mut out = Vec::new();
            feed.poll(false, &mut |e| {
                if let FeedEvent::Samples { chunk, offset_ms, .. } = e {
                    out.push((chunk, offset_ms));
                }
            })
            .unwrap();
            out
        };
        assert_eq!(offsets(&mut feed), vec![(1, 0), (2, 1_000)]);
        offsets(&mut feed);
        write_wav(&root.join("chunks/0001/system.wav"), &vec![0; 48_000]);
        write_wav(&root.join("chunks/0002/system.wav"), &vec![0; 3_200]);
        assert_eq!(offsets(&mut feed), vec![(2, 1_000)]);
    }
}
