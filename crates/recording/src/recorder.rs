// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Recorder facade — orchestrates state machine + worker + session manifest + heartbeat.

use crate::error::{RecordingError, Result};
use crate::heartbeat::Heartbeat;
use system_sleep::SleepInhibitor;
use crate::live_pipeline::{LiveMode, LivePipeline, LivePipelineEvent};
use crate::manifest::{AecMode, ChunkManifest, RecordingSegment, SessionManifest};
use crate::session::{chunk_rel, Session};
use crate::state::StateMachine;
use crate::worker::CaptureWorker;
use audio_engine::capture::StreamingCaptureRequest;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HEARTBEAT_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// How long a chunk stays open before `maybe_rotate_chunk` rolls it.
const CHUNK_ROTATE_SECS: u64 = 300;
/// Wait before retrying a chunk rotation whose next chunk failed to open.
const ROTATE_RETRY_SECS: i64 = 60;

/// Background thread that periodically touches the heartbeat file.
struct HeartbeatKeeper {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl HeartbeatKeeper {
    fn spawn(heartbeat: Heartbeat, needs_aec: Arc<AtomicBool>, poll_routing: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);
        let join = thread::spawn(move || {
            while !stop_t.load(Ordering::Relaxed) {
                if let Err(e) = heartbeat.touch() {
                    log::warn!("heartbeat refresh failed: {e}");
                }
                // Sticky: once needs_aec is true, no more routing checks.
                if poll_routing && !needs_aec.load(Ordering::Relaxed) {
                    if let Ok(r) = audio_engine::routing::detect_routing() {
                        if r.needs_aec() {
                            log::info!(
                                "routing flipped to speaker-class ({}) — AEC will run at finalize",
                                r.default_sink_description
                            );
                            needs_aec.store(true, Ordering::Relaxed);
                        }
                    }
                }
                // Sleeps in short slices; shutdown is checked every 200 ms.
                let mut slept = Duration::ZERO;
                while slept < HEARTBEAT_REFRESH_INTERVAL && !stop_t.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(200));
                    slept += Duration::from_millis(200);
                }
            }
        });
        Self {
            stop,
            join: Some(join),
        }
    }

    fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for HeartbeatKeeper {
    fn drop(&mut self) {
        // If shutdown() wasn't called, signal the thread to exit on Drop too.
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// The Daisy-owned capture sink plus its routing bookkeeping, shared between
/// the supervisor's reroute/route closures and stop's restore.
struct SinkPlumbing {
    vs: audio_engine::virtual_sink::VirtualSink,
    /// Playback streams moved into the capture sink; restored on stop.
    moved: Vec<(u32, String)>,
}

/// User override for the AEC decision. `Auto` derives it from routing;
/// `Always`/`Never` pin it for the whole session (initial decision, the
/// sticky re-poll, and the live pipeline's engage signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AecOverride {
    #[default]
    Auto,
    Always,
    Never,
}

pub struct RecorderConfig {
    pub session_root: PathBuf,
    pub mic_source_id: u32,
    pub mic_source_node_name: String,
    /// Stable identity of the chosen mic ([`Source::key`]); the supervisor
    /// tracks the active mic by this.
    pub mic_source_key: audio_engine::topology::DeviceKey,
    pub mic_source_description: String,
    /// Daisy-owned capture sink (virtual-sink mode). The recorder owns its
    /// routing lifecycle: initial playback sweep at start, loopback repoint
    /// and stream routing on topology events, restore on stop. `None` when
    /// the caller captures an existing monitor/loopback source.
    pub virtual_sink: Option<audio_engine::virtual_sink::VirtualSink>,
    /// The process-wide topology hub; the recorder subscribes its
    /// supervisor for the recording's lifetime.
    pub topology_hub: Arc<audio_engine::topology::TopologyHub>,
    pub system_source_id: u32,
    pub system_source_node_name: String,
    pub system_source_description: String,
    pub sample_rate: u32,
    /// Human-readable session id stamped in the manifest.
    pub session_id: String,
    /// Which live transcription mode to use for this session.
    pub live_mode: LiveMode,
    /// The meeting's proper nouns (title, attendees, tag names/vocab) — the
    /// same terms that prime the decoder. Names among them are never
    /// lowercased when a caption fragment opens with one.
    pub live_terms: Vec<String>,
    /// Write the metrics.jsonl flight-recorder sidecar. Rides the debug
    /// logging setting — no toggle of its own.
    pub flight_recorder: bool,
    /// Build and machine facts for the telemetry.jsonl header.
    pub telemetry_host: crate::telemetry::Host,
    /// User AEC override: `Always`/`Never` pin the AEC decision; `Auto`
    /// derives it from routing.
    pub aec_override: AecOverride,
    /// Looks up a device's learned levels in one pass, for the capture
    /// domain the session is running in: `(live AGC guard floor, dead-air
    /// floor)`. Called at start and again on every mic switch, so the guard
    /// re-seeds and the watchdog re-arms for the new device. `None` = the
    /// built-in guard default and no watchdog.
    pub mic_levels_lookup:
        Option<Box<dyn Fn(&str, bool) -> (Option<f32>, Option<i16>) + Send + Sync>>,
    /// Write the live echo-cancelled mic per chunk
    /// (`chunks/NNNN/mic_live_aec.wav`) for live readers: set when the local
    /// end holds more than one speaker.
    pub live_aec_mic_wav: bool,
}

pub struct Recorder {
    session: Session,
    state: StateMachine,
    worker: Option<CaptureWorker>,
    heartbeat: Heartbeat,
    heartbeat_keeper: Option<HeartbeatKeeper>,
    _inhibitor: SleepInhibitor,
    needs_aec: Arc<AtomicBool>,
    live_paused: Arc<AtomicBool>,
    open_chunk_started_unix: Option<i64>,
    /// No rotation is attempted before this time; set after a failed one.
    rotate_not_before_unix: i64,
    /// Capture timing for the open chunk, filled by the capture callbacks and
    /// written into the manifest when the chunk closes.
    mic_clock: Arc<audio_engine::capture::ChunkClock>,
    sys_clock: Arc<audio_engine::capture::ChunkClock>,
    live_pipeline: Option<LivePipeline>,
    /// Receiver for events from the live pipeline; taken by the caller to
    /// forward events to the frontend.
    live_events_rx: Option<tokio::sync::mpsc::Receiver<LivePipelineEvent>>,
    /// Retained tee senders; pause() detaches the realtime transcriber feed
    /// and resume() reattaches it.
    mic_tee_tx: Option<tokio::sync::mpsc::UnboundedSender<audio_engine::capture::TeeFrame>>,
    sys_tee_tx: Option<tokio::sync::mpsc::UnboundedSender<audio_engine::capture::TeeFrame>>,
    /// Tells the auto-gain tap to re-baseline to a new mic after a mid-call
    /// switch (restore the old device's gain, read the new device's).
    mic_switch_tx: tokio::sync::mpsc::UnboundedSender<(u32, audio_engine::topology::DeviceKey)>,
    /// The active mic's id + key + previous key, updated together on
    /// switch_mic and supervisor failover.
    mic: Arc<StdMutex<crate::supervisor::MicIdentity>>,
    /// Live AGC guard floor (f32 bits), shared with the live pipeline and
    /// re-seeded on mic switch.
    speech_env_min: Arc<std::sync::atomic::AtomicU32>,
    /// Mic-track health counters (from the live pipeline); switch_mic
    /// re-arms the dead-air floor through them.
    mic_health: Arc<crate::health::TrackHealth>,
    /// System-track health counters (from the live pipeline).
    system_health: Arc<crate::health::TrackHealth>,
    /// Epoch shared with the health monitor's tick clock.
    health_epoch: std::time::Instant,
    /// Session metrics sidecar (metrics.jsonl).
    flight: Arc<crate::flight_recorder::FlightRecorder>,
    /// Session telemetry (telemetry.jsonl).
    telemetry: Arc<crate::telemetry::Telemetry>,
    /// Writes process samples into `telemetry` until dropped.
    proc_sampler: Option<crate::proc_stats::ProcSamplerThread>,
    /// Keep-alive for the per-OS topology watcher.
    /// Hub keep-alive first, then the subscription.
    _topology_hub: Arc<audio_engine::topology::TopologyHub>,
    _topology_sub: audio_engine::topology::HubSubscription,
    supervisor: Option<crate::supervisor::SupervisorHandle>,
    sink: Option<Arc<StdMutex<SinkPlumbing>>>,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Recorder {
    /// Create a new session at `config.session_root`, spawn the worker, open
    /// the first chunk, and transition to Recording.
    pub fn start(config: RecorderConfig) -> Result<Self> {
        // Initial AEC decision. A user override pins it; otherwise a routing
        // snapshot drives the sticky flag from t=0. AEC is skipped when the
        // output is headphone-class or the mic is a headset/Bluetooth device.
        let aec_forced = config.aec_override != AecOverride::Auto;
        let initial_needs_aec = match config.aec_override {
            AecOverride::Always => true,
            AecOverride::Never => false,
            AecOverride::Auto => {
                let output_needs_aec = audio_engine::routing::detect_routing()
                    .map(|r| r.needs_aec())
                    .unwrap_or(true);
                output_needs_aec
                    && !audio_engine::routing::mic_is_headphone(config.mic_source_id)
            }
        };

        let created_at_unix_seconds = now_unix();
        let manifest = SessionManifest {
            recording_kind: None,
            origin_device: None,
            extra: Default::default(),
            schema_version: SessionManifest::SCHEMA,
            session_id: config.session_id.clone(),
            created_at_unix_seconds,
            sample_rate: config.sample_rate,
            channels: 1,
            mic_source_id: config.mic_source_id,
            mic_source_node_name: config.mic_source_node_name.clone(),
            mic_source_description: config.mic_source_description.clone(),
            system_source_id: config.system_source_id,
            system_source_node_name: config.system_source_node_name.clone(),
            system_source_description: config.system_source_description.clone(),
            // Provisional value; finalized at stop based on observed routing.
            aec_mode: if initial_needs_aec { AecMode::Always } else { AecMode::Disabled },
            chunks: vec![],
            finalized_at_unix_seconds: None,
            title: None,
            meeting_id: uuid::Uuid::new_v4().to_string(),
            tag_ids: Vec::new(),
            notes_md_relative: None,
            attendees: Vec::new(),
            calendar: None,
            recording_segments: vec![RecordingSegment {
                reason: None,
                extra: Default::default(),
                started_at_unix_seconds: created_at_unix_seconds,
                stopped_at_unix_seconds: None,
                first_chunk_index: 1,
                last_chunk_index: None,
            }],
        speaker_map: vec![],
        language: None,
            diarization_unavailable: false,
            single_local_speaker: true,
            expected_speakers: None,
            sent_integration_ids: vec![],
            cluster_sides: vec![],
            mixed_clusters: vec![], cluster_sample_spans: vec![], sample_spans_on_audio_timeline: false, speaker_separation: None, diarize_count: None, diarization_id: None,
            interrupted: false,
            denoise_applied: None,
            dismissed_suggested_tag_ids: vec![],
        };
        let session_start_wall_ms = crate::telemetry::wall_ms();
        let session = Session::create(&config.session_root, manifest)?;
        let heartbeat = Heartbeat::create(&config.session_root.join("heartbeat"))?;
        let telemetry = Arc::new(crate::telemetry::Telemetry::open(session.root()));
        let (live_mode, engine, fallbacks) = match &config.live_mode {
            LiveMode::Realtime { client, fallbacks } => (
                "realtime",
                Some(crate::telemetry::EngineId::of(client.as_ref())),
                fallbacks.iter().map(|f| crate::telemetry::EngineId::of(f.as_ref())).collect(),
            ),
            LiveMode::Off => ("off", None, Vec::new()),
        };
        telemetry.header(
            &config.telemetry_host,
            &crate::telemetry::Header {
                session_id: config.session_id.clone(),
                session_start_wall_ms,
                live_mode: live_mode.into(),
                engine,
                fallbacks,
                mic_device: config.mic_source_description.clone(),
                system_device: config.system_source_description.clone(),
                sample_rate: config.sample_rate,
            },
        );
        let proc_sampler = crate::proc_stats::ProcSamplerThread::spawn(
            Arc::clone(&telemetry),
            std::time::Duration::from_secs(5),
        );

        let needs_aec = Arc::new(AtomicBool::new(initial_needs_aec));
        // Mirrors the recorder's paused state for the live pipeline.
        let live_paused = Arc::new(AtomicBool::new(false));
        // Live AEC engagement: bidirectional with the default output class.
        let aec_live = Arc::new(AtomicBool::new(initial_needs_aec));
        let heartbeat_keeper =
            HeartbeatKeeper::spawn(heartbeat.clone(), Arc::clone(&needs_aec), !aec_forced);
        let inhibitor = SleepInhibitor::try_acquire("Daisy is recording");

        let mic_clock = Arc::new(audio_engine::capture::ChunkClock::default());
        let sys_clock = Arc::new(audio_engine::capture::ChunkClock::default());
        // One event channel feeds the supervisor: capture stream deaths,
        // watcher notifications, and health transitions.
        let (topology_tx, topology_rx) = std::sync::mpsc::channel::<audio_engine::topology::TopologyEvent>();
        let req = StreamingCaptureRequest {
            mic_source_id: config.mic_source_id,
            system_source_id: config.system_source_id,
            sample_rate: config.sample_rate,
            mic_clock: Arc::clone(&mic_clock),
            system_clock: Arc::clone(&sys_clock),
            topology_tx: Some(topology_tx.clone()),
            // On Windows, Auto lets the OS voice-process the mic (software
            // AEC then skips per mic_voice_processed) while Always/Never keep
            // the stream raw.
            allow_os_voice_processing: config.aec_override == AecOverride::Auto,
        };
        let worker = CaptureWorker::spawn(req)?;

        // Set up audio tees + live pipeline. The live pipeline creates the
        // transcript writer itself, and only in Realtime mode — LiveMode::Off
        // leaves no live_transcript.jsonl on disk.
        let live_transcript_path = config.session_root.join("live_transcript.jsonl");

        // Unbounded channel; see audio_engine::capture::TeeSender.
        let (mic_tee_tx, mic_tee_rx) =
            tokio::sync::mpsc::unbounded_channel::<audio_engine::capture::TeeFrame>();
        let (sys_tee_tx, sys_tee_rx) =
            tokio::sync::mpsc::unbounded_channel::<audio_engine::capture::TeeFrame>();
        let (events_tx, events_rx) = tokio::sync::mpsc::channel::<LivePipelineEvent>(256);

        // Retained clones let pause()/resume() detach + reattach the tees.
        // The mpsc channels persist (the live pipeline owns the rx ends);
        // toggling the sender stops/starts audio reaching the realtime
        // transcriber without tearing the pipeline down.
        let mic_tee_tx_keep = mic_tee_tx.clone();
        let sys_tee_tx_keep = sys_tee_tx.clone();

        worker.set_mic_tee(Some(mic_tee_tx))?;
        worker.set_system_tee(Some(sys_tee_tx))?;

        let (mic_switch_tx, mic_switch_rx) = tokio::sync::mpsc::unbounded_channel::<(
            u32,
            audio_engine::topology::DeviceKey,
        )>();
        let epoch = std::time::Instant::now();
        let self_writes = Arc::new(StdMutex::new(crate::supervisor::SelfWrites::default()));
        let snapshot = Arc::new(std::sync::RwLock::new(
            audio_engine::topology::Snapshot::default(),
        ));
        let start_mic_key = config.mic_source_key.clone();
        let mic_identity = Arc::new(StdMutex::new(crate::supervisor::MicIdentity {
            id: config.mic_source_id,
            key: start_mic_key.clone(),
            prev_key: None,
        }));
        // Reads the learned-levels seed once capture is up.
        let voice_processed = worker.mic_voice_processed().load(Ordering::Relaxed);
        if voice_processed {
            log::info!(
                "mic capture is OS voice-processed: echo cancellation happens in the capture path and the software AEC stage never runs (override={:?})",
                config.aec_override
            );
        }
        let (seed_env_min, dead_air_floor) = match config.mic_levels_lookup.as_ref() {
            Some(f) => f(&config.mic_source_description, voice_processed),
            None => (None, None),
        };
        let speech_env_min = Arc::new(std::sync::atomic::AtomicU32::new(
            crate::live_pipeline::speech_env_min_bits(seed_env_min),
        ));
        let flight = Arc::new(crate::flight_recorder::FlightRecorder::open_if(
            config.flight_recorder,
            session.root(),
        ));
        flight.session(
            &config.mic_source_description,
            config.mic_source_id,
            &config.system_source_description,
            config.sample_rate,
            seed_env_min,
            voice_processed,
        );
        // Notice drain shared by the supervisor and the live pipeline.
        let (notices_tx, notices_rx) = std::sync::mpsc::channel::<crate::supervisor::Notice>();
        let live_pipeline = LivePipeline::start(crate::live_pipeline::LiveStartConfig {
            mode: config.live_mode,
            live_terms: config.live_terms,
            sample_rate: config.sample_rate,
            mic_audio_rx: mic_tee_rx,
            system_audio_rx: sys_tee_rx,
            transcript_path: live_transcript_path.clone(),
            events_tx,
            mic_source_id: config.mic_source_id,
            speech_env_min: Arc::clone(&speech_env_min),
            mic_switch_rx,
            needs_aec: Arc::clone(&aec_live),
            mic_voice_processed: Arc::clone(worker.mic_voice_processed()),
            paused: Arc::clone(&live_paused),
            flight: Arc::clone(&flight),
            topology_tx: topology_tx.clone(),
            mic_key: start_mic_key.clone(),
            snapshot: Arc::clone(&snapshot),
            self_writes: Arc::clone(&self_writes),
            epoch,
            dead_air_floor,
            notices_tx: notices_tx.clone(),
            telemetry: Arc::clone(&telemetry),
            live_aec_root: config.live_aec_mic_wav.then(|| session.root().to_path_buf()),
        })
        .map_err(|e| RecordingError::Io { path: live_transcript_path, source: e })?;
        let mic_health = Arc::clone(&live_pipeline.mic_health);
        let system_health = Arc::clone(&live_pipeline.system_health);

        let topology_hub = Arc::clone(&config.topology_hub);
        let topology_sub = topology_hub.subscribe(topology_tx);
        // Daisy-owned sink: sweep already-playing streams in at start; the
        // supervisor's reroute/route closures keep it current mid-call.
        let sink = config.virtual_sink.map(|vs| {
            let moved = vs.route_playing_streams().unwrap_or_default();
            Arc::new(StdMutex::new(SinkPlumbing { vs, moved }))
        });
        let supervisor = crate::supervisor::Supervisor::start(crate::supervisor::SupervisorDeps {
            events_rx: topology_rx,
            snapshot: Arc::clone(&snapshot),
            mic: Arc::clone(&mic_identity),
            paused: Arc::clone(&live_paused),
            switch: {
                let handle = worker.streaming_handle();
                let switch_tx = mic_switch_tx.clone();
                let env_min = Arc::clone(&speech_env_min);
                let flight_sw = Arc::clone(&flight);
                let lookup = config.mic_levels_lookup;
                let mic_health_sw = Arc::clone(&mic_health);
                let mic_id_sw = Arc::clone(&mic_identity);
                let epoch_sw = epoch;
                Arc::new(move |s: &audio_engine::source::Source| {
                    let same_device = mic_id_sw.lock().unwrap().key == s.key;
                    if handle.switch_mic(s.clone()).is_err() {
                        return false;
                    }
                    let _ = switch_tx.send((s.id, s.key.clone()));
                    // Re-seed the live AGC guard floor for the new device and
                    // stamp the flight marker, same as the user switch path.
                    // One store read yields both levels.
                    let voice_processed =
                        handle.mic_voice_processed().load(Ordering::Relaxed);
                    let (floor, da) = match lookup.as_ref() {
                        Some(f) => f(&s.description, voice_processed),
                        None => (None, None),
                    };
                    env_min.store(
                        crate::live_pipeline::speech_env_min_bits(floor),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    // A device without residue history disarms the dead-air
                    // watchdog (floor 0). A same-device recovery rebuild
                    // keeps the starvation window.
                    let da = da.unwrap_or(0);
                    if same_device {
                        mic_health_sw.refresh_dead_floor(da);
                    } else {
                        mic_health_sw
                            .set_dead_floor(da, epoch_sw.elapsed().as_millis() as u64);
                    }
                    flight_sw.mic_switch(s.id, floor, voice_processed);
                    true
                })
            },
            reopen_system: {
                let handle = worker.streaming_handle();
                Arc::new(move || handle.reopen_system().is_ok())
            },
            reroute: {
                let sink = sink.clone();
                Arc::new(move || {
                    let Some(sink) = &sink else { return };
                    let sp = &mut *sink.lock().unwrap();
                    if let Err(e) = sp.vs.repoint_loopback() {
                        log::warn!("output reroute: loopback repoint failed: {e}");
                    }
                    match sp.vs.route_playing_streams() {
                        Ok(mut moved) => sp.moved.append(&mut moved),
                        Err(e) => log::warn!("output reroute: resweep failed: {e}"),
                    }
                })
            },
            route_stream: {
                let sink = sink.clone();
                Arc::new(move |stream_id| {
                    let Some(sink) = &sink else { return };
                    let sp = &mut *sink.lock().unwrap();
                    match sp.vs.route_stream(stream_id) {
                        Ok(Some(orig)) => sp.moved.push((stream_id, orig)),
                        Ok(None) => {}
                        Err(e) => log::warn!("route new stream {stream_id}: {e}"),
                    }
                })
            },
            enumerate: Arc::new(|| audio_engine::source::list_sources().unwrap_or_default()),
            default_input: Arc::new(audio_engine::topology::default_input_key),
            aec_live: Arc::clone(&aec_live),
            aec_forced,
            needs_aec_sticky: Arc::clone(&needs_aec),
            mic_voice_processed: Arc::clone(worker.streaming_handle().mic_voice_processed()),
            system_shape: if audio_engine::topology::system_capture_reopens_on_default_change() {
                crate::supervisor::SystemShape::ReopenOnDefaultChange
            } else {
                crate::supervisor::SystemShape::Passive
            },
            mic_silence: if audio_engine::topology::mic_silence_waits_for_release() {
                crate::supervisor::MicSilence::WaitForRelease
            } else {
                crate::supervisor::MicSilence::Rebuild
            },
            self_writes: Arc::clone(&self_writes),
            gain_epoch: epoch,
            rebaseline: {
                let tx = mic_switch_tx.clone();
                let mic = Arc::clone(&mic_identity);
                Arc::new(move || {
                    let (id, key) = {
                        let m = mic.lock().unwrap();
                        (m.id, m.key.clone())
                    };
                    let _ = tx.send((id, key));
                })
            },
        }, notices_tx, notices_rx);

        let mut sm = StateMachine::new();
        sm.start()?;

        let mut me = Self {
            session,
            state: sm,
            worker: Some(worker),
            heartbeat,
            heartbeat_keeper: Some(heartbeat_keeper),
            _inhibitor: inhibitor,
            needs_aec,
            live_paused,
            open_chunk_started_unix: None,
            rotate_not_before_unix: 0,
            mic_clock,
            sys_clock,
            live_pipeline: Some(live_pipeline),
            live_events_rx: Some(events_rx),
            mic_tee_tx: Some(mic_tee_tx_keep),
            sys_tee_tx: Some(sys_tee_tx_keep),
            mic_switch_tx,
            mic: mic_identity,
            speech_env_min,
            mic_health,
            system_health,
            health_epoch: epoch,
            flight,
            telemetry,
            proc_sampler: Some(proc_sampler),
            _topology_hub: topology_hub,
            _topology_sub: topology_sub,
            supervisor: Some(supervisor),
            sink,
        };
        if let Err(e) = me.open_new_chunk() {
            // A start whose first chunk can't open fails whole: capture,
            // the supervisor and playback routing are released here, and
            // the caller removes the session directory.
            if let Err(t) = me.discard() {
                log::warn!("recording: teardown after a failed first chunk: {t}");
            }
            return Err(e);
        }
        Ok(me)
    }

    /// Mute/unmute the local mic by driving its OS input gain to 0 (mute) or
    /// back to unity (unmute). Records near-silence on the mic track while
    /// muted. Auto-gain only ever lowers on clipping and does not counteract
    /// the mute.
    pub fn set_mic_muted(&self, muted: bool) -> bool {
        // OS input gain: the actual mute on Linux (PipeWire honors it) and
        // Windows (WASAPI honors it); a no-op against the macOS capture tap
        // and unreliable on Bluetooth.
        let mic_id = self.mic.lock().unwrap().id;
        let _ = audio_engine::gain::set_input_gain(mic_id, if muted { 0.0 } else { 1.0 });
        // Callback-level mute: zeros mic frames before the wav and the live
        // tee. Implemented on macOS only (where the gain path above does
        // nothing); the Linux/Windows dispatch arms are no-ops.
        if let Some(w) = self.worker.as_ref() {
            let _ = w.set_mic_muted(muted);
        }
        true
    }

    /// True while the OS voice-processes this session's mic capture; a
    /// caller reading learned levels must ask for that domain.
    pub fn mic_voice_processed(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|w| w.mic_voice_processed().load(Ordering::Relaxed))
    }

    /// Switch the recording mic to a different source mid-session (e.g.
    /// plugging in AirPods). Fail-safe in the capture layer: if the new device
    /// won't start, recording continues on the current mic. Also re-baselines
    /// auto-gain to the new device. No-op if it's already the active mic.
    /// `speech_env_min` is the new device's learned AGC guard floor (None =
    /// built-in default); the live pipeline picks it up on the next frame.
    pub fn switch_mic(
        &mut self,
        source: audio_engine::source::Source,
        speech_env_min: Option<f32>,
        dead_air_floor: Option<i16>,
    ) -> Result<()> {
        if source.id == self.mic.lock().unwrap().id {
            return Ok(());
        }
        let new_id = source.id;
        let new_key = source.key.clone();
        if let Some(w) = self.worker.as_ref() {
            w.switch_mic(source)?;
        }
        // Re-baseline auto-gain to the new device (best-effort).
        let _ = self.mic_switch_tx.send((new_id, new_key.clone()));
        self.speech_env_min.store(
            crate::live_pipeline::speech_env_min_bits(speech_env_min),
            std::sync::atomic::Ordering::Relaxed,
        );
        // Re-arm the dead-air watchdog for the new device (0 disarms).
        self.mic_health.set_dead_floor(
            dead_air_floor.unwrap_or(0),
            self.health_epoch.elapsed().as_millis() as u64,
        );
        {
            let mut mic = self.mic.lock().unwrap();
            if mic.key != new_key {
                mic.prev_key = Some(mic.key.clone());
            }
            mic.id = new_id;
            mic.key = new_key;
        }
        self.flight
            .mic_switch(new_id, speech_env_min, self.mic_voice_processed());
        Ok(())
    }

    /// Drain pending supervisor notices (non-blocking).
    pub fn poll_notices(&mut self) -> Vec<crate::supervisor::Notice> {
        let mut out = Vec::new();
        if let Some(sup) = self.supervisor.as_ref() {
            while let Ok(n) = sup.notices.try_recv() {
                out.push(n);
            }
        }
        out
    }

    /// Whether the system (remote) track carried voice-level audio since the
    /// previous call.
    pub fn take_system_activity(&self) -> bool {
        self.system_health.take_activity()
    }

    pub fn state(&self) -> crate::State {
        self.state.state()
    }

    pub fn session_root(&self) -> &Path {
        self.session.root()
    }

    /// Take the live event receiver. Used by the Tauri command at start
    /// time to wire up frontend event forwarding. Returns None if already
    /// taken or if the pipeline mode was Off.
    pub fn take_live_events_rx(&mut self) -> Option<tokio::sync::mpsc::Receiver<LivePipelineEvent>> {
        self.live_events_rx.take()
    }

    pub fn pause(&mut self) -> Result<()> {
        self.state.pause()?;
        self.live_paused.store(true, Ordering::Relaxed);
        self.close_open_chunk()?;
        // Detach the live tees; the realtime transcriber receives no audio
        // while paused.
        if let Some(worker) = self.worker.as_ref() {
            let _ = worker.set_mic_tee(None);
            let _ = worker.set_system_tee(None);
        }
        self.heartbeat.touch()?;
        Ok(())
    }

    pub fn resume(&mut self) -> Result<()> {
        self.state.resume()?;
        self.live_paused.store(false, Ordering::Relaxed);
        // Tees attach before the chunk opens.
        if let Some(worker) = self.worker.as_ref() {
            worker.set_mic_tee(self.mic_tee_tx.clone())?;
            worker.set_system_tee(self.sys_tee_tx.clone())?;
        }
        self.open_new_chunk()?;
        self.heartbeat.touch()?;
        Ok(())
    }

    /// Rotate the active chunk once it has been open for five minutes.
    /// Returns `Ok(true)` when a rotation actually happened, `Ok(false)`
    /// otherwise (not recording, no open chunk, or chunk still young).
    ///
    /// Callers tick this; chunk length is bounded by how often they do.
    ///
    /// The CaptureWorker's `OpenChunk` handler closes the previous writer
    /// and opens the new one in a single control-message handler.
    pub fn maybe_rotate_chunk(&mut self) -> Result<bool> {
        if !matches!(self.state.state(), crate::State::Recording) {
            return Ok(false);
        }
        let Some(started) = self.open_chunk_started_unix else {
            return Ok(false);
        };
        let age = (now_unix() - started).max(0) as u64;
        if age < CHUNK_ROTATE_SECS || now_unix() < self.rotate_not_before_unix {
            return Ok(false);
        }
        // Stamp the prev chunk's end + duration before rolling.
        self.stamp_open_chunk_end()?;
        self.open_chunk_started_unix = None;
        // Atomic open replaces the writer without a prior close_chunk; no
        // frames are dropped between the close and the next open.
        if let Err(e) = self.open_new_chunk() {
            // Capture keeps writing the current chunk when the next one
            // can't open: it stays the open chunk, with no end stamp (a
            // crash before it closes is then closed from its audio), and
            // rotation retries later.
            self.open_chunk_started_unix = Some(started);
            self.rotate_not_before_unix = now_unix() + ROTATE_RETRY_SECS;
            if let Err(u) = self.session.update_manifest(|m| {
                if let Some(last) = m.chunks.last_mut() {
                    last.ended_at_unix_seconds = None;
                    last.duration_seconds = None;
                }
            }) {
                log::warn!("chunk rotation: clearing the open chunk's end stamp: {u}");
            }
            return Err(e);
        }
        log::info!("chunk rotated after {age}s (interval {CHUNK_ROTATE_SECS}s)");
        Ok(true)
    }

    /// Stops capture and closes the session for finalization: the live
    /// pipeline drains its last words and the manifest closes the segment.
    pub fn stop(self) -> Result<PathBuf> {
        self.stop_with(true)
    }

    /// Stops capture for a recording that is about to be deleted: the live
    /// pipeline is abandoned without draining and the manifest is left as
    /// is. Devices, the supervisor and playback routing are released the
    /// same way as [`Self::stop`].
    pub fn discard(self) -> Result<PathBuf> {
        self.stop_with(false)
    }

    fn stop_with(mut self, keep: bool) -> Result<PathBuf> {
        // The fallible teardown runs first; the supervisor stop and playback-stream
        // restore below run whatever its result.
        let teardown: Result<()> = (|| {
            self.state.stop()?;
            self.close_open_chunk()?;
            // The keeper stops before the heartbeat is deleted.
            if let Some(k) = self.heartbeat_keeper.take() {
                k.shutdown();
            }
            if let Some(worker) = self.worker.take() {
                // Tees detach first: the senders drop before shutdown,
                // signaling end-of-stream to the pipeline tasks.
                let _ = worker.set_mic_tee(None);
                let _ = worker.set_system_tee(None);
                // Drop the recorder's own sender clones.
                drop(self.mic_tee_tx.take());
                drop(self.sys_tee_tx.take());
                worker.shutdown()?;
            }
            // Tear down the live pipeline (blocks until tasks finish, ≤10s)
            // when its words are kept; a discard walks away from it.
            if let Some(pipeline) = self.live_pipeline.take() {
                if keep {
                    pipeline.shutdown();
                } else {
                    pipeline.abandon();
                }
            }
            Ok(())
        })();

        // The supervisor stops before the sink restore; then playback streams go
        // back where they came from and the sink's modules unload (VirtualSink::drop).
        if let Some(mut sup) = self.supervisor.take() {
            sup.stop();
        }
        if let Some(sink) = self.sink.take() {
            let sp = sink.lock().unwrap();
            if !sp.moved.is_empty() {
                audio_engine::virtual_sink::VirtualSink::restore_routing(&sp.moved);
            }
        }
        drop(self.proc_sampler.take());
        self.telemetry.end();
        teardown?;

        let root = self.session.root().to_path_buf();
        if !keep {
            let _ = syncsafe::remove_file(&root.join("heartbeat"));
            return Ok(root);
        }

        // Finalize the AEC mode based on the routing observed during
        // recording. AEC itself does not run here; the caller (or `daisy
        // session-finalize`) runs it out-of-process. The session stays "not
        // yet finalized" until finalized_at_unix_seconds is set.
        let needs = self.needs_aec.load(Ordering::Relaxed);
        let stopped_at = now_unix();
        self.session.update_manifest(|m| {
            m.aec_mode = resolve_aec_mode(&m.chunks, needs);
            // Close the open recording segment; resume appends a fresh one
            // with a continuing chunk index.
            if let Some(last_chunk_index) = m.chunks.last().map(|c| c.index) {
                crate::manifest_ops::close_active_segment(m, stopped_at, last_chunk_index);
            }
            // finalized_at_unix_seconds is not set here; the post-stop AEC
            // pass (or its skip path) sets that flag.
        })?;
        // Remove heartbeat — session is no longer live.
        let hb_path = root.join("heartbeat");
        let _ = syncsafe::remove_file(&hb_path);
        Ok(root)
    }

    fn open_new_chunk(&mut self) -> Result<()> {
        let (idx, dir) = self.session.allocate_chunk_dir()?;
        let mic_wav = dir.join("mic.wav");
        let system_wav = dir.join("system.wav");
        let started = now_unix();
        let worker = self.worker.as_ref().ok_or(RecordingError::WorkerGone)?;
        worker.open_chunk(idx, &mic_wav, &system_wav)?;
        // Stamped per chunk: a mid-session failover onto a device the OS
        // voice processor refuses changes this for chunks opened after it.
        let mic_voice_processed = worker.mic_voice_processed().load(Ordering::Relaxed);
        self.open_chunk_started_unix = Some(started);
        self.session.update_manifest(|m| {
            m.chunks.push(ChunkManifest {
                extra: Default::default(),
                index: idx,
                started_at_unix_seconds: started,
                ended_at_unix_seconds: None,
                duration_seconds: None,
                mic_wav_relative: chunk_rel(idx, "mic.wav"),
                system_wav_relative: chunk_rel(idx, "system.wav"),
                mic_aec_wav_relative: None,
                mic_dn_wav_relative: None,
                mic_host_ns: None,
                system_host_ns: None,
                mic_samples: None,
                system_samples: None,
                mic_voice_processed,
                mic_zero_samples: None,
                mic_longest_zero_run: None,
                audio_samples: None,
            });
        })?;
        Ok(())
    }

    fn close_open_chunk(&mut self) -> Result<()> {
        if let Some(worker) = self.worker.as_ref() {
            worker.close_chunk()?;
        }
        if self.open_chunk_started_unix.take().is_some() {
            self.stamp_open_chunk_end()?;
        }
        Ok(())
    }

    /// Audio recorded so far, in milliseconds: every chunk's audio back to
    /// back, the open one as far as its WAVs reach on disk. Paused spans and
    /// capture gaps (a sleeping machine, a stalled process) add nothing.
    pub fn recorded_ms(&self) -> u64 {
        let m = self.session.manifest();
        let rate = u64::from(m.sample_rate.max(1));
        let open = self.open_chunk_started_unix.is_some();
        let last = m.chunks.len().saturating_sub(1);
        m.chunks
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if open && i == last {
                    self.open_chunk_samples(c) * 1000 / rate
                } else {
                    c.audio_ms(m.sample_rate).unwrap_or(0)
                }
            })
            .sum()
    }

    /// Samples the open chunk holds on disk; none before its WAVs exist.
    fn open_chunk_samples(&self, c: &ChunkManifest) -> u64 {
        crate::manifest_ops::chunk_wav_samples(self.session.root(), c).unwrap_or(0)
    }

    /// Stamp the open chunk's end, its audio length, and the capture-clock
    /// totals into the manifest's last chunk entry. The length is the audio
    /// on disk.
    fn stamp_open_chunk_end(&mut self) -> Result<()> {
        let ended = now_unix();
        let (mic_ns, mic_n) = self.mic_clock.read();
        let (sys_ns, sys_n) = self.sys_clock.read();
        let audio = self
            .session
            .manifest()
            .chunks
            .last()
            .and_then(|c| crate::manifest_ops::chunk_wav_samples(self.session.root(), c))
            .unwrap_or(mic_n.max(sys_n));
        let dur = crate::manifest_ops::whole_seconds_of(audio, self.session.manifest().sample_rate);
        let (mic_zeros, mic_zero_run) = self.mic_clock.zeros();
        if mic_n > 0 && mic_zeros * 10 > mic_n * 9 {
            log::warn!(
                "mic capture was {:.0}% zero samples this chunk (longest run {} samples of {mic_n}) — dead device, or an OS voice processor gating a silent mic",
                mic_zeros as f64 * 100.0 / mic_n as f64,
                mic_zero_run,
            );
        }
        self.session.update_manifest(|m| {
            if let Some(last) = m.chunks.last_mut() {
                last.ended_at_unix_seconds = Some(ended);
                last.duration_seconds = Some(dur);
                last.audio_samples = Some(audio);
                last.mic_host_ns = mic_ns;
                last.system_host_ns = sys_ns;
                last.mic_samples = Some(mic_n);
                last.system_samples = Some(sys_n);
                last.mic_zero_samples = Some(mic_zeros);
                last.mic_longest_zero_run = Some(mic_zero_run);
            }
        })
    }
}

fn run_aec_finalize(session: &mut Session) -> Result<()> {
    use rayon::prelude::*;

    let sample_rate = session.manifest().sample_rate;
    let chunks = session.manifest().chunks.clone();
    let root = session.root().to_path_buf();

    // Chunks are independent (each gets a fresh canceller) and fan out across
    // the finalize pool; within a chunk the canceller runs sequentially.
    let pool = transcript::cpu::finalize_pool();
    let updates: Vec<(usize, PathBuf)> = pool.install(|| {
        chunks
            .par_iter()
            .enumerate()
            .filter_map(|(i, chunk)| {
                if chunk.ended_at_unix_seconds.is_none() {
                    return None; // chunk never closed
                }
                if chunk.mic_voice_processed {
                    // The OS cancelled echo during capture.
                    return None;
                }
                // Idempotent: skip chunks whose mic_aec.wav already exists.
                if let Some(rel) = &chunk.mic_aec_wav_relative {
                    if syncsafe::join_rel(&root, rel).is_file() {
                        return None;
                    }
                }
                match process_chunk_aec(i, chunk, &root, sample_rate) {
                    Ok(Some(x)) => Some(Ok(x)),
                    Ok(None) => None, // silence-skipped: orchestrator falls back to raw mic.wav
                    Err(e) => Some(Err(e)),
                }
            })
            .collect::<Result<Vec<_>>>()
    })?;

    session.update_manifest(|m| {
        for (i, rel) in updates {
            if let Some(c) = m.chunks.get_mut(i) {
                c.mic_aec_wav_relative = Some(rel);
            }
        }
    })?;
    Ok(())
}

/// Echo-cancel one chunk: read its mic + system WAVs, run the canceller
/// frame by frame (its adaptive state chains within the chunk) where a
/// far-end reference exists, and write `chunks/NNNN/mic_aec.wav` at the mic
/// track's full length. Returns `Some((chunk_index, rel_path))` when
/// AEC ran, `None` when the system track is effectively silent and AEC was
/// skipped (the orchestrator falls back to raw mic.wav for that chunk).
/// Safe to call from multiple threads: each call owns its own canceller and
/// touches only its own chunk's files.
fn process_chunk_aec(
    i: usize,
    chunk: &crate::manifest::ChunkManifest,
    root: &Path,
    sample_rate: u32,
) -> Result<Option<(usize, PathBuf)>> {
    const FRAME_SIZE: usize = aec::AcousticEchoCanceller::FRAME_SIZE;

    let (mic, _) = transcript::wav_io::read_wav_mono_i16(&syncsafe::join_rel(root, &chunk.mic_wav_relative))?;
    let (sys_raw, _) = transcript::wav_io::read_wav_mono_i16(&syncsafe::join_rel(root, &chunk.system_wav_relative))?;

    // Silence-skip: AEC is skipped when the system track is effectively silent.
    const PEAK_SILENT_I16: i16 = 58;
    let sys_peak: i16 = sys_raw
        .iter()
        .map(|&s| s.saturating_abs())
        .max()
        .unwrap_or(0);
    if sys_peak < PEAK_SILENT_I16 {
        log::info!(
            "chunk {} system silent (peak={}) — skipping AEC, orchestrator will use raw mic.wav",
            chunk.index,
            sys_peak
        );
        return Ok(None);
    }

    // Stamp-seeded alignment: the frame loop pairs by index, so the far-end
    // reference is pre-shifted by the chunk's structural mic/system offset
    // (manifest capture stamps + file lengths). Offset 0 — aligned tracks or
    // a pre-v3 manifest — leaves the reference untouched.
    let offset = transcript::echo_direction::stamp_align_offset(
        mic.len(),
        sys_raw.len(),
        chunk.mic_host_ns,
        chunk.system_host_ns,
        chunk.mic_samples,
        chunk.system_samples,
        sample_rate,
    );
    let sys_shifted: Vec<i16>;
    let sys: &[i16] = match offset {
        0 => &sys_raw,
        o if o > 0 => &sys_raw[(o as usize).min(sys_raw.len())..],
        o => {
            let mut v = vec![0i16; (-o) as usize];
            v.extend_from_slice(&sys_raw);
            sys_shifted = v;
            &sys_shifted
        }
    };
    if offset != 0 {
        log::info!(
            "chunk {} AEC reference pre-shifted by {offset} samples ({:.2}s) from capture stamps",
            chunk.index,
            offset as f64 / sample_rate as f64
        );
    }

    // Frame by frame over the whole mic track. `RefGate` decides per frame
    // whether the canceller runs; a frame it skips is copied through raw.
    let mut canc =
        aec::AcousticEchoCanceller::new().map_err(|e| RecordingError::Aec(e.to_string()))?;
    let mut gate = crate::aec_gate::RefGate::new();
    let mut clean: Vec<i16> = Vec::with_capacity(mic.len());
    let mut near = [0i16; FRAME_SIZE];
    let mut far = [0i16; FRAME_SIZE];
    let mut cancelled_frames = 0usize;
    let mut frames = 0usize;
    for start in (0..mic.len()).step_by(FRAME_SIZE) {
        let len = FRAME_SIZE.min(mic.len() - start);
        near.fill(0);
        near[..len].copy_from_slice(&mic[start..start + len]);
        far.fill(0);
        let covered = start < sys.len();
        if covered {
            let end = (start + FRAME_SIZE).min(sys.len());
            far[..end - start].copy_from_slice(&sys[start..end]);
        }
        frames += 1;
        if gate.step(covered, &far) {
            cancelled_frames += 1;
            let fc = canc
                .process(&near, &far)
                .map_err(|e| RecordingError::Aec(e.to_string()))?;
            clean.extend_from_slice(&fc[..len]);
        } else {
            clean.extend_from_slice(&mic[start..start + len]);
        }
    }
    if cancelled_frames < frames {
        log::info!(
            "chunk {} AEC: cancelled {cancelled_frames} of {frames} frames; the rest had no far-end reference and pass through raw",
            chunk.index
        );
    }

    let aec_rel = chunk_rel(chunk.index, "mic_aec.wav");
    transcript::wav_io::write_wav_mono_i16(&syncsafe::join_rel(root, &aec_rel), &clean, sample_rate)?;
    Ok(Some((i, aec_rel)))
}

/// What a finished session records about echo cancellation: `Capture` when
/// every chunk arrived echo-cancelled from the OS, else `Always`.
fn resolve_aec_mode(chunks: &[ChunkManifest], needs_aec: bool) -> AecMode {
    if !chunks.is_empty() && chunks.iter().all(|c| c.mic_voice_processed) {
        AecMode::Capture
    } else if needs_aec {
        AecMode::Always
    } else {
        AecMode::Disabled
    }
}

/// Run AEC on a stopped (but not necessarily finalized) session, producing
/// `chunks/NNNN/mic_aec.wav` per chunk and recording the relative path in
/// each chunk's `mic_aec_wav_relative`. Idempotent — chunks whose mic_aec.wav
/// already exists are skipped.
///
/// Caller must ensure the session is no longer being written to (i.e. the
/// Recorder has stopped). Returns `true` if `aec_mode == Always`; returns
/// `false` (no-op) otherwise.
pub fn apply_aec(root: &Path) -> Result<bool> {
    let mut session = Session::load(root)?;
    if session.manifest().aec_mode != AecMode::Always {
        return Ok(false);
    }
    run_aec_finalize(&mut session)?;
    Ok(true)
}

/// Run DFN3 denoise over every closed chunk's mic track, writing
/// `chunks/NNNN/mic_dn.wav` and recording it in the manifest. Input prefers
/// `mic_aec.wav` (echo-cancelled) when present, else raw `mic.wav`.
/// Idempotent — chunks whose mic_dn.wav already exists are skipped. A chunk
/// whose denoise fails is logged and skipped (consumers fall back to the
/// un-denoised file); only manifest I/O can fail the call.
///
/// Caller must ensure the session is no longer being written to.
pub fn apply_denoise(root: &Path) -> Result<bool> {
    use rayon::prelude::*;

    let mut session = Session::load(root)?;
    let chunks = session.manifest().chunks.clone();
    let root_buf = session.root().to_path_buf();

    // Chunks are independent (model state resets per chunk) and fan out
    // across the finalize pool.
    let pool = transcript::cpu::finalize_pool();
    let updates: Vec<(usize, PathBuf)> = pool.install(|| {
        chunks
            .par_iter()
            .enumerate()
            .filter_map(|(i, chunk)| {
                if chunk.ended_at_unix_seconds.is_none() {
                    return None; // chunk never closed
                }
                if let Some(rel) = &chunk.mic_dn_wav_relative {
                    if syncsafe::join_rel(&root_buf, rel).is_file() {
                        return None;
                    }
                }
                let in_rel = match chunk.mic_aec_wav_relative.as_ref() {
                    Some(p) if syncsafe::join_rel(&root_buf, p).is_file() => p.clone(),
                    _ => chunk.mic_wav_relative.clone(),
                };
                let out_rel = chunk_rel(chunk.index, "mic_dn.wav");
                match denoise::denoise_wav(&syncsafe::join_rel(&root_buf, &in_rel), &syncsafe::join_rel(&root_buf, &out_rel)) {
                    Ok(()) => Some((i, out_rel)),
                    Err(e) => {
                        log::warn!("denoise chunk {i} failed (falling back to un-denoised): {e}");
                        None
                    }
                }
            })
            .collect()
    });

    session.update_manifest(|m| {
        for (i, rel) in updates {
            if let Some(c) = m.chunks.get_mut(i) {
                c.mic_dn_wav_relative = Some(rel);
            }
        }
        m.denoise_applied = Some(true);
    })?;
    Ok(true)
}

#[cfg(test)]
mod aec_align_tests {
    use super::*;
    use crate::manifest::ChunkManifest;

    /// Deterministic speech-like signal: enveloped tone bursts with a
    /// wandering pitch, one burst per half second.
    fn speechish(n: usize, sr: usize) -> Vec<i16> {
        let mut v = vec![0i16; n];
        let mut burst = 0usize;
        while burst * sr / 2 < n {
            let at = burst * sr / 2;
            let dur = (sr * 2 / 5).min(n - at);
            let f0 = 160.0 + 70.0 * ((burst * 7 % 5) as f32);
            for i in 0..dur {
                let t = i as f32 / sr as f32;
                let env = (std::f32::consts::PI * i as f32 / dur as f32).sin();
                let x = (2.0 * std::f32::consts::PI * f0 * t).sin()
                    + 0.5 * (2.0 * std::f32::consts::PI * 2.3 * f0 * t).sin();
                v[at + i] = (env * x * 0.35 * 32_767.0) as i16;
            }
            burst += 1;
        }
        v
    }

    fn rms(v: &[i16]) -> f64 {
        if v.is_empty() {
            return 0.0;
        }
        (v.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / v.len() as f64).sqrt()
    }

    fn chunk(idx: u32, stamps: bool, mic_n: u64, sys_n: u64) -> ChunkManifest {
        ChunkManifest {
            extra: Default::default(),
            index: idx,
            started_at_unix_seconds: 0,
            ended_at_unix_seconds: Some(1),
            duration_seconds: Some(1),
            mic_wav_relative: chunk_rel(idx, "mic.wav"),
            system_wav_relative: chunk_rel(idx, "system.wav"),
            mic_aec_wav_relative: None,
            mic_dn_wav_relative: None,
            mic_host_ns: stamps.then_some(1_000),
            system_host_ns: stamps.then_some(1_000),
            mic_samples: stamps.then_some(mic_n),
            system_samples: stamps.then_some(sys_n),
            mic_voice_processed: false,
            mic_zero_samples: None,
            mic_longest_zero_run: None,
            audio_samples: None,
        }
    }

    #[test]
    fn batch_aec_cancels_echo_across_a_stamp_offset() {
        let sr = 16_000usize;
        let structural = 67_200usize; // 4.2 s: system content sits later in its file
        let base = speechish(8 * sr, sr);
        // Mic = pure far-end echo at half level, aligned to chunk t=0.
        let mic: Vec<i16> = base.iter().map(|&s| s / 2).collect();
        // System = the source, displaced by the structural offset (both
        // tracks co-closed, so the offset is the length difference).
        let mut system = vec![0i16; structural];
        system.extend_from_slice(&base);

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for idx in [1u32, 2] {
            std::fs::create_dir_all(root.join(format!("chunks/{idx:04}"))).unwrap();
            transcript::wav_io::write_wav_mono_i16(
                &root.join(chunk_rel(idx, "mic.wav")),
                &mic,
                sr as u32,
            )
            .unwrap();
            transcript::wav_io::write_wav_mono_i16(
                &root.join(chunk_rel(idx, "system.wav")),
                &system,
                sr as u32,
            )
            .unwrap();
        }

        // Chunk 1: stamps present → reference pre-shifted → cancellation.
        let seeded = chunk(1, true, mic.len() as u64, system.len() as u64);
        let out = process_chunk_aec(0, &seeded, root, sr as u32)
            .expect("aec ran")
            .expect("not silence-skipped");
        let (clean, _) = transcript::wav_io::read_wav_mono_i16(&root.join(&out.1)).unwrap();

        // Chunk 2: no stamps → legacy index-0 pairing → reference is 4.2 s
        // off and the echo survives.
        let unseeded = chunk(2, false, 0, 0);
        let out2 = process_chunk_aec(1, &unseeded, root, sr as u32)
            .expect("aec ran")
            .expect("not silence-skipped");
        let (residual, _) = transcript::wav_io::read_wav_mono_i16(&root.join(&out2.1)).unwrap();

        let raw = rms(&mic);
        let aligned = rms(&clean);
        let misaligned = rms(&residual);
        assert!(
            aligned < 0.5 * raw,
            "aligned AEC must actually cancel: raw={raw:.1} aligned={aligned:.1}"
        );
        assert!(
            aligned < 0.6 * misaligned,
            "stamp seeding must beat index-0 pairing: aligned={aligned:.1} misaligned={misaligned:.1}"
        );
    }

    /// System track: far end for 0–3 s, exact zeros 3–6 s, far end 6–7 s,
    /// then nothing (the file ends 1 s before the mic's). Spans without a
    /// live reference come through raw; the output keeps the mic's length.
    #[test]
    fn batch_aec_passes_spans_without_a_reference_through_raw() {
        let sr = 16_000usize;
        let base = speechish(8 * sr, sr);
        let mic: Vec<i16> = base.iter().map(|&s| s / 2).collect();
        let mut system = base[..7 * sr].to_vec();
        system[3 * sr..6 * sr].fill(0);

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("chunks/0001")).unwrap();
        transcript::wav_io::write_wav_mono_i16(
            &root.join(chunk_rel(1, "mic.wav")),
            &mic,
            sr as u32,
        )
        .unwrap();
        transcript::wav_io::write_wav_mono_i16(
            &root.join(chunk_rel(1, "system.wav")),
            &system,
            sr as u32,
        )
        .unwrap();
        let out = process_chunk_aec(0, &chunk(1, false, 0, 0), root, sr as u32)
            .expect("aec ran")
            .expect("not silence-skipped");
        let (clean, _) = transcript::wav_io::read_wav_mono_i16(&root.join(&out.1)).unwrap();

        assert_eq!(clean.len(), mic.len(), "mic_aec.wav must keep the mic's length");
        let span = |a: f32, b: f32| (a * sr as f32) as usize..(b * sr as f32) as usize;
        assert_eq!(&clean[span(4.1, 6.0)], &mic[span(4.1, 6.0)], "zero reference must bypass");
        assert_eq!(&clean[span(7.0, 8.0)], &mic[span(7.0, 8.0)], "no reference must bypass");
        let ratio = |r: std::ops::Range<usize>| rms(&clean[r.clone()]) / rms(&mic[r]);
        assert!(ratio(span(0.5, 3.0)) < 0.5, "live reference cancels: {}", ratio(span(0.5, 3.0)));
        assert!(ratio(span(6.2, 7.0)) < 0.6, "returned reference cancels: {}", ratio(span(6.2, 7.0)));
    }

    /// A 150 ms far-end burst after 2 s of digital silence. Its echo lands
    /// 30 ms after the burst, on zero reference frames: those are cancelled.
    #[test]
    fn batch_aec_cancels_the_echo_tail_of_a_short_burst() {
        let sr = 16_000usize;
        let mut seed: u32 = 0x2468_ace1;
        let mut noise = |n: usize| -> Vec<i16> {
            (0..n)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    ((seed >> 16) as i16) / 4
                })
                .collect()
        };
        let mut system = noise(3 * sr);
        system.extend(vec![0; 2 * sr]);
        let burst_end = system.len() + sr * 15 / 100;
        system.extend(noise(sr * 15 / 100));
        system.resize(7 * sr, 0);
        const DELAY: usize = 480;
        let mut mic = vec![0i16; system.len()];
        for i in DELAY..mic.len() {
            mic[i] = system[i - DELAY] / 2;
        }

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("chunks/0001")).unwrap();
        for (name, v) in [("mic.wav", &mic), ("system.wav", &system)] {
            transcript::wav_io::write_wav_mono_i16(
                &root.join(chunk_rel(1, name)),
                v,
                sr as u32,
            )
            .unwrap();
        }
        let out = process_chunk_aec(0, &chunk(1, false, 0, 0), root, sr as u32)
            .expect("aec ran")
            .expect("not silence-skipped");
        let (clean, _) = transcript::wav_io::read_wav_mono_i16(&root.join(&out.1)).unwrap();

        let tail = burst_end..burst_end + DELAY;
        let ratio = rms(&clean[tail.clone()]) / rms(&mic[tail]);
        assert!(ratio < 0.3, "echo tail after the burst passed through: ratio {ratio}");
    }
}

#[cfg(test)]
mod aec_mode_tests {
    use super::*;
    use crate::manifest::ChunkManifest;

    fn chunk(idx: u32, voice_processed: bool) -> ChunkManifest {
        ChunkManifest {
            extra: Default::default(),
            index: idx,
            started_at_unix_seconds: 0,
            ended_at_unix_seconds: Some(1),
            duration_seconds: Some(1),
            mic_wav_relative: chunk_rel(idx, "mic.wav"),
            system_wav_relative: chunk_rel(idx, "system.wav"),
            mic_aec_wav_relative: None,
            mic_dn_wav_relative: None,
            mic_host_ns: None,
            system_host_ns: None,
            mic_samples: None,
            system_samples: None,
            mic_voice_processed: voice_processed,
            mic_zero_samples: None,
            mic_longest_zero_run: None,
            audio_samples: None,
        }
    }

    #[test]
    fn all_voice_processed_records_capture_whatever_routing_said() {
        let chunks = [chunk(0, true), chunk(1, true)];
        assert_eq!(resolve_aec_mode(&chunks, true), AecMode::Capture);
        assert_eq!(resolve_aec_mode(&chunks, false), AecMode::Capture);
    }

    #[test]
    fn a_mixed_session_still_owes_its_raw_chunks_a_pass() {
        // One AUHAL chunk after a mid-session fallback.
        let chunks = [chunk(0, true), chunk(1, false)];
        assert_eq!(resolve_aec_mode(&chunks, true), AecMode::Always);
        assert_eq!(resolve_aec_mode(&chunks, false), AecMode::Disabled);
    }

    #[test]
    fn raw_capture_and_the_empty_session_keep_the_routing_verdict() {
        let raw = [chunk(0, false)];
        assert_eq!(resolve_aec_mode(&raw, true), AecMode::Always);
        assert_eq!(resolve_aec_mode(&raw, false), AecMode::Disabled);
        // No chunks at all is not "entirely voice-processed".
        assert_eq!(resolve_aec_mode(&[], true), AecMode::Always);
        assert_eq!(resolve_aec_mode(&[], false), AecMode::Disabled);
    }
}
