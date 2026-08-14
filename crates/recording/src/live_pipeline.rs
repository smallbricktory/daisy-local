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
        /// Ordered fallback engines the supervisor tries when `client` fails
        /// fatally (init or repeated mid-session death). Empty = whisper-era
        /// behavior (no fallback).
        fallbacks: Vec<Arc<dyn RealtimeTranscriber>>,
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
    /// Faint-audio annotation: possible missed speech in this span (engine
    /// decoded nothing, VAD disagreed). Display + persistence only.
    PossibleMissedSpeech { start_ms: u32, end_ms: u32 },
    /// Transient error from the provider; pipeline continues.
    Error(String),
    /// The live engine for this track changed mid-session (fallback switch
    /// or record-only demotion). `model` is the display label now running;
    /// "off" = live transcription stopped for this track.
    EngineSwitched { model: String },
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
    /// Mic-track health counters; the recorder re-arms the dead-air floor
    /// through this on mic switches.
    pub mic_health: Arc<crate::health::TrackHealth>,
}

/// Everything `LivePipeline::start` needs, in one place.
///
/// `mic_audio_rx` and `system_audio_rx` are the audio-engine tee receivers
/// (16-bit PCM at `sample_rate`); the caller sets the corresponding senders
/// on the audio engine via `set_mic_tee` / `set_system_tee`.
/// `transcript_path` is where the live transcript writer is opened; both
/// realtime tasks append to that one file. The file is created only in
/// Realtime mode — `LiveMode::Off` leaves nothing on disk. `events_tx` is a
/// single channel both tasks fan into; the caller drains it and forwards to
/// the frontend.
pub struct LiveStartConfig {
    pub mode: LiveMode,
    /// The meeting's proper nouns, as passed to the decoder for priming.
    pub live_terms: Vec<String>,
    pub sample_rate: u32,
    pub mic_audio_rx: mpsc::UnboundedReceiver<TeeFrame>,
    pub system_audio_rx: mpsc::UnboundedReceiver<TeeFrame>,
    pub transcript_path: PathBuf,
    pub events_tx: mpsc::Sender<LivePipelineEvent>,
    pub mic_source_id: u32,
    /// Live AGC guard floor (f32 bits), shared so a mic switch re-seeds it.
    pub speech_env_min: Arc<std::sync::atomic::AtomicU32>,
    pub mic_switch_rx: mpsc::UnboundedReceiver<(u32, audio_engine::topology::DeviceKey)>,
    pub needs_aec: Arc<AtomicBool>,
    /// True while the OS is voice-processing the mic capture. Software AEC
    /// is skipped then: the frames arrive echo-cancelled, and a second pass
    /// would process already-clean audio.
    pub mic_voice_processed: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
    pub flight: Arc<FlightRecorder>,
    /// Health transitions are forwarded here for the supervisor.
    pub topology_tx: std::sync::mpsc::Sender<audio_engine::topology::TopologyEvent>,
    /// Stable key of the starting mic; gain writes resolve through it.
    pub mic_key: audio_engine::topology::DeviceKey,
    /// Live id-resolution snapshot maintained by the supervisor.
    pub snapshot: Arc<std::sync::RwLock<audio_engine::topology::Snapshot>>,
    /// Gain writes are marked here so the supervisor can ignore their echo.
    pub self_writes: Arc<Mutex<crate::supervisor::SelfWrites>>,
    /// Shared epoch for health and gain timestamps.
    pub epoch: Instant,
    /// Learned dead-air peak floor for the starting mic; None disables the
    /// watchdog until a switch arms it.
    pub dead_air_floor: Option<i16>,
    /// Sender into the recorder's notice drain; the AEC bridge reports
    /// sustained reference starvation through it.
    pub notices_tx: std::sync::mpsc::Sender<crate::supervisor::Notice>,
}

impl LivePipeline {
    /// Spawn the live pipeline. Returns immediately.
    pub fn start(cfg: LiveStartConfig) -> std::io::Result<Self> {
        let LiveStartConfig {
            mode,
            live_terms,
            sample_rate,
            mic_audio_rx,
            system_audio_rx,
            transcript_path,
            events_tx,
            mic_source_id,
            speech_env_min,
            mic_switch_rx,
            needs_aec,
            mic_voice_processed,
            paused,
            flight,
            topology_tx,
            mic_key,
            snapshot,
            self_writes,
            epoch,
            dead_air_floor,
            notices_tx,
        } = cfg;
        let realtime = matches!(mode, LiveMode::Realtime { .. });
        log::info!(
            "live pipeline: start called, mode={}",
            if realtime { "Realtime" } else { "Off" }
        );

        // The runtime always comes up: even with captions off, the mic
        // auto-gain tap runs. 3 worker threads cover the tap, the AEC
        // bridge, and the 2 realtime tasks.
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
        // Per-track capture health, fed from the tee paths, ticked below.
        let health_epoch = epoch;
        let mic_health = Arc::new(crate::health::TrackHealth::default());
        let sys_health = Arc::new(crate::health::TrackHealth::default());
        match dead_air_floor {
            Some(f) => {
                mic_health.set_dead_floor(f, health_epoch.elapsed().as_millis() as u64);
                log::info!("dead-air watchdog armed: floor={f}");
            }
            None => log::info!("dead-air watchdog disabled: no residue history for this device"),
        }
        runtime.spawn(autogain_tap(
            mic_audio_rx,
            fwd,
            mic_source_id,
            mic_switch_rx,
            events_tx.clone(),
            Arc::clone(&mic_health),
            health_epoch,
            mic_key,
            snapshot,
            self_writes,
            Arc::clone(&mic_voice_processed),
        ));
        {
            let mut monitor = crate::health::HealthMonitor::new(
                Arc::clone(&mic_health),
                Arc::clone(&sys_health),
            );
            let events_health = events_tx.clone();
            let paused_health = Arc::clone(&paused);
            runtime.spawn(async move {
                let mut iv = tokio::time::interval(Duration::from_secs(1));
                loop {
                    iv.tick().await;
                    let now_ms = health_epoch.elapsed().as_millis() as u64;
                    let is_paused = paused_health.load(Ordering::Relaxed);
                    for ev in monitor.tick(now_ms, is_paused) {
                        use audio_engine::topology::{Track as TTrack, TopologyEvent as TEv};
                        // The supervisor echoes every event at info (the trace-check
                        // contract); this layer stays at debug to avoid the double log.
                        log::debug!("capture health: {ev:?}");
                        let _ = topology_tx.send(ev.clone());
                        let mic_alert = matches!(
                            &ev,
                            TEv::StreamDied { track: TTrack::Mic, .. }
                                | TEv::SilenceStarted { track: TTrack::Mic }
                        );
                        if mic_alert {
                            let _ = events_health
                                .send(LivePipelineEvent {
                                    track: LiveTrack::Mic,
                                    kind: LivePipelineEventKind::MicSilent { elapsed_ms: now_ms },
                                })
                                .await;
                        }
                    }
                }
            });
        }

        match mode {
            LiveMode::Realtime { client, fallbacks } => {
                let transcript_writer = Arc::new(Mutex::new(
                    LiveTranscriptWriter::open(&transcript_path)?,
                ));
                // System tee splits: one copy feeds the system transcriber,
                // one is the AEC far-end reference for the mic path.
                let (sys_fwd_tx, sys_fwd_rx) = mpsc::unbounded_channel::<TeeFrame>();
                let (aec_ref_tx, aec_ref_rx) = mpsc::unbounded_channel::<TeeFrame>();
                runtime.spawn(system_split(
                    system_audio_rx,
                    sys_fwd_tx,
                    aec_ref_tx,
                    Arc::clone(&sys_health),
                    health_epoch,
                ));

                // Echo-cancel the live mic feed before it reaches the
                // transcriber, so the ASR never hears the speakers. The
                // recorded mic.wav upstream is not echo-cancelled;
                // finalize's batch AEC is unchanged.
                let (mic_clean_tx, mic_clean_rx) = mpsc::unbounded_channel::<Vec<i16>>();
                let energy = Arc::new(EnergyLog::new());
                // Session time zero in capture-clock ns (first stamped mic
                // frame); anchors the system stream, whose tap can idle.
                let session_epoch_ns = Arc::new(std::sync::OnceLock::new());
                runtime.spawn(aec_bridge(
                    mic_fwd_rx,
                    aec_ref_rx,
                    mic_clean_tx,
                    needs_aec,
                    mic_voice_processed,
                    Arc::clone(&energy),
                    speech_env_min,
                    Arc::clone(&flight),
                    notices_tx,
                    Arc::clone(&session_epoch_ns),
                ));

                let recent_system: RecentSystemFinals =
                    Arc::new(Mutex::new(std::collections::VecDeque::new()));
                let terms = Arc::new(live_terms);
                spawn_realtime_task(
                    runtime.handle(),
                    LiveTrack::Mic,
                    sample_rate,
                    Arc::clone(&client),
                    fallbacks.clone(),
                    mic_clean_rx,
                    Arc::clone(&transcript_writer),
                    events_tx.clone(),
                    energy,
                    Arc::clone(&recent_system),
                    Arc::clone(&terms),
                    // The mic delivers from the session's first callback;
                    // its stream time already is session time.
                    Arc::new(std::sync::atomic::AtomicU64::new(0)),
                );
                let sys_energy = Arc::new(EnergyLog::new());
                let sys_anchor_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
                let sys_samples_rx = system_gain_tap(
                    runtime.handle(),
                    sys_fwd_rx,
                    Arc::clone(&sys_energy),
                    flight,
                    session_epoch_ns,
                    Arc::clone(&sys_anchor_ms),
                );
                spawn_realtime_task(
                    runtime.handle(),
                    LiveTrack::System,
                    sample_rate,
                    client,
                    fallbacks,
                    sys_samples_rx,
                    transcript_writer,
                    events_tx,
                    sys_energy,
                    recent_system,
                    terms,
                    sys_anchor_ms,
                );
            }
            LiveMode::Off => {
                // Mic frames are consumed by the auto-gain tap; system frames
                // feed capture health only.
                let sys_health_off = Arc::clone(&sys_health);
                runtime.spawn(async move {
                    let mut rx = system_audio_rx;
                    while let Some(frame) = rx.recv().await {
                        sys_health_off
                            .observe(&frame.samples, health_epoch.elapsed().as_millis() as u64);
                    }
                });
            }
        }

        Ok(Self {
            runtime: Some(runtime),
            mic_health,
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
/// The recorded `mic.wav` is written upstream and this AGC never touches it.
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
    /// Lift ceiling.
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

    fn new() -> Self {
        Self {
            env: Self::TARGET,
            speech_env_min: Self::SPEECH_ENV_MIN,
        }
    }

    /// Returns the gain applied to the frame (1.0 = passthrough).
    fn apply(&mut self, frame: &mut [i16]) -> f32 {
        let peak = peak_norm(frame);
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

/// Normalized peak (0..1) of an i16 frame.
fn peak_norm(samples: &[i16]) -> f32 {
    samples.iter().fold(0i16, |m, &s| m.max(s.saturating_abs())) as f32 / 32768.0
}

/// Advance a capture stamp from its anchor sample to `at`. A stale stamp
/// paired with an advanced sample position shrinks the alignment base by
/// exactly the drained amount, re-reading the same reference span.
fn anchored_ns(anchor_ns: u64, anchor_sample: u64, at: u64, rate: u64) -> u64 {
    if anchor_ns == 0 {
        return 0;
    }
    let delta = at as i64 - anchor_sample as i64;
    (anchor_ns as i64 + delta * 1_000_000_000 / rate.max(1) as i64).max(0) as u64
}

/// Baseline the device: read the gain to restore later and apply the
/// safe-start cap. Returns the controller, the pre-recording gain, and
/// whether the cap was written.
fn baseline_gain(mic_id: u32) -> (AutoGain, Option<f32>, bool) {
    let original = audio_engine::gain::input_gain(mic_id);
    let mut capped = false;
    let ag = AutoGain::new(cap_start_gain(mic_id, original, &mut capped));
    (ag, original, capped)
}

/// Put the device's pre-recording input gain back. `None` when nothing was
/// stepped or the write failed; otherwise the id and gain written.
fn restore_gain(
    snapshot: &Arc<std::sync::RwLock<audio_engine::topology::Snapshot>>,
    key: &audio_engine::topology::DeviceKey,
    mic_id: u32,
    original: Option<f32>,
    stepped: bool,
) -> Option<(u32, f32)> {
    let orig = original.filter(|_| stepped)?;
    let rid = resolve_gain_id(snapshot, key, mic_id);
    audio_engine::gain::set_input_gain(rid, orig).then_some((rid, orig))
}

/// Current numeric id for the mic key; falls back to the last known id when
/// the snapshot has no resolved entry.
fn resolve_gain_id(
    snapshot: &Arc<std::sync::RwLock<audio_engine::topology::Snapshot>>,
    key: &audio_engine::topology::DeviceKey,
    fallback: u32,
) -> u32 {
    snapshot
        .read()
        .ok()
        .and_then(|s| s.id_of(key))
        .unwrap_or(fallback)
}

#[allow(clippy::too_many_arguments)]
async fn autogain_tap(
    mut rx: mpsc::UnboundedReceiver<TeeFrame>,
    fwd: Option<mpsc::UnboundedSender<TeeFrame>>,
    mic_source_id: u32,
    mut switch_rx: mpsc::UnboundedReceiver<(u32, audio_engine::topology::DeviceKey)>,
    events_tx: mpsc::Sender<LivePipelineEvent>,
    health: Arc<crate::health::TrackHealth>,
    health_epoch: Instant,
    mut mic_key: audio_engine::topology::DeviceKey,
    snapshot: Arc<std::sync::RwLock<audio_engine::topology::Snapshot>>,
    self_writes: Arc<Mutex<crate::supervisor::SelfWrites>>,
    mic_voice_processed: Arc<AtomicBool>,
) {
    let mut mic_id = mic_source_id;
    let mark = |id: u32, g: f32| {
        if let Ok(mut w) = self_writes.lock() {
            w.mark(id, g, health_epoch.elapsed().as_millis() as u64);
        }
    };
    // `None` while the OS owns the level: voice-processed capture runs its own
    // AGC, and a second loop writing the hardware gain from post-AGC frames
    // walks the device to the floor. Baselined on the first frame we control,
    // so a mid-session backend change hands the level over either way.
    let mut ag: Option<AutoGain> = None;
    let mut original = None;
    let mut stepped = false;
    let mut prev_gain = 0.0;
    let start = Instant::now();

    // Throttle for the in-call meter level event (~12 Hz).
    let mut last_level_emit = Instant::now();

    loop {
        tokio::select! {
            // Mid-call mic switch: restore the old device, re-baseline to the new.
            Some((new_id, new_key)) = switch_rx.recv() => {
                // Coalesce to the newest queued switch: a stale rebuild
                // message processed after a failover must not re-baseline
                // gain onto (and log) the pre-switch mic.
                let (mut new_id, mut new_key) = (new_id, new_key);
                while let Ok((id, key)) = switch_rx.try_recv() {
                    new_id = id;
                    new_key = key;
                }
                health.rebaseline(health_epoch.elapsed().as_millis() as u64);
                if let Some((rid, orig)) =
                    restore_gain(&snapshot, &mic_key, mic_id, original, stepped)
                {
                    mark(rid, orig);
                }
                mic_key = new_key;
                mic_id = resolve_gain_id(&snapshot, &mic_key, new_id);
                original = None;
                stepped = false;
                ag = None;
                log::info!("mic auto-gain: re-baselined to switched mic {mic_id}");
            }
            frame = rx.recv() => {
                let Some(frame) = frame else { break };
                health.observe(&frame.samples, health_epoch.elapsed().as_millis() as u64);
                // In-call meter: emits the recording's own mic peak
                // (normalized), throttled. Reflects the OS input gain; a
                // muted input (gain→0) reads 0. Dropped when the channel is
                // full; the meter never backpressures capture.
                let now = Instant::now();
                if now.duration_since(last_level_emit) >= Duration::from_millis(80) {
                    last_level_emit = now;
                    let fpeak = peak_norm(&frame.samples);
                    let _ = events_tx.try_send(LivePipelineEvent {
                        track: crate::live_transcript::LiveTrack::Mic,
                        kind: LivePipelineEventKind::MicLevel { peak: fpeak },
                    });
                }
                let now_ms = start.elapsed().as_millis() as u64;
                if mic_voice_processed.load(Ordering::Relaxed) {
                    if let Some((rid, orig)) =
                        restore_gain(&snapshot, &mic_key, mic_id, original, stepped)
                    {
                        mark(rid, orig);
                        log::info!(
                            "mic auto-gain: OS voice processing owns the level — restored input gain to {orig:.2}"
                        );
                    }
                    original = None;
                    stepped = false;
                    ag = None;
                } else {
                    if ag.is_none() {
                        let (fresh, orig, capped) = baseline_gain(mic_id);
                        if capped {
                            mark(mic_id, SAFE_START_CEILING);
                        }
                        prev_gain = fresh.current_gain();
                        (ag, original, stepped) = (Some(fresh), orig, capped);
                    }
                    // Anti-clipping observer runs on the RAW frame (OS input gain).
                    let step = ag.as_mut().and_then(|a| a.observe(&frame.samples, now_ms));
                    if let Some(step) = step {
                        let raised = step.new_gain > prev_gain;
                        let rid = resolve_gain_id(&snapshot, &mic_key, mic_id);
                        mic_id = rid;
                        if audio_engine::gain::set_input_gain(rid, step.new_gain) {
                            mark(rid, step.new_gain);
                            stepped = true;
                            if raised {
                                log::info!("mic clean — raised input gain to {:.2}", step.new_gain);
                            } else {
                                log::info!("mic clipping — lowered input gain to {:.2}", step.new_gain);
                            }
                        }
                        prev_gain = step.new_gain;
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
    if let Some((rid, orig)) = restore_gain(&snapshot, &mic_key, mic_id, original, stepped) {
        mark(rid, orig);
        log::info!("mic auto-gain: restored input gain to {orig:.2} on stop");
    }
}

/// Fan the system tee out to the system transcriber and the AEC reference.
async fn system_split(
    mut rx: mpsc::UnboundedReceiver<TeeFrame>,
    fwd_tx: mpsc::UnboundedSender<TeeFrame>,
    ref_tx: mpsc::UnboundedSender<TeeFrame>,
    health: Arc<crate::health::TrackHealth>,
    health_epoch: Instant,
) {
    while let Some(frame) = rx.recv().await {
        health.observe(&frame.samples, health_epoch.elapsed().as_millis() as u64);
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
/// peaks are well above this even for quiet talkers.
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
/// (The canceller reads far=0 as "nothing to cancel" and passes the
/// mic through for those samples.)
struct RefRing {
    /// Absolute sample index of `buf[0]`.
    start: u64,
    buf: VecDeque<i16>,
    /// Retention cap; also the bound on silence-filled gaps.
    cap: usize,
}

/// Retain at most this much reference audio (5s @ 16k). Mic time older than
/// this can no longer be paired; anything further ahead is genuine clock
/// skew beyond repair by buffering.
const REF_RING_CAP: usize = 80_000;

impl RefRing {
    fn new(cap: usize) -> Self {
        Self { start: 0, buf: VecDeque::new(), cap }
    }

    fn push(&mut self, frame: &TeeFrame) {
        let end = self.start + self.buf.len() as u64;
        if self.buf.is_empty() {
            self.start = frame.start_sample;
        } else if frame.start_sample > end {
            // Engine-side gap: fill with silence so addressing stays exact.
            let gap = (frame.start_sample - end) as usize;
            if gap > self.cap {
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
        if self.buf.len() > self.cap {
            let drop = self.buf.len() - self.cap;
            self.buf.drain(..drop);
            self.start += drop as u64;
        }
    }

    /// Absolute sample index just past the ring's newest sample; None while
    /// empty.
    fn newest_end(&self) -> Option<u64> {
        if self.buf.is_empty() { None } else { Some(self.start + self.buf.len() as u64) }
    }

    /// Fill `out` with the far-end audio for mic span [at, at+out.len()),
    /// zero-filled where absent. Consumes the ring through the span's end.
    /// Returns false only when NO overlap exists (complete underrun): `out`
    /// is then untouched and nothing is consumed.
    fn take_into(&mut self, at: u64, out: &mut [i16]) -> bool {
        let ring_end = self.start + self.buf.len() as u64;
        let req_end = at + out.len() as u64;
        if ring_end <= at || self.buf.is_empty() {
            return false;
        }
        out.fill(0);
        let copy_from = at.max(self.start);
        let copy_to = req_end.min(ring_end);
        if copy_from < copy_to {
            let src = (copy_from - self.start) as usize;
            let dst = (copy_from - at) as usize;
            let len = (copy_to - copy_from) as usize;
            let (a, b) = self.buf.as_slices();
            if src + len <= a.len() {
                out[dst..dst + len].copy_from_slice(&a[src..src + len]);
            } else if src >= a.len() {
                out[dst..dst + len].copy_from_slice(&b[src - a.len()..src - a.len() + len]);
            } else {
                let head = a.len() - src;
                out[dst..dst + head].copy_from_slice(&a[src..]);
                out[dst + head..dst + len].copy_from_slice(&b[..len - head]);
            }
        }
        self.consume_through(req_end);
        true
    }

    /// The discard form of [`RefRing::take_into`] for the AEC-inactive path:
    /// same consumption bookkeeping and the same underrun verdict, no copy.
    /// False = no overlap at all.
    fn advance(&mut self, at: u64, n: usize) -> bool {
        let ring_end = self.start + self.buf.len() as u64;
        if ring_end <= at || self.buf.is_empty() {
            return false;
        }
        self.consume_through(at + n as u64);
        true
    }

    /// Test-facing wrapper over [`RefRing::take_into`] with the original
    /// allocating signature.
    #[cfg(test)]
    fn take(&mut self, at: u64, n: usize) -> Option<Vec<i16>> {
        let mut out = vec![0i16; n];
        self.take_into(at, &mut out).then_some(out)
    }

    /// Consume the ring up to `req_end`, but never past what has actually
    /// been delivered: clamping to `req_end` on a partial take would clip
    /// the data out of a merely-late delivery (its push then overlap-skips),
    /// turning transient lateness into sustained loss. A late remainder is
    /// skipped by the NEXT take's drain instead.
    fn consume_through(&mut self, req_end: u64) {
        let adv = ((req_end.saturating_sub(self.start)) as usize).min(self.buf.len());
        self.buf.drain(..adv);
        self.start += adv as u64;
    }
}

/// Cushion kept between the addressed span and the ring's newest sample
/// after an occupancy re-anchor; absorbs delivery jitter between the two
/// streams. The occupancy re-anchor is a gross rescue — fine alignment
/// comes from the acoustic probe — so the cushion is generous.
const ALIGN_SLACK: u64 = 1_600; // 100 ms @ 16 k
/// A ring extending further than this past the addressed span means the
/// address is stale (reads return silence from before `ring.start`).
const ALIGN_MAX_LEAD: i64 = 32_000; // 2 s
/// Consecutive misaligned frames before re-anchoring (~200 ms @ 128-sample
/// frames); delivery jitter alone must not trigger it — a span the ring
/// has not reached yet (any negative lead) is a miss and always counts.
const ALIGN_STREAK: u32 = 25;

/// Maps the mic stream's clock onto the reference stream's clock.
///
/// The two tracks stamp frames from independent delivered-sample counters:
/// the epochs differ by the streams' startup offset and the rates differ by
/// the devices' crystals. Addressing the reference ring with a raw mic time
/// therefore starves the canceller — permanently, once the gap exceeds one
/// frame. This tracks where the ring's newest data sits relative to the
/// addressed span and re-anchors the offset after a sustained miss in
/// either direction.
struct ClockAlign {
    /// Stream pairing derived from the two capture stamps. Exact when both
    /// streams carry one; falls back to the occupancy heuristic otherwise.
    base: i64,
    /// Acoustic path (speaker to mic), measured by the GCC-PHAT probe.
    residual: i64,
    /// How far the two stamps say the streams have drifted apart beyond what
    /// their sample counts imply. Zero when either stream is unstamped.
    drift: i64,
    bad_streak: u32,
}

impl ClockAlign {
    fn new() -> Self {
        Self { base: 0, residual: 0, drift: 0, bad_streak: 0 }
    }

    /// Adopts an acoustically measured PAIRING offset (GCC-PHAT probe) —
    /// ground truth for `offset()`. Stored relative to the stamp base so
    /// the applied offset equals the measurement (base + residual would
    /// otherwise double-count the pairing), while later stamp-driven base
    /// drift still tracks on top of the fixed acoustic path.
    fn set_measured(&mut self, offset: i64) {
        if (offset - self.offset()).abs() > 32 {
            log::info!(
                "live AEC: acoustic offset {} → {} samples",
                self.offset(), offset
            );
        }
        self.residual = offset - self.base;
        self.bad_streak = 0;
    }

    /// Sets the pairing base from one stamped frame on each stream. Each
    /// frame gives a (time, sample) pair, so the reference sample captured at
    /// the mic frame's instant is
    /// `ref_sample + (mic_ns - ref_ns) * rate`, and the base is its distance
    /// from the mic sample. A zero stamp on either side leaves the base alone,
    /// so an unstamped backend keeps the occupancy heuristic.
    fn set_base_from_pair(
        &mut self,
        mic_ns: u64,
        mic_sample: u64,
        ref_ns: u64,
        ref_sample: u64,
        sample_rate: u32,
    ) {
        if mic_ns == 0 || ref_ns == 0 {
            return;
        }
        // A reference stamp more than a second stale means the tap is
        // under-delivering (failover storm, teardown): extrapolating
        // through it would swing the base by the whole outage and
        // overwrite the occupancy rescue. Keep the current anchor.
        if mic_ns.abs_diff(ref_ns) > 1_000_000_000 {
            return;
        }
        let drift = (mic_ns as i64 - ref_ns as i64) * sample_rate as i64 / 1_000_000_000;
        self.drift = drift;
        self.base = (ref_sample as i64 - mic_sample as i64) + drift;
    }

    fn offset(&self) -> i64 {
        self.base + self.residual
    }

    /// The ring address for the mic span [at, at+n). `ring_end` is the ring's
    /// newest absolute sample (None while empty — nothing to anchor to).
    fn address(&mut self, at: u64, n: usize, ring_end: Option<u64>) -> u64 {
        let adj = at.saturating_add_signed(self.offset());
        // Empty ring: no evidence either way — a miss consumes the ring, so
        // empties are routine between reference deliveries. The streak must
        // survive them or it can never reach ALIGN_STREAK.
        let Some(end) = ring_end else {
            return adj;
        };
        let lead = end as i64 - (adj + n as u64) as i64;
        if (0..=ALIGN_MAX_LEAD).contains(&lead) {
            self.bad_streak = 0;
            return adj;
        }
        self.bad_streak += 1;
        if self.bad_streak < ALIGN_STREAK {
            return adj;
        }
        self.bad_streak = 0;
        let new_offset = end as i64 - (at + n as u64 + ALIGN_SLACK) as i64;
        log::info!(
            "live AEC: reference clock re-anchored (offset {} → {} samples)",
            self.offset(), new_offset
        );
        self.base = new_offset - self.residual;
        at.saturating_add_signed(self.offset())
    }
}

#[allow(clippy::too_many_arguments)]
/// Sustained AEC reference starvation → one SystemAudioLost per bout;
/// reference resumption → one SystemAudioRecovered.
const UNDERRUN_NOTIFY_AFTER: Duration = Duration::from_secs(30);

/// A call gap above this restarts the bout clock: frames stop flowing
/// entirely across pause/resume, and a paused bout must not mature on
/// wall-clock time alone.
const UNDERRUN_GAP_RESET: Duration = Duration::from_secs(5);

#[derive(Default)]
struct UnderrunBout {
    since: Option<std::time::Instant>,
    last: Option<std::time::Instant>,
    notified: bool,
}

impl UnderrunBout {
    fn on_underrun(&mut self, now: std::time::Instant) -> Option<crate::supervisor::Notice> {
        if let Some(last) = self.last {
            if now.duration_since(last) > UNDERRUN_GAP_RESET {
                self.since = None;
            }
        }
        self.last = Some(now);
        let since = *self.since.get_or_insert(now);
        if !self.notified && now.duration_since(since) >= UNDERRUN_NOTIFY_AFTER {
            self.notified = true;
            return Some(crate::supervisor::Notice::SystemAudioLost);
        }
        None
    }

    fn on_ok(&mut self) -> Option<crate::supervisor::Notice> {
        self.since = None;
        if std::mem::take(&mut self.notified) {
            return Some(crate::supervisor::Notice::SystemAudioRecovered);
        }
        None
    }
}

/// Software echo cancellation runs only when routing calls for it AND the OS
/// is not already voice-processing the capture.
fn aec_wanted(needs_aec: &AtomicBool, mic_voice_processed: &AtomicBool) -> bool {
    needs_aec.load(Ordering::Relaxed) && !mic_voice_processed.load(Ordering::Relaxed)
}

/// Echo-cancel the live mic stream against the system reference, then apply
/// the live-captions AGC. Passthrough (plus AGC) whenever routing says no
/// AEC is needed, the model failed to load, or the reference underruns.
async fn aec_bridge(
    mut mic_rx: mpsc::UnboundedReceiver<TeeFrame>,
    mut ref_rx: mpsc::UnboundedReceiver<TeeFrame>,
    out_tx: mpsc::UnboundedSender<Vec<i16>>,
    needs_aec: Arc<AtomicBool>,
    mic_voice_processed: Arc<AtomicBool>,
    energy: Arc<EnergyLog>,
    speech_env_min: Arc<std::sync::atomic::AtomicU32>,
    flight: Arc<FlightRecorder>,
    notices_tx: std::sync::mpsc::Sender<crate::supervisor::Notice>,
    session_epoch_ns: Arc<std::sync::OnceLock<u64>>,
) {
    const FRAME: usize = aec::AcousticEchoCanceller::FRAME_SIZE;
    let mut canceller = match aec::AcousticEchoCanceller::new() {
        Ok(c) => Some(c),
        Err(e) => {
            log::warn!("live AEC: canceller init failed ({e}) — live mic feed stays raw");
            None
        }
    };
    let mut live_gain = LiveGain::new();
    // Pending mic samples + the stream time of the first pending sample.
    // Mic-side stamp gaps are zero-filled (bounded) so the ASR's
    // sample-count clock stays equal to the engine's stamp clock.
    let mut mic_buf: VecDeque<i16> = VecDeque::new();
    let mut mic_time: u64 = 0;
    // Reused far-end scratch: the 125 Hz drain loop must not allocate.
    let mut far_scratch = [0i16; FRAME];
    let mut refs = RefRing::new(REF_RING_CAP);
    // Cumulative output cursor in ms — the clock the ASR's segment
    // timestamps count in (identical to mic stream time by construction).
    let mut stream_ms: u64 = 0;
    let mut underrun_frames = 0u64;
    let mut underrun_bout = UnderrunBout::default();
    // A reference that never delivered is an idle tap (nothing playing),
    // not a loss — the notice only makes sense after first delivery.
    let mut ref_ever_delivered = false;
    let mut next_metrics_ms: u64 = 0;
    let mut was_active = false;
    let mut align = ClockAlign::new();
    // Newest stamped reference frame: (host_ns, start_sample). Pairs with the
    // mic frame's own stamp to give the alignment base.
    let mut last_ref_stamp: Option<(u64, u64)> = None;
    // Stamp of the mic frame feeding the current drain.
    let mut mic_host_ns: u64 = 0;
    let mut mic_anchor_sample: u64 = 0;
    // Acoustic alignment probe: rolling raw tails of both streams. The
    // GCC-PHAT lag between them measures where the far-end audio actually
    // appears in the mic (device + room path included) — ground truth for
    // the align offset.
    const PROBE_WINDOW: usize = 32_000; // 2 s @ 16 k
    const PROBE_MAX_LAG: usize = 4_000; // ±250 ms
    const PROBE_INTERVAL: u64 = 160_000; // 10 s of mic clock, once locked
    const PROBE_INTERVAL_SEEK: u64 = 32_000; // 2 s until the first lock
    /// A lone measurement needs a decisive peak to be trusted…
    const PROBE_STRONG_RATIO: f32 = 10.0;
    /// …but a weaker peak that lands within CORROBORATE_SPAN of the previous
    /// measurement is real: 2 s windows of quiet room echo score 5–7 where
    /// finalize's 20 s spans score ≥ 28.
    const PROBE_MIN_RATIO: f32 = 5.0;
    const PROBE_CORROBORATE_SPAN: i64 = 320; // 20 ms
    /// Alignment only moves when the median of accepted measurements
    /// disagrees with the current offset by more than this — every applied
    /// jump costs the canceller a re-convergence.
    const PROBE_HYSTERESIS: i64 = 160; // 10 ms
    let mut probe_mic: VecDeque<i16> = VecDeque::new();
    // Probe tail rides the same stamped-ring discipline as `refs`, capped at
    // the probe window and never consumed (read in place each probe).
    let mut probe_ref = RefRing::new(PROBE_WINDOW);
    let mut next_probe_at: u64 = 32_000; // first probe once 2 s of tail exists
    let mut probe_history: VecDeque<i64> = VecDeque::new();
    let mut probe_last_candidate: Option<i64> = None;

    loop {
        tokio::select! {
            r = ref_rx.recv() => {
                match r {
                    Some(frame) => {
                        if canceller.is_some() {
                            probe_ref.push(&frame);
                        }
                        if frame.host_ns != 0 {
                            last_ref_stamp = Some((frame.host_ns, frame.start_sample));
                        }
                        refs.push(&frame);
                        ref_ever_delivered = true;
                    }
                    None => { /* system stream ended; keep draining mic */ }
                }
            }
            m = mic_rx.recv() => {
                let Some(frame) = m else { break };
                if frame.host_ns != 0 {
                    // First stamped mic frame ≈ session start: the mic
                    // delivers from the first callback, unlike the system
                    // tap, which idles until something renders. The system
                    // task anchors its stream clock against this.
                    let _ = session_epoch_ns.set(frame.host_ns);
                    mic_host_ns = frame.host_ns;
                    mic_anchor_sample = frame.start_sample;
                }
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
            }
        }
        // Drain after EVERY event: a reference arrival must resume
        // frames held for delivery headroom, not just the next mic
        // frame.
        let mut out: Vec<i16> = Vec::with_capacity(mic_buf.len().max(1));
        while mic_buf.len() >= FRAME {
            // Delivery headroom: a truthful pairing offset points at
            // reference audio that lands a few ms AFTER the mic
            // frame it matches. Hold the frame until its span has
            // arrived — bounded to 100 ms of backlog, past which the
            // miss is a real underrun and the frame proceeds. Held
            // audio resumes on the next mic delivery (≤ one engine
            // frame later). Both the cancelling and the passthrough
            // path hold: underrun accounting is the dead-tap signal
            // on OS voice processing, and it is truthful only after
            // delivery lag is absorbed.
            const HEADROOM_CAP: usize = 1_600; // 100 ms @ 16 k
            if mic_buf.len() + FRAME <= HEADROOM_CAP {
                // Learn the pairing BEFORE deciding to hold: a reference
                // that starts late arrives with a stamp that legitimately
                // moves the base — holding on the stale offset would starve
                // on a span the new base says is already covered.
                let hold_ns = anchored_ns(mic_host_ns, mic_anchor_sample, mic_time, 16_000);
                if let Some((ref_ns, ref_sample)) = last_ref_stamp {
                    align.set_base_from_pair(hold_ns, mic_time, ref_ns, ref_sample, 16_000);
                }
                let adj_end = mic_time
                    .saturating_add_signed(align.offset())
                    .saturating_add(FRAME as u64);
                let covered = refs.newest_end().is_some_and(|end| end >= adj_end);
                if !covered && ref_ever_delivered {
                    break;
                }
            }
            let near: Vec<i16> = mic_buf.drain(..FRAME).collect();
            let at = mic_time;
            mic_time += FRAME as u64;
            let active = aec_wanted(&needs_aec, &mic_voice_processed);
            if active != was_active {
                was_active = active;
                if active {
                    if let Some(c) = canceller.as_mut() { c.reset(); }
                    log::info!("live AEC: engaged (routing detected speakers)");
                }
            }
            // Probe tails are only kept while a canceller exists to align.
            if canceller.is_some() {
                probe_mic.extend(&near);
                while probe_mic.len() > PROBE_WINDOW {
                    probe_mic.pop_front();
                }
            }
            let mic_end = at + FRAME as u64;
            if active
                && canceller.is_some()
                && mic_end >= next_probe_at
                && probe_mic.len() == PROBE_WINDOW
                && probe_ref.buf.len() == PROBE_WINDOW
            {
                let locked = probe_history.len() >= 3;
                next_probe_at = mic_end
                    + if locked { PROBE_INTERVAL } else { PROBE_INTERVAL_SEEK };
                let m: Vec<f32> =
                    probe_mic.iter().map(|&s| s as f32 / 32768.0).collect();
                let r: Vec<f32> =
                    probe_ref.buf.iter().map(|&s| s as f32 / 32768.0).collect();
                let ref_end = probe_ref.newest_end().unwrap_or(0);
                if let Some((d, ratio)) = aec::align::acoustic_lag(&m, &r, PROBE_MAX_LAG) {
                    // Equal-length tails ending at mic_end (mic
                    // clock) and ref_end (ref clock): the echo
                    // trails the reference by `d`, so the pairing
                    // offset is E − A − d.
                    let candidate = ref_end as i64 - mic_end as i64 - d;
                    let corroborated = ratio >= PROBE_MIN_RATIO
                        && probe_last_candidate.is_some_and(|p| {
                            (candidate - p).abs() <= PROBE_CORROBORATE_SPAN
                        });
                    probe_last_candidate = Some(candidate);
                    let mut applied = false;
                    if ratio >= PROBE_STRONG_RATIO || corroborated {
                        probe_history.push_back(candidate);
                        if probe_history.len() > 3 {
                            probe_history.pop_front();
                        }
                        // Median of accepted measurements, applied
                        // with hysteresis once two agree: a lone
                        // spurious peak cannot move alignment, and a
                        // stable alignment is never twitched.
                        let mut sorted: Vec<i64> =
                            probe_history.iter().copied().collect();
                        sorted.sort();
                        let median = sorted[sorted.len() / 2];
                        applied = probe_history.len() >= 2
                            && (median - align.offset()).abs() > PROBE_HYSTERESIS;
                        if applied {
                            align.set_measured(median);
                        }
                    }
                    flight.aec_probe((mic_end / 16) as u64, candidate, ratio, applied);
                }
            }
            // The mic and reference streams stamp from independent
            // clocks; the align map keeps the address inside the
            // ring across epoch offset and rate drift. The stamp is
            // anchored to `mic_anchor_sample` and advanced to THIS
            // frame's position — a stale stamp would shrink the base
            // by exactly the drained amount and re-read the same
            // reference span every iteration.
            let iter_ns = anchored_ns(mic_host_ns, mic_anchor_sample, at, 16_000);
            if let Some((ref_ns, ref_sample)) = last_ref_stamp {
                align.set_base_from_pair(iter_ns, at, ref_ns, ref_sample, 16_000);
            }
            let at = align.address(at, FRAME, refs.newest_end());
            if active && canceller.is_some() {
                // Far end for exactly this span. With no reference the
                // canceller is skipped rather than run against silence:
                // adapting to a far end that was never played mistrains
                // the filter, and a mistrained filter adds echo instead
                // of removing it. The NEXT frame is realigned by its
                // stamp regardless.
                match refs.take_into(at, &mut far_scratch).then_some(&far_scratch) {
                    Some(far) => {
                        if let Some(n) = underrun_bout.on_ok() {
                            let _ = notices_tx.send(n);
                        }
                        match canceller.as_mut().unwrap().process(&near, far) {
                            Ok(clean) => out.extend(clean),
                            Err(e) => {
                                log::warn!("live AEC: process failed ({e}) — raw passthrough from here");
                                canceller = None;
                                out.extend(near);
                            }
                        }
                    }
                    None => {
                        underrun_frames += 1;
                        if underrun_frames == 1 || underrun_frames % 500 == 0 {
                            log::warn!("live AEC: reference underrun x{underrun_frames} — adaptation frozen");
                        }
                        if ref_ever_delivered {
                            if let Some(n) =
                                underrun_bout.on_underrun(std::time::Instant::now())
                            {
                                let _ = notices_tx.send(n);
                            }
                        }
                        out.extend(near);
                    }
                }
            } else {
                // Inactive: consume the span anyway so a mid-call engage
                // starts aligned. The far end is the recorded system track
                // whether or not we cancel against it, so a tap that stops
                // delivering is reported on this path too — it is the only
                // path a mac on OS voice processing ever takes.
                let notice = if refs.advance(at, FRAME) {
                    underrun_bout.on_ok()
                } else if ref_ever_delivered {
                    underrun_frames += 1;
                    if underrun_frames == 1 || underrun_frames % 500 == 0 {
                        log::warn!("live path: system reference underrun x{underrun_frames} — far end not arriving");
                    }
                    underrun_bout.on_underrun(std::time::Instant::now())
                } else {
                    None
                };
                if let Some(n) = notice {
                    let _ = notices_tx.send(n);
                }
                out.extend(near);
            }
        }
        if !out.is_empty() {
            let peak = peak_norm(&out);
            let at_ms = stream_ms;
            stream_ms += (out.len() as u64) * 1000 / 16_000;
            // Guard floor is shared and switch-updatable: a mid-call
            // mic change re-seeds it for the new device.
            live_gain.speech_env_min = f32::from_bits(speech_env_min.load(Ordering::Relaxed));
            let gain = live_gain.apply(&mut out);
            // Post-AEC, post-gain: the hallucination gate keys off
            // the level the ASR hears. Residue is never lifted
            // (gain 1.0), so its recorded level is unchanged.
            energy.record(at_ms, (peak * gain).min(1.0));
            // Flight recorder: the AGC/AEC state actually applied,
            // sampled every ~5 s of stream time.
            if stream_ms >= next_metrics_ms {
                next_metrics_ms = stream_ms + 5_000;
                flight.agc(stream_ms, "mic", gain, live_gain.env, peak, live_gain.speech_env_min);
                // Zeros on the reference side mean none has arrived
                // yet; `align.drift` is then still zero.
                let (ref_ns, ref_sample) = last_ref_stamp.unwrap_or((0, 0));
                flight.clock(
                    stream_ms,
                    mic_host_ns,
                    ref_ns,
                    stream_ms * 16,
                    ref_sample,
                    align.base,
                    align.drift,
                );
                flight.aec(
                    stream_ms,
                    canceller.is_some() && aec_wanted(&needs_aec, &mic_voice_processed),
                    underrun_frames,
                    canceller.as_ref().map(|c| c.stats()).unwrap_or_default(),
                );
            }
            if out_tx.send(out).is_err() {
                break;
            }
        }
    }
}

/// True when a segment's span never rose above the silence floor — an ASR
/// hallucination on near-silence, not speech. A span older than the
/// retained history keeps the segment.
fn is_silence_hallucination(energy: &EnergyLog, start_ms: u32, end_ms: u32) -> bool {
    match energy.hot_buckets(start_ms as u64, end_ms as u64) {
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
    session_epoch_ns: Arc<std::sync::OnceLock<u64>>,
    anchor_ms: Arc<std::sync::atomic::AtomicU64>,
) -> mpsc::UnboundedReceiver<Vec<i16>> {
    let (tx, out_rx) = mpsc::unbounded_channel();
    handle.spawn(async move {
        let mut live_gain = LiveGain::new();
        let mut stream_ms: u64 = 0;
        let mut next_metrics_ms: u64 = 0;
        let mut anchored = false;
        while let Some(f) = rx.recv().await {
            // The system tap idles until some process renders audio, so the
            // first frame can arrive well into the session while the ASR
            // counts its own samples from zero. Anchor this task's stream
            // clock — and, via `anchor_ms`, the ASR's segment stamps — to
            // the session by the stamp gap against the first mic frame.
            // Without this, system live lines land early by the idle span:
            // cross-track order breaks and the echo pool loses headroom.
            if !anchored && f.host_ns != 0 {
                anchored = true;
                let pad_ms = session_epoch_ns
                    .get()
                    .map(|e| f.host_ns.saturating_sub(*e) / 1_000_000)
                    .unwrap_or(0);
                if pad_ms > 0 {
                    log::info!(
                        "live pipeline: system stream anchored {:.1}s into the session (tap idle)",
                        pad_ms as f64 / 1000.0
                    );
                }
                stream_ms = pad_ms;
                anchor_ms.store(pad_ms, std::sync::atomic::Ordering::Relaxed);
            }
            let mut samples = f.samples;
            if samples.is_empty() {
                continue;
            }
            let peak = peak_norm(&samples);
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
    let segs: Vec<LiveSeg> = recent
        .lock()
        .unwrap()
        .iter()
        .map(|(t, s)| LiveSeg { is_system: true, start_ms: *t, end_ms: *t, text: s.clone() })
        .collect();
    let mic_seg = LiveSeg { is_system: false, start_ms, end_ms, text: text.to_string() };
    matches!(
        words_contained_ratio_pooled(&mic_seg, &segs, PROMOTE_BLEED_WINDOW_MS),
        Some(r) if r >= PROMOTE_BLEED_CONTAIN
    )
}

#[allow(clippy::too_many_arguments)]
/// Shift a provider event's stream-relative times into session time.
fn anchor_event(event: RealtimeEvent, anchor_ms: u32) -> RealtimeEvent {
    match event {
        RealtimeEvent::Interim { mut segment } => {
            segment.start_ms += anchor_ms;
            segment.end_ms += anchor_ms;
            RealtimeEvent::Interim { segment }
        }
        RealtimeEvent::Final { mut segment } => {
            segment.start_ms += anchor_ms;
            segment.end_ms += anchor_ms;
            RealtimeEvent::Final { segment }
        }
        RealtimeEvent::PossibleMissedSpeech { start_ms, end_ms } => {
            RealtimeEvent::PossibleMissedSpeech {
                start_ms: start_ms + anchor_ms,
                end_ms: end_ms + anchor_ms,
            }
        }
        other => other,
    }
}

fn spawn_realtime_task(
    handle: &tokio::runtime::Handle,
    track: LiveTrack,
    sample_rate: u32,
    client: Arc<dyn RealtimeTranscriber>,
    fallbacks: Vec<Arc<dyn RealtimeTranscriber>>,
    audio_rx: mpsc::UnboundedReceiver<Vec<i16>>,
    writer: Arc<Mutex<LiveTranscriptWriter>>,
    events_tx: mpsc::Sender<LivePipelineEvent>,
    energy: Arc<EnergyLog>,
    recent_system: RecentSystemFinals,
    live_terms: Arc<Vec<String>>,
    anchor_ms: Arc<std::sync::atomic::AtomicU64>,
) {
    log::info!("live pipeline: spawning realtime task for track {:?}", track);
    handle.spawn(async move {
        // Forward provider events into the pipeline channel + writer.
        let (provider_events_tx, mut provider_events_rx) = mpsc::channel::<RealtimeEvent>(128);
        // One per track: each decode window is transcribed cold, so a sentence
        // continued across two of them arrives with its second half
        // capitalized.
        let mut casing = transcript::casing::CaseSmoother::with_known_names(&live_terms);
        let signals = Arc::new(SupervisorSignals::default());
        let signals_for_events = Arc::clone(&signals);
        let writer_for_events = Arc::clone(&writer);
        let events_tx_for_events = events_tx.clone();
        let consumer = tokio::spawn(async move {
            while let Some(event) = provider_events_rx.recv().await {
                // The provider counts stream time from its first sample;
                // shift into session time (the system stream can start well
                // after the session — see `system_gain_tap`). Applied before
                // anything reads the times, so the hallucination gate, the
                // echo pool, the writer and the UI all see one clock.
                let event = anchor_event(
                    event,
                    anchor_ms.load(std::sync::atomic::Ordering::Relaxed) as u32,
                );
                let event_type = match &event {
                    RealtimeEvent::Interim { .. } => "Interim",
                    RealtimeEvent::Final { .. } => "Final",
                    RealtimeEvent::PossibleMissedSpeech { .. } => "PossibleMissedSpeech",
                    RealtimeEvent::FallingBehind => "FallingBehind",
                    RealtimeEvent::Error(_) => "Error",
                };
                log::debug!(
                    "live pipeline: consumer received {:?} {:?} event",
                    track,
                    event_type
                );
                let pipeline_event = match event {
                    RealtimeEvent::Interim { segment } => {
                        signals_for_events.note_event();
                        if is_silence_hallucination(&energy, segment.start_ms, segment.end_ms)
                            || transcript::hallucination::is_outro(&segment.text)
                        {
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
                        signals_for_events.note_event();
                        if is_silence_hallucination(&energy, segment.start_ms, segment.end_ms)
                            || transcript::hallucination::is_outro(&segment.text)
                        {
                            log::debug!(
                                "live: suppressed hallucination @{}ms: {:?}",
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
                                    log::debug!(
                                        "live mic: suppressed pooled echo @{}ms: {:?}",
                                        segment.start_ms,
                                        segment.text
                                    );
                                    continue;
                                }
                            }
                        }
                        // Recased before it is written or shown, so the stored
                        // line, the caption and the promoted transcript all
                        // carry the same text.
                        let text = casing.smooth(&segment.text);
                        // Append to live_transcript.jsonl
                        if let Ok(mut w) = writer_for_events.lock() {
                            let _ = w.append(&LiveTranscriptLine::now(
                                track,
                                segment.start_ms,
                                segment.end_ms,
                                text.clone(),
                                true,
                            ));
                        }
                        LivePipelineEvent {
                            track,
                            kind: LivePipelineEventKind::Final {
                                start_ms: segment.start_ms,
                                end_ms: segment.end_ms,
                                text,
                                confidence: segment.confidence,
                            },
                        }
                    }
                    RealtimeEvent::PossibleMissedSpeech { start_ms, end_ms } => {
                        // Annotation only: persisted for the session view,
                        // surfaced live. Promotion ignores non-final lines.
                        if let Ok(mut w) = writer_for_events.lock() {
                            let _ = w.append(&LiveTranscriptLine::marker(track, start_ms, end_ms));
                        }
                        LivePipelineEvent {
                            track,
                            kind: LivePipelineEventKind::PossibleMissedSpeech { start_ms, end_ms },
                        }
                    }
                    RealtimeEvent::FallingBehind => {
                        // Supervisor-only signal; nothing reaches the UI.
                        signals_for_events.note_falling();
                        continue;
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
        let result = supervise_backends(
            track,
            sample_rate,
            client,
            fallbacks,
            audio_rx,
            provider_events_tx,
            &events_tx,
            &signals,
        )
        .await;
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

/// One supervision decision after a backend died mid-session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorStep {
    /// Rebuild the same backend once (transient failure).
    RestartSame,
    /// Move to the next fallback engine.
    TryNext,
    /// Stop decoding; capture continues and finalize's batch pass covers the
    /// session.
    RecordOnly,
}

/// The retry ladder: a backend that never produced an event gets no restart
/// (its init is broken — try the next engine); one that worked gets exactly
/// one restart before falling through.
fn next_step(had_events: bool, restarts_used: u32, candidates_left: usize) -> SupervisorStep {
    if had_events && restarts_used == 0 {
        SupervisorStep::RestartSame
    } else if candidates_left > 0 {
        SupervisorStep::TryNext
    } else {
        SupervisorStep::RecordOnly
    }
}

/// Sustained `FallingBehind` without a quiet period this long re-arms the
/// demotion clock.
const FALLING_QUIET_RESET_S: u64 = 60;
/// Continuous falling-behind this long demotes the session to record-only.
const DEMOTE_AFTER_S: u64 = 180;

/// Cross-task signals between the event consumer and the backend supervisor.
#[derive(Default)]
struct SupervisorSignals {
    saw_event: std::sync::atomic::AtomicBool,
    /// (first, last) `FallingBehind` stamps of the current bout.
    falling: Mutex<Option<(std::time::Instant, std::time::Instant)>>,
}

impl SupervisorSignals {
    fn note_event(&self) {
        self.saw_event.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    fn saw_event(&self) -> bool {
        self.saw_event.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn reset_for_next_candidate(&self) {
        self.saw_event.store(false, std::sync::atomic::Ordering::Relaxed);
        *self.falling.lock().unwrap() = None;
    }
    fn note_falling(&self) {
        let now = std::time::Instant::now();
        let mut g = self.falling.lock().unwrap();
        *g = Some(match *g {
            Some((first, _)) => (first, now),
            None => (now, now),
        });
    }
    fn should_demote(&self) -> bool {
        let mut g = self.falling.lock().unwrap();
        match *g {
            Some((_, last)) if last.elapsed().as_secs() >= FALLING_QUIET_RESET_S => {
                *g = None; // caught up: bout over
                false
            }
            Some((first, _)) => first.elapsed().as_secs() >= DEMOTE_AFTER_S,
            None => false,
        }
    }
}

/// Run backends under supervision: feed audio through a per-attempt child
/// channel so a dead backend can be replaced without touching the tee. Audio
/// capture is never blocked by any of this; when decoding stops for good the
/// remaining audio is drained (the WAVs persist and finalize's batch pass
/// transcribes them).
#[allow(clippy::too_many_arguments)]
async fn supervise_backends(
    track: LiveTrack,
    sample_rate: u32,
    client: Arc<dyn RealtimeTranscriber>,
    fallbacks: Vec<Arc<dyn RealtimeTranscriber>>,
    mut audio_rx: mpsc::UnboundedReceiver<Vec<i16>>,
    provider_events_tx: mpsc::Sender<RealtimeEvent>,
    events_tx: &mpsc::Sender<LivePipelineEvent>,
    signals: &SupervisorSignals,
) -> std::result::Result<Vec<transcript::Segment>, providers_realtime::RealtimeError> {
    let notify = |msg: String| {
        let events_tx = events_tx.clone();
        async move {
            let _ = events_tx
                .send(LivePipelineEvent { track, kind: LivePipelineEventKind::Error(msg) })
                .await;
        }
    };
    // Keeps the frontend's "Transcribing live: X" label honest across
    // fallback switches and demotions.
    let switched = |model: String| {
        let events_tx = events_tx.clone();
        async move {
            let _ = events_tx
                .send(LivePipelineEvent {
                    track,
                    kind: LivePipelineEventKind::EngineSwitched { model },
                })
                .await;
        }
    };
    let mut current = client;
    let mut candidates = std::collections::VecDeque::from(fallbacks);
    let mut restarts_used = 0u32;
    loop {
        let (child_tx, child_rx) = mpsc::unbounded_channel::<Vec<i16>>();
        let running = Arc::clone(&current);
        let tx = provider_events_tx.clone();
        let mut run_fut =
            Box::pin(async move { running.run(sample_rate, child_rx, tx).await });
        let mut child_tx = Some(child_tx);
        let mut input_open = true;
        let mut demoted = false;
        let result = loop {
            tokio::select! {
                r = &mut run_fut => break r,
                frame = audio_rx.recv(), if child_tx.is_some() => match frame {
                    Some(f) => {
                        let _ = child_tx.as_ref().expect("guarded").send(f);
                        if signals.should_demote() {
                            // Close the child feed: the backend flushes its
                            // pending tail and returns cleanly.
                            child_tx = None;
                            demoted = true;
                        }
                    }
                    None => {
                        input_open = false;
                        child_tx = None;
                    }
                },
            }
        };
        match result {
            Ok(segs) => {
                if demoted {
                    log::warn!(
                        "live supervisor ({track:?}): demoting to record-only — sustained falling-behind"
                    );
                    notify(
                        "Live transcription paused — this machine can't keep up. Recording \
                         continues; the transcript completes when the recording ends."
                            .into(),
                    )
                    .await;
                    switched("off".into()).await;
                    drain_audio(&mut audio_rx).await;
                } else if input_open {
                    // Backend self-closed with input still flowing: treat as
                    // done but keep the tee alive.
                    log::warn!("live supervisor ({track:?}): backend ended early; record-only remainder");
                    switched("off".into()).await;
                    drain_audio(&mut audio_rx).await;
                }
                return Ok(segs);
            }
            Err(e) if !input_open || demoted => {
                // Failure racing shutdown/demotion: nothing left to feed.
                // NOTE: the return value is best-effort — segments from
                // earlier candidates live in the persisted jsonl, which is
                // the source of truth; callers must not treat this Ok as
                // "the whole session's segments".
                log::warn!("live supervisor ({track:?}): backend failed at close: {e}");
                if demoted {
                    notify(
                        "Live transcription paused — this machine can't keep up. Recording \
                         continues; the transcript completes when the recording ends."
                            .into(),
                    )
                    .await;
                    switched("off".into()).await;
                    drain_audio(&mut audio_rx).await;
                }
                return Ok(Vec::new());
            }
            Err(e) => {
                let step = next_step(signals.saw_event(), restarts_used, candidates.len());
                log::warn!(
                    "live supervisor ({track:?}): backend {} failed ({e}) → {:?}",
                    current.name(),
                    step
                );
                match step {
                    SupervisorStep::RestartSame => {
                        restarts_used += 1;
                    }
                    SupervisorStep::TryNext => {
                        let next = candidates.pop_front().expect("candidates_left > 0");
                        notify(format!(
                            "{} failed to transcribe — switching to {}.",
                            current.model(),
                            next.model()
                        ))
                        .await;
                        switched(next.model().to_string()).await;
                        current = next;
                        restarts_used = 0;
                        signals.reset_for_next_candidate();
                    }
                    SupervisorStep::RecordOnly => {
                        notify(
                            "Live transcription stopped — recording continues. The transcript \
                             is produced when the recording ends."
                                .into(),
                        )
                        .await;
                        switched("off".into()).await;
                        drain_audio(&mut audio_rx).await;
                        return Ok(Vec::new());
                    }
                }
            }
        }
    }
}

/// Keep the tee sender alive while discarding frames (capture and WAV
/// persistence are upstream and unaffected).
async fn drain_audio(audio_rx: &mut mpsc::UnboundedReceiver<Vec<i16>>) {
    while audio_rx.recv().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underrun_bout_fires_once_after_threshold_and_recovers() {
        use crate::supervisor::Notice;
        let t0 = std::time::Instant::now();
        let mut b = UnderrunBout::default();
        // Frames arrive continuously (well under the gap-reset spacing).
        let mut fired = None;
        for s in 0..16u64 {
            let r = b.on_underrun(t0 + Duration::from_secs(s * 2));
            if r.is_some() {
                fired = Some((s * 2, r.unwrap()));
            }
        }
        assert_eq!(fired, Some((30, Notice::SystemAudioLost)));
        // Once per bout.
        assert_eq!(b.on_underrun(t0 + Duration::from_secs(34)), None);
        // Reference back → recovered exactly once.
        assert_eq!(b.on_ok(), Some(Notice::SystemAudioRecovered));
        assert_eq!(b.on_ok(), None);
        // Next bout re-arms from its own start.
        let t1 = t0 + Duration::from_secs(100);
        let mut fired = false;
        for s in 0..16u64 {
            fired |= b.on_underrun(t1 + Duration::from_secs(s * 2)).is_some();
        }
        assert!(fired);
    }

    #[test]
    fn underrun_bout_call_gap_restarts_the_clock() {
        // A pause detaches the tees; the bout must not mature on wall-clock
        // time across the gap.
        let t0 = std::time::Instant::now();
        let mut b = UnderrunBout::default();
        assert_eq!(b.on_underrun(t0), None);
        assert_eq!(b.on_underrun(t0 + Duration::from_secs(2)), None);
        // 60s frame gap (pause) → clock restarts; 20s more of underruns
        // stays quiet, only a full fresh 30s fires.
        let t1 = t0 + Duration::from_secs(62);
        let mut fired = false;
        for s in 0..11u64 {
            fired |= b.on_underrun(t1 + Duration::from_secs(s * 2)).is_some();
        }
        assert!(!fired, "gap must restart the bout clock");
        for s in 11..16u64 {
            fired |= b.on_underrun(t1 + Duration::from_secs(s * 2)).is_some();
        }
        assert!(fired);
    }

    #[test]
    fn short_underrun_bursts_never_fire() {
        let t0 = std::time::Instant::now();
        let mut b = UnderrunBout::default();
        for i in 0..100u64 {
            assert_eq!(b.on_underrun(t0 + Duration::from_secs(i)), None);
            assert_eq!(b.on_ok(), None);
        }
    }

    #[test]
    fn probe_apply_yields_exactly_the_measured_offset() {
        // The probe candidate is the ABSOLUTE pairing offset; applying it
        // must make offset() equal it, not add it on top of the stamp base.
        let mut a = ClockAlign::new();
        a.set_base_from_pair(1_000_000_000, 0, 1_000_000_000, 943, 16_000);
        assert_eq!(a.offset(), 943);
        a.set_measured(276);
        assert_eq!(a.offset(), 276, "base+residual double-counted the pairing");
        // Later stamp drift tracks on top of the fixed acoustic path.
        a.set_base_from_pair(2_000_000_000, 16_000, 2_000_000_000, 17_043, 16_000);
        assert_eq!(a.offset(), 276 + (1_043 - 943));
    }

    #[test]
    fn stale_ref_stamp_never_moves_the_base() {
        // A reference stamp >1s older than the mic stamp means the tap is
        // under-delivering; extrapolating through it would swing the base
        // by the outage and overwrite the occupancy rescue.
        let mut a = ClockAlign::new();
        a.set_base_from_pair(1_000_000_000, 0, 1_000_000_000, 500, 16_000);
        let before = a.offset();
        a.set_base_from_pair(5_000_000_000, 64_000, 1_500_000_000, 8_500, 16_000);
        assert_eq!(a.offset(), before, "stale stamp pair moved the base");
        // A fresh pair still updates.
        a.set_base_from_pair(5_000_000_000, 64_000, 4_999_000_000, 64_300, 16_000);
        assert_ne!(a.offset(), before);
    }

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

    #[test]
    fn mic_live_gain_lifts_speech_not_residue() {
        let mut g = LiveGain::new();
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

        let mut g = LiveGain::new();
        let mut f = quiet.clone();
        for _ in 0..50 {
            f = quiet.clone();
            g.apply(&mut f);
        }
        assert_eq!(f, quiet, "default guard must not lift below 0.02");

        let mut g = LiveGain::new();
        g.speech_env_min = 0.008;
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
            Arc::new(std::sync::OnceLock::new()),
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
        );
        // Quiet far-end speaker (~0.03 peak, above the static guard): after
        // the envelope settles, frames come out lifted.
        let quiet: Vec<i16> = vec![1000; 320];
        let mut at = 0u64;
        for _ in 0..80 {
            tx.send(TeeFrame { start_sample: at, host_ns: 0, samples: quiet.clone(), chunk: None }).unwrap();
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
            tx.send(TeeFrame { start_sample: at, host_ns: 0, samples: silence.clone(), chunk: None }).unwrap();
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
    fn anchor_event_shifts_segments_and_markers_into_session_time() {
        let seg = transcript::Segment {
            start_ms: 1_000,
            end_ms: 2_000,
            text: "hi".into(),
            ..Default::default()
        };
        match anchor_event(RealtimeEvent::Final { segment: seg.clone() }, 15_800) {
            RealtimeEvent::Final { segment } => {
                assert_eq!((segment.start_ms, segment.end_ms), (16_800, 17_800));
            }
            other => panic!("wrong variant: {other:?}"),
        }
        match anchor_event(RealtimeEvent::Interim { segment: seg.clone() }, 15_800) {
            RealtimeEvent::Interim { segment } => {
                assert_eq!((segment.start_ms, segment.end_ms), (16_800, 17_800));
            }
            other => panic!("wrong variant: {other:?}"),
        }
        match anchor_event(
            RealtimeEvent::PossibleMissedSpeech { start_ms: 5, end_ms: 9 },
            100,
        ) {
            RealtimeEvent::PossibleMissedSpeech { start_ms, end_ms } => {
                assert_eq!((start_ms, end_ms), (105, 109));
            }
            other => panic!("wrong variant: {other:?}"),
        }
        // Anchor zero (the mic track) is a no-op.
        match anchor_event(RealtimeEvent::Final { segment: seg.clone() }, 0) {
            RealtimeEvent::Final { segment } => assert_eq!(segment, seg),
            other => panic!("wrong variant: {other:?}"),
        }
        // Non-timed events pass through.
        assert!(matches!(
            anchor_event(RealtimeEvent::FallingBehind, 15_800),
            RealtimeEvent::FallingBehind
        ));
    }

    #[tokio::test]
    async fn system_gain_tap_anchors_an_idle_tap_to_session_time() {
        let (tx, rx) = mpsc::unbounded_channel::<TeeFrame>();
        let epoch = Arc::new(std::sync::OnceLock::new());
        epoch.set(1_000_000_000u64).unwrap();
        let anchor = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut out = system_gain_tap(
            &tokio::runtime::Handle::current(),
            rx,
            Arc::new(EnergyLog::new()),
            Arc::new(FlightRecorder::disabled()),
            Arc::clone(&epoch),
            Arc::clone(&anchor),
        );
        // First tap frame lands 15.8 s after the first mic frame.
        tx.send(TeeFrame {
            start_sample: 0,
            host_ns: 1_000_000_000 + 15_800 * 1_000_000,
            samples: vec![1000; 320],
            chunk: None,
        })
        .unwrap();
        let _ = out.recv().await.unwrap();
        assert_eq!(anchor.load(Ordering::Relaxed), 15_800);

        // A tap that delivers from session start anchors at zero.
        let (tx2, rx2) = mpsc::unbounded_channel::<TeeFrame>();
        let anchor2 = Arc::new(std::sync::atomic::AtomicU64::new(7));
        let mut out2 = system_gain_tap(
            &tokio::runtime::Handle::current(),
            rx2,
            Arc::new(EnergyLog::new()),
            Arc::new(FlightRecorder::disabled()),
            epoch,
            Arc::clone(&anchor2),
        );
        tx2.send(TeeFrame {
            start_sample: 0,
            host_ns: 1_000_000_000,
            samples: vec![1000; 320],
            chunk: None,
        })
        .unwrap();
        let _ = out2.recv().await.unwrap();
        assert_eq!(anchor2.load(Ordering::Relaxed), 0);
    }

    // ---- Live replay harness (AEC arm) -----------------------------------
    // Replays a real session's raw track WAVs through the live AEC bridge
    // with the cross-track clock pathology injected (epoch offset +
    // delivery-rate drift), and prints underrun/heal/suppression
    // measurements. Field verification without a live meeting.
    //
    //   DAISY_REPLAY_SESSION_DIR=…/sessions/daisy-… \
    //   cargo test -p recording --lib live_replay_aec -- --ignored --nocapture
    //
    // Optional: DAISY_REPLAY_OFFSET_MS (default 30), DAISY_REPLAY_DRIFT_PPM
    // (default 130), DAISY_REPLAY_MAX_S (default 120 — replay cap).

    fn read_wav_16k(path: &std::path::Path) -> Vec<i16> {
        let mut r = hound::WavReader::open(path)
            .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
        assert_eq!(r.spec().sample_rate, 16_000, "{}", path.display());
        assert_eq!(r.spec().channels, 1, "{}", path.display());
        r.samples::<i16>().map(|s| s.unwrap()).collect()
    }

    /// Concatenated track audio across all chunk dirs, in chunk order.
    fn read_session_track(session_dir: &std::path::Path, file: &str) -> Vec<i16> {
        let chunks = session_dir.join("chunks");
        let mut dirs: Vec<_> = std::fs::read_dir(&chunks)
            .unwrap_or_else(|e| panic!("read {}: {e}", chunks.display()))
            .map(|d| d.unwrap().path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        let mut out = Vec::new();
        for d in dirs {
            let p = d.join(file);
            if p.is_file() {
                out.extend(read_wav_16k(&p));
            }
        }
        out
    }

    thread_local! {
        /// Clock-divergence series from the most recent `replay_bridge` run.
        static CLOCK_DIVERGENCE: std::cell::RefCell<Vec<i64>> =
            const { std::cell::RefCell::new(Vec::new()) };
        /// Alignment-base series from the most recent `replay_bridge` run.
        static CLOCK_BASE: std::cell::RefCell<Vec<i64>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Runs `aec_bridge` over the given tracks with an injected reference
    /// clock pathology. Delivery: one 160-sample frame per track per tick;
    /// the reference starts `offset_ticks` late and accrues delivery credit
    /// at `1 - ppm/1e6` per tick (stamps stay contiguous — its clock simply
    /// runs slow, like a real device crystal). Returns (bridge output,
    /// underrun series from the flight recorder).
    async fn replay_bridge(
        label: &str,
        mic: &[i16],
        sys: &[i16],
        active: bool,
        offset_ticks: u64,
        drift_ppm: f64,
    ) -> (Vec<i16>, Vec<u64>) {
        replay_bridge_ticked(label, mic, sys, active, offset_ticks, drift_ppm, 160).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn replay_bridge_ticked(
        label: &str,
        mic: &[i16],
        sys: &[i16],
        active: bool,
        offset_ticks: u64,
        drift_ppm: f64,
        tick_samples: usize,
    ) -> (Vec<i16>, Vec<u64>) {
        let (mic_tx, mic_rx) = mpsc::unbounded_channel::<TeeFrame>();
        let (ref_tx, ref_rx) = mpsc::unbounded_channel::<TeeFrame>();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<i16>>();
        let tmp = tempfile::tempdir().unwrap();
        let flight = Arc::new(FlightRecorder::open_if(true, tmp.path()));
        let bridge = tokio::spawn(aec_bridge(
            mic_rx,
            ref_rx,
            out_tx,
            Arc::new(AtomicBool::new(active)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(EnergyLog::new()),
            Arc::new(std::sync::atomic::AtomicU32::new(0.004f32.to_bits())),
            Arc::clone(&flight),
            std::sync::mpsc::channel().0,
            Arc::new(std::sync::OnceLock::new()),
        ));

        let tick: usize = tick_samples;
        let ticks = mic.len() / tick;
        let mut mic_pos = 0u64;
        let mut ref_pos = 0u64;
        // Crystal drift = the reference device produces slightly fewer
        // samples per wall second: its CONTENT stretches while stamps track
        // delivered samples (delivery itself keeps up). Modeled by a
        // fractional source cursor with nearest-neighbor repetition — at
        // 130 ppm that is one repeated sample every ~7700.
        let step = 1.0 - drift_ppm / 1e6;
        let mut ref_src = 0.0f64;
        let mut out = Vec::new();
        for i in 0..ticks as u64 {
            if i >= offset_ticks {
                let need = ref_src + tick as f64 * step;
                if (need as usize) < sys.len() {
                    let frame: Vec<i16> = (0..tick)
                        .map(|k| sys[(ref_src + k as f64 * step) as usize])
                        .collect();
                    ref_src = need;
                    let _ = ref_tx.send(TeeFrame {
                        start_sample: ref_pos,
                        host_ns: (i + 1) * 10_000_000,
                        samples: frame,
                        chunk: None,
                    });
                    ref_pos += tick as u64;
                }
            }
            let s = (i as usize) * tick;
            let _ = mic_tx.send(TeeFrame {
                start_sample: mic_pos,
                host_ns: (i + 1) * 10_000_000,
                samples: mic[s..s + tick].to_vec(),
                chunk: None,
            });
            mic_pos += tick as u64;
            // Deterministic lockstep: wait until the bridge has consumed
            // this tick (its output trails input by at most one AEC frame).
            // Ring occupancy then reflects only the injected pathology, not
            // scheduler timing — runs are repeatable.
            let expected = ((i as usize + 1) * tick).saturating_sub(127);
            while out.len() < expected {
                match out_rx.recv().await {
                    Some(chunk) => out.extend(chunk),
                    None => break,
                }
            }
        }
        drop(mic_tx);
        drop(ref_tx);
        while let Some(chunk) = out_rx.recv().await {
            out.extend(chunk);
        }
        bridge.await.unwrap();
        drop(flight);
        // Writer thread appends + flushes per line; give it a beat.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let metrics = std::fs::read_to_string(tmp.path().join("metrics.jsonl")).unwrap_or_default();
        let underruns: Vec<u64> = metrics
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["kind"] == "aec")
            .filter_map(|v| v["underrun_frames"].as_u64())
            .collect();
        CLOCK_BASE.with(|c| {
            *c.borrow_mut() = metrics
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter(|v| v["kind"] == "clock")
                .filter_map(|v| v["base_samples"].as_i64())
                .collect();
        });
        CLOCK_DIVERGENCE.with(|c| {
            *c.borrow_mut() = metrics
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter(|v| v["kind"] == "clock")
                .filter_map(|v| v["divergence_samples"].as_i64())
                .collect();
        });
        for v in metrics
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["kind"] == "aec_probe")
        {
            println!(
                "  [{label}] probe t={:>6}ms offset={:>7} ratio={:>6.1} applied={}",
                v["t_ms"], v["offset"], v["ratio"].as_f64().unwrap_or(0.0), v["applied"]
            );
        }
        if !underruns.is_empty() {
            let q = underruns.len() / 4;
            println!(
                "  [{label}] underrun series: 25%={} 50%={} 75%={} end={}",
                underruns[q], underruns[q * 2], underruns[q * 3],
                underruns[underruns.len() - 1]
            );
        }
        (out, underruns)
    }

    fn rms(s: &[i16]) -> f64 {
        if s.is_empty() {
            return 0.0;
        }
        (s.iter().map(|&x| (x as f64 / 32768.0).powi(2)).sum::<f64>() / s.len() as f64).sqrt()
    }

    /// Mean output-level ratio (dB) between two bridge outputs, over the
    /// seconds where the system track is active (far-end speech present).
    fn suppression_db(sys: &[i16], a: &[i16], b: &[i16]) -> f64 {
        const SEC: usize = 16_000;
        let mut ratios = Vec::new();
        let n = sys.len().min(a.len()).min(b.len()) / SEC;
        for i in 0..n {
            let w = i * SEC..(i + 1) * SEC;
            if rms(&sys[w.clone()]) > 0.02 {
                let (ra, rb) = (rms(&a[w.clone()]), rms(&b[w]));
                if rb > 1e-6 {
                    ratios.push(20.0 * (ra / rb).log10());
                }
            }
        }
        if ratios.is_empty() {
            return 0.0;
        }
        ratios.iter().sum::<f64>() / ratios.len() as f64
    }

    /// What finalize produces for the same audio: the canceller over the
    /// whole chunk, mic paired index-to-index with the system track.
    fn finalize_equivalent(mic: &[i16], sys: &[i16]) -> Vec<i16> {
        const FRAME_SIZE: usize = aec::AcousticEchoCanceller::FRAME_SIZE;
        let mut canc = aec::AcousticEchoCanceller::new().expect("build canceller");
        let n = mic.len().min(sys.len());
        let frames = n / FRAME_SIZE;
        let mut clean = Vec::with_capacity(frames * FRAME_SIZE);
        for f in 0..frames {
            let (s, e) = (f * FRAME_SIZE, f * FRAME_SIZE + FRAME_SIZE);
            match canc.process(&mic[s..e], &sys[s..e]) {
                Ok(c) => clean.extend_from_slice(&c),
                Err(_) => clean.extend_from_slice(&mic[s..e]),
            }
        }
        clean
    }

    #[tokio::test]
    #[ignore]
    async fn live_replay_aec() {
        let session = std::path::PathBuf::from(
            std::env::var("DAISY_REPLAY_SESSION_DIR").expect("DAISY_REPLAY_SESSION_DIR"),
        );
        let offset_ms: u64 = std::env::var("DAISY_REPLAY_OFFSET_MS")
            .ok().and_then(|s| s.parse().ok()).unwrap_or(30);
        let drift_ppm: f64 = std::env::var("DAISY_REPLAY_DRIFT_PPM")
            .ok().and_then(|s| s.parse().ok()).unwrap_or(130.0);
        let max_s: usize = std::env::var("DAISY_REPLAY_MAX_S")
            .ok().and_then(|s| s.parse().ok()).unwrap_or(120);

        let mut mic = read_session_track(&session, "mic.wav");
        let mut sys = read_session_track(&session, "system.wav");
        mic.truncate(max_s * 16_000);
        sys.truncate(max_s * 16_000);
        assert!(!mic.is_empty() && !sys.is_empty(), "session has no track audio");
        println!(
            "replay: {} — mic {:.1}s sys {:.1}s, offset {offset_ms}ms drift {drift_ppm}ppm",
            session.display(),
            mic.len() as f64 / 16_000.0,
            sys.len() as f64 / 16_000.0,
        );

        // Passthrough reference (AEC inactive; AGC still applied — the two
        // active runs are compared against this, isolating the canceller).
        let (pass, _) = replay_bridge("pass", &mic, &sys, false, 0, 0.0).await;
        // AEC with clean clocks.
        let (clean, u_clean) = replay_bridge("clean", &mic, &sys, true, 0, 0.0).await;
        // AEC with the injected pathology (the field scenario).
        let offset_ticks = offset_ms * 16 / 160;
        let (pathological, u_path) =
            replay_bridge("path", &mic, &sys, true, offset_ticks, drift_ppm).await;

        assert_eq!(pass.len(), clean.len());
        assert_eq!(pass.len(), pathological.len());

        let total_frames =
            (mic.len() / aec::AcousticEchoCanceller::FRAME_SIZE) as u64;
        let (u_clean_end, u_path_end) =
            (*u_clean.last().unwrap_or(&0), *u_path.last().unwrap_or(&0));
        // Healed: underruns must stop growing after the re-anchor settles.
        // Growth over the last half of the run stays under 1% of frames.
        let half = u_path.len() / 2;
        let late_growth = u_path_end.saturating_sub(*u_path.get(half).unwrap_or(&0));
        println!(
            "underruns: clean {u_clean_end}/{total_frames} pathological {u_path_end}/{total_frames} (late growth {late_growth})"
        );
        let fin = finalize_equivalent(&mic, &sys);
        // Each arm is divided by an uncancelled baseline with the same gain
        // treatment: the bridge applies AGC on every arm including
        // passthrough, so live arms use `pass` and the finalize arm, which
        // has no AGC, uses the raw mic.
        let live_db = suppression_db(&sys, &clean, &pass);
        let path_db = suppression_db(&sys, &pathological, &pass);
        let fin_db = suppression_db(&sys, &fin, &mic);
        println!(
            "suppression during far-end speech: live-clean {live_db:.1} dB, \
             live-pathological {path_db:.1} dB, finalize-equivalent {fin_db:.1} dB",
        );
        assert!(
            (late_growth as f64) < (total_frames as f64) * 0.01,
            "pathological run must heal: late underrun growth {late_growth} of {total_frames} frames"
        );
    }

    #[test]
    fn ref_ring_pairs_by_time_and_self_heals() {
        let mut r = RefRing::new(REF_RING_CAP);
        // Contiguous audio 0..640.
        r.push(&TeeFrame { start_sample: 0, host_ns: 0, samples: vec![1; 320], chunk: None });
        r.push(&TeeFrame { start_sample: 320, host_ns: 0, samples: vec![2; 320], chunk: None });
        assert_eq!(r.take(0, 128), Some(vec![1; 128]));
        // Mid-stream request spanning the 1→2 boundary.
        assert_eq!(r.take(256, 128).unwrap(), [vec![1; 64], vec![2; 64]].concat());

        // Engine gap: frames for 640..960 never arrive; next stamp is 960.
        r.push(&TeeFrame { start_sample: 960, host_ns: 0, samples: vec![3; 320], chunk: None });
        // The gap span reads as silence...
        assert_eq!(r.take(640, 128), Some(vec![0; 128]));
        // ...and the next stamped audio is exactly aligned — no drift.
        assert_eq!(r.take(960, 128), Some(vec![3; 128]));

        // Complete underrun: nothing at/after the requested span.
        assert_eq!(r.take(5_000, 128), None);
        // Recovery after underrun: a later stamped frame pairs exactly.
        r.push(&TeeFrame { start_sample: 6_000, host_ns: 0, samples: vec![4; 320], chunk: None });
        assert_eq!(r.take(6_000, 128), Some(vec![4; 128]));
    }

    #[test]
    fn ref_ring_advance_reports_the_same_underrun_as_take() {
        // The AEC-inactive path consumes through `advance`, and its verdict
        // is what raises SystemAudioLost there — a mac on OS voice
        // processing never takes the other branch.
        let mut r = RefRing::new(REF_RING_CAP);
        r.push(&TeeFrame { start_sample: 0, host_ns: 0, samples: vec![1; 320], chunk: None });
        assert!(r.advance(0, 128), "covered span must report covered");
        assert!(!r.advance(5_000, 128), "span past the newest sample is an underrun");
        // An empty ring is an underrun, not a covered span.
        let mut empty = RefRing::new(REF_RING_CAP);
        assert!(!empty.advance(0, 128));
    }

    #[test]
    fn ref_ring_ignores_replayed_overlap_and_bounds_memory() {
        let mut r = RefRing::new(REF_RING_CAP);
        r.push(&TeeFrame { start_sample: 0, host_ns: 0, samples: vec![7; 320], chunk: None });
        // Replay of the same span must not duplicate audio.
        r.push(&TeeFrame { start_sample: 0, host_ns: 0, samples: vec![9; 320], chunk: None });
        assert_eq!(r.take(0, 320), Some(vec![7; 320]));
        // Cap: pushing far more than REF_RING_CAP retains only the newest.
        r.push(&TeeFrame { start_sample: 320, host_ns: 0, samples: vec![1; REF_RING_CAP + 4_000], chunk: None });
        assert!(r.buf.len() <= REF_RING_CAP);
        let newest_start = 320 + (REF_RING_CAP as u64 + 4_000) - REF_RING_CAP as u64;
        assert_eq!(r.start, newest_start);
    }

    #[test]
    fn clock_align_re_anchors_a_lagging_reference_clock() {
        // Regression: the system stream's counter sat a
        // fixed ~30 ms behind the mic's, so every raw address missed the
        // ring and the canceller ran on zeros the whole session.
        let mut a = ClockAlign::new();
        let lag = 4_000u64; // ref clock 250 ms behind mic clock
        let mut mic_at = 100_000u64;
        let ring_end = |at: u64| Some(at - lag + 64); // newest ref, mid-delivery
        let mut adj = 0u64;
        for _ in 0..=ALIGN_STREAK {
            adj = a.address(mic_at, 128, ring_end(mic_at));
            mic_at += 128;
        }
        // Re-anchored: the addressed span now ends ALIGN_SLACK behind the
        // ring's newest sample.
        let end = ring_end(mic_at - 128).unwrap();
        assert_eq!(adj + 128 + ALIGN_SLACK, end);
        // Healthy from here on: address stays inside the ring, offset stable.
        let next = a.address(mic_at, 128, Some(end + 128));
        assert_eq!(next, adj + 128);
    }

    #[test]
    fn clock_align_tolerates_jitter_without_re_anchoring() {
        let mut a = ClockAlign::new();
        // Brief misses (fewer than ALIGN_STREAK) interleaved with healthy
        // frames: the offset must stay put.
        for i in 0..200u64 {
            let at = i * 128;
            let end = if i % 10 < 3 { at } else { at + 128 + 200 }; // 30% brief lag
            let adj = a.address(at, 128, Some(end));
            assert_eq!(adj, at, "offset must remain 0 under jitter (frame {i})");
        }
    }

    #[test]
    fn clock_align_re_anchors_a_stale_leading_ring() {
        // Ring far AHEAD of the address (mic clock behind): reads would
        // return pre-ring silence forever without a re-anchor.
        let mut a = ClockAlign::new();
        let mut adj = 0;
        for i in 0..=ALIGN_STREAK as u64 {
            adj = a.address(i * 128, 128, Some(50_000 + i * 128));
        }
        let end = 50_000 + ALIGN_STREAK as u64 * 128;
        assert_eq!(adj + 128 + ALIGN_SLACK, end);
    }

    #[test]
    fn clock_align_empty_ring_neither_shifts_nor_counts() {
        let mut a = ClockAlign::new();
        for i in 0..100u64 {
            assert_eq!(a.address(i * 128, 128, None), i * 128);
        }
    }

    #[test]
    fn clock_align_streak_survives_empty_ring_interleaves() {
        // Regression: a miss consumes and clears the ring,
        // so roughly every other address() call sees an empty ring. Resetting
        // the streak on empty kept it below ALIGN_STREAK forever and the
        // re-anchor never fired — AEC stayed starved on the fixed build.
        let mut a = ClockAlign::new();
        let lag = 4_000u64;
        let mut re_anchored = false;
        for i in 0..200u64 {
            let at = 100_000 + i * 128;
            let ring = if i % 2 == 0 { Some(at - lag + 64) } else { None };
            let adj = a.address(at, 128, ring);
            if adj != at {
                re_anchored = true;
                break;
            }
        }
        assert!(re_anchored, "re-anchor must fire despite interleaved empty-ring calls");
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

    #[test]
    fn voice_processed_capture_suppresses_software_aec() {
        let routing_wants = AtomicBool::new(true);
        let voice_processed = AtomicBool::new(false);
        assert!(
            aec_wanted(&routing_wants, &voice_processed),
            "speakers routing with raw capture must run software AEC"
        );
        // The OS cancelled it during capture; a second pass would process
        // already-clean audio.
        voice_processed.store(true, Ordering::Relaxed);
        assert!(!aec_wanted(&routing_wants, &voice_processed));
        // A mid-session failover onto a device the OS voice processor
        // refuses puts software AEC back in charge.
        voice_processed.store(false, Ordering::Relaxed);
        assert!(aec_wanted(&routing_wants, &voice_processed));
        // Headphones: neither path wants it.
        routing_wants.store(false, Ordering::Relaxed);
        assert!(!aec_wanted(&routing_wants, &voice_processed));
    }

    /// With AEC inactive the bridge is a passthrough that preserves samples
    /// and ordering across arbitrary frame sizes.
    #[tokio::test]
    async fn aec_bridge_passthrough_preserves_stream() {
        const FRAME: usize = aec::AcousticEchoCanceller::FRAME_SIZE;
        let (mic_tx, mic_rx) = mpsc::unbounded_channel::<TeeFrame>();
        let (_ref_tx, ref_rx) = mpsc::unbounded_channel::<TeeFrame>();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<i16>>();
        let needs_aec = Arc::new(AtomicBool::new(false));
        let bridge = tokio::spawn(aec_bridge(
            mic_rx,
            ref_rx,
            out_tx,
            needs_aec,
            Arc::new(AtomicBool::new(false)),
            Arc::new(EnergyLog::new()),
            Arc::new(std::sync::atomic::AtomicU32::new(speech_env_min_bits(None))),
            Arc::new(FlightRecorder::disabled()),
            std::sync::mpsc::channel().0,
            Arc::new(std::sync::OnceLock::new()),
        ));

        // Ragged frame sizes exercise the FRAME-accumulator repacking.
        let mut sent: Vec<i16> = Vec::new();
        let mut at: u64 = 0;
        for n in [320usize, 100, 380, 480] {
            let f = loud_frame(n);
            sent.extend(&f);
            mic_tx.send(TeeFrame { start_sample: at, host_ns: 0, samples: f, chunk: None }).unwrap();
            at += n as u64;
        }
        drop(mic_tx);
        bridge.await.unwrap();

        let mut got: Vec<i16> = Vec::new();
        while let Ok(f) = out_rx.try_recv() {
            got.extend(f);
        }
        // Everything except a possible sub-frame tail must come through intact.
        assert!(sent.len() - got.len() < FRAME, "lost more than a partial frame");
        assert_eq!(&sent[..got.len()], &got[..], "samples mutated in passthrough");
    }

    /// A reference that arrives a tap-latency behind the mic is absorbed by
    /// the delivery-headroom hold on the passthrough path too: no underruns
    /// are booked and no re-anchor churn runs while the far end is flowing.
    #[tokio::test]
    async fn passthrough_with_lagging_reference_books_no_underruns() {
        const FRAME: usize = aec::AcousticEchoCanceller::FRAME_SIZE;
        let dir = tempfile::tempdir().unwrap();
        let flight = Arc::new(FlightRecorder::open_if(true, dir.path()));
        let (mic_tx, mic_rx) = mpsc::unbounded_channel::<TeeFrame>();
        let (ref_tx, ref_rx) = mpsc::unbounded_channel::<TeeFrame>();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<i16>>();
        let bridge = tokio::spawn(aec_bridge(
            mic_rx,
            ref_rx,
            out_tx,
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(true)), // OS voice-processed: software AEC off
            Arc::new(EnergyLog::new()),
            Arc::new(std::sync::atomic::AtomicU32::new(speech_env_min_bits(None))),
            Arc::clone(&flight),
            std::sync::mpsc::channel().0,
            Arc::new(std::sync::OnceLock::new()),
        ));

        // 6 s of stamped audio; every reference frame is delivered LAG
        // samples after the matching mic frame, like a real system tap.
        const LAG: u64 = 800; // 50 ms @ 16 k
        let ns = |s: u64| 1 + s * 1_000_000_000 / 16_000;
        let mut t: u64 = 0;
        while t < 96_000 {
            if t >= LAG {
                let r = t - LAG;
                ref_tx
                    .send(TeeFrame {
                        start_sample: r,
                        host_ns: ns(r),
                        samples: loud_frame(FRAME),
                        chunk: None,
                    })
                    .unwrap();
            }
            mic_tx
                .send(TeeFrame {
                    start_sample: t,
                    host_ns: ns(t),
                    samples: loud_frame(FRAME),
                    chunk: None,
                })
                .unwrap();
            t += FRAME as u64;
            tokio::task::yield_now().await;
        }
        // Cover the held tail so the last mic frames drain before close.
        ref_tx
            .send(TeeFrame {
                start_sample: t - LAG,
                host_ns: ns(t - LAG),
                samples: vec![0; LAG as usize + FRAME],
                chunk: None,
            })
            .unwrap();
        drop(mic_tx);
        drop(ref_tx);
        bridge.await.unwrap();
        while out_rx.try_recv().is_ok() {}

        drop(flight); // joins the writer thread; the sidecar is complete
        let text = std::fs::read_to_string(dir.path().join("metrics.jsonl")).unwrap();
        let max_underrun = text
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["kind"] == "aec")
            .map(|v| v["underrun_frames"].as_u64().unwrap_or(0))
            .max();
        assert_eq!(
            max_underrun,
            Some(0),
            "reference delivery lag must not be booked as underrun"
        );
    }

    #[test]
    fn base_comes_from_stamps_and_residual_from_the_probe() {
        let mut a = ClockAlign::new();
        // Mic sample 1000 captured at t=2ms; reference sample 500 at t=1ms.
        // The reference sample at the mic's instant is 500 + 1ms*16 = 516,
        // so the base is 516 - 1000 = -484.
        a.set_base_from_pair(2_000_000, 1_000, 1_000_000, 500, 16_000);
        assert_eq!(a.base, -484);
        assert_eq!(a.offset(), -484);

        // The probe measures the ABSOLUTE pairing offset; applying it makes
        // offset() equal the measurement (residual = measured − base), so
        // the stamp base is never double-counted into the address.
        a.set_measured(120);
        assert_eq!(a.base, -484, "the probe must not move the base");
        assert_eq!(a.residual, 604);
        assert_eq!(a.offset(), 120);
    }

    #[test]
    fn an_unstamped_stream_leaves_the_base_alone() {
        let mut a = ClockAlign::new();
        a.set_base_from_pair(0, 1_000, 1_000_000, 500, 16_000);
        assert_eq!(a.base, 0);
        a.set_base_from_pair(2_000_000, 1_000, 0, 500, 16_000);
        assert_eq!(a.base, 0);
    }

    /// Far-end speech-ish tone plus a delayed, attenuated copy in the mic —
    /// enough structure for the bridge to run without needing a real session.
    fn synthetic_pair(secs: usize) -> (Vec<i16>, Vec<i16>) {
        let n = secs * 16_000;
        let sys: Vec<i16> = (0..n)
            .map(|i| {
                let t = i as f64 / 16_000.0;
                let env = if (i / 8_000) % 2 == 0 { 1.0 } else { 0.05 };
                ((t * 220.0 * std::f64::consts::TAU).sin() * 8000.0 * env) as i16
            })
            .collect();
        let delay = 320; // 20 ms speaker-to-mic path
        let mic: Vec<i16> = (0..n)
            .map(|i| {
                let echo = if i >= delay { sys[i - delay] / 4 } else { 0 };
                echo.saturating_add(((i % 97) as i16 - 48) * 4)
            })
            .collect();
        (mic, sys)
    }

    #[tokio::test]
    async fn aligned_streams_report_near_zero_clock_divergence() {
        let (mic, sys) = synthetic_pair(7);
        let _ = replay_bridge("aligned", &mic, &sys, true, 0, 0.0).await;
        let series = CLOCK_DIVERGENCE.with(|c| c.borrow().clone());
        assert!(!series.is_empty(), "no clock records emitted");
        // The harness stamps per 10 ms tick, so delivery order can put the
        // two streams one tick apart. Anything beyond that is real drift.
        let worst = series.iter().map(|v| v.abs()).max().unwrap();
        assert!(worst <= 160, "divergence {worst} samples on aligned streams");
    }

    #[test]
    fn stamp_advances_with_the_drained_position() {
        let t = 1_000_000_000_000u64;
        // Second drain iteration of a 320-sample delivery: +128 samples = +8 ms.
        assert_eq!(anchored_ns(t, 1_000, 1_128, 16_000), t + 8_000_000);
        // Leftover buffer: `at` BEHIND the anchor gets an earlier stamp.
        assert_eq!(anchored_ns(t, 1_000, 872, 16_000), t - 8_000_000);
        // Same position: unchanged. No stamp: stays zero.
        assert_eq!(anchored_ns(t, 1_000, 1_000, 16_000), t);
        assert_eq!(anchored_ns(0, 1_000, 1_128, 16_000), 0);
    }

    #[tokio::test]
    async fn multi_iteration_drains_keep_alignment_tight() {
        let (mic, sys) = synthetic_pair(7);
        // Production frame size: 320 samples per delivery — every recv drains
        // 2+ AEC FRAMEs, so a stamp that is not advanced per iteration would
        // shrink the base by the drained amount and re-read the same
        // reference span.
        let _ = replay_bridge_ticked("multi-drain", &mic, &sys, true, 0, 0.0, 320).await;
        let series = CLOCK_DIVERGENCE.with(|c| c.borrow().clone());
        assert!(!series.is_empty(), "no clock records emitted");
        let worst = series.iter().map(|v| v.abs()).max().unwrap();
        assert!(worst <= 320, "divergence {worst} samples with 320-sample deliveries");
        let base = CLOCK_BASE.with(|c| c.borrow().clone());
        // Aligned streams: the base must stay near zero, not walk downward
        // by FRAME per iteration.
        let deepest = base.iter().map(|v| v.abs()).max().unwrap();
        assert!(deepest <= 320, "base {deepest} drifted on aligned streams");
    }

    #[tokio::test]
    async fn a_late_reference_lands_in_the_base_not_the_divergence() {
        let (mic, sys) = synthetic_pair(7);
        // Reference starts 50 ticks (500 ms = 8000 samples) late. Both pairing
        // methods agree about that, so it is a base offset, not divergence.
        let _ = replay_bridge("late-ref", &mic, &sys, true, 50, 0.0).await;
        let base = CLOCK_BASE.with(|c| c.borrow().clone());
        let div = CLOCK_DIVERGENCE.with(|c| c.borrow().clone());
        assert!(!base.is_empty(), "no clock records emitted");
        let deepest = base.iter().map(|v| v.abs()).max().unwrap();
        assert!(deepest > 4_000, "late reference did not move the base ({deepest})");
        let worst_div = div.iter().map(|v| v.abs()).max().unwrap();
        assert!(worst_div <= 160, "stamp and sample pairing disagreed ({worst_div})");
    }
}

#[cfg(test)]
mod supervisor_tests {
    use super::{next_step, SupervisorStep};

    #[test]
    fn retry_ladder_covers_all_arms() {
        // Worked once, first failure → one restart of the same backend.
        assert_eq!(next_step(true, 0, 1), SupervisorStep::RestartSame);
        assert_eq!(next_step(true, 0, 0), SupervisorStep::RestartSame);
        // Restart already spent → next candidate if one exists.
        assert_eq!(next_step(true, 1, 1), SupervisorStep::TryNext);
        assert_eq!(next_step(true, 1, 0), SupervisorStep::RecordOnly);
        // Never produced an event → init is broken; no restart.
        assert_eq!(next_step(false, 0, 2), SupervisorStep::TryNext);
        assert_eq!(next_step(false, 0, 0), SupervisorStep::RecordOnly);
    }
}
