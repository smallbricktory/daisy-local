//! Live transcription pipeline orchestrator.
//!
//! Owns a dedicated tokio runtime + two parallel tasks (mic + system); the
//! tasks consume audio sample streams from the audio-engine tees and either:
//!
//! - **Realtime mode**: forward samples to a `RealtimeTranscriber` (local
//!   streaming whisper); emit `RealtimeEvent`s.
//! - **Off**: no-op; the pipeline owns nothing.
//!
//! Realtime mode appends `LiveTranscriptLine`s to
//! `<session>/live_transcript.jsonl` and emits `LivePipelineEvent`s to the
//! caller.

use crate::flight_recorder::FlightRecorder;
use crate::live_transcript::{LiveTrack, LiveTranscriptLine, LiveTranscriptWriter};
use audio_engine::autogain::AutoGain;
use audio_engine::capture::TeeFrame;
use providers_realtime::{RealtimeEvent, RealtimeTranscriber};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// What kind of live transcription to run for this session.
pub enum LiveMode {
    Realtime {
        client: Arc<dyn RealtimeTranscriber>,
    },
    Off,
}

/// Event emitted from one of the pipeline tasks; forwarded to the frontend
/// as `transcript:segment` events.
#[derive(Debug, Clone)]
pub struct LivePipelineEvent {
    pub track: LiveTrack,
    pub kind: LivePipelineEventKind,
}

#[derive(Debug, Clone)]
pub enum LivePipelineEventKind {
    /// Provider produced an interim hypothesis (only realtime mode emits these).
    Interim {
        start_ms: u32,
        end_ms: u32,
        text: String,
        confidence: Option<f32>,
    },
    /// Final committed segment.
    Final {
        start_ms: u32,
        end_ms: u32,
        text: String,
        confidence: Option<f32>,
    },
    /// Transient error from the provider; pipeline continues.
    Error(String),
    /// The mic track delivered no usable signal in the first few seconds of
    /// recording — the OS capture stream is dead (e.g. another app such as
    /// Teams/Zoom grabbed the input device). System audio is unaffected.
    /// Fired at most once per session. `elapsed_ms` is the watchdog window.
    MicSilent { elapsed_ms: u64 },
    /// Periodic mic input level (normalized peak, 0..1) for the in-call
    /// meter. Driven from the recording's own mic stream, after OS input
    /// gain; a muted input reads 0.
    MicLevel { peak: f32 },
}

/// Owns the live pipeline. Drop or call `shutdown()` to stop.
pub struct LivePipeline {
    runtime: Option<tokio::runtime::Runtime>,
}

/// Everything `LivePipeline::start` needs, in one place.
///
/// `mic_audio_rx` and `system_audio_rx` are the audio-engine tee receivers
/// (16-bit PCM at `sample_rate`); the caller sets the corresponding senders
/// on the audio engine via `set_mic_tee` / `set_system_tee`.
/// `transcript_writer` is shared (Arc<Mutex>); both tasks append to the same
/// file. `events_tx` is a single channel both tasks fan into; the caller
/// drains it and forwards to the frontend.
pub struct LiveStartConfig {
    pub mode: LiveMode,
    pub sample_rate: u32,
    pub mic_audio_rx: mpsc::UnboundedReceiver<TeeFrame>,
    pub system_audio_rx: mpsc::UnboundedReceiver<TeeFrame>,
    pub transcript_writer: Arc<Mutex<LiveTranscriptWriter>>,
    pub events_tx: mpsc::Sender<LivePipelineEvent>,
    pub mic_source_id: u32,
    /// Live AGC guard floor (f32 bits), shared so a mic switch re-seeds it.
    pub speech_env_min: Arc<std::sync::atomic::AtomicU32>,
    pub mic_switch_rx: mpsc::UnboundedReceiver<u32>,
    pub needs_aec: Arc<AtomicBool>,
    pub aec_model_dir: PathBuf,
    pub paused: Arc<AtomicBool>,
    pub flight: Arc<FlightRecorder>,
}

impl LivePipeline {
    /// Spawn the live pipeline. Returns immediately.
    pub fn start(cfg: LiveStartConfig) -> std::io::Result<Self> {
        let LiveStartConfig {
            mode,
            sample_rate,
            mic_audio_rx,
            system_audio_rx,
            transcript_writer,
            events_tx,
            mic_source_id,
            speech_env_min,
            mic_switch_rx,
            needs_aec,
            aec_model_dir,
            paused,
            flight,
        } = cfg;
        let realtime = matches!(mode, LiveMode::Realtime { .. });
        log::info!(
            "live pipeline: start called, mode={}",
            if realtime { "Realtime" } else { "Off" }
        );

        // The runtime always comes up: even with captions off, the mic
        // auto-gain tap runs. 3 worker threads cover the tap, the AEC
        // bridge (CPU-bound DTLN frames), and the 2 realtime tasks.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(3)
            .enable_all()
            .thread_name("daisy-live-pipeline")
            .build()?;

        // Mic auto-gain tap sits in front of the mic transcriber: it observes
        // every mic frame, lowers the OS input gain on sustained clipping
        // (off the audio thread — applying gain shells wpctl / hits CoreAudio /
        // WASAPI), restores on stop, and forwards frames to the realtime mic
        // task when captions are on.
        let (mic_fwd_tx, mic_fwd_rx) = mpsc::unbounded_channel::<TeeFrame>();
        let fwd = if realtime { Some(mic_fwd_tx) } else { None };
        runtime.spawn(autogain_tap(
            mic_audio_rx,
            fwd,
            mic_source_id,
            mic_switch_rx,
            sample_rate,
            events_tx.clone(),
            paused,
        ));

        match mode {
            LiveMode::Realtime { client } => {
                // System tee splits: one copy feeds the system transcriber,
                // one is the AEC far-end reference for the mic path.
                let (sys_fwd_tx, sys_fwd_rx) = mpsc::unbounded_channel::<TeeFrame>();
                let (aec_ref_tx, aec_ref_rx) = mpsc::unbounded_channel::<TeeFrame>();
                runtime.spawn(system_split(system_audio_rx, sys_fwd_tx, aec_ref_tx));

                // Echo-cancel the live mic feed before it reaches the
                // transcriber, so the ASR never hears the speakers. The
                // recorded mic.wav upstream stays raw; finalize's batch AEC
                // is unchanged.
                let (mic_clean_tx, mic_clean_rx) = mpsc::unbounded_channel::<Vec<i16>>();
                let energy = Arc::new(EnergyLog::new());
                runtime.spawn(aec_bridge(
                    mic_fwd_rx,
                    aec_ref_rx,
                    mic_clean_tx,
                    needs_aec,
                    aec_model_dir,
                    Arc::clone(&energy),
                    speech_env_min,
                    Arc::clone(&flight),
                ));

                let recent_system: RecentSystemFinals =
                    Arc::new(Mutex::new(std::collections::VecDeque::new()));
                spawn_realtime_task(
                    runtime.handle(),
                    LiveTrack::Mic,
                    sample_rate,
                    Arc::clone(&client),
                    mic_clean_rx,
                    Arc::clone(&transcript_writer),
                    events_tx.clone(),
                    Some(energy),
                    Arc::clone(&recent_system),
                );
                let sys_energy = Arc::new(EnergyLog::new());
                let sys_samples_rx = system_gain_tap(
                    runtime.handle(),
                    sys_fwd_rx,
                    Arc::clone(&sys_energy),
                    flight,
                );
                spawn_realtime_task(
                    runtime.handle(),
                    LiveTrack::System,
                    sample_rate,
                    client,
                    sys_samples_rx,
                    transcript_writer,
                    events_tx,
                    Some(sys_energy),
                    recent_system,
                );
            }
            LiveMode::Off => {
                // mic frames are consumed by the auto-gain tap; system track unused.
                drop(system_audio_rx);
            }
        }

        Ok(Self {
            runtime: Some(runtime),
        })
    }

    /// Tear down the runtime, awaiting in-flight tasks. Idempotent.
    pub fn shutdown(mut self) {
        // Dropping the runtime blocks until all tasks complete. The tasks
        // themselves exit when their audio_rx channels close, which the
        // recorder ensures by dropping the audio senders before calling
        // shutdown().
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_timeout(std::time::Duration::from_secs(10));
        }
    }
}

impl Drop for LivePipeline {
    fn drop(&mut self) {
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_timeout(std::time::Duration::from_secs(5));
        }
    }
}

/// Live-captions upward AGC for the mic track. Lifts the transcriber-bound
/// frames toward a target peak — applied in the AEC bridge, after echo
/// cancellation (gain would distort the echo path the canceller models).
/// The recorded `mic.wav` is written upstream and is never touched.
///
/// Tracks a slow speech envelope and bounds the gain; the gain is a
/// near-constant multiplier, and true silence/noise stays proportionally
/// low. `AutoGain` (OS input gain, down-only anti-clipping) runs on the raw
/// frame before this.
struct LiveGain {
    env: f32,
    speech_env_min: f32,
}
impl LiveGain {
    /// Normalized envelope target (~-16 dBFS). Speech rides up to here;
    /// louder transients clamp.
    const TARGET: f32 = 0.15;
    const MAX_GAIN: f32 = 8.0;
    /// A frame below this is treated as non-speech and does not pull the
    /// envelope down.
    const FLOOR: f32 = 0.002;
    /// No lift unless the recent envelope shows real speech: post-AEC echo
    /// residue (peaks ~−37 dBFS, ~0.014) sits below this floor and is never
    /// amplified, while genuinely quiet mics (AirPods HFP) speak above it.
    /// Default guard floor; a per-device learned seed replaces it when the
    /// speech-level store has data for the active mic.
    const SPEECH_ENV_MIN: f32 = 0.02;

    fn new(speech_env_min: Option<f32>) -> Self {
        Self {
            env: Self::TARGET,
            speech_env_min: speech_env_min.unwrap_or(Self::SPEECH_ENV_MIN),
        }
    }

    /// Returns the gain applied to the frame (1.0 = passthrough).
    fn apply(&mut self, frame: &mut [i16]) -> f32 {
        let peak = frame.iter().fold(0i16, |m, &s| m.max(s.saturating_abs())) as f32 / 32768.0;
        if peak > Self::FLOOR {
            // Fast attack, slow release toward the observed speech peak.
            let a = if peak > self.env { 0.5 } else { 0.05 };
            self.env += a * (peak - self.env);
        }
        let gain = (Self::TARGET / self.env.max(Self::FLOOR)).clamp(1.0, Self::MAX_GAIN);
        // Lift only frames that are themselves speech-loud, and only while
        // the envelope shows real speech — residue-level audio is never
        // amplified, including during the envelope's decay after speech
        // ends. See SPEECH_ENV_MIN.
        if gain <= 1.0 || peak < self.speech_env_min || self.env < self.speech_env_min {
            return 1.0;
        }
        for s in frame.iter_mut() {
            *s = ((*s as f32) * gain).clamp(-32768.0, 32767.0) as i16;
        }
        gain
    }
}

/// Bit-encoding for the shared, switch-updatable AGC guard floor. None =
/// the built-in default.
pub(crate) fn speech_env_min_bits(seed: Option<f32>) -> u32 {
    seed.unwrap_or(LiveGain::SPEECH_ENV_MIN).to_bits()
}

/// Safe-start input-gain ceiling. A device that starts above this is lowered
/// pre-emptively; the reactive AutoGain then fine-tunes downward from there.
/// Mics already at or below the ceiling are left untouched.
const SAFE_START_CEILING: f32 = 0.8;

/// Apply the safe-start cap. Returns the gain `AutoGain` baselines from;
/// sets `*capped = true` when it actually lowered the device (stop/switch
/// then restores the user's `original` gain).
fn cap_start_gain(mic_id: u32, original: Option<f32>, capped: &mut bool) -> f32 {
    let start = original.unwrap_or(1.0);
    if start > SAFE_START_CEILING
        && audio_engine::gain::set_input_gain(mic_id, SAFE_START_CEILING)
    {
        log::info!(
            "mic auto-gain: input gain {start:.2} at start exceeds {SAFE_START_CEILING:.2} — capped to avoid opening clip"
        );
        *capped = true;
        return SAFE_START_CEILING;
    }
    start
}

async fn autogain_tap(
    mut rx: mpsc::UnboundedReceiver<TeeFrame>,
    fwd: Option<mpsc::UnboundedSender<TeeFrame>>,
    mic_source_id: u32,
    mut switch_rx: mpsc::UnboundedReceiver<u32>,
    sample_rate: u32,
    events_tx: mpsc::Sender<LivePipelineEvent>,
    paused: Arc<AtomicBool>,
) {
    let mut mic_id = mic_source_id;
    let mut original = audio_engine::gain::input_gain(mic_id);
    let mut stepped = false;
    // Pre-emptive safe-start cap (see cap_start_gain): a too-hot start is
    // lowered before the first frame.
    let mut ag = AutoGain::new(cap_start_gain(mic_id, original, &mut stepped));
    let start = Instant::now();

    // Dead-mic watchdog. After a grace window the mic peak is checked once;
    // a value below the noise floor of any live mic reports capture as dead.
    // Latches: one warning per session.
    const MIC_WATCHDOG_SECS: u64 = 5;
    // ≈ -72 dBFS, under the -55 dBFS silence gate used elsewhere; only an
    // all-zero stream trips it.
    const MIC_DEAD_PEAK_I16: i16 = 8;
    let mut mic_peak: i16 = 0;
    let mut watchdog_fired = false;
    // Throttle for the in-call meter level event (~12 Hz).
    let mut last_level_emit = Instant::now();
    let mut watchdog = tokio::time::interval(Duration::from_secs(MIC_WATCHDOG_SECS));
    watchdog.tick().await; // first tick is immediate — skip it
    let _ = sample_rate;

    loop {
        tokio::select! {
            _ = watchdog.tick(), if !watchdog_fired => {
                // Paused = tee detached = silence by design; re-check on a
                // later tick once recording resumes.
                if paused.load(Ordering::Relaxed) {
                    mic_peak = 0; // pre-pause peak is stale for the re-check
                    continue;
                }
                watchdog_fired = true;
                if mic_peak < MIC_DEAD_PEAK_I16 {
                    let elapsed_ms = start.elapsed().as_millis() as u64;
                    log::warn!(
                        "mic watchdog: no signal after {}s (peak={}) — capture appears \
                         dead; another app may hold the input device",
                        MIC_WATCHDOG_SECS, mic_peak
                    );
                    let _ = events_tx
                        .send(LivePipelineEvent {
                            track: crate::live_transcript::LiveTrack::Mic,
                            kind: LivePipelineEventKind::MicSilent { elapsed_ms },
                        })
                        .await;
                }
            }
            // Mid-call mic switch: restore the old device, re-baseline to the new.
            Some(new_id) = switch_rx.recv() => {
                if stepped {
                    if let Some(orig) = original {
                        let _ = audio_engine::gain::set_input_gain(mic_id, orig);
                    }
                }
                mic_id = new_id;
                original = audio_engine::gain::input_gain(mic_id);
                stepped = false;
                ag = AutoGain::new(cap_start_gain(mic_id, original, &mut stepped));
                log::info!("mic auto-gain: re-baselined to switched mic {mic_id}");
            }
            frame = rx.recv() => {
                let Some(frame) = frame else { break };
                if !watchdog_fired {
                    for &s in &frame.samples {
                        mic_peak = mic_peak.max(s.saturating_abs());
                    }
                }
                // In-call meter: emits the recording's own mic peak
                // (normalized), throttled. Reflects the OS input gain; a
                // muted input (gain→0) reads 0. Dropped when the channel is
                // full; the meter never backpressures capture.
                let now = Instant::now();
                if now.duration_since(last_level_emit) >= Duration::from_millis(80) {
                    last_level_emit = now;
                    let fpeak = frame.samples.iter().fold(0i16, |m, &s| m.max(s.saturating_abs())) as f32
                        / 32768.0;
                    let _ = events_tx.try_send(LivePipelineEvent {
                        track: crate::live_transcript::LiveTrack::Mic,
                        kind: LivePipelineEventKind::MicLevel { peak: fpeak },
                    });
                }
                let now_ms = start.elapsed().as_millis() as u64;
                // Anti-clipping observer runs on the RAW frame (OS input gain).
                if let Some(step) = ag.observe(&frame.samples, now_ms) {
                    if audio_engine::gain::set_input_gain(mic_id, step.new_gain) {
                        stepped = true;
                        log::info!("mic clipping — lowered input gain to {:.2}", step.new_gain);
                    }
                }
                if let Some(tx) = &fwd {
                    // Forwarded raw: the AEC bridge downstream echo-cancels and
                    // then applies the live-captions AGC. The wav is already
                    // written upstream, untouched.
                    // Realtime consumer may have exited; keep observing regardless.
                    let _ = tx.send(frame);
                }
            }
        }
    }
    if stepped {
        if let Some(orig) = original {
            if audio_engine::gain::set_input_gain(mic_id, orig) {
                log::info!("mic auto-gain: restored input gain to {:.2} on stop", orig);
            }
        }
    }
}

/// Fan the system tee out to the system transcriber and the AEC reference.
async fn system_split(
    mut rx: mpsc::UnboundedReceiver<TeeFrame>,
    fwd_tx: mpsc::UnboundedSender<TeeFrame>,
    ref_tx: mpsc::UnboundedSender<TeeFrame>,
) {
    while let Some(frame) = rx.recv().await {
        let _ = ref_tx.send(frame.clone());
        if fwd_tx.send(frame).is_err() {
            break; // system transcriber gone; reference alone is useless
        }
    }
}

/// Rolling record of post-AEC mic energy, bucketed at 100ms, written by the
/// AEC bridge and consulted when a mic segment arrives: a "speech" segment
/// whose whole span never rose above the silence floor is an ASR
/// hallucination (Whisper invents fillers like "Okay" on near-silence) and
/// is suppressed before display or the live-transcript store.
pub(crate) struct EnergyLog {
    /// (bucket start ms, max normalized peak in bucket), oldest first.
    buckets: Mutex<std::collections::VecDeque<(u64, f32)>>,
}

/// ≈ -38 dBFS. Post-AEC silence residue sits around -50 dBFS; real speech
/// peaks are well above this even for quiet talkers. Field-calibrated on a
/// ground-truth silent-mic meeting (typing only): every hallucinated
/// segment had fewer hot buckets than the floor below.
const SPEECH_PEAK_FLOOR: f32 = 0.0125;
/// Speech is sustained; a keyboard click lights 1-2 buckets. A segment
/// needs this many 100ms buckets above the floor to count as speech.
const SPEECH_MIN_HOT_BUCKETS: usize = 3;
const ENERGY_BUCKET_MS: u64 = 100;
const ENERGY_HISTORY_BUCKETS: usize = 1800; // 3 minutes

impl EnergyLog {
    fn new() -> Self {
        Self { buckets: Mutex::new(std::collections::VecDeque::new()) }
    }

    fn record(&self, at_ms: u64, peak: f32) {
        let bucket = at_ms - at_ms % ENERGY_BUCKET_MS;
        let mut b = self.buckets.lock().unwrap();
        match b.back_mut() {
            Some((start, max)) if *start == bucket => *max = max.max(peak),
            _ => {
                b.push_back((bucket, peak));
                if b.len() > ENERGY_HISTORY_BUCKETS {
                    b.pop_front();
                }
            }
        }
    }

    /// Buckets in [start_ms, end_ms] whose peak clears the speech floor.
    /// `None` when the span predates the retained history — the caller must
    /// not suppress on unknown energy.
    fn hot_buckets(&self, start_ms: u64, end_ms: u64) -> Option<usize> {
        let b = self.buckets.lock().unwrap();
        let oldest = b.front().map(|(m, _)| *m)?;
        if start_ms < oldest {
            return None;
        }
        Some(
            b.iter()
                .filter(|(m, p)| {
                    *m + ENERGY_BUCKET_MS > start_ms && *m <= end_ms && *p >= SPEECH_PEAK_FLOOR
                })
                .count(),
        )
    }
}

/// Reference audio addressed by the engine's per-stream sample clock.
///
/// The AEC pairs mic and far-end by ABSOLUTE stream time (`TeeFrame`
/// stamps), not by arrival order: a dropped or late frame on either side
/// becomes an explicit zero-filled gap for exactly its own span, and the
/// very next stamped frame is realigned — timing errors cannot accumulate.
/// (The DTLN canceller reads far=0 as "nothing to cancel" and passes the
/// mic through for those samples.)
struct RefRing {
    /// Absolute sample index of `buf[0]`.
    start: u64,
    buf: VecDeque<i16>,
}

/// Retain at most this much reference audio (5s @ 16k). Mic time older than
/// this can no longer be paired; anything further ahead is genuine clock
/// skew beyond repair by buffering.
const REF_RING_CAP: usize = 80_000;

impl RefRing {
    fn new() -> Self {
        Self { start: 0, buf: VecDeque::new() }
    }

    fn push(&mut self, frame: &TeeFrame) {
        let end = self.start + self.buf.len() as u64;
        if self.buf.is_empty() {
            self.start = frame.start_sample;
        } else if frame.start_sample > end {
            // Engine-side gap: fill with silence so addressing stays exact.
            let gap = (frame.start_sample - end) as usize;
            if gap > REF_RING_CAP {
                self.buf.clear();
                self.start = frame.start_sample;
            } else {
                self.buf.extend(std::iter::repeat(0).take(gap));
            }
        } else if frame.start_sample < end {
            // Overlap (stamp replay): keep the audio already present.
            let skip = (end - frame.start_sample) as usize;
            if skip >= frame.samples.len() {
                return;
            }
            self.buf.extend(&frame.samples[skip..]);
            self.trim();
            return;
        }
        self.buf.extend(&frame.samples);
        self.trim();
    }

    fn trim(&mut self) {
        if self.buf.len() > REF_RING_CAP {
            let drop = self.buf.len() - REF_RING_CAP;
            self.buf.drain(..drop);
            self.start += drop as u64;
        }
    }

    /// The far-end audio for mic span [at, at+n), zero-filled where absent.
    /// Consumes the ring through `at + n`. Returns None only when NO overlap
    /// exists (complete underrun for the span).
    fn take(&mut self, at: u64, n: usize) -> Option<Vec<i16>> {
        let ring_end = self.start + self.buf.len() as u64;
        let req_end = at + n as u64;
        if ring_end <= at || self.buf.is_empty() {
            return None;
        }
        let mut out = vec![0i16; n];
        let copy_from = at.max(self.start);
        let copy_to = req_end.min(ring_end);
        if copy_from < copy_to {
            let src = (copy_from - self.start) as usize;
            let dst = (copy_from - at) as usize;
            let len = (copy_to - copy_from) as usize;
            for i in 0..len {
                out[dst + i] = self.buf[src + i];
            }
        }
        // Consume through the end of the span.
        let adv = ((req_end.saturating_sub(self.start)) as usize).min(self.buf.len());
        self.buf.drain(..adv);
        self.start += adv as u64;
        if self.start < req_end {
            self.start = req_end;
            self.buf.clear();
        }
        Some(out)
    }
}

/// Echo-cancel the live mic stream against the system reference, then apply
/// the live-captions AGC. Passthrough (plus AGC) whenever routing says no
/// AEC is needed, the model failed to load, or the reference underruns.
async fn aec_bridge(
    mut mic_rx: mpsc::UnboundedReceiver<TeeFrame>,
    mut ref_rx: mpsc::UnboundedReceiver<TeeFrame>,
    out_tx: mpsc::UnboundedSender<Vec<i16>>,
    needs_aec: Arc<AtomicBool>,
    model_dir: PathBuf,
    energy: Arc<EnergyLog>,
    speech_env_min: Arc<std::sync::atomic::AtomicU32>,
    flight: Arc<FlightRecorder>,
) {
    const FRAME: usize = aec::echo_canceller::AcousticEchoCanceller::FRAME_SIZE;
    let mut canceller = match tokio::task::spawn_blocking(move || {
        aec::echo_canceller::AcousticEchoCanceller::load(&model_dir)
    })
    .await
    {
        Ok(Ok(c)) => Some(c),
        Ok(Err(e)) => {
            log::warn!("live AEC: model load failed ({e}) — live mic feed stays raw");
            None
        }
        Err(e) => {
            log::warn!("live AEC: loader task failed ({e}) — live mic feed stays raw");
            None
        }
    };
    let mut live_gain = LiveGain::new(None);
    // Pending mic samples + the stream time of the first pending sample.
    // Mic-side stamp gaps are zero-filled (bounded) so the ASR's
    // sample-count clock stays equal to the engine's stamp clock.
    let mut mic_buf: VecDeque<i16> = VecDeque::new();
    let mut mic_time: u64 = 0;
    let mut refs = RefRing::new();
    // Cumulative output cursor in ms — the clock the ASR's segment
    // timestamps count in (identical to mic stream time by construction).
    let mut stream_ms: u64 = 0;
    let mut underrun_frames = 0u64;
    let mut next_metrics_ms: u64 = 0;
    let mut was_active = false;

    loop {
        tokio::select! {
            r = ref_rx.recv() => {
                match r {
                    Some(frame) => refs.push(&frame),
                    None => { /* system stream ended; keep draining mic */ }
                }
            }
            m = mic_rx.recv() => {
                let Some(frame) = m else { break };
                if mic_buf.is_empty() {
                    mic_time = frame.start_sample;
                } else {
                    let expected = mic_time + mic_buf.len() as u64;
                    if frame.start_sample > expected {
                        // Engine-side mic gap: keep the clock exact with
                        // silence (bounded — a huge gap resets instead).
                        let gap = (frame.start_sample - expected) as usize;
                        if gap <= 32_000 {
                            mic_buf.extend(std::iter::repeat(0).take(gap));
                        } else {
                            log::warn!("live AEC: mic stream jumped {gap} samples — resynced");
                            mic_buf.clear();
                            mic_time = frame.start_sample;
                        }
                    }
                    // Overlap: engine stamps are monotonic; trust the buffer.
                }
                mic_buf.extend(&frame.samples);
                let mut out: Vec<i16> = Vec::with_capacity(frame.samples.len());
                while mic_buf.len() >= FRAME {
                    let near: Vec<i16> = mic_buf.drain(..FRAME).collect();
                    let at = mic_time;
                    mic_time += FRAME as u64;
                    let active = needs_aec.load(Ordering::Relaxed);
                    if active != was_active {
                        was_active = active;
                        if active {
                            if let Some(c) = canceller.as_mut() { c.reset(); }
                            log::info!("live AEC: engaged (routing detected speakers)");
                        }
                    }
                    if active && canceller.is_some() {
                        // Far end for exactly this span; silence where the
                        // reference hasn't arrived (or was dropped) — the
                        // canceller passes those samples through, and the
                        // NEXT frame is realigned by its stamp regardless.
                        let far = refs.take(at, FRAME).unwrap_or_else(|| {
                            underrun_frames += 1;
                            if underrun_frames == 1 || underrun_frames % 500 == 0 {
                                log::warn!("live AEC: reference underrun x{underrun_frames}");
                            }
                            vec![0i16; FRAME]
                        });
                        match canceller.as_mut().unwrap().process(&near, &far) {
                            Ok(clean) => out.extend(clean),
                            Err(e) => {
                                log::warn!("live AEC: process failed ({e}) — raw passthrough from here");
                                canceller = None;
                                out.extend(near);
                            }
                        }
                    } else {
                        // Inactive: consume the span anyway so a mid-call
                        // engage starts aligned.
                        let _ = refs.take(at, FRAME);
                        out.extend(near);
                    }
                }
                if !out.is_empty() {
                    // Post-AEC, pre-gain: the hallucination gate keys off the
                    // true signal level, not the lifted one.
                    let peak = out.iter().fold(0i16, |m, &s| m.max(s.saturating_abs())) as f32
                        / 32768.0;
                    energy.record(stream_ms, peak);
                    stream_ms += (out.len() as u64) * 1000 / 16_000;
                    // Guard floor is shared and switch-updatable: a mid-call
                    // mic change re-seeds it for the new device.
                    live_gain.speech_env_min =
                        f32::from_bits(speech_env_min.load(Ordering::Relaxed));
                    let gain = live_gain.apply(&mut out);
                    // Flight recorder: the AGC/AEC state actually applied,
                    // sampled every ~5 s of stream time.
                    if stream_ms >= next_metrics_ms {
                        next_metrics_ms = stream_ms + 5_000;
                        flight.agc(stream_ms, "mic", gain, live_gain.env, peak, live_gain.speech_env_min);
                        flight.aec(
                            stream_ms,
                            canceller.is_some() && needs_aec.load(Ordering::Relaxed),
                            underrun_frames,
                        );
                    }
                    if out_tx.send(out).is_err() {
                        break;
                    }
                }
            }
        }
    }
}

/// True when a mic segment's span never rose above the silence floor — an
/// ASR hallucination on near-silence, not speech. `None` energy (span older
/// than the retained history) keeps the segment.
fn is_silence_hallucination(energy: Option<&EnergyLog>, start_ms: u32, end_ms: u32) -> bool {
    let Some(log) = energy else { return false };
    match log.hot_buckets(start_ms as u64, end_ms as u64) {
        Some(hot) => hot < SPEECH_MIN_HOT_BUCKETS,
        None => false,
    }
}

/// Adapt the system tee to the plain sample stream the transcriber consumes
/// (the mic path arrives already unwrapped via the AEC bridge), recording
/// its energy timeline for the silence-hallucination gate and lifting quiet
/// far-end speech with the same envelope AGC as the mic path — the remote
/// mix spans wide dynamics (a soft speaker after a loud one). The guard
/// floor stays static: there is no echo-residue class on this side.
fn system_gain_tap(
    handle: &tokio::runtime::Handle,
    mut rx: mpsc::UnboundedReceiver<TeeFrame>,
    energy: Arc<EnergyLog>,
    flight: Arc<FlightRecorder>,
) -> mpsc::UnboundedReceiver<Vec<i16>> {
    let (tx, out_rx) = mpsc::unbounded_channel();
    handle.spawn(async move {
        let mut live_gain = LiveGain::new(None);
        let mut stream_ms: u64 = 0;
        let mut next_metrics_ms: u64 = 0;
        while let Some(f) = rx.recv().await {
            let mut samples = f.samples;
            if samples.is_empty() {
                continue;
            }
            let peak = samples.iter().fold(0i16, |m, &s| m.max(s.saturating_abs())) as f32
                / 32768.0;
            energy.record(stream_ms, peak);
            stream_ms += (samples.len() as u64) * 1000 / 16_000;
            let gain = live_gain.apply(&mut samples);
            if stream_ms >= next_metrics_ms {
                next_metrics_ms = stream_ms + 5_000;
                flight.agc(stream_ms, "system", gain, live_gain.env, peak, live_gain.speech_env_min);
            }
            if tx.send(samples).is_err() {
                break;
            }
        }
    });
    out_rx
}

/// Rolling store of recent system finals, shared by the two track tasks so
/// mic finals can be checked for pooled echo before emission.
type RecentSystemFinals = Arc<Mutex<std::collections::VecDeque<(u32, String)>>>;

const ECHO_RECENT_RETAIN_MS: u32 = 40_000;

fn record_system_final(recent: &RecentSystemFinals, start_ms: u32, text: &str) {
    let mut q = recent.lock().unwrap();
    q.push_back((start_ms, text.to_string()));
    while q
        .front()
        .is_some_and(|(t, _)| start_ms.saturating_sub(*t) > ECHO_RECENT_RETAIN_MS)
    {
        q.pop_front();
    }
}

/// True when a mic final's words are order-free contained in the pooled
/// recent system text (same containment rule as the finalize promotion
/// gate).
fn mic_final_is_pooled_echo(
    recent: &RecentSystemFinals,
    start_ms: u32,
    end_ms: u32,
    text: &str,
) -> bool {
    use transcript::promote::{
        words_contained_ratio_pooled, LiveSeg, PROMOTE_BLEED_CONTAIN, PROMOTE_BLEED_WINDOW_MS,
    };
    let mut segs: Vec<LiveSeg> = recent
        .lock()
        .unwrap()
        .iter()
        .map(|(t, s)| LiveSeg { is_system: true, start_ms: *t, end_ms: *t, text: s.clone() })
        .collect();
    segs.push(LiveSeg { is_system: false, start_ms, end_ms, text: text.to_string() });
    matches!(
        words_contained_ratio_pooled(&segs[segs.len() - 1], &segs, PROMOTE_BLEED_WINDOW_MS),
        Some(r) if r >= PROMOTE_BLEED_CONTAIN
    )
}

#[allow(clippy::too_many_arguments)]
fn spawn_realtime_task(
    handle: &tokio::runtime::Handle,
    track: LiveTrack,
    sample_rate: u32,
    client: Arc<dyn RealtimeTranscriber>,
    audio_rx: mpsc::UnboundedReceiver<Vec<i16>>,
    writer: Arc<Mutex<LiveTranscriptWriter>>,
    events_tx: mpsc::Sender<LivePipelineEvent>,
    energy: Option<Arc<EnergyLog>>,
    recent_system: RecentSystemFinals,
) {
    log::info!("live pipeline: spawning realtime task for track {:?}", track);
    handle.spawn(async move {
        // Forward provider events into the pipeline channel + writer.
        let (provider_events_tx, mut provider_events_rx) = mpsc::channel::<RealtimeEvent>(128);
        let writer_for_events = Arc::clone(&writer);
        let events_tx_for_events = events_tx.clone();
        let consumer = tokio::spawn(async move {
            while let Some(event) = provider_events_rx.recv().await {
                let event_type = match &event {
                    RealtimeEvent::Interim { .. } => "Interim",
                    RealtimeEvent::Final { .. } => "Final",
                    RealtimeEvent::Error(_) => "Error",
                };
                log::debug!(
                    "live pipeline: consumer received {:?} {:?} event",
                    track,
                    event_type
                );
                let pipeline_event = match event {
                    RealtimeEvent::Interim { segment } => {
                        if is_silence_hallucination(energy.as_deref(), segment.start_ms, segment.end_ms) {
                            continue;
                        }
                        LivePipelineEvent {
                            track,
                            kind: LivePipelineEventKind::Interim {
                                start_ms: segment.start_ms,
                                end_ms: segment.end_ms,
                                text: segment.text,
                                confidence: segment.confidence,
                            },
                        }
                    }
                    RealtimeEvent::Final { segment } => {
                        if is_silence_hallucination(energy.as_deref(), segment.start_ms, segment.end_ms) {
                            log::debug!(
                                "live mic: suppressed silence hallucination @{}ms: {:?}",
                                segment.start_ms,
                                segment.text
                            );
                            continue;
                        }
                        match track {
                            LiveTrack::System => {
                                record_system_final(&recent_system, segment.start_ms, &segment.text)
                            }
                            LiveTrack::Mic => {
                                if mic_final_is_pooled_echo(
                                    &recent_system,
                                    segment.start_ms,
                                    segment.end_ms,
                                    &segment.text,
                                ) {
                                    log::info!(
                                        "live mic: suppressed pooled echo @{}ms: {:?}",
                                        segment.start_ms,
                                        segment.text
                                    );
                                    continue;
                                }
                            }
                        }
                        // Append to live_transcript.jsonl
                        if let Ok(mut w) = writer_for_events.lock() {
                            let _ = w.append(&LiveTranscriptLine::now(
                                track,
                                segment.start_ms,
                                segment.end_ms,
                                segment.text.clone(),
                                true,
                            ));
                        }
                        LivePipelineEvent {
                            track,
                            kind: LivePipelineEventKind::Final {
                                start_ms: segment.start_ms,
                                end_ms: segment.end_ms,
                                text: segment.text,
                                confidence: segment.confidence,
                            },
                        }
                    }
                    RealtimeEvent::Error(s) => {
                        // Logged as well as shown to the user as red
                        // live-transcript text.
                        log::warn!("live transcribe error ({:?}): {}", track, s);
                        LivePipelineEvent {
                            track,
                            kind: LivePipelineEventKind::Error(s),
                        }
                    }
                };
                let _ = events_tx_for_events.send(pipeline_event).await;
            }
            log::debug!("live pipeline: consumer task for {:?} exiting", track);
        });

        log::info!("live pipeline: calling client.run() for track {:?}", track);
        let result = client.run(sample_rate, audio_rx, provider_events_tx).await;
        match &result {
            Ok(segs) => log::info!(
                "live pipeline: client.run() for {:?} returned Ok({} segments)",
                track,
                segs.len()
            ),
            Err(e) => log::info!(
                "live pipeline: client.run() for {:?} returned Err: {}",
                track,
                e
            ),
        }
        if let Err(e) = result {
            log::warn!(
                "realtime transcriber ({:?}) ended with error: {}",
                track,
                e
            );
            let _ = events_tx
                .send(LivePipelineEvent {
                    track,
                    kind: LivePipelineEventKind::Error(format!("{e}")),
                })
                .await;
        }
        let _ = consumer.await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooled_echo_suppresses_shuffled_mic_duplicate() {
        let recent: RecentSystemFinals = Arc::new(Mutex::new(Default::default()));
        record_system_final(&recent, 10_000, "the quarterly northwind forecast needs another full revision pass");
        record_system_final(&recent, 16_000, "borealis freight signs the renewal after the security review closes");
        assert!(mic_final_is_pooled_echo(
            &recent,
            14_000,
            17_000,
            "needs quarterly the forecastt northwind revision another pass full",
        ));
        assert!(!mic_final_is_pooled_echo(
            &recent,
            14_000,
            17_000,
            "let me pull up my calendar and check thursday afternoon instead",
        ));
    }

    #[test]
    fn recent_system_finals_evict_beyond_retention() {
        let recent: RecentSystemFinals = Arc::new(Mutex::new(Default::default()));
        record_system_final(&recent, 1_000, "old line");
        record_system_final(&recent, 1_000 + ECHO_RECENT_RETAIN_MS + 1, "new line");
        let q = recent.lock().unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].1, "new line");
    }

    fn loud_frame(n: usize) -> Vec<i16> {
        // Peak ≈ LiveGain::TARGET so the AGC is an identity — passthrough
        // assertions compare exact samples.
        (0..n).map(|i| if i % 2 == 0 { 4915 } else { -4915 }).collect()
    }

    /// Paused = tee detached = silence by design; the dead-mic watchdog must
    /// not fire during pause, and must still fire after resume when the mic
    /// is genuinely silent.
    #[tokio::test(start_paused = true)]
    async fn watchdog_skips_paused_fires_after_resume() {
        let (_mic_tx, mic_rx) = mpsc::unbounded_channel::<TeeFrame>();
        let (_sw_tx, sw_rx) = mpsc::unbounded_channel::<u32>();
        let (events_tx, mut events_rx) = mpsc::channel::<LivePipelineEvent>(16);
        let paused = Arc::new(AtomicBool::new(true));
        let p2 = Arc::clone(&paused);
        let tap = tokio::spawn(autogain_tap(mic_rx, None, 0, sw_rx, 16_000, events_tx, p2));

        // Two full watchdog windows while paused: nothing may fire.
        tokio::time::sleep(Duration::from_secs(11)).await;
        assert!(
            events_rx.try_recv().is_err(),
            "watchdog fired while paused"
        );

        // Resume with a still-silent mic: the next tick reports the dead mic.
        paused.store(false, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_secs(6)).await;
        let ev = events_rx.try_recv().expect("watchdog should fire after resume");
        assert!(matches!(ev.kind, LivePipelineEventKind::MicSilent { .. }));
        tap.abort();
    }

    #[test]
    fn mic_live_gain_lifts_speech_not_residue() {
        let mut g = LiveGain::new(None);
        // Sustained echo residue (~0.014 peak): envelope decays toward it,
        // but no frame may be amplified.
        let residue: Vec<i16> = vec![450; 320];
        for _ in 0..200 {
            let mut f = residue.clone();
            g.apply(&mut f);
            assert_eq!(f, residue, "residue must not be lifted");
        }
        // Real quiet speech (~0.05 peak) re-arms the lift.
        let speech: Vec<i16> = vec![1600; 320];
        let mut lifted = speech.clone();
        for _ in 0..50 {
            lifted = speech.clone();
            g.apply(&mut lifted);
        }
        assert!(lifted[0] > speech[0], "quiet speech should be lifted");
    }

    #[test]
    fn mic_live_gain_seed_overrides_static_threshold() {
        // Quiet-mic speech (~0.012 peak): under the default 0.02 guard, above
        // a learned 0.008 seed.
        let quiet: Vec<i16> = vec![400; 320];

        let mut g = LiveGain::new(None);
        let mut f = quiet.clone();
        for _ in 0..50 {
            f = quiet.clone();
            g.apply(&mut f);
        }
        assert_eq!(f, quiet, "default guard must not lift below 0.02");

        let mut g = LiveGain::new(Some(0.008));
        let mut f = quiet.clone();
        for _ in 0..50 {
            f = quiet.clone();
            g.apply(&mut f);
        }
        assert!(f[0] > quiet[0], "seeded guard lifts quiet real speech");
    }

    #[tokio::test]
    async fn system_gain_tap_lifts_quiet_far_end_not_silence() {
        let (tx, rx) = mpsc::unbounded_channel::<TeeFrame>();
        let energy = Arc::new(EnergyLog::new());
        let mut out = system_gain_tap(
            &tokio::runtime::Handle::current(),
            rx,
            Arc::clone(&energy),
            Arc::new(FlightRecorder::disabled()),
        );
        // Quiet far-end speaker (~0.03 peak, above the static guard): after
        // the envelope settles, frames come out lifted.
        let quiet: Vec<i16> = vec![1000; 320];
        let mut at = 0u64;
        for _ in 0..80 {
            tx.send(TeeFrame { start_sample: at, samples: quiet.clone() }).unwrap();
            at += 320;
        }
        let mut last = Vec::new();
        for _ in 0..80 {
            last = out.recv().await.unwrap();
        }
        assert!(last[0] > quiet[0], "quiet far-end speech lifted, got {}", last[0]);

        // Near-silence stays untouched (below the guard floor).
        let silence: Vec<i16> = vec![50; 320];
        for _ in 0..40 {
            tx.send(TeeFrame { start_sample: at, samples: silence.clone() }).unwrap();
            at += 320;
        }
        for _ in 0..40 {
            last = out.recv().await.unwrap();
        }
        assert_eq!(last, silence, "silence must not be amplified");
        // The tap recorded the energy timeline for the hallucination gate.
        assert!(energy.hot_buckets(0, 1_000).is_some());
    }

    #[test]
    fn ref_ring_pairs_by_time_and_self_heals() {
        let mut r = RefRing::new();
        // Contiguous audio 0..640.
        r.push(&TeeFrame { start_sample: 0, samples: vec![1; 320] });
        r.push(&TeeFrame { start_sample: 320, samples: vec![2; 320] });
        assert_eq!(r.take(0, 128), Some(vec![1; 128]));
        // Mid-stream request spanning the 1→2 boundary.
        assert_eq!(r.take(256, 128).unwrap(), [vec![1; 64], vec![2; 64]].concat());

        // Engine gap: frames for 640..960 never arrive; next stamp is 960.
        r.push(&TeeFrame { start_sample: 960, samples: vec![3; 320] });
        // The gap span reads as silence...
        assert_eq!(r.take(640, 128), Some(vec![0; 128]));
        // ...and the next stamped audio is exactly aligned — no drift.
        assert_eq!(r.take(960, 128), Some(vec![3; 128]));

        // Complete underrun: nothing at/after the requested span.
        assert_eq!(r.take(5_000, 128), None);
        // Recovery after underrun: a later stamped frame pairs exactly.
        r.push(&TeeFrame { start_sample: 6_000, samples: vec![4; 320] });
        assert_eq!(r.take(6_000, 128), Some(vec![4; 128]));
    }

    #[test]
    fn ref_ring_ignores_replayed_overlap_and_bounds_memory() {
        let mut r = RefRing::new();
        r.push(&TeeFrame { start_sample: 0, samples: vec![7; 320] });
        // Replay of the same span must not duplicate audio.
        r.push(&TeeFrame { start_sample: 0, samples: vec![9; 320] });
        assert_eq!(r.take(0, 320), Some(vec![7; 320]));
        // Cap: pushing far more than REF_RING_CAP retains only the newest.
        r.push(&TeeFrame { start_sample: 320, samples: vec![1; REF_RING_CAP + 4_000] });
        assert!(r.buf.len() <= REF_RING_CAP);
        let newest_start = 320 + (REF_RING_CAP as u64 + 4_000) - REF_RING_CAP as u64;
        assert_eq!(r.start, newest_start);
    }

    #[test]
    fn energy_log_hot_buckets_and_history_bounds() {
        let log = EnergyLog::new();
        // 500ms of silence residue, then 400ms of speech, then a lone click.
        for ms in (0..500).step_by(100) { log.record(ms, 0.004); }
        for ms in (500..900).step_by(100) { log.record(ms, 0.09); }
        log.record(1000, 0.05); // isolated transient
        assert_eq!(log.hot_buckets(0, 499), Some(0));
        assert_eq!(log.hot_buckets(500, 899), Some(4));
        assert_eq!(log.hot_buckets(900, 1099), Some(1));
        // Span older than retained history → None (never suppress blind).
        let log2 = EnergyLog::new();
        for i in 0..(ENERGY_HISTORY_BUCKETS as u64 + 10) {
            log2.record(i * ENERGY_BUCKET_MS, 0.5);
        }
        assert_eq!(log2.hot_buckets(0, 200), None);
    }

    /// With no canceller (bogus model dir) the bridge is a passthrough that
    /// preserves samples and ordering across arbitrary frame sizes.
    #[tokio::test]
    async fn aec_bridge_passthrough_preserves_stream() {
        let (mic_tx, mic_rx) = mpsc::unbounded_channel::<TeeFrame>();
        let (_ref_tx, ref_rx) = mpsc::unbounded_channel::<TeeFrame>();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<i16>>();
        let needs_aec = Arc::new(AtomicBool::new(true));
        let bridge = tokio::spawn(aec_bridge(
            mic_rx,
            ref_rx,
            out_tx,
            needs_aec,
            std::path::PathBuf::from("/nonexistent/aec-models"),
            Arc::new(EnergyLog::new()),
            Arc::new(std::sync::atomic::AtomicU32::new(speech_env_min_bits(None))),
            Arc::new(FlightRecorder::disabled()),
        ));

        // Ragged frame sizes exercise the FRAME-accumulator (128) repacking.
        let mut sent: Vec<i16> = Vec::new();
        let mut at: u64 = 0;
        for n in [320usize, 100, 380, 480] {
            let f = loud_frame(n);
            sent.extend(&f);
            mic_tx.send(TeeFrame { start_sample: at, samples: f }).unwrap();
            at += n as u64;
        }
        drop(mic_tx);
        bridge.await.unwrap();

        let mut got: Vec<i16> = Vec::new();
        while let Ok(f) = out_rx.try_recv() {
            got.extend(f);
        }
        // Everything except a possible sub-frame tail must come through intact.
        assert!(sent.len() - got.len() < 128, "lost more than a partial frame");
        assert_eq!(&sent[..got.len()], &got[..], "samples mutated in passthrough");
    }
}
