//! Pure helpers for mutating a SessionManifest across the recording lifecycle.
use crate::manifest::{RecordingSegment, SessionManifest};

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
        started_at_unix_seconds: started_at,
        stopped_at_unix_seconds: None,
        first_chunk_index: next_chunk,
        last_chunk_index: None,
    });
}
