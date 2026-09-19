// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Cross-track dedup: drop Me segments that are bleed-through of Them speech.

use crate::text::norm_word;
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
    /// Count of segment spans clipped to their audible extent.
    #[serde(default)]
    pub clipped_spans: usize,
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
    let mut clipped_spans: usize = 0;

    for chunk in &transcript.chunks {
        let mic_track_idx = chunk
            .tracks
            .iter()
            .position(|t| t.track.is_mic());
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
        let sys_wav_path = session_root.join(&sys_track.source_wav_relative);
        let sys_wav = WavSamples::load(&sys_wav_path)?;

        // Boundary-repeat trim runs on the raw whisper stamps: the padded
        // spans overlapping at a boundary ARE the evidence of a re-decode
        // repeat. Then every span is clipped to its audible extent, so the
        // bleed passes below measure RMS over speech instead of padding —
        // and the rendered timeline breaks where the audio does.
        let mic_segs = trim_boundary_repeats(
            mic_track.segments.clone(),
            params.overlap_slack_ms,
            &mut dropped,
        );
        let sys_segs = trim_boundary_repeats(
            sys_track.segments.clone(),
            params.overlap_slack_ms,
            &mut dropped,
        );
        let mic_segs = clip_segs(mic_segs, &mic_wav, &mut clipped_spans);
        let sys_segs = clip_segs(sys_segs, &sys_wav, &mut clipped_spans);

        let mut new_mic_segs: Vec<Segment> = Vec::with_capacity(mic_segs.len());

        for m in &mic_segs {
            // 1. Backchannel filter — drops "yeah" / "mm-hmm" / "oh" etc.
            //    Independent of overlap with the system track.
            if params.drop_backchannels && is_backchannel(&m.text) {
                dropped += 1;
                continue;
            }

            // 2. Cross-track bleed dedup.
            if find_echo(m, &sys_segs, params) {
                let db = rms_dbfs_window(&mic_wav, m.start_ms, m.end_ms);
                if db <= params.mic_quiet_dbfs {
                    dropped += 1;
                    continue;
                }
            }
            new_mic_segs.push(m.clone());
        }

        // System-side filters. A sys segment is dropped when it is:
        //   a. a pure backchannel; or
        //   b. bleed-from-mic: an overlapping mic segment with high jaccard
        //      while the system RMS is quiet.
        let mut new_sys_segs: Vec<Segment> = Vec::with_capacity(sys_segs.len());
        for s in &sys_segs {
            if params.drop_backchannels && is_backchannel(&s.text) {
                dropped += 1;
                continue;
            }
            // Compares against the pre-bleed-drop mic segments, not
            // new_mic_segs.
            if find_echo(s, &mic_segs, params) {
                let sys_db = rms_dbfs_window(&sys_wav, s.start_ms, s.end_ms);
                if sys_db <= params.sys_quiet_dbfs {
                    dropped += 1;
                    continue;
                }
            }
            new_sys_segs.push(s.clone());
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
        kept_count += new_tracks[mi].segments.len();
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
            clipped_spans,
        },
    })
}

/// Clip each segment's span to its audible extent; a span with no audible
/// window keeps its stamps.
fn clip_segs(segs: Vec<Segment>, wav: &WavSamples, clipped: &mut usize) -> Vec<Segment> {
    segs.into_iter()
        .map(|mut s| {
            if s.start_ms < wav.duration_ms() {
                if let Some((cs, ce)) = crate::span_clip::clip_span(wav, s.start_ms, s.end_ms) {
                    if (cs, ce) != (s.start_ms, s.end_ms) {
                        *clipped += 1;
                    }
                    s.start_ms = cs;
                    s.end_ms = ce;
                }
            }
            s
        })
        .collect()
}

/// Longest prev-segment word tail remembered for the boundary-repeat trim.
const BOUNDARY_TAIL_WORDS: usize = 12;
/// Minimum repeated run, in words, for a boundary trim to apply. Shorter
/// repeats at a boundary are treated as genuine speech.
const BOUNDARY_MIN_RUN: usize = 3;

/// Same-track boundary-repeat trim. The live window re-decodes retained audio
/// after a guarded trim and can re-commit a word run it already emitted, so
/// consecutive segments repeat that run across a time-overlapping boundary.
/// The leading repeat — optionally hidden behind one variant word — is
/// trimmed off the later segment; a segment left with no words is dropped.
fn trim_boundary_repeats(segs: Vec<Segment>, slack_ms: u32, dropped: &mut usize) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::with_capacity(segs.len());
    for seg in segs {
        let Some(prev) = out.last() else {
            out.push(seg);
            continue;
        };
        if seg.start_ms >= prev.end_ms.saturating_add(slack_ms) {
            out.push(seg);
            continue;
        }
        let tail: Vec<String> = prev
            .text
            .split_whitespace()
            .map(norm_word)
            .filter(|w| !w.is_empty())
            .collect();
        let tail = &tail[tail.len().saturating_sub(BOUNDARY_TAIL_WORDS)..];
        let words: Vec<&str> = seg.text.split_whitespace().collect();
        let normed: Vec<String> = words.iter().map(|w| norm_word(w)).collect();
        let strip = [0usize, 1]
            .into_iter()
            .filter_map(|skip| {
                let max_k = tail.len().min(words.len().saturating_sub(skip));
                (BOUNDARY_MIN_RUN..=max_k)
                    .rev()
                    .find(|&k| {
                        tail[tail.len() - k..]
                            .iter()
                            .zip(&normed[skip..skip + k])
                            .all(|(p, n)| !n.is_empty() && p == n)
                    })
                    .map(|k| skip + k)
            })
            .next()
            .unwrap_or(0);
        if strip == 0 {
            out.push(seg);
            continue;
        }
        if strip >= words.len() {
            *dropped += 1;
            continue;
        }
        let span = seg.end_ms.saturating_sub(seg.start_ms);
        let new_start =
            seg.start_ms + (span as u64 * strip as u64 / words.len() as u64) as u32;
        out.push(Segment {
            start_ms: new_start,
            text: words[strip..].join(" "),
            ..seg
        });
    }
    out
}

fn overlaps(a: &Segment, b: &Segment, slack: u32) -> bool {
    a.start_ms.saturating_sub(slack) <= b.end_ms && a.end_ms.saturating_add(slack) >= b.start_ms
}

/// True when some segment in `others` time-overlaps `self_seg` (with slack)
/// AND has bigram-Jaccard text similarity ≥ `params.jaccard_threshold`.
/// Used by both directions of bleed dedup (mic→sys and sys→mic).
fn find_echo(self_seg: &Segment, others: &[Segment], params: &DedupParams) -> bool {
    others.iter().any(|o| {
        overlaps(self_seg, o, params.overlap_slack_ms)
            && bigram_jaccard(&self_seg.text, &o.text) >= params.jaccard_threshold
    })
}
