// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Local streaming-whisper backend implementing `RealtimeTranscriber`. One
//! serial decode in flight per track (the trait is invoked once per track by
//! the live pipeline): a sliding ~30 s window + LocalAgreement-2 commit. The
//! window always overlaps the previous one.

use std::path::PathBuf;
use std::sync::Arc;

use providers_realtime::{RealtimeError, RealtimeEvent, RealtimeTranscriber, Result};
use tokio::sync::{mpsc, Semaphore};
use transcript::Segment;

use super::agreement::{clamp_to_window, split_into_words, StreamToken};
use super::controller::CatchupController;
use super::emit::{
    emit_finals, join_tokens, mark_missed_speech, normalize_for_asr, samples_to_f32,
    to_segment, window_has_speech, FallingBehindGate,
};
use super::window::{StreamWindow, SAMPLE_RATE};
use crate::WhisperLocalTranscriber;

/// Minimum new audio (ms) to accumulate before kicking the next decode; caps
/// the decode rate at ~1 Hz.
const MIN_HOP_MS: i64 = 1000;
/// Minimum window length (ms) before the first decode.
const MIN_WINDOW_MS: i64 = 1200;

/// User-tunable whisper knobs (Settings → Providers → Advanced); rides the
/// per-machine `asr` settings block.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct WhisperTune {
    /// ISO 639-1 language hint (`"en"`, `"de"`); `None` = autodetect
    /// (English-only models ignore it).
    pub language: Option<String>,
    /// Live decode window cap (s).
    pub window_cap_s: u32,
    /// Live decode threads; 0 = auto (half the cores, capped at 6).
    pub threads: u32,
    /// Catch-up hop ladder override (ms), ascending. `None` = the default
    /// ladder. Settings-file only, no UI; malformed input is sanitized by
    /// the controller.
    pub hop_ladder_ms: Option<Vec<i64>>,
}

impl Default for WhisperTune {
    fn default() -> Self {
        Self { language: None, window_cap_s: 30, threads: 0, hop_ladder_ms: None }
    }
}

impl WhisperTune {
    /// Clamp to the supported ranges (UI edits re-clamp here on read).
    pub fn clamped(mut self) -> Self {
        self.window_cap_s = self.window_cap_s.clamp(15, 300);
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        self.threads = self.threads.min(cores as u32);
        self
    }

    /// The live decode thread budget: the explicit setting, else auto.
    pub fn live_threads(&self) -> i32 {
        if self.threads > 0 {
            return self.threads as i32;
        }
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        (cores / 2).clamp(1, 6) as i32
    }
}

pub struct LocalWhisperRealtime {
    model_path: PathBuf,
    n_threads: i32,
    label: String,
    /// Optional vocabulary hint (names/jargon) biasing live decoding.
    /// `None` = none.
    initial_prompt: Option<String>,
    /// One decode in flight across all tracks. The live pipeline shares one
    /// instance (Arc) across the mic + system tasks; this single-permit
    /// semaphore serializes their decodes, and each decode gets the full
    /// `n_threads` budget.
    decode_gate: Arc<Semaphore>,
    /// The loaded whisper context, shared by both track tasks and initialized
    /// exactly once. The pipeline calls `run()` per track on this shared Arc;
    /// `OnceCell` serializes the first-init and the second track reuses the
    /// same context. The ggml GPU backend must not be initialized twice
    /// concurrently.
    whisper: tokio::sync::OnceCell<Arc<WhisperLocalTranscriber>>,
    /// Cross-track catch-up controller, shared (via the shared instance Arc)
    /// by both track loops. Computes global serialized utilization
    /// `rho = Σ D_i/hop` and adapts the shared hop — see
    /// [`super::controller`].
    controller: Arc<CatchupController>,
    /// Faint-audio marker detector; `None` = markers off.
    marker: Option<Arc<super::vad::MarkerDetector>>,
    /// Live decode window cap (ms); the default matches
    /// [`super::window::MAX_WINDOW_MS`].
    window_cap_ms: i64,
    /// Language hint forwarded to the whisper context.
    language: Option<String>,
}

impl LocalWhisperRealtime {
    /// `n_threads` is the shared decode budget. Decodes are serialized across
    /// tracks (see `decode_gate`); one decode at a time uses this many
    /// threads.
    pub fn new(model_path: PathBuf, n_threads: i32, label: impl Into<String>) -> Self {
        Self {
            model_path,
            n_threads,
            label: label.into(),
            initial_prompt: None,
            decode_gate: Arc::new(Semaphore::new(1)),
            whisper: tokio::sync::OnceCell::new(),
            controller: Arc::new(CatchupController::new()),
            marker: None,
            window_cap_ms: super::window::MAX_WINDOW_MS,
            language: None,
        }
    }

    /// Override the live decode window cap (s).
    pub fn with_window_cap_s(mut self, cap_s: u32) -> Self {
        self.window_cap_ms = (cap_s as i64) * 1000;
        self
    }

    /// Language hint for decoding; `None` = autodetect.
    pub fn with_language(mut self, language: Option<String>) -> Self {
        self.language = language;
        self
    }

    /// Attach the faint-audio marker detector (Silero model path).
    pub fn with_marker(mut self, vad_model: Option<std::path::PathBuf>) -> Self {
        self.marker = vad_model.map(|p| Arc::new(super::vad::MarkerDetector::new(p)));
        self
    }

    /// Override the catch-up controller's hop ladder (ms), from the whisper
    /// `hop_ladder_ms` tunable. `None` keeps the default ladder. A malformed
    /// ladder is sanitized to the default inside the controller.
    pub fn with_hop_ladder(mut self, ladder: Option<Vec<i64>>) -> Self {
        if let Some(l) = ladder {
            self.controller = Arc::new(CatchupController::with_ladder(l));
        }
        self
    }

    /// Attach a Whisper `initial_prompt` vocabulary hint for live decoding.
    pub fn with_initial_prompt(mut self, prompt: Option<String>) -> Self {
        self.initial_prompt = prompt;
        self
    }
}

#[async_trait::async_trait]
impl RealtimeTranscriber for LocalWhisperRealtime {
    fn name(&self) -> &'static str {
        "whisper-local"
    }

    fn model(&self) -> &str {
        &self.label
    }

    async fn run(
        &self,
        _sample_rate: u32,
        mut audio_rx: mpsc::UnboundedReceiver<Vec<i16>>,
        events_tx: mpsc::Sender<RealtimeEvent>,
    ) -> Result<Vec<Segment>> {
        // Load the model + its ggml GPU backend once, shared across both track
        // tasks (mic + system call run() on this same Arc). OnceCell makes the
        // second caller wait for and reuse the first init. The blocking load
        // runs on a worker thread.
        let whisper = self
            .whisper
            .get_or_try_init(|| {
                let model_path = self.model_path.clone();
                let n_threads = self.n_threads;
                let initial_prompt = self.initial_prompt.clone();
                let language = self.language.clone();
                async move {
                    tokio::task::spawn_blocking(move || {
                        WhisperLocalTranscriber::with_threads(&model_path, n_threads)
                            .map(|w| {
                                Arc::new(
                                    w.with_initial_prompt(initial_prompt).with_language(language),
                                )
                            })
                    })
                    .await
                    .map_err(|e| RealtimeError::Backend(format!("whisper load join: {e}")))?
                    .map_err(|e| RealtimeError::Backend(e.to_string()))
                }
            })
            .await?
            .clone();

        let gate = Arc::clone(&self.decode_gate);
        let mut window = StreamWindow::new().with_cap_ms(self.window_cap_ms);
        let mut accepted: Vec<Segment> = Vec::new();
        let mut new_ms_since_decode: i64 = 0;

        // Register this track loop with the cross-track controller and start a
        // monotonic clock for its hop/utilization decisions. Both track loops
        // share `self.controller`; the hop adapts to the global serialized
        // load, not per-track.
        let slot = self.controller.register();
        let clock = std::time::Instant::now();
        // Sustained can't-keep-up → FallingBehind (throttled); the pipeline
        // supervisor owns the demotion decision.
        let mut falling = FallingBehindGate::new();

        loop {
            match audio_rx.recv().await {
                Some(frame) => {
                    let pcm = samples_to_f32(frame);
                    let hop_ms = self.controller.hop_ms();
                    let ing = ingest_frame(
                        &mut window,
                        &mut new_ms_since_decode,
                        &pcm,
                        hop_ms,
                        || audio_rx.try_recv().ok().map(samples_to_f32),
                    );
                    // Backlog = audio that was queued behind real-time,
                    // collapsed into the window above. Reported to telemetry.
                    crate::streaming::live_metrics::record_backlog(ing.drained_ms);

                    if let Some(win_ms) = ing.decode_win_ms {
                        let (_, wait_ms, decode_ms) = decode_and_emit(
                            &whisper,
                            &gate,
                            self.marker.as_ref(),
                            &mut window,
                            &events_tx,
                            &mut accepted,
                        )
                        .await;
                        crate::streaming::live_metrics::record_decode(decode_ms, win_ms);
                        crate::streaming::live_metrics::record_wait(wait_ms);
                        // The cross-track controller gets the true decode
                        // service time (no semaphore wait): it updates this
                        // slot's EWMA, recomputes global rho, fast-sheds on a
                        // deadline miss, and adapts the shared hop.
                        let now_ms = clock.elapsed().as_millis() as u64;
                        let hop = self.controller.report_decode(slot, decode_ms, now_ms);
                        crate::streaming::live_metrics::record_controller(
                            hop,
                            self.controller.rho(now_ms),
                            self.controller.cannot_keep_up(now_ms),
                        );
                        if falling.sustained(self.controller.cannot_keep_up(now_ms))
                            && falling.should_emit()
                        {
                            let _ = events_tx.send(RealtimeEvent::FallingBehind).await;
                        }
                    }
                }
                None => {
                    // Input closed (recording stopped): one last decode, then
                    // the remaining interim tail is committed as final.
                    let (interim, _, _) = decode_and_emit(
                        &whisper,
                        &gate,
                        self.marker.as_ref(),
                        &mut window,
                        &events_tx,
                        &mut accepted,
                    )
                    .await;
                    if let Some(seg) = join_tokens(&interim).map(|t| to_segment(&t)) {
                        let _ = events_tx.send(RealtimeEvent::Final { segment: seg.clone() }).await;
                        accepted.push(seg);
                    }
                    break;
                }
            }
        }
        Ok(accepted)
    }
}

fn pcm_ms(pcm: &[f32]) -> i64 {
    (pcm.len() as i64) * 1000 / SAMPLE_RATE as i64
}

/// Outcome of ingesting one received frame plus any queued backlog.
struct Ingest {
    /// `Some(window_ms)` if a decode should fire this turn (min-window + hop
    /// gates met), else `None`.
    decode_win_ms: Option<u64>,
    /// Backlog audio (ms) collapsed into the window beyond the first frame.
    drained_ms: u64,
}

/// Push the received `frame` plus any queued backlog into `window`, then
/// decide whether a decode fires this turn. The whole backlog drains into
/// the window in one shot and the next decode covers it as one batch —
/// lossless: the window force-emits a batch's tokens as Final before
/// trimming the decoded audio.
fn ingest_frame(
    window: &mut StreamWindow,
    new_ms_since_decode: &mut i64,
    frame: &[f32],
    hop_ms: i64,
    mut next_backlog: impl FnMut() -> Option<Vec<f32>>,
) -> Ingest {
    *new_ms_since_decode += pcm_ms(frame);
    window.push_audio(frame);

    let mut drained_ms: i64 = 0;
    let mut drained = 0usize;
    while let Some(more) = next_backlog() {
        *new_ms_since_decode += pcm_ms(&more);
        drained_ms += pcm_ms(&more);
        window.push_audio(&more);
        drained += 1;
        if drained > 4096 {
            break; // safety bound
        }
    }

    // `hop_ms` is the controller's current shared hop (≥ MIN_HOP_MS floor).
    let hop = hop_ms.max(MIN_HOP_MS);
    let decode_win_ms =
        if window.window_len_ms() >= MIN_WINDOW_MS && *new_ms_since_decode >= hop {
            *new_ms_since_decode = 0;
            Some(window.window_len_ms().max(0) as u64)
        } else {
            None
        };
    Ingest { decode_win_ms, drained_ms: drained_ms.max(0) as u64 }
}

/// Live-Whisper Full-trace flag (`DAISY_LIVE_TRACE`, set at app startup when
/// the debug level is Full). Read once.
fn live_trace() -> bool {
    use std::sync::OnceLock;
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| std::env::var("DAISY_LIVE_TRACE").is_ok())
}

/// One decode of the current window; emits committed tokens as Final and the
/// uncommitted tail as a single Interim. Returns `(interim_tail, wait_ms,
/// decode_ms)`: `wait_ms` is time spent waiting for the shared decode permit,
/// `decode_ms` is the actual whisper decode work.
async fn decode_and_emit(
    whisper: &Arc<WhisperLocalTranscriber>,
    gate: &Arc<Semaphore>,
    marker: Option<&Arc<super::vad::MarkerDetector>>,
    window: &mut StreamWindow,
    events_tx: &mpsc::Sender<RealtimeEvent>,
    accepted: &mut Vec<Segment>,
) -> (Vec<StreamToken>, u64, u64) {
    let samples = window.window_samples().to_vec();
    if samples.is_empty() {
        return (Vec::new(), 0, 0);
    }
    // Speech gate (peak-amplitude): the decode is skipped when the window is
    // below the speech floor.
    let has_speech = window_has_speech(&samples);
    if log::log_enabled!(log::Level::Debug) {
        let peak = samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        log::debug!(
            "live gate: {} — window_peak={peak:.4} ({:.1} dBFS) win_samples={}",
            if has_speech { "DECODE" } else { "SKIP (no speech)" },
            20.0 * (peak.max(1e-9)).log10(),
            samples.len(),
        );
    }
    if !has_speech {
        return (Vec::new(), 0, 0);
    }
    // One decode in flight across all tracks (the mic + system tasks share
    // this semaphore). The wait is timed separately from decode work.
    let t_wait = std::time::Instant::now();
    let permit = match gate.acquire().await {
        Ok(p) => p,
        Err(_) => return (Vec::new(), 0, 0), // semaphore closed: shutting down
    };
    let wait_ms = t_wait.elapsed().as_millis() as u64;
    // Level-normalize what whisper sees: quiet windows are lifted toward a
    // target RMS. Up-only + capped + peak-clamped. Live only; the recorded
    // WAV and the finalize pass are untouched.
    let samples = normalize_for_asr(samples);
    let win_ms = (samples.len() as i64) * 1000 / 16_000;
    // Capture level + length for the Full trace before `samples` moves into
    // the decode closure below.
    let trace = live_trace().then(|| {
        let peak = samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        (peak, samples.len())
    });
    let w = Arc::clone(whisper);
    let t_decode = std::time::Instant::now();
    let toks = match tokio::task::spawn_blocking(move || w.transcribe_samples(&samples)).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => {
            let _ = events_tx.send(RealtimeEvent::Error(format!("whisper: {e}"))).await;
            return (Vec::new(), wait_ms, 0);
        }
        Err(e) => {
            let _ = events_tx.send(RealtimeEvent::Error(format!("whisper join: {e}"))).await;
            return (Vec::new(), wait_ms, 0);
        }
    };
    let decode_ms = t_decode.elapsed().as_millis() as u64;
    // The permit only covers the whisper decode; the marker VAD below must
    // not extend the cross-track serialization.
    drop(permit);
    // The energy gate passed above, so a zero-token decode is marker
    // territory (the un-normalized window is what the VAD scans).
    if toks.is_empty() {
        mark_missed_speech(
            marker,
            window.window_samples(),
            window.origin_ms(),
            win_ms.max(0) as u64,
            events_tx,
        )
        .await;
    }
    // Full-trace telemetry (Settings → Recordings = Full): the exact text
    // Whisper produced from this window + the level it saw + timing.
    if let Some((peak, nlen)) = trace {
        let text: String = toks.iter().map(|t| t.text.as_str()).collect();
        log::info!(
            target: "live_asr",
            "decode win={:.1}s norm_peak={:.1}dBFS wait={wait_ms}ms decode={decode_ms}ms text={text:?}",
            nlen as f32 / 16_000.0,
            20.0 * (peak.max(1e-9)).log10(),
        );
    }
    // LocalAgreement runs at word granularity: whisper's segment tokens are
    // split into per-word tokens first (agreement::split_into_words). Spans
    // are clamped to the window first — the commit-trim cuts audio at the
    // committed word's end, so a span stamped into whisper's silence padding
    // must not reach past the real audio.
    let words = split_into_words(clamp_to_window(toks, win_ms));
    let result = window.ingest_decode(words);
    // The words newly committed by this decode are joined into one Final per
    // utterance: a decode of sparse speech can commit words separated by long
    // silence, and a single Final spanning the gap would lump separate turns
    // into one segment that overlaps the other track's timeline.
    emit_finals(&result.committed, events_tx, accepted).await;
    if let Some(tail) = join_tokens(&result.interim) {
        let _ = events_tx.send(RealtimeEvent::Interim { segment: to_segment(&tail) }).await;
    }
    (result.interim, wait_ms, decode_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_second() -> Vec<f32> {
        vec![0.1f32; SAMPLE_RATE]
    }

    #[test]
    fn whisper_tune_clamps_out_of_range_values() {
        let t = WhisperTune { window_cap_s: 5, threads: 10_000, ..Default::default() }.clamped();
        assert_eq!(t.window_cap_s, 15);
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        assert!(t.threads as usize <= cores);
        let t = WhisperTune { window_cap_s: 9_999, ..Default::default() }.clamped();
        assert_eq!(t.window_cap_s, 300);
        assert_eq!(t.threads, 0, "0 = auto passes through");
        assert!(t.live_threads() >= 1);
    }

    #[test]
    fn whisper_tune_defaults_pass_through_clamp() {
        assert_eq!(WhisperTune::default().clamped(), WhisperTune::default());
    }

    #[test]
    fn batch_drain_pulls_whole_backlog_into_one_decode() {
        // A 60s backlog queued behind the received frame is drained into the
        // buffer window as one batch and decoded as a single ~60s window.
        use std::collections::VecDeque;
        let mut w = StreamWindow::new();
        let mut nm = 0i64;
        let mut backlog: VecDeque<Vec<f32>> = (0..59).map(|_| one_second()).collect();
        let ing = ingest_frame(&mut w, &mut nm, &one_second(), MIN_HOP_MS, || backlog.pop_front());
        assert_eq!(w.window_len_ms(), 60_000, "whole backlog drained into the window");
        assert_eq!(ing.decode_win_ms, Some(60_000), "ONE batch decode over the 60s window");
        assert_eq!(ing.drained_ms, 59_000, "59s drained beyond the first 1s frame");
        assert!(backlog.is_empty(), "backlog fully drained");
    }

    #[test]
    fn hop_gate_holds_decode_until_min_hop() {
        // Without backlog, a sub-hop frame does not trigger a decode.
        let mut w = StreamWindow::new();
        let mut nm = 0i64;
        let half_hop = vec![0.1f32; SAMPLE_RATE / 2]; // 500ms < MIN_HOP_MS
        let ing = ingest_frame(&mut w, &mut nm, &half_hop, MIN_HOP_MS, || None);
        assert_eq!(ing.decode_win_ms, None, "500ms < 1s hop → no decode yet");
        assert_eq!(ing.drained_ms, 0);
    }

}
