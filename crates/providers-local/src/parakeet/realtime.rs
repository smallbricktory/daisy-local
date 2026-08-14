//! Parakeet streaming backend implementing `RealtimeTranscriber`: a sliding
//! ring window decoded on a fixed hop, with age-margin commit. One decode in
//! flight across all tracks (the shared `Mutex` is the decode gate); both
//! track tasks share one loaded model via the instance `Arc`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use providers_realtime::{RealtimeError, RealtimeEvent, RealtimeTranscriber, Result};
use tokio::sync::{mpsc, Mutex, OnceCell};
use transcript::Segment;

use super::committer::AgeMarginCommitter;
use super::{ParakeetASR, ParakeetOpts};
use crate::streaming::agreement::StreamToken;
use crate::streaming::emit::{
    emit_finals, extend_samples_f32, join_tokens, mark_missed_speech, samples_to_f32,
    to_segment, window_has_speech, FallingBehindGate,
};
use crate::streaming::live_metrics;
use crate::streaming::window::SAMPLE_RATE;

/// Minimum window length (ms) before the first decode.
const MIN_WINDOW_MS: i64 = 1_200;
/// Backlog watchdog: drained backlog above `2 × hop`, sustained for the
/// [`FallingBehindGate`] window, widens the effective hop — never past this
/// factor. Audio is never dropped.
const WATCHDOG_MAX_FACTOR: u32 = 4;

/// Streaming tunables (per-machine, Settings → Advanced).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ParakeetTune {
    pub window_s: u32,
    pub hop_s: u32,
    pub margin_s: u32,
    pub encoder_threads: usize,
    pub spin: bool,
}

impl Default for ParakeetTune {
    fn default() -> Self {
        // Windows int8 encoder is ~4× slower at short windows; the short
        // window + tighter margin keep duty viable there.
        if cfg!(windows) {
            Self { window_s: 15, hop_s: 5, margin_s: 8, encoder_threads: 4, spin: false }
        } else {
            Self { window_s: 30, hop_s: 5, margin_s: 10, encoder_threads: 4, spin: false }
        }
    }
}

impl From<ParakeetTune> for ParakeetOpts {
    fn from(t: ParakeetTune) -> Self {
        Self { encoder_threads: t.encoder_threads, spin: t.spin }
    }
}

impl ParakeetTune {
    /// Clamp to the supported ranges (UI edits re-clamp here on read).
    pub fn clamped(mut self) -> Self {
        self.window_s = self.window_s.clamp(10, 60);
        self.hop_s = self.hop_s.clamp(2, 15);
        // margin must leave slide room inside the window: the ingest loop's
        // free==0 arm trims `window - margin` samples, so margin >= window
        // means zero (or underflowed) trim and a stuck ring.
        self.margin_s = self.margin_s.clamp(5, 30).min(self.window_s - 1);
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        self.encoder_threads = self.encoder_threads.clamp(1, cores);
        self
    }
}

pub struct ParakeetRealtime {
    model_dir: PathBuf,
    tune: ParakeetTune,
    label: String,
    /// The loaded model, shared by both track tasks and initialized exactly
    /// once; the `Mutex` serializes decodes across tracks.
    asr: OnceCell<Arc<Mutex<ParakeetASR>>>,
    /// Faint-audio marker detector; `None` = markers off.
    marker: Option<Arc<crate::streaming::vad::MarkerDetector>>,
}

impl ParakeetRealtime {
    pub fn new(model_dir: PathBuf, tune: ParakeetTune, label: impl Into<String>) -> Self {
        Self {
            model_dir,
            tune: tune.clamped(),
            label: label.into(),
            asr: OnceCell::new(),
            marker: None,
        }
    }

    /// Attach the faint-audio marker detector (Silero model path).
    pub fn with_marker(mut self, vad_model: Option<PathBuf>) -> Self {
        self.marker = vad_model.map(|p| Arc::new(crate::streaming::vad::MarkerDetector::new(p)));
        self
    }
}

#[async_trait::async_trait]
impl RealtimeTranscriber for ParakeetRealtime {
    fn name(&self) -> &'static str {
        "parakeet-local"
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
        let asr = self
            .asr
            .get_or_try_init(|| {
                let dir = self.model_dir.clone();
                let opts: ParakeetOpts = self.tune.into();
                async move {
                    tokio::task::spawn_blocking(move || {
                        ParakeetASR::new(&dir, &opts).map(|a| Arc::new(Mutex::new(a)))
                    })
                    .await
                    .map_err(|e| RealtimeError::Backend(format!("parakeet load join: {e}")))?
                    .map_err(|e| RealtimeError::Backend(e.to_string()))
                }
            })
            .await?
            .clone();

        let window_samples = self.tune.window_s as usize * SAMPLE_RATE;
        let hop_ms = (self.tune.hop_s as i64) * 1000;
        let margin_ms = (self.tune.margin_s as i64) * 1000;

        let mut ring: VecDeque<f32> = VecDeque::with_capacity(window_samples);
        // Absolute ms of ring[0] / of the ingest head.
        let mut origin_ms: i64 = 0;
        let mut abs_ms: i64 = 0;
        let mut new_ms: i64 = 0;
        // Audio before this absolute time has been decoded with the full
        // margin of right context; the ring front may be trimmed up to here
        // without losing words.
        let mut decoded_horizon_ms: i64 = 0;
        let mut committer = AgeMarginCommitter::new();
        let mut accepted: Vec<Segment> = Vec::new();
        // Backlog watchdog state.
        let mut hop_factor: u32 = 1;
        let mut falling = FallingBehindGate::new();

        loop {
            match audio_rx.recv().await {
                Some(frame) => {
                    // Collect the received frame plus any queued backlog.
                    let mut pcm = samples_to_f32(frame);
                    let mut drained_ms: u64 = 0;
                    let mut drained = 0usize;
                    while let Ok(more) = audio_rx.try_recv() {
                        drained_ms += (more.len() * 1000 / SAMPLE_RATE) as u64;
                        extend_samples_f32(&mut pcm, &more);
                        drained += 1;
                        if drained > 4096 {
                            break;
                        }
                    }
                    live_metrics::record_backlog(drained_ms);

                    // Lossless ingest. The ring front may only be trimmed up
                    // to `decoded_horizon_ms`; a catch-up batch that needs
                    // more room forces a decode first (window marches over
                    // the backlog), so audio is never dropped undecoded.
                    let margin_samples = (self.tune.margin_s as usize) * SAMPLE_RATE;
                    let mut off = 0usize;
                    while off < pcm.len() {
                        let remaining = pcm.len() - off;
                        let free = window_samples - ring.len();
                        if free == 0 {
                            let need = remaining.min(window_samples - margin_samples);
                            let mut safe = safe_trim_samples(decoded_horizon_ms, origin_ms, ring.len());
                            if safe < need {
                                decode_turn(
                                    &asr,
                                    self.marker.as_ref(),
                                    &ring,
                                    origin_ms,
                                    abs_ms - margin_ms,
                                    &mut committer,
                                    &events_tx,
                                    &mut accepted,
                                )
                                .await;
                                decoded_horizon_ms = abs_ms - margin_ms;
                                new_ms = 0;
                                safe = safe_trim_samples(decoded_horizon_ms, origin_ms, ring.len());
                            }
                            let trim = need.min(safe.max(1));
                            ring.drain(..trim);
                            origin_ms += (trim * 1000 / SAMPLE_RATE) as i64;
                        } else {
                            let take = remaining.min(free);
                            let slice = &pcm[off..off + take];
                            let ms = (slice.len() * 1000 / SAMPLE_RATE) as i64;
                            ring.extend(slice.iter().copied());
                            abs_ms += ms;
                            new_ms += ms;
                            off += take;
                        }
                    }

                    // Watchdog: sustained backlog above 2× hop widens the
                    // effective hop (capped); it never drops audio. At the
                    // cap with backlog still growing, the supervisor is told.
                    let hop_eff = hop_ms * hop_factor as i64;
                    if falling.sustained(drained_ms as i64 > 2 * hop_eff) {
                        if hop_factor < WATCHDOG_MAX_FACTOR {
                            hop_factor *= 2;
                            falling.reset();
                            log::warn!(
                                "parakeet live: sustained backlog — widening hop to {}s",
                                (hop_ms * hop_factor as i64) / 1000
                            );
                        } else if falling.should_emit() {
                            let _ = events_tx.send(RealtimeEvent::FallingBehind).await;
                        }
                    }

                    if new_ms >= hop_eff && ring.len() as i64 * 1000 / SAMPLE_RATE as i64 >= MIN_WINDOW_MS {
                        new_ms = 0;
                        decode_turn(
                            &asr,
                            self.marker.as_ref(),
                            &ring,
                            origin_ms,
                            abs_ms - margin_ms,
                            &mut committer,
                            &events_tx,
                            &mut accepted,
                        )
                        .await;
                        decoded_horizon_ms = abs_ms - margin_ms;
                    }
                }
                None => {
                    // Input closed (recording stopped): one last decode, then
                    // commit everything pending.
                    decode_turn(&asr, self.marker.as_ref(), &ring, origin_ms, i64::MAX, &mut committer, &events_tx, &mut accepted)
                        .await;
                    break;
                }
            }
        }
        Ok(accepted)
    }
}

/// Ring-front samples that end at or before the decoded horizon — safe to
/// trim without losing undecoded words.
fn safe_trim_samples(decoded_horizon_ms: i64, origin_ms: i64, ring_len: usize) -> usize {
    let safe_ms = (decoded_horizon_ms - origin_ms).max(0) as usize;
    (safe_ms * SAMPLE_RATE / 1000).min(ring_len)
}

/// One decode of the ring: absolute-time words → age-margin commit → Final
/// runs + volatile Interim tail. Decode errors are reported and skipped; the
/// loop never aborts on them.
#[allow(clippy::too_many_arguments)]
async fn decode_turn(
    asr: &Arc<Mutex<ParakeetASR>>,
    marker: Option<&Arc<crate::streaming::vad::MarkerDetector>>,
    ring: &VecDeque<f32>,
    origin_ms: i64,
    horizon_ms: i64,
    committer: &mut AgeMarginCommitter,
    events_tx: &mpsc::Sender<RealtimeEvent>,
    accepted: &mut Vec<Segment>,
) {
    if ring.is_empty() || !window_has_speech(ring) {
        return;
    }
    let (front, back) = ring.as_slices();
    let mut samples = Vec::with_capacity(ring.len());
    samples.extend_from_slice(front);
    samples.extend_from_slice(back);
    let win_ms = (samples.len() * 1000 / SAMPLE_RATE) as u64;
    let t_wait = Instant::now();
    let guard = Arc::clone(asr).lock_owned().await;
    let wait_ms = t_wait.elapsed().as_millis() as u64;
    let t_decode = Instant::now();
    let decoded = tokio::task::spawn_blocking(move || {
        let mut g = guard;
        let words = g.decode(&samples);
        (words, samples)
    })
    .await;
    let decode_ms = t_decode.elapsed().as_millis() as u64;
    let (words, samples): (Vec<StreamToken>, Vec<f32>) = match decoded {
        Ok((Ok(w), s)) => (w, s),
        Ok((Err(e), _)) => {
            let _ = events_tx.send(RealtimeEvent::Error(format!("parakeet: {e}"))).await;
            return;
        }
        Err(e) => {
            let _ = events_tx.send(RealtimeEvent::Error(format!("parakeet join: {e}"))).await;
            return;
        }
    };
    live_metrics::record_wait(wait_ms);
    live_metrics::record_decode(decode_ms, win_ms);

    // The energy gate passed above, so a zero-word decode is marker territory.
    if words.is_empty() {
        mark_missed_speech(marker, &samples, origin_ms, win_ms, events_tx).await;
        return;
    }

    let words_abs: Vec<StreamToken> = words
        .into_iter()
        .map(|t| StreamToken {
            text: t.text,
            start_ms: origin_ms + t.start_ms,
            end_ms: origin_ms + t.end_ms,
        })
        .collect();
    let committed = committer.commit(&words_abs, horizon_ms);
    emit_finals(&committed, events_tx, accepted).await;
    let tail: Vec<StreamToken> =
        words_abs.into_iter().filter(|w| w.end_ms > horizon_ms).collect();
    if let Some(t) = join_tokens(&tail) {
        let _ = events_tx.send(RealtimeEvent::Interim { segment: to_segment(&t) }).await;
    }
}

#[cfg(test)]
mod tune_tests {
    use super::*;

    #[test]
    fn parakeet_tune_clamps_out_of_range_values() {
        let t = ParakeetTune { window_s: 1, hop_s: 100, margin_s: 0, encoder_threads: 10_000, spin: false }
            .clamped();
        assert_eq!(t.window_s, 10);
        assert_eq!(t.hop_s, 15);
        assert_eq!(t.margin_s, 5);
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        assert!(t.encoder_threads <= cores);
        assert!(t.encoder_threads >= 1);
    }

    #[test]
    fn parakeet_tune_defaults_pass_through_clamp() {
        assert_eq!(ParakeetTune::default().clamped(), ParakeetTune::default());
    }

    #[test]
    fn parakeet_tune_margin_stays_below_window() {
        // margin == window would make the ingest loop's slide amount zero
        // (free==0 with nothing trimmable → infinite spin); margin > window
        // would underflow window_samples - margin_samples.
        let t = ParakeetTune { window_s: 10, hop_s: 5, margin_s: 10, encoder_threads: 4, spin: false }
            .clamped();
        assert_eq!(t.margin_s, 9);
        let t = ParakeetTune { window_s: 10, hop_s: 5, margin_s: 30, encoder_threads: 4, spin: false }
            .clamped();
        assert_eq!(t.margin_s, 9);
        let t = ParakeetTune { window_s: 60, hop_s: 5, margin_s: 30, encoder_threads: 4, spin: false }
            .clamped();
        assert_eq!(t.margin_s, 30);
    }
}
