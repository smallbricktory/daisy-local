//! Engine-agnostic helpers shared by the streaming backends: sample
//! conversion, the peak-energy speech gate, level normalization, the
//! committed-token → `Segment` emit path, the sustained-overload reporter,
//! and the faint-audio marker emit.

use std::sync::Arc;
use std::time::Instant;

use providers_realtime::RealtimeEvent;
use tokio::sync::mpsc;
use transcript::Segment;

use super::agreement::StreamToken;
use super::vad::MarkerDetector;

/// Speech gate: a decode whose window peak is below this (~-55 dBFS,
/// i16 peak ≈ 58) is skipped.
pub const SILENCE_PEAK_F32: f32 = 58.0 / 32768.0;

/// Committed-run split threshold: consecutive words further apart than this
/// are separate utterances and emit as separate Finals. Word spans inside one
/// decoded segment are contiguous interpolations; a gap this large only
/// appears between segments, where the stamps are real.
pub const FINAL_GAP_SPLIT_MS: i64 = 2_000;

/// Committed words → one `Segment` per utterance run (split at real word
/// gaps — one commit batch can span separate turns; a single Final bridging
/// the gap would lump turns and overlap the other track's timeline).
pub fn finals_from(words: &[StreamToken]) -> Vec<Segment> {
    split_at_gaps(words, FINAL_GAP_SPLIT_MS)
        .into_iter()
        .filter_map(|run| join_tokens(run).map(|t| to_segment(&t)))
        .collect()
}

/// Emit each utterance run of `committed` as a Final and record it in
/// `accepted` — the shared tail of every engine's decode step.
pub async fn emit_finals(
    committed: &[StreamToken],
    events_tx: &mpsc::Sender<RealtimeEvent>,
    accepted: &mut Vec<Segment>,
) {
    for seg in finals_from(committed) {
        let _ = events_tx.send(RealtimeEvent::Final { segment: seg.clone() }).await;
        accepted.push(seg);
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

/// Lift a quiet window toward a target RMS. Up-only (never attenuates loud
/// audio), gain-capped, and peak-clamped. Returns the input unchanged when it
/// is already at/above target or is effectively silent.
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
        // Profanity is masked at the single live-segment constructor,
        // covering the real-time view and the persisted
        // live_transcript.jsonl. Finalized transcripts are masked again at
        // dedup.
        text: transcript::text::mask_profanity(&t.text),
        confidence: None,
        speaker_id: None,
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
/// `FallingBehind` emit repeats at most once per throttle interval. The
/// pipeline supervisor owns the demotion decision.
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

/// Faint-audio marker: an energy-gate-positive window that decoded to zero
/// words is annotated as possible missed speech when the VAD confirms a
/// speech-like tail. Never gates decoding; no detector = fails quiet. The VAD
/// inference is blocking ONNX work, so it runs off the async thread on a copy
/// of the scanned tail.
pub async fn mark_missed_speech(
    marker: Option<&Arc<MarkerDetector>>,
    samples: &[f32],
    origin_ms: i64,
    win_ms: u64,
    events_tx: &mpsc::Sender<RealtimeEvent>,
) {
    let Some(det) = marker else { return };
    let det = Arc::clone(det);
    let tail = samples[samples.len().saturating_sub(super::vad::SCAN_SAMPLES)..].to_vec();
    let confirmed = tokio::task::spawn_blocking(move || det.confirms(&tail)).await;
    if matches!(confirmed, Ok(Some(true))) {
        let start = origin_ms.max(0) as u32;
        let _ = events_tx
            .send(RealtimeEvent::PossibleMissedSpeech {
                start_ms: start,
                end_ms: start + win_ms as u32,
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_run_splits_at_a_real_word_gap() {
        // Two "Okay"s ~25s apart committed by one decode must not merge into
        // a single 30s-span Final that overlaps the other track's turns in
        // the rendered timeline.
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
}
