//! Local streaming-whisper backend implementing `RealtimeTranscriber`. One
//! serial decode in flight per track (the trait is invoked once per track by
//! the live pipeline): a sliding ~30 s window + LocalAgreement-2 commit. The
//! window always overlaps the previous one.

use std::path::PathBuf;
use std::sync::Arc;

use providers_realtime::{RealtimeError, RealtimeEvent, RealtimeTranscriber, Result};
use tokio::sync::{mpsc, Semaphore};
use transcript::Segment;

use super::agreement::{split_into_words, StreamToken};
use super::controller::CatchupController;
use super::window::{StreamWindow, SAMPLE_RATE};
use crate::WhisperLocalTranscriber;

/// Minimum new audio (ms) to accumulate before kicking the next decode; caps
/// the decode rate at ~1 Hz.
const MIN_HOP_MS: i64 = 1000;
/// Minimum window length (ms) before the first decode.
const MIN_WINDOW_MS: i64 = 1200;
/// Speech gate: a decode whose window peak is below this (~-55 dBFS,
/// i16 peak ≈ 58) is skipped.
const SILENCE_PEAK_F32: f32 = 58.0 / 32768.0;

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
        }
    }

    /// Override the catch-up controller's hop ladder (ms), e.g. from the
    /// `live_hop_ladder_ms` setting. `None` keeps the default ladder. A malformed
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
                async move {
                    tokio::task::spawn_blocking(move || {
                        WhisperLocalTranscriber::with_threads(&model_path, n_threads)
                            .map(|w| Arc::new(w.with_initial_prompt(initial_prompt)))
                    })
                    .await
                    .map_err(|e| RealtimeError::Backend(format!("whisper load join: {e}")))?
                    .map_err(|e| RealtimeError::Backend(e.to_string()))
                }
            })
            .await?
            .clone();

        let gate = Arc::clone(&self.decode_gate);
        let mut window = StreamWindow::new();
        let mut accepted: Vec<Segment> = Vec::new();
        let mut new_ms_since_decode: i64 = 0;
        // Variable buffer-window batch drain. Each decode drains a batch of
        // queued backlog into the 30s-capped buffer window:
        //   - variable (default): drains ALL queued audio into the window and
        //     decodes it as one batch.
        //   - fixed (DAISY_LIVE_CATCHUP=fixed): drains exactly one hop (~1s)
        //     per decode.
        // Both are lossless: the window force-emits a batch's tokens as Final
        // before trimming the decoded audio.
        let variable_batch = std::env::var("DAISY_LIVE_CATCHUP")
            .map(|v| !v.eq_ignore_ascii_case("fixed"))
            .unwrap_or(true);

        // Register this track loop with the cross-track controller and start a
        // monotonic clock for its hop/utilization decisions. Both track loops
        // share `self.controller`; the hop adapts to the global serialized
        // load, not per-track.
        let slot = self.controller.register();
        let clock = std::time::Instant::now();

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
                        variable_batch,
                        || audio_rx.try_recv().ok().map(samples_to_f32),
                    );
                    // Backlog = audio that was queued behind real-time,
                    // collapsed into the window above. Reported to telemetry.
                    crate::streaming::live_metrics::record_backlog(ing.drained_ms);

                    if let Some(win_ms) = ing.decode_win_ms {
                        let (_, wait_ms, decode_ms) = decode_and_emit(
                            &whisper,
                            &gate,
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
                    }
                }
                None => {
                    // Input closed (recording stopped): one last decode, then
                    // the remaining interim tail is committed as final.
                    let (interim, _, _) = decode_and_emit(
                        &whisper,
                        &gate,
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

fn samples_to_f32(s: Vec<i16>) -> Vec<f32> {
    s.iter().map(|&v| v as f32 / 32768.0).collect()
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

/// Push the received `frame` (and, in variable mode, any queued backlog) into
/// `window`, then decide whether a decode fires this turn.
///
/// `variable_batch` selects the batch size pulled into the window per decode:
///   - **variable** (`true`, default): drains ALL queued backlog into the
///     window in one shot; the next decode covers the whole batch. The window
///     force-emits the batch before trimming the decoded audio.
///   - **fixed** (`false`): drains exactly one received frame (~1 hop) per
///     turn. A queued backlog stays in the channel and is consumed in order,
///     one hop at a time.
fn ingest_frame(
    window: &mut StreamWindow,
    new_ms_since_decode: &mut i64,
    frame: &[f32],
    hop_ms: i64,
    variable_batch: bool,
    mut next_backlog: impl FnMut() -> Option<Vec<f32>>,
) -> Ingest {
    *new_ms_since_decode += pcm_ms(frame);
    window.push_audio(frame);

    let mut drained_ms: i64 = 0;
    if variable_batch {
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
    let _permit = match gate.acquire().await {
        Ok(p) => p,
        Err(_) => return (Vec::new(), 0, 0), // semaphore closed: shutting down
    };
    let wait_ms = t_wait.elapsed().as_millis() as u64;
    // Level-normalize what whisper sees: quiet windows are lifted toward a
    // target RMS. Up-only + capped + peak-clamped. Live only; the recorded
    // WAV and the finalize pass are untouched.
    let samples = normalize_for_asr(samples);
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
    // split into per-word tokens first (agreement::split_into_words).
    let words = split_into_words(toks);
    let result = window.ingest_decode(words);
    // The words newly committed by this decode are joined into a single
    // Final; event volume stays ~1/decode, not one-per-word.
    if let Some(seg) = join_tokens(&result.committed).map(|t| to_segment(&t)) {
        let _ = events_tx.send(RealtimeEvent::Final { segment: seg.clone() }).await;
        accepted.push(seg);
    }
    if let Some(tail) = join_tokens(&result.interim) {
        let _ = events_tx.send(RealtimeEvent::Interim { segment: to_segment(&tail) }).await;
    }
    (result.interim, wait_ms, decode_ms)
}

/// True when the window's peak amplitude clears the speech gate (~-55 dBFS).
/// Normalized f32 samples in [-1, 1]. Empty window = no speech.
fn window_has_speech(samples: &[f32]) -> bool {
    samples.iter().fold(0.0f32, |m, &s| m.max(s.abs())) >= SILENCE_PEAK_F32
}

/// Lift a quiet window toward a target RMS. Up-only (never attenuates loud
/// audio), gain-capped, and peak-clamped. Returns the input unchanged when it
/// is already at/above target or is effectively silent.
fn normalize_for_asr(samples: Vec<f32>) -> Vec<f32> {
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

fn to_segment(t: &StreamToken) -> Segment {
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

/// Collapse tokens into a single span+text (for the interim display line).
fn join_tokens(toks: &[StreamToken]) -> Option<StreamToken> {
    if toks.is_empty() {
        return None;
    }
    let text = toks.iter().map(|t| t.text.trim()).collect::<Vec<_>>().join(" ");
    Some(StreamToken { text, start_ms: toks.first()?.start_ms, end_ms: toks.last()?.end_ms })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_second() -> Vec<f32> {
        vec![0.1f32; SAMPLE_RATE]
    }

    #[test]
    fn variable_batch_drains_whole_backlog_in_one_decode() {
        // variable=true (default): a 60s backlog queued behind the received
        // frame is drained into the buffer window as one batch and decoded as
        // a single ~60s window.
        use std::collections::VecDeque;
        let mut w = StreamWindow::new();
        let mut nm = 0i64;
        let mut backlog: VecDeque<Vec<f32>> = (0..59).map(|_| one_second()).collect();
        let ing = ingest_frame(&mut w, &mut nm, &one_second(), MIN_HOP_MS, true, || backlog.pop_front());
        assert_eq!(w.window_len_ms(), 60_000, "whole backlog drained into the window");
        assert_eq!(ing.decode_win_ms, Some(60_000), "ONE batch decode over the 60s window");
        assert_eq!(ing.drained_ms, 59_000, "59s drained beyond the first 1s frame");
        assert!(backlog.is_empty(), "backlog fully drained");
    }

    #[test]
    fn fixed_batch_processes_one_hop_and_leaves_queue() {
        // variable=false: batch size fixed at one frame — the same 60s backlog
        // is not drained; only the received frame enters the window, one hop
        // decode fires, and the queue is left intact to be consumed in order.
        use std::collections::VecDeque;
        let mut w = StreamWindow::new();
        let mut nm = 0i64;
        let received = vec![0.1f32; SAMPLE_RATE * 2]; // 2s ≥ MIN_WINDOW_MS
        let mut backlog: VecDeque<Vec<f32>> = (0..59).map(|_| one_second()).collect();
        let ing = ingest_frame(&mut w, &mut nm, &received, MIN_HOP_MS, false, || backlog.pop_front());
        assert_eq!(w.window_len_ms(), 2_000, "only the single received frame entered the window");
        assert_eq!(ing.decode_win_ms, Some(2_000), "one hop decode over the received frame only");
        assert_eq!(ing.drained_ms, 0, "no backlog drained");
        assert_eq!(backlog.len(), 59, "queue left intact for in-order processing");
    }

    #[test]
    fn hop_gate_holds_decode_until_min_hop() {
        // Without backlog, a sub-hop frame does not trigger a decode.
        let mut w = StreamWindow::new();
        let mut nm = 0i64;
        let half_hop = vec![0.1f32; SAMPLE_RATE / 2]; // 500ms < MIN_HOP_MS
        let ing = ingest_frame(&mut w, &mut nm, &half_hop, MIN_HOP_MS, false, || None);
        assert_eq!(ing.decode_win_ms, None, "500ms < 1s hop → no decode yet");
        assert_eq!(ing.drained_ms, 0);
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
