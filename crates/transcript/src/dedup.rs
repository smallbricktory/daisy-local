//! Cross-track dedup: drop Me segments that are bleed-through of Them speech.

use crate::backchannel::is_backchannel;
use crate::error::Result;
use crate::model::{ChunkTranscript, Segment, SessionTranscript, Track, TrackTranscript};
use crate::rms::{rms_dbfs_window, WavSamples};
use crate::text::bigram_jaccard;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DedupParams {
    /// Bigram Jaccard threshold (inclusive). Default 0.6.
    pub jaccard_threshold: f32,
    /// Mic RMS dBFS threshold. Mic must be quieter than this to count as
    /// bleed-from-system (mic-side drop). Default -35 dB.
    pub mic_quiet_dbfs: f32,
    /// System RMS dBFS threshold. System must be quieter than this to count
    /// as bleed-from-mic (system-side drop). Default -35 dB.
    #[serde(default = "default_sys_quiet_dbfs")]
    pub sys_quiet_dbfs: f32,
    /// Time-overlap slack in ms. Default 500.
    pub overlap_slack_ms: u32,
    /// Drop pure-backchannel mic segments ("yeah", "mm-hmm", "oh", etc.). Default true.
    #[serde(default = "default_drop_backchannels")]
    pub drop_backchannels: bool,
}

fn default_drop_backchannels() -> bool {
    true
}

fn default_sys_quiet_dbfs() -> f32 {
    -35.0
}

impl Default for DedupParams {
    fn default() -> Self {
        Self {
            jaccard_threshold: 0.6,
            mic_quiet_dbfs: -35.0,
            sys_quiet_dbfs: -35.0,
            overlap_slack_ms: 500,
            drop_backchannels: true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DedupReport {
    pub params: DedupParams,
    /// Count of segments dropped (backchannels + cross-track bleed).
    pub dropped: usize,
    pub kept_count: usize,
}

pub struct DedupResult {
    pub deduped: SessionTranscript,
    pub report: DedupReport,
}

pub fn dedup_session(
    transcript: &SessionTranscript,
    session_root: &Path,
    params: &DedupParams,
) -> Result<DedupResult> {
    let mut out_chunks: Vec<ChunkTranscript> = Vec::with_capacity(transcript.chunks.len());
    let mut dropped: usize = 0;
    let mut kept_count: usize = 0;

    for chunk in &transcript.chunks {
        let mic_track_idx = chunk
            .tracks
            .iter()
            .position(|t| matches!(t.track, Track::Mic | Track::MicAec));
        let sys_track_idx = chunk
            .tracks
            .iter()
            .position(|t| t.track == Track::System);

        // If either track is absent, pass through unchanged.
        let (Some(mi), Some(si)) = (mic_track_idx, sys_track_idx) else {
            kept_count += chunk.tracks.iter().map(|t| t.segments.len()).sum::<usize>();
            out_chunks.push(chunk.clone());
            continue;
        };

        let mic_track = &chunk.tracks[mi];
        let sys_track = &chunk.tracks[si];

        // An empty track also passes through, without loading its source wav;
        // audio imports write a placeholder mic entry with zero segments and
        // no mic.wav on disk.
        if mic_track.segments.is_empty() || sys_track.segments.is_empty() {
            kept_count += chunk.tracks.iter().map(|t| t.segments.len()).sum::<usize>();
            out_chunks.push(chunk.clone());
            continue;
        }

        let mic_wav_path = session_root.join(&mic_track.source_wav_relative);
        let mic_wav = WavSamples::load(&mic_wav_path)?;

        let mut new_mic_segs: Vec<Segment> = Vec::with_capacity(mic_track.segments.len());

        for m in &mic_track.segments {
            // 1. Backchannel filter — drops "yeah" / "mm-hmm" / "oh" etc.
            //    Independent of overlap with the system track.
            if params.drop_backchannels && is_backchannel(&m.text) {
                dropped += 1;
                continue;
            }

            // 2. Cross-track bleed dedup.
            let dup = find_echo(m, &sys_track.segments, params);
            match dup {
                Some(_) => {
                    let db = rms_dbfs_window(&mic_wav, m.start_ms, m.end_ms);
                    if db <= params.mic_quiet_dbfs {
                        dropped += 1;
                    } else {
                        kept_count += 1;
                        new_mic_segs.push(m.clone());
                    }
                }
                None => {
                    kept_count += 1;
                    new_mic_segs.push(m.clone());
                }
            }
        }

        // System-side filters. A sys segment is dropped when it is:
        //   a. a pure backchannel; or
        //   b. bleed-from-mic: an overlapping mic segment with high jaccard
        //      while the system RMS is quiet.
        let sys_wav_path = session_root.join(&sys_track.source_wav_relative);
        let sys_wav = WavSamples::load(&sys_wav_path)?;

        let mut new_sys_segs: Vec<Segment> = Vec::with_capacity(sys_track.segments.len());
        for s in &sys_track.segments {
            if params.drop_backchannels && is_backchannel(&s.text) {
                dropped += 1;
                continue;
            }
            // Compares against the original (pre-dedup) mic segments, not
            // new_mic_segs.
            let echo = find_echo(s, &mic_track.segments, params);
            match echo {
                Some(_) => {
                    let sys_db = rms_dbfs_window(&sys_wav, s.start_ms, s.end_ms);
                    if sys_db <= params.sys_quiet_dbfs {
                        dropped += 1;
                        continue;
                    }
                    new_sys_segs.push(s.clone());
                }
                None => new_sys_segs.push(s.clone()),
            }
        }

        let mut new_tracks = chunk.tracks.clone();
        new_tracks[mi] = TrackTranscript {
            track: mic_track.track,
            source_wav_relative: mic_track.source_wav_relative.clone(),
            segments: new_mic_segs,
        };
        new_tracks[si] = TrackTranscript {
            track: sys_track.track,
            source_wav_relative: sys_track.source_wav_relative.clone(),
            segments: new_sys_segs,
        };
        kept_count += new_tracks[si].segments.len();

        out_chunks.push(ChunkTranscript {
            chunk_index: chunk.chunk_index,
            tracks: new_tracks,
        });
    }

    Ok(DedupResult {
        deduped: SessionTranscript {
            chunks: out_chunks,
            ..transcript.clone()
        },
        report: DedupReport {
            params: *params,
            dropped,
            kept_count,
        },
    })
}

fn overlaps(a: &Segment, b: &Segment, slack: u32) -> bool {
    a.start_ms.saturating_sub(slack) <= b.end_ms && a.end_ms.saturating_add(slack) >= b.start_ms
}

/// Find the first segment in `others` that time-overlaps `self_seg` (with
/// slack) AND has bigram-Jaccard text similarity ≥ `params.jaccard_threshold`.
/// Returns the similarity score. Used by both directions of bleed dedup
/// (mic→sys and sys→mic).
fn find_echo(self_seg: &Segment, others: &[Segment], params: &DedupParams) -> Option<f32> {
    others.iter().find_map(|o| {
        if !overlaps(self_seg, o, params.overlap_slack_ms) {
            return None;
        }
        let sim = bigram_jaccard(&self_seg.text, &o.text);
        (sim >= params.jaccard_threshold).then_some(sim)
    })
}
