// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Shrink segment spans to the speech energy they actually contain.
//!
//! Whisper stamps segments wall-to-wall across a decode window, so a segment
//! span can carry seconds of leading/trailing silence (or the other side's
//! turn). Every downstream consumer that reads spans — cross-track dedup's
//! RMS-quiet checks and time-overlap test, the render timeline, echo
//! windows — then measures the wrong audio. This pass clips each span to the
//! first/last audible window inside it. Spans only shrink, never grow, and a
//! segment with no audible window keeps its original span.

use crate::rms::{rms_dbfs_window, WavSamples};

/// RMS scan granularity inside a span.
const WIN_MS: u32 = 100;
/// Kept around the clipped bounds so word onsets/decays survive.
const MARGIN_MS: u32 = 120;
/// A window this far under the segment's loudest window is padding.
const REL_FLOOR_DB: f32 = 20.0;
/// Windows quieter than this never count as speech regardless of peak.
const ABS_FLOOR_DBFS: f32 = -55.0;

/// Clipped `[start_ms, end_ms)` for one span, or `None` when no window inside
/// clears the audibility floor (span is left as-is by the caller).
pub fn clip_span(wav: &WavSamples, start_ms: u32, end_ms: u32) -> Option<(u32, u32)> {
    let end_ms = end_ms.min(wav.duration_ms());
    if end_ms <= start_ms {
        return None;
    }
    let mut loudest = -120.0f32;
    let mut levels: Vec<(u32, f32)> = Vec::new();
    let mut t = start_ms;
    while t < end_ms {
        let e = (t + WIN_MS).min(end_ms);
        let db = rms_dbfs_window(wav, t, e);
        loudest = loudest.max(db);
        levels.push((t, db));
        t = e;
    }
    let threshold = (loudest - REL_FLOOR_DB).max(ABS_FLOOR_DBFS);
    if loudest < ABS_FLOOR_DBFS {
        return None;
    }
    let first = levels.iter().find(|(_, db)| *db >= threshold)?.0;
    let last = levels.iter().rev().find(|(_, db)| *db >= threshold)?.0 + WIN_MS;
    Some((
        first.saturating_sub(MARGIN_MS).max(start_ms),
        last.saturating_add(MARGIN_MS).min(end_ms),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16_000;

    /// `spec` = per-100ms slots: true = tone at ~-12 dBFS, false = silence.
    fn wav_from_slots(spec: &[bool]) -> WavSamples {
        let slot = (SR / 10) as usize;
        let mut samples = Vec::with_capacity(spec.len() * slot);
        for &on in spec {
            for i in 0..slot {
                let v = if on {
                    let phase = i as f32 / SR as f32 * 2.0 * std::f32::consts::PI * 440.0;
                    (8_000.0 * phase.sin()) as i16
                } else {
                    0
                };
                samples.push(v);
            }
        }
        WavSamples::from_raw(samples, SR)
    }

    #[test]
    fn trailing_padding_is_clipped() {
        // Speech 0-2s, silence 2-9s — the padded-stamp shape whisper emits.
        let mut slots = vec![true; 20];
        slots.extend(vec![false; 70]);
        let wav = wav_from_slots(&slots);
        let (s, e) = clip_span(&wav, 0, 9_000).unwrap();
        assert_eq!(s, 0);
        assert!((1_900..=2_200).contains(&e), "end must clip to ~2s, got {e}");
    }

    #[test]
    fn leading_padding_is_clipped() {
        let mut slots = vec![false; 50];
        slots.extend(vec![true; 20]);
        let wav = wav_from_slots(&slots);
        let (s, e) = clip_span(&wav, 0, 7_000).unwrap();
        assert!((4_800..=5_100).contains(&s), "start must clip to ~5s, got {s}");
        assert_eq!(e, 7_000);
    }

    #[test]
    fn silent_span_is_left_alone() {
        let wav = wav_from_slots(&vec![false; 50]);
        assert!(clip_span(&wav, 0, 5_000).is_none());
    }

    #[test]
    fn full_speech_span_keeps_its_bounds() {
        let wav = wav_from_slots(&vec![true; 30]);
        let (s, e) = clip_span(&wav, 0, 3_000).unwrap();
        assert_eq!((s, e), (0, 3_000));
    }

    #[test]
    fn interior_silence_does_not_split_only_bounds_clip() {
        // Bursts at 1-2s and 6-7s: the clip keeps one span across both.
        let mut slots = vec![false; 10];
        slots.extend(vec![true; 10]);
        slots.extend(vec![false; 40]);
        slots.extend(vec![true; 10]);
        slots.extend(vec![false; 10]);
        let wav = wav_from_slots(&slots);
        let (s, e) = clip_span(&wav, 0, 8_000).unwrap();
        assert!((800..=1_100).contains(&s), "got start {s}");
        assert!((6_900..=7_200).contains(&e), "got end {e}");
    }

    #[test]
    fn span_past_the_audio_is_unmeasurable() {
        let wav = wav_from_slots(&vec![true; 10]);
        assert!(clip_span(&wav, 2_000, 3_000).is_none());
    }
}
