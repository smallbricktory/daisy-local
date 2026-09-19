// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Helpers for mutating a SessionManifest across the recording lifecycle.
use crate::error::Result;
use crate::manifest::{RecordingSegment, SessionManifest};
use crate::session::Session;

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Fill in any chunk missing `duration_seconds` from its mic WAV length
/// (chunks whose duration is already known are left alone), and set
/// `ended_at_unix_seconds` to start + duration where it is missing.
/// Idempotent; writes the manifest only when something changed.
pub fn backfill_chunk_durations(session: &mut Session) -> Result<()> {
    let sr = session.manifest().sample_rate as u64;
    let root = session.root().to_path_buf();
    let mut updates: Vec<(usize, u64)> = Vec::new();
    for (i, c) in session.manifest().chunks.iter().enumerate() {
        if c.duration_seconds.is_some() {
            continue;
        }
        let r = hound::WavReader::open(root.join(&c.mic_wav_relative))?;
        let n = r.duration() as u64; // frames, mono
        updates.push((i, n / sr.max(1)));
    }
    if updates.is_empty() {
        return Ok(());
    }
    session.update_manifest(|m| {
        for (i, dur) in updates {
            if let Some(c) = m.chunks.get_mut(i) {
                if c.ended_at_unix_seconds.is_none() {
                    c.ended_at_unix_seconds = Some(c.started_at_unix_seconds + dur as i64);
                }
                c.duration_seconds = Some(dur);
            }
        }
    })
}

/// Idempotently stamp `finalized_at_unix_seconds` (kept if already set), set
/// the `interrupted` flag to `interrupted`, and close any still-open chunk /
/// recording segment: a chunk closes at start + duration when a duration is
/// known and at now otherwise; a segment closes at the last chunk's end (or
/// now). Writes the manifest only when something changed.
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
    })
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
        extra: Default::default(),
        started_at_unix_seconds: started_at,
        stopped_at_unix_seconds: None,
        first_chunk_index: next_chunk,
        last_chunk_index: None,
    });
}
