// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Helpers for mutating a SessionManifest across the recording lifecycle.
use crate::error::Result;
use crate::manifest::{ChunkManifest, RecordingSegment, SessionManifest};
use crate::session::Session;
use std::path::Path;

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Frames in a WAV file, read from its header; `None` when it does not open.
/// The capture writer updates the header with every frame, so a file still
/// being written reads as what is on disk so far.
pub fn wav_frames(path: &Path) -> Option<u64> {
    hound::WavReader::open(path).ok().map(|r| u64::from(r.duration()))
}

/// Samples a chunk holds on disk: the longer of its mic and system WAVs.
/// `None` when neither opens.
pub fn chunk_wav_samples(root: &Path, c: &ChunkManifest) -> Option<u64> {
    let mic = wav_frames(&syncsafe::join_rel(root, &c.mic_wav_relative));
    let sys = if c.system_wav_relative.as_os_str().is_empty() {
        None
    } else {
        wav_frames(&syncsafe::join_rel(root, &c.system_wav_relative))
    };
    mic.into_iter().chain(sys).max()
}

/// Whole seconds of `samples` at `sample_rate`, rounded to the nearest.
pub fn whole_seconds_of(samples: u64, sample_rate: u32) -> u64 {
    let rate = u64::from(sample_rate.max(1));
    (samples + rate / 2) / rate
}

/// True when every chunk has a WAV on disk to measure.
pub fn chunk_audio_on_disk(root: &Path, m: &SessionManifest) -> bool {
    m.chunks.iter().all(|c| chunk_wav_samples(root, c).is_some())
}

/// Stamp every chunk's `audio_samples` from its WAVs and bring
/// `duration_seconds` to that audio when it is missing or more than a second
/// off. A missing `ended_at_unix_seconds` becomes start + duration.
/// A chunk whose WAVs do
/// not open keeps what it has. Returns whether anything changed.
pub fn stamp_chunk_audio_at(root: &Path, m: &mut SessionManifest) -> bool {
    let rate = m.sample_rate;
    let mut changed = false;
    for c in m.chunks.iter_mut() {
        let Some(n) = chunk_wav_samples(root, c) else { continue };
        let secs = whole_seconds_of(n, rate);
        if c.audio_samples != Some(n) {
            c.audio_samples = Some(n);
            changed = true;
        }
        if c.duration_seconds.is_none_or(|d| d.abs_diff(secs) > 1) {
            c.duration_seconds = Some(secs);
            changed = true;
        }
        if c.ended_at_unix_seconds.is_none() {
            let d = c.duration_seconds.unwrap_or(secs);
            c.ended_at_unix_seconds = Some(c.started_at_unix_seconds + d as i64);
            changed = true;
        }
    }
    changed
}

/// [`stamp_chunk_audio_at`] on a session, writing the manifest only when
/// something changed. Idempotent.
pub fn stamp_chunk_audio(session: &mut Session) -> Result<()> {
    let root = session.root().to_path_buf();
    let mut m = session.manifest().clone();
    if !stamp_chunk_audio_at(&root, &mut m) {
        return Ok(());
    }
    session.update_manifest(|cur| cur.chunks = m.chunks)
}

/// Idempotently stamp `finalized_at_unix_seconds` (kept if already set), set
/// the `interrupted` flag to `interrupted`, and close any still-open chunk /
/// recording segment: a chunk closes at start + duration when a duration is
/// known and at now otherwise; a segment closes at the last chunk's end (or
/// now). Writes the manifest only when something changed. Deletes each
/// chunk's live echo-cancelled mic ([`crate::live_aec_wav`]).
///
/// Polarity: `interrupted` = "recording ended without a clean stop" — set
/// `true` when an orphan is detected, cleared (`false`) by every successful
/// finalize.
pub fn stamp_finalized(session: &mut Session, interrupted: bool) -> Result<()> {
    let now = now_unix();
    session.update_manifest(|m| {
        if m.finalized_at_unix_seconds.is_none() {
            m.finalized_at_unix_seconds = Some(now);
        }
        m.interrupted = interrupted;
        for c in m.chunks.iter_mut() {
            if c.ended_at_unix_seconds.is_none() {
                let ended = match c.duration_seconds {
                    Some(d) if d > 0 => c.started_at_unix_seconds + d as i64,
                    _ => now,
                };
                c.ended_at_unix_seconds = Some(ended);
            }
        }
        let last_end = m.chunks.last().and_then(|c| c.ended_at_unix_seconds);
        let last_index = m.chunks.last().map(|c| c.index);
        for seg in m.recording_segments.iter_mut() {
            if seg.stopped_at_unix_seconds.is_none() {
                seg.stopped_at_unix_seconds = Some(last_end.unwrap_or(now));
                seg.last_chunk_index = last_index;
            }
        }
    })?;
    crate::live_aec_wav::remove_all(session.root(), session.manifest());
    Ok(())
}

/// Last-modified time of `path` in unix seconds.
fn mtime_unix(path: &std::path::Path) -> Option<i64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
}

/// True when the manifest holds a recording segment with no stop time.
pub fn has_open_segment(m: &SessionManifest) -> bool {
    m.recording_segments.iter().any(|s| s.stopped_at_unix_seconds.is_none())
}

/// Closes what a recording process left open when it died: every chunk
/// with no end and every segment with no stop time, and marks the session
/// `interrupted`. Times come from the audio itself:
/// - an open chunk ends at its start plus the audio its WAVs hold
///   ([`chunk_wav_samples`], whole seconds by [`whole_seconds_of`]), and
///   that sample count is stamped as its `audio_samples`; with neither WAV
///   readable, at the later of its two files' modification times; with no
///   file, at its start;
/// - an open segment stops at the end of its last chunk; a segment with no
///   chunk stops at `last_alive_unix` (the heartbeat's last stamp) when
///   that is later than its start, else at its start.
///
/// Returns whether anything changed; the manifest is written only then.
pub fn close_abandoned(session: &mut Session, last_alive_unix: Option<i64>) -> Result<bool> {
    let root = session.root().to_path_buf();
    let m = session.manifest();
    let rate = m.sample_rate;
    // (chunk index, end, audio samples when a WAV opened)
    let open_chunks: Vec<(u32, i64, Option<u64>)> = m
        .chunks
        .iter()
        .filter(|c| c.ended_at_unix_seconds.is_none())
        .map(|c| {
            let start = c.started_at_unix_seconds;
            match chunk_wav_samples(&root, c) {
                Some(n) => (c.index, start + whole_seconds_of(n, rate) as i64, Some(n)),
                None => {
                    let mic = syncsafe::join_rel(&root, &c.mic_wav_relative);
                    let sys = syncsafe::join_rel(&root, &c.system_wav_relative);
                    let end = mtime_unix(&mic).max(mtime_unix(&sys)).unwrap_or(start).max(start);
                    (c.index, end, None)
                }
            }
        })
        .collect();
    if open_chunks.is_empty() && !has_open_segment(m) {
        return Ok(false);
    }
    session.update_manifest(|m| {
        for (index, end, samples) in open_chunks {
            let Some(c) = m.chunks.iter_mut().find(|c| c.index == index) else { continue };
            c.ended_at_unix_seconds = Some(end);
            c.duration_seconds = Some((end - c.started_at_unix_seconds).max(0) as u64);
            if samples.is_some() {
                c.audio_samples = samples;
            }
        }
        let chunks = m.chunks.clone();
        for seg in m.recording_segments.iter_mut().filter(|s| s.stopped_at_unix_seconds.is_none()) {
            let last = chunks
                .iter()
                .filter(|c| c.index >= seg.first_chunk_index)
                .max_by_key(|c| c.index);
            match last {
                Some(c) => {
                    seg.stopped_at_unix_seconds =
                        Some(c.ended_at_unix_seconds.unwrap_or(c.started_at_unix_seconds));
                    seg.last_chunk_index = Some(c.index);
                }
                None => {
                    let start = seg.started_at_unix_seconds;
                    seg.stopped_at_unix_seconds = Some(last_alive_unix.unwrap_or(start).max(start));
                }
            }
        }
        m.interrupted = true;
    })?;
    Ok(true)
}

/// True when the session directory at `root` holds nothing a recording
/// produced, the shape a start that never opened its first chunk leaves:
/// at the top only `manifest.json`, `heartbeat`, the diagnostics logs
/// (`metrics.jsonl`, `telemetry.jsonl`), empty files and a `chunks`
/// directory; under `chunks/<NNNN>/` only files with no audio samples
/// (empty, or a WAV header with no frames). A symlink, an unreadable entry,
/// an unparseable non-empty WAV or any other directory reads as a
/// recording.
pub fn holds_no_recording(root: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(root) else { return false };
    if !meta.is_dir() {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(root) else { return false };
    for e in entries {
        let Ok(e) = e else { return false };
        let Ok(ft) = e.file_type() else { return false };
        let name = e.file_name();
        if ft.is_dir() && name == "chunks" {
            if !chunks_hold_no_audio(&e.path()) {
                return false;
            }
        } else if ft.is_file() {
            let known = matches!(
                name.to_str(),
                Some("manifest.json" | "heartbeat" | crate::flight_recorder::METRICS_FILE | crate::telemetry::TELEMETRY_FILE)
            );
            if !known && !e.metadata().is_ok_and(|m| m.len() == 0) {
                return false;
            }
        } else {
            return false;
        }
    }
    true
}

/// [`holds_no_recording`] for the `chunks` directory.
fn chunks_hold_no_audio(chunks: &std::path::Path) -> bool {
    let Ok(dirs) = std::fs::read_dir(chunks) else { return false };
    for d in dirs {
        let Ok(d) = d else { return false };
        if !d.file_type().is_ok_and(|t| t.is_dir()) {
            return false;
        }
        let Ok(files) = std::fs::read_dir(d.path()) else { return false };
        for f in files {
            let Ok(f) = f else { return false };
            if !f.file_type().is_ok_and(|t| t.is_file()) {
                return false;
            }
            let empty = f.metadata().is_ok_and(|m| m.len() == 0)
                || hound::WavReader::open(f.path()).is_ok_and(|r| r.duration() == 0);
            if !empty {
                return false;
            }
        }
    }
    true
}

/// Close the currently-open recording segment (last one with no stop time).
/// No-op if there is no open segment.
pub fn close_active_segment(m: &mut SessionManifest, stopped_at: i64, last_chunk_index: u32) {
    if let Some(seg) = m
        .recording_segments
        .iter_mut()
        .rev()
        .find(|s| s.stopped_at_unix_seconds.is_none())
    {
        seg.stopped_at_unix_seconds = Some(stopped_at);
        seg.last_chunk_index = Some(last_chunk_index);
    }
}

/// Open a new recording segment starting after the highest chunk index seen so far.
pub fn open_segment(m: &mut SessionManifest, started_at: i64) {
    let next_chunk = m
        .recording_segments
        .iter()
        .filter_map(|s| s.last_chunk_index)
        .max()
        .map(|n| n + 1)
        .unwrap_or(1);
    m.recording_segments.push(RecordingSegment {
        reason: None,
        extra: Default::default(),
        started_at_unix_seconds: started_at,
        stopped_at_unix_seconds: None,
        first_chunk_index: next_chunk,
        last_chunk_index: None,
    });
}

/// Set `reason` on the currently-open recording segment (last one with no
/// stop time). No-op if there is no open segment.
pub fn set_open_segment_reason(m: &mut SessionManifest, reason: &str) {
    if let Some(seg) = m
        .recording_segments
        .iter_mut()
        .rev()
        .find(|s| s.stopped_at_unix_seconds.is_none())
    {
        seg.reason = Some(reason.to_string());
    }
}
