// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Engine-agnostic helpers shared by the streaming backends: sample
//! conversion, the peak-energy speech gate, level normalization, the
//! committed-token → `Segment` emit path, and the sustained-overload
//! reporter.

use std::time::Instant;

use providers_realtime::RealtimeEvent;
use tokio::sync::mpsc;
use transcript::Segment;

use super::agreement::StreamToken;

/// Speech gate: a decode whose window peak is below this (~-55 dBFS,
/// i16 peak ≈ 58) is skipped.
pub const SILENCE_PEAK_F32: f32 = 58.0 / 32768.0;

/// Committed-run split threshold: consecutive words further apart than this
/// are separate utterances and emit as separate Finals.
pub const FINAL_GAP_SPLIT_MS: i64 = 2_000;

/// A run of continuous speech closes at the next word gap of at least
/// [`SPLIT_MIN_GAP_MS`] once it spans [`TARGET_SEG_MS`].
pub const TARGET_SEG_MS: i64 = 12_000;
pub const SPLIT_MIN_GAP_MS: i64 = 250;

/// Committed words grouped into utterances: a gap of
/// [`FINAL_GAP_SPLIT_MS`] closes one, and so does a
/// [`SPLIT_MIN_GAP_MS`] gap once it spans [`TARGET_SEG_MS`].
#[derive(Default)]
pub struct Utterances {
    open: Vec<StreamToken>,
}

impl Utterances {
    /// Adds a word; returns the utterance it closed, if any.
    pub fn push(&mut self, w: StreamToken) -> Option<Vec<StreamToken>> {
        let closed = match (self.open.first(), self.open.last()) {
            (Some(first), Some(last)) => {
                let gap = w.start_ms - last.end_ms;
                let long = last.end_ms - first.start_ms >= TARGET_SEG_MS;
                (gap >= FINAL_GAP_SPLIT_MS || (long && gap >= SPLIT_MIN_GAP_MS))
                    .then(|| std::mem::take(&mut self.open))
            }
            _ => None,
        };
        self.open.push(w);
        closed
    }

    /// Closes the open utterance once `now_ms` is [`FINAL_GAP_SPLIT_MS`]
    /// past its last word.
    pub fn close_if_quiet(&mut self, now_ms: i64) -> Option<Vec<StreamToken>> {
        let last = self.open.last()?;
        (now_ms - last.end_ms >= FINAL_GAP_SPLIT_MS).then(|| std::mem::take(&mut self.open))
    }

    pub fn close(&mut self) -> Option<Vec<StreamToken>> {
        (!self.open.is_empty()).then(|| std::mem::take(&mut self.open))
    }

    pub fn open(&self) -> &[StreamToken] {
        &self.open
    }
}

/// The stored segment for one utterance: masked word texts joined, with the
/// per-word timing kept on the segment.
pub fn utterance_segment(words: &[StreamToken]) -> Option<Segment> {
    Segment::from_words(
        words.iter().map(|t| (transcript::text::mask_profanity(&t.text), t.start_ms, t.end_ms)),
    )
}

/// The interim line for `toks`; `None` when there are no tokens.
pub fn interim_event(toks: &[StreamToken]) -> Option<RealtimeEvent> {
    join_tokens(toks).map(|t| RealtimeEvent::Interim { segment: to_segment(&t) })
}

/// Sends `toks` as one Final and records it in `accepted`.
pub async fn emit_final(
    toks: &[StreamToken],
    events_tx: &mpsc::Sender<RealtimeEvent>,
    accepted: &mut Vec<Segment>,
) {
    if let Some(seg) = utterance_segment(toks) {
        let _ = events_tx.send(RealtimeEvent::Final { segment: seg.clone() }).await;
        accepted.push(seg);
    }
}

/// Committed words → one `Segment` per utterance run (split at real word
/// gaps).
pub fn finals_from(words: &[StreamToken]) -> Vec<Segment> {
    split_at_gaps(words, FINAL_GAP_SPLIT_MS)
        .into_iter()
        .filter_map(utterance_segment)
        .collect()
}

/// Emit each utterance run of `committed` as a Final and record it in
/// `accepted`: the shared tail of the whisper and SpeechAnalyzer decode
/// steps.
pub async fn emit_finals(
    committed: &[StreamToken],
    events_tx: &mpsc::Sender<RealtimeEvent>,
    accepted: &mut Vec<Segment>,
) {
    for run in split_at_gaps(committed, FINAL_GAP_SPLIT_MS) {
        emit_final(run, events_tx, accepted).await;
    }
}

pub fn samples_to_f32(s: Vec<i16>) -> Vec<f32> {
    let mut out = Vec::with_capacity(s.len());
    extend_samples_f32(&mut out, &s);
    out
}

/// Append i16 PCM to an f32 buffer without an intermediate allocation.
pub fn extend_samples_f32(dst: &mut Vec<f32>, src: &[i16]) {
    dst.extend(src.iter().map(|&v| v as f32 / 32768.0));
}

/// True when the window's peak amplitude clears the speech gate (~-55 dBFS).
/// Normalized f32 samples in [-1, 1]. Empty window = no speech.
pub fn window_has_speech<'a>(samples: impl IntoIterator<Item = &'a f32>) -> bool {
    samples.into_iter().any(|&s| s.abs() >= SILENCE_PEAK_F32)
}

/// Lift a quiet window toward a target RMS. Up-only, gain-capped, and
/// peak-clamped. Returns the input unchanged when it is already at/above
/// target or is effectively silent.
pub fn normalize_for_asr(samples: Vec<f32>) -> Vec<f32> {
    /// ~-20 dBFS.
    const TARGET_RMS: f32 = 0.10;
    /// A window below this RMS is not amplified.
    const MIN_RMS: f32 = 1.0e-4;
    const MAX_GAIN: f32 = 12.0;

    if samples.is_empty() {
        return samples;
    }
    let rms =
        (samples.iter().map(|&s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    if rms < MIN_RMS {
        return samples;
    }
    let gain = (TARGET_RMS / rms).clamp(1.0, MAX_GAIN);
    if gain <= 1.0 {
        return samples; // already at/above target
    }
    samples.iter().map(|&s| (s * gain).clamp(-1.0, 1.0)).collect()
}

pub fn to_segment(t: &StreamToken) -> Segment {
    Segment {
        start_ms: t.start_ms.max(0) as u32,
        end_ms: t.end_ms.max(0) as u32,
        // Masks profanity in the text.
        text: transcript::text::mask_profanity(&t.text),
        confidence: None,
        speaker_id: None,
        words: Vec::new(), split_group: None,
    }
}

/// Split a committed token run at word gaps ≥ `gap_ms`.
pub fn split_at_gaps(toks: &[StreamToken], gap_ms: i64) -> Vec<&[StreamToken]> {
    let mut runs = Vec::new();
    let mut start = 0;
    for i in 1..toks.len() {
        if toks[i].start_ms - toks[i - 1].end_ms >= gap_ms {
            runs.push(&toks[start..i]);
            start = i;
        }
    }
    if start < toks.len() {
        runs.push(&toks[start..]);
    }
    runs
}

/// Collapse tokens into a single span+text (for the interim display line).
pub fn join_tokens(toks: &[StreamToken]) -> Option<StreamToken> {
    if toks.is_empty() {
        return None;
    }
    let text = toks.iter().map(|t| t.text.trim()).collect::<Vec<_>>().join(" ");
    Some(StreamToken { text, start_ms: toks.first()?.start_ms, end_ms: toks.last()?.end_ms })
}

/// Sustained-overload reporter shared by the streaming backends: the behind
/// condition must hold for the sustain window before anything fires, and a
/// `FallingBehind` emit repeats at most once per throttle interval.
pub struct FallingBehindGate {
    behind_since: Option<Instant>,
    last_emit: Option<Instant>,
}

impl Default for FallingBehindGate {
    fn default() -> Self {
        Self::new()
    }
}

impl FallingBehindGate {
    const SUSTAIN_S: u64 = 60;
    const THROTTLE_S: u64 = 30;

    pub fn new() -> Self {
        Self { behind_since: None, last_emit: None }
    }

    /// Feed this turn's behind/ok observation; true = the condition has held
    /// for the full sustain window. An ok turn restarts the window.
    pub fn sustained(&mut self, behind: bool) -> bool {
        if !behind {
            self.behind_since = None;
            return false;
        }
        self.behind_since.get_or_insert_with(Instant::now).elapsed().as_secs()
            >= Self::SUSTAIN_S
    }

    /// Throttled emit decision; call only after `sustained` returned true.
    pub fn should_emit(&mut self) -> bool {
        let due = self.last_emit.map(|t| t.elapsed().as_secs() >= Self::THROTTLE_S).unwrap_or(true);
        if due {
            self.last_emit = Some(Instant::now());
        }
        due
    }

    /// Restart the sustain window (the backend absorbed the overload itself).
    pub fn reset(&mut self) {
        self.behind_since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_run_splits_at_a_real_word_gap() {
        // Two words far apart in time become two Finals.
        let toks = vec![
            StreamToken { text: "Okay.".into(), start_ms: 189_100, end_ms: 190_000 },
            StreamToken { text: "Okay.".into(), start_ms: 218_200, end_ms: 219_100 },
        ];
        let runs = split_at_gaps(&toks, FINAL_GAP_SPLIT_MS);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].len(), 1);
        assert_eq!(runs[1][0].start_ms, 218_200);
    }

    #[test]
    fn contiguous_words_stay_one_run() {
        let toks = vec![
            StreamToken { text: "hello".into(), start_ms: 0, end_ms: 400 },
            StreamToken { text: "there".into(), start_ms: 400, end_ms: 800 },
            StreamToken { text: "world".into(), start_ms: 1_900, end_ms: 2_300 },
        ];
        let runs = split_at_gaps(&toks, FINAL_GAP_SPLIT_MS);
        assert_eq!(runs.len(), 1, "sub-threshold gaps must not split");
        assert!(split_at_gaps(&[], FINAL_GAP_SPLIT_MS).is_empty());
    }

    #[test]
    fn speech_gate_skips_silence_and_quiet_noise() {
        // Pure silence -> no speech.
        assert!(!window_has_speech(&[0.0; 1000]));
        // Below ~-55 dBFS (peak i16 ~58 -> ~0.00177) -> still no speech.
        let quiet = 40.0 / 32768.0;
        assert!(!window_has_speech(&[quiet, -quiet, quiet]));
        // Empty window -> no speech.
        assert!(!window_has_speech(&[]));
    }

    #[test]
    fn speech_gate_passes_real_signal() {
        // Above the gate (peak i16 ~200 -> ~0.006) -> speech.
        let loud = 200.0 / 32768.0;
        assert!(window_has_speech(&[0.0, loud, 0.0]));
        // Full-scale -> speech.
        assert!(window_has_speech(&[0.0, 1.0, -1.0]));
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|&x| x * x).sum::<f32>() / s.len() as f32).sqrt()
    }

    #[test]
    fn normalize_lifts_quiet_toward_target() {
        // ~-34 dBFS sine — well below the -20 dBFS target.
        let quiet: Vec<f32> = (0..1600)
            .map(|i| 0.02 * (i as f32 * 0.2).sin())
            .collect();
        let out = normalize_for_asr(quiet.clone());
        assert!(rms(&out) > rms(&quiet) * 3.0, "quiet audio should be boosted");
        assert!(out.iter().all(|&s| s.abs() <= 1.0), "no clipping after boost");
    }

    #[test]
    fn normalize_leaves_loud_and_silence_alone() {
        // Already-loud (~-6 dBFS): unchanged (up-only).
        let loud: Vec<f32> = (0..1600).map(|i| 0.5 * (i as f32 * 0.2).sin()).collect();
        assert_eq!(normalize_for_asr(loud.clone()), loud);
        // Near-silence: not amplified.
        let silence = vec![1.0e-5_f32; 1600];
        assert_eq!(normalize_for_asr(silence.clone()), silence);
    }

    fn w(text: &str, s: i64, e: i64) -> StreamToken {
        StreamToken { text: text.into(), start_ms: s, end_ms: e }
    }

    #[test]
    fn utterance_segment_keeps_masked_words_with_their_own_spans() {
        let seg = utterance_segment(&[w("well", 0, 300), w(" ", 300, 300), w("shit", 400, 700)]).unwrap();
        assert_eq!(seg.text, "well s***");
        assert_eq!((seg.start_ms, seg.end_ms), (0, 700));
        let words: Vec<(&str, u32, u32)> =
            seg.words.iter().map(|x| (x.text.as_str(), x.start_ms, x.end_ms)).collect();
        assert_eq!(words, [("well", 0, 300), ("s***", 400, 700)]);
        assert!(seg.words_consistent());
    }

    #[test]
    fn a_phrase_token_stays_one_word() {
        let seg = utterance_segment(&[w("North Wind", 1_000, 1_600), w("and", 1_600, 1_800)]).unwrap();
        assert_eq!(seg.text, "North Wind and");
        assert_eq!(seg.words.len(), 2);
        assert_eq!((seg.words[0].start_ms, seg.words[0].end_ms), (1_000, 1_600));
        assert!(seg.words_consistent());
    }

    #[test]
    fn finals_from_split_runs_carry_their_own_words() {
        let segs = finals_from(&[w("okay", 0, 400), w("okay", 30_000, 30_400)]);
        assert_eq!(segs.len(), 2);
        assert!(segs.iter().all(|s| s.words.len() == 1 && s.words_consistent()));
    }

    #[test]
    fn interim_masks_without_words() {
        match interim_event(&[w("well", 0, 300), w("shit", 400, 700)]) {
            Some(RealtimeEvent::Interim { segment }) => assert_eq!(segment.text, "well s***"),
            other => panic!("expected an interim, got {other:?}"),
        }
        assert!(interim_event(&[]).is_none());
    }

    #[test]
    fn a_two_second_gap_closes_the_utterance() {
        let mut u = Utterances::default();
        assert!(u.push(w("one", 0, 400)).is_none());
        assert!(u.push(w("two", 1_900, 2_300)).is_none(), "1.5 s gap stays open");
        let closed = u.push(w("three", 4_300, 4_700)).expect("2 s gap closes");
        assert_eq!(closed.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["one", "two"]);
        assert_eq!(u.open().len(), 1);
    }

    #[test]
    fn a_long_run_closes_at_the_next_breath_gap() {
        let mut u = Utterances::default();
        let mut t = 0i64;
        let mut closed = Vec::new();
        for i in 0..60 {
            if let Some(c) = u.push(w("w", t, t + 400)) {
                closed.push(c);
            }
            t += if i == 40 { 700 } else { 450 };
        }
        assert_eq!(closed.len(), 1, "one split at the first >=250 ms gap past 12 s");
        let first = &closed[0];
        assert!(first.last().unwrap().end_ms - first[0].start_ms >= TARGET_SEG_MS);
    }

    #[test]
    fn quiet_closes_after_two_seconds_past_the_last_word() {
        let mut u = Utterances::default();
        u.push(w("hi", 0, 400));
        assert!(u.close_if_quiet(2_399).is_none());
        assert_eq!(u.close_if_quiet(2_400).unwrap().len(), 1);
        assert!(u.close().is_none());
    }
}
