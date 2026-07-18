//! Recorder facade — orchestrates state machine + worker + session manifest + heartbeat.

use crate::error::{RecordingError, Result};
use crate::heartbeat::Heartbeat;
use crate::inhibit::SleepInhibitor;
use crate::live_pipeline::{LiveMode, LivePipeline, LivePipelineEvent};
use crate::manifest::{AecMode, ChunkManifest, RecordingSegment, SessionManifest};
use crate::session::Session;
use crate::state::StateMachine;
use crate::worker::CaptureWorker;
use audio_engine::capture::StreamingCaptureRequest;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HEARTBEAT_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Background thread that periodically touches the heartbeat file.
struct HeartbeatKeeper {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl HeartbeatKeeper {
    fn spawn(heartbeat: Heartbeat, needs_aec: Arc<AtomicBool>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);
        let join = thread::spawn(move || {
            while !stop_t.load(Ordering::Relaxed) {
                if let Err(e) = heartbeat.touch() {
                    log::warn!("heartbeat refresh failed: {e}");
                }
                // Sticky: once needs_aec is true, no more routing checks.
                if !needs_aec.load(Ordering::Relaxed) {
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

pub struct RecorderConfig {
    pub session_root: PathBuf,
    pub mic_source_id: u32,
    pub mic_source_node_name: String,
    pub mic_source_description: String,
    pub system_source_id: u32,
    pub system_source_node_name: String,
    pub system_source_description: String,
    pub sample_rate: u32,
    // pub aec_mode: AecMode,   // REMOVED — derived at stop from routing observations
    /// Human-readable session id stamped in the manifest.
    pub session_id: String,
    /// Which live transcription mode to use for this session.
    pub live_mode: LiveMode,
    /// Learned per-device floor for the live AGC's speech guard (linear
    /// peak, e.g. 0.008). None = the built-in default.
    pub speech_env_min: Option<f32>,
    /// Write the metrics.jsonl flight-recorder sidecar. Rides the debug
    /// logging setting — no toggle of its own.
    pub flight_recorder: bool,
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
    live_pipeline: Option<LivePipeline>,
    /// Receiver for events from the live pipeline; taken by the caller to
    /// forward events to the frontend.
    live_events_rx: Option<tokio::sync::mpsc::Receiver<LivePipelineEvent>>,
    /// Retained tee senders; pause() detaches the realtime transcriber feed
    /// and resume() reattaches it. Clones of the senders handed to the
    /// worker at start.
    mic_tee_tx: tokio::sync::mpsc::UnboundedSender<audio_engine::capture::TeeFrame>,
    sys_tee_tx: tokio::sync::mpsc::UnboundedSender<audio_engine::capture::TeeFrame>,
    /// Tells the auto-gain tap to re-baseline to a new mic after a mid-call
    /// switch (restore the old device's gain, read the new device's).
    mic_switch_tx: tokio::sync::mpsc::UnboundedSender<u32>,
    /// Current mic source id (updated on switch_mic).
    mic_source_id: u32,
    /// Live AGC guard floor (f32 bits), shared with the live pipeline and
    /// re-seeded on mic switch.
    speech_env_min: Arc<std::sync::atomic::AtomicU32>,
    /// Session metrics sidecar (metrics.jsonl).
    flight: Arc<crate::flight_recorder::FlightRecorder>,
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
        // Initial routing snapshot — drives the sticky flag from t=0. AEC is
        // only needed when speaker output can bleed into the mic; skip it when
        // the OUTPUT is headphone-class OR the MIC itself is a headset/Bluetooth
        // device (its mic can't pick up speakers it isn't near — the common
        // "recording on AirPods" case).
        let output_needs_aec = audio_engine::routing::detect_routing()
            .map(|r| r.needs_aec())
            .unwrap_or(true); // conservative on detection error
        let initial_needs_aec =
            output_needs_aec && !audio_engine::routing::mic_is_headphone(config.mic_source_id);

        let created_at_unix_seconds = now_unix();
        let manifest = SessionManifest {
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
            interrupted: false,
            denoise_applied: None,
        };
        let session = Session::create(&config.session_root, manifest)?;
        let heartbeat = Heartbeat::create(&config.session_root.join("heartbeat"))?;

        let needs_aec = Arc::new(AtomicBool::new(initial_needs_aec));
        // Mirrors the recorder's paused state for the live pipeline: pause
        // detaches the tees, and the dead-mic watchdog must not read that
        // silence as a broken capture stream.
        let live_paused = Arc::new(AtomicBool::new(false));
        let heartbeat_keeper = HeartbeatKeeper::spawn(heartbeat.clone(), Arc::clone(&needs_aec));
        let inhibitor = SleepInhibitor::try_acquire(&format!(
            "Daisy is recording session {}",
            config.session_id
        ));

        let req = StreamingCaptureRequest {
            mic_source_id: config.mic_source_id,
            system_source_id: config.system_source_id,
            sample_rate: config.sample_rate,
        };
        let worker = CaptureWorker::spawn(req)?;

        // Set up audio tees + live pipeline.
        let live_transcript_path = config.session_root.join("live_transcript.jsonl");
        let live_writer = Arc::new(StdMutex::new(
            crate::live_transcript::LiveTranscriptWriter::open(&live_transcript_path)
                .map_err(|e| RecordingError::Io { path: live_transcript_path.clone(), source: e })?,
        ));

        // Unbounded: the audio callback must never block or drop. See
        // audio_engine::capture::TeeSender.
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

        let (mic_switch_tx, mic_switch_rx) = tokio::sync::mpsc::unbounded_channel::<u32>();
        let speech_env_min = Arc::new(std::sync::atomic::AtomicU32::new(
            crate::live_pipeline::speech_env_min_bits(config.speech_env_min),
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
            config.speech_env_min,
        );
        let live_pipeline = LivePipeline::start(crate::live_pipeline::LiveStartConfig {
            mode: config.live_mode,
            sample_rate: config.sample_rate,
            mic_audio_rx: mic_tee_rx,
            system_audio_rx: sys_tee_rx,
            transcript_writer: live_writer,
            events_tx,
            mic_source_id: config.mic_source_id,
            speech_env_min: Arc::clone(&speech_env_min),
            mic_switch_rx,
            needs_aec: Arc::clone(&needs_aec),
            aec_model_dir: aec::constants::model_dir(),
            paused: Arc::clone(&live_paused),
            flight: Arc::clone(&flight),
        })
        .map_err(|e| RecordingError::Io { path: live_transcript_path, source: e })?;

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
            live_pipeline: Some(live_pipeline),
            live_events_rx: Some(events_rx),
            mic_tee_tx: mic_tee_tx_keep,
            sys_tee_tx: sys_tee_tx_keep,
            mic_switch_tx,
            mic_source_id: config.mic_source_id,
            speech_env_min,
            flight,
        };
        me.open_new_chunk()?;
        Ok(me)
    }

    /// Switch the recording mic to a different source mid-session (e.g. plugging
    /// in AirPods). Fail-safe in the capture layer: if the new device won't
    /// start, recording continues on the current mic. Also re-baselines
    /// auto-gain to the new device. No-op if it's already the active mic.
    /// Mute/unmute the local mic by driving its OS input gain to 0 (mute) or
    /// back to unity (unmute). Records near-silence on the mic track while
    /// muted. Auto-gain only ever lowers on clipping and does not counteract
    /// the mute.
    pub fn set_mic_muted(&self, muted: bool) -> bool {
        // Best-effort OS input gain — works on some built-in mics, but is a
        // no-op against the macOS AVAudioEngine tap and unreliable on Bluetooth.
        let _ = audio_engine::gain::set_input_gain(self.mic_source_id, if muted { 0.0 } else { 1.0 });
        // Authoritative: software mute in the capture callback zeros the mic
        // frames before they reach the wav and the live tee. Always effective.
        if let Some(w) = self.worker.as_ref() {
            let _ = w.set_mic_muted(muted);
        }
        true
    }

    /// `speech_env_min` is the new device's learned AGC guard floor (None =
    /// built-in default); the live pipeline picks it up on the next frame.
    pub fn switch_mic(
        &mut self,
        source: audio_engine::source::Source,
        speech_env_min: Option<f32>,
    ) -> Result<()> {
        if source.id == self.mic_source_id {
            return Ok(());
        }
        let new_id = source.id;
        if let Some(w) = self.worker.as_ref() {
            w.switch_mic(source)?;
        }
        // Re-baseline auto-gain to the new device (best-effort).
        let _ = self.mic_switch_tx.send(new_id);
        self.speech_env_min.store(
            crate::live_pipeline::speech_env_min_bits(speech_env_min),
            std::sync::atomic::Ordering::Relaxed,
        );
        self.mic_source_id = new_id;
        self.flight.mic_switch(new_id, speech_env_min);
        log::info!("recorder: switched mic to source {new_id}");
        Ok(())
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
        self.open_new_chunk()?;
        // Reattach the live tees to the same channels the pipeline is still
        // listening on; realtime transcription picks back up.
        if let Some(worker) = self.worker.as_ref() {
            worker.set_mic_tee(Some(self.mic_tee_tx.clone()))?;
            worker.set_system_tee(Some(self.sys_tee_tx.clone()))?;
        }
        self.heartbeat.touch()?;
        Ok(())
    }

    /// Rotate the active chunk if its open duration exceeds `interval_secs`.
    /// Returns `Ok(true)` when a rotation actually happened, `Ok(false)`
    /// otherwise (not recording, no open chunk, or chunk still young).
    ///
    /// The CaptureWorker's `OpenChunk` handler closes the previous writer
    /// and opens the new one in a single control-message handler; audio
    /// frames between the two are not dropped (the pipewire mainloop
    /// processes the control and audio callbacks serially).
    pub fn maybe_rotate_chunk(&mut self, interval_secs: u64) -> Result<bool> {
        if !matches!(self.state.state(), crate::State::Recording) {
            return Ok(false);
        }
        let Some(started) = self.open_chunk_started_unix else {
            return Ok(false);
        };
        let age = (now_unix() - started).max(0) as u64;
        if age < interval_secs {
            return Ok(false);
        }
        // Stamp the prev chunk's end + duration before rolling.
        let ended = now_unix();
        let dur = (ended - started).max(0) as u64;
        self.session.update_manifest(|m| {
            if let Some(last) = m.chunks.last_mut() {
                last.ended_at_unix_seconds = Some(ended);
                last.duration_seconds = Some(dur);
            }
        })?;
        self.open_chunk_started_unix = None;
        // Atomic open replaces the writer without a prior close_chunk; no
        // frames are dropped between the close and the next open.
        self.open_new_chunk()?;
        log::info!(
            "chunk rotated after {age}s (interval {interval_secs}s)"
        );
        Ok(true)
    }

    pub fn stop(mut self) -> Result<PathBuf> {
        self.state.stop()?;
        self.close_open_chunk()?;
        // The keeper stops before the heartbeat is deleted; the refresh
        // thread never races the cleanup unlink.
        if let Some(k) = self.heartbeat_keeper.take() {
            k.shutdown();
        }
        if let Some(worker) = self.worker.take() {
            // Tees detach first: the senders drop before shutdown, signaling
            // end-of-stream to the pipeline tasks.
            let _ = worker.set_mic_tee(None);
            let _ = worker.set_system_tee(None);
            worker.shutdown()?;
        }

        // Tear down the live pipeline (blocks until tasks finish, max 10s).
        if let Some(pipeline) = self.live_pipeline.take() {
            pipeline.shutdown();
        }

        // Finalize the AEC mode based on the routing observed during
        // recording. AEC itself does not run here; the caller (or `daisy
        // session-finalize`) runs it out-of-process. The session stays "not
        // yet finalized" until finalized_at_unix_seconds is set.
        let needs = self.needs_aec.load(Ordering::Relaxed);
        let mode = if needs { AecMode::Always } else { AecMode::Disabled };
        let stopped_at = now_unix();
        self.session.update_manifest(|m| {
            m.aec_mode = mode;
            // Close the open recording segment; resume appends a fresh one
            // with a continuing chunk index.
            if let Some(last_chunk_index) = m.chunks.last().map(|c| c.index) {
                crate::manifest_ops::close_active_segment(m, stopped_at, last_chunk_index);
            }
            // finalized_at_unix_seconds is not set here; the post-stop AEC
            // pass (or its skip path) sets that flag.
        })?;

        let root = self.session.root().to_path_buf();
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
        self.worker
            .as_ref()
            .ok_or(RecordingError::WorkerGone)?
            .open_chunk(&mic_wav, &system_wav)?;
        self.open_chunk_started_unix = Some(started);
        self.session.update_manifest(|m| {
            m.chunks.push(ChunkManifest {
                index: idx,
                started_at_unix_seconds: started,
                ended_at_unix_seconds: None,
                duration_seconds: None,
                mic_wav_relative: PathBuf::from(format!("chunks/{:04}/mic.wav", idx)),
                system_wav_relative: PathBuf::from(format!("chunks/{:04}/system.wav", idx)),
                mic_aec_wav_relative: None,
                mic_dn_wav_relative: None,
            });
        })?;
        Ok(())
    }

    fn close_open_chunk(&mut self) -> Result<()> {
        if let Some(worker) = self.worker.as_ref() {
            worker.close_chunk()?;
        }
        if let Some(started) = self.open_chunk_started_unix.take() {
            let ended = now_unix();
            let dur = (ended - started).max(0) as u64;
            self.session.update_manifest(|m| {
                if let Some(last) = m.chunks.last_mut() {
                    last.ended_at_unix_seconds = Some(ended);
                    last.duration_seconds = Some(dur);
                }
            })?;
        }
        Ok(())
    }
}

/// Read each chunk's mic.wav, run AEC against the matching system.wav, and
/// write `mic_aec.wav` next to it. Updates manifest entries with the relative path.
///
/// A thread-local canceller is built once per rayon worker and `reset()`
/// between streams (see `with_thread_canceller`). `process()` expects exactly
/// BLOCK_SHIFT (128) samples per call; the raw buffers are iterated in
/// BLOCK_SHIFT-sized strides and any trailing partial frame is padded with
/// silence.
fn run_aec_finalize(session: &mut Session) -> Result<()> {
    use aec::constants::model_dir;
    use rayon::prelude::*;

    let model_path = model_dir();
    let sample_rate = session.manifest().sample_rate;
    let chunks = session.manifest().chunks.clone();
    let root = session.root().to_path_buf();

    // Chunks are independent (the DTLN echo-canceller's LSTM state only
    // chains within a single chunk's frame loop) and fan out across the
    // rayon pool. For single-chunk recordings the inner sub-chunk parallel
    // path inside process_chunk_aec still parallelizes.
    let updates: Vec<(usize, PathBuf)> = chunks
        .par_iter()
        .enumerate()
        .filter_map(|(i, chunk)| {
            if chunk.ended_at_unix_seconds.is_none() {
                return None; // chunk never closed
            }
            // Idempotent: skip chunks whose mic_aec.wav already exists.
            if let Some(rel) = &chunk.mic_aec_wav_relative {
                if root.join(rel).is_file() {
                    return None;
                }
            }
            match process_chunk_aec(i, chunk, &root, &model_path, sample_rate) {
                Ok(Some(x)) => Some(Ok(x)),
                Ok(None) => None, // silence-skipped: orchestrator falls back to raw mic.wav
                Err(e) => Some(Err(e)),
            }
        })
        .collect::<Result<Vec<_>>>()?;

    session.update_manifest(|m| {
        for (i, rel) in updates {
            if let Some(c) = m.chunks.get_mut(i) {
                c.mic_aec_wav_relative = Some(rel);
            }
        }
    })?;
    Ok(())
}

thread_local! {
    /// One AEC canceller per rayon worker thread, reused across every chunk
    /// and sub-chunk range that lands on this thread (reset() between
    /// streams). One session build per worker thread for the process
    /// lifetime.
    static THREAD_CANCELLER: std::cell::RefCell<Option<aec::echo_canceller::AcousticEchoCanceller>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` with this thread's reused AEC canceller — built on first use,
/// reset to a clean per-stream state each call. `model_path` must be
/// constant for the process lifetime.
fn with_thread_canceller<R>(
    model_path: &Path,
    f: impl FnOnce(&mut aec::echo_canceller::AcousticEchoCanceller) -> Result<R>,
) -> Result<R> {
    THREAD_CANCELLER.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(
                aec::echo_canceller::AcousticEchoCanceller::load(model_path)
                    .map_err(|e| RecordingError::Aec(e.to_string()))?,
            );
        }
        let canc = slot.as_mut().expect("canceller just initialized");
        canc.reset();
        f(canc)
    })
}

/// Echo-cancel one chunk: read its mic + system WAVs, run the two-stage DTLN
/// canceller frame by frame (state chains within the chunk), and write
/// `chunks/NNNN/mic_aec.wav`. Returns `Some((chunk_index, rel_path))` when
/// AEC ran, `None` when the system track is effectively silent and AEC was
/// skipped (the orchestrator falls back to raw mic.wav for that chunk).
/// Safe to call from multiple threads: each call owns its own canceller(s)
/// and touches only its own chunk's files.
///
/// Parallelization: for chunks ≥ ~30s the frame loop is split across up to
/// `parallelism` rayon workers. Each worker loads its own canceller,
/// processes a few seconds of priming frames (filling the rolling near/far
/// buffers and warming the dual-LSTM state), then handles its assigned
/// range. Priming output is discarded; the boundary between workers is
/// sample-exact. The last worker also handles the trailing partial frame.
fn process_chunk_aec(
    i: usize,
    chunk: &crate::manifest::ChunkManifest,
    root: &Path,
    model_path: &Path,
    sample_rate: u32,
) -> Result<Option<(usize, PathBuf)>> {
    use aec::constants::BLOCK_SHIFT;
    use rayon::prelude::*;

    let mic = read_wav_i16(&root.join(&chunk.mic_wav_relative))?;
    let sys = read_wav_i16(&root.join(&chunk.system_wav_relative))?;

    // Silence-skip: AEC is skipped when the system track peak is below
    // ~-55 dBFS (i16 magnitude < 58). The threshold matches the
    // trailing-Whisper silence gate.
    const PEAK_SILENT_I16: i16 = 58;
    let sys_peak: i16 = sys
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

    let n = mic.len().min(sys.len());
    let num_frames = n / BLOCK_SHIFT;
    let remainder = n % BLOCK_SHIFT;

    // Sub-chunk parallel. The DTLN LSTM state chains across frames. Each
    // worker loads its own canceller, processes `PRIMING_FRAMES` frames
    // before its assigned range (warming the state + filling the 512-sample
    // rolling buffers), then handles its range. Priming output is discarded;
    // the rest is sample-aligned across workers.
    //
    // 250 priming frames = 2 s @ 8 ms/frame.
    const PRIMING_FRAMES: usize = 250;
    // Chunks below this are processed serially. 30 s @ 16 kHz / 128 = 3750
    // frames.
    const PAR_MIN_FRAMES: usize = 3750;

    let parallelism = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let num_workers = parallelism.clamp(1, 4);
    let use_parallel = num_frames >= PAR_MIN_FRAMES && num_workers > 1;

    let clean: Vec<i16> = if use_parallel {
        let frames_per_worker = num_frames / num_workers;
        log::info!(
            "chunk {} AEC starting: {} workers, {} frames each (~{}s of audio)",
            chunk.index,
            num_workers,
            frames_per_worker,
            frames_per_worker * BLOCK_SHIFT / sample_rate as usize
        );
        // (assigned_start_frame, assigned_end_frame, prime_skip_frames, is_last)
        let layout: Vec<(usize, usize, usize, bool)> = (0..num_workers)
            .map(|w| {
                let assigned_start = w * frames_per_worker;
                let assigned_end = if w == num_workers - 1 {
                    num_frames
                } else {
                    (w + 1) * frames_per_worker
                };
                let prime_start = assigned_start.saturating_sub(PRIMING_FRAMES);
                let prime_skip = assigned_start - prime_start;
                (prime_start, assigned_end, prime_skip, w == num_workers - 1)
            })
            .collect();

        let parts: Vec<Vec<i16>> = layout
            .par_iter()
            .map(|&(start_frame, end_frame, prime_skip, is_last)| {
                with_thread_canceller(model_path, |canc| {
                    let total_frames = end_frame - start_frame;
                    let mut out: Vec<i16> = Vec::with_capacity(total_frames * BLOCK_SHIFT);
                    for frame_idx in start_frame..end_frame {
                        let s = frame_idx * BLOCK_SHIFT;
                        let e = s + BLOCK_SHIFT;
                        let fc = canc
                            .process(&mic[s..e], &sys[s..e])
                            .map_err(|e| RecordingError::Aec(e.to_string()))?;
                        out.extend_from_slice(&fc);
                    }
                    // Drop priming output. After this, `out` is exactly the
                    // assigned range.
                    let mut clean_part = out.split_off(prime_skip * BLOCK_SHIFT);
                    // Last worker also runs the trailing partial frame, with the
                    // canceller now fully primed by all preceding audio.
                    if is_last && remainder > 0 {
                        let start = num_frames * BLOCK_SHIFT;
                        let mut near_pad = vec![0i16; BLOCK_SHIFT];
                        let mut far_pad = vec![0i16; BLOCK_SHIFT];
                        near_pad[..remainder].copy_from_slice(&mic[start..start + remainder]);
                        far_pad[..remainder].copy_from_slice(&sys[start..start + remainder]);
                        let fc = canc
                            .process(&near_pad, &far_pad)
                            .map_err(|e| RecordingError::Aec(e.to_string()))?;
                        clean_part.extend_from_slice(&fc[..remainder]);
                    }
                    Ok(clean_part)
                })
            })
            .collect::<Result<Vec<_>>>()?;

        log::info!("chunk {} AEC done", chunk.index);
        let mut clean = Vec::with_capacity(num_frames * BLOCK_SHIFT + remainder);
        for p in parts {
            clean.extend(p);
        }
        clean
    } else {
        // Sequential — small chunks or single-core box. Single canceller, no
        // priming overhead. Reuses this thread's canceller (reset per stream).
        with_thread_canceller(model_path, |canc| {
            let mut clean: Vec<i16> = Vec::with_capacity(num_frames * BLOCK_SHIFT + remainder);
            for frame_idx in 0..num_frames {
                let start = frame_idx * BLOCK_SHIFT;
                let end = start + BLOCK_SHIFT;
                let frame_clean = canc
                    .process(&mic[start..end], &sys[start..end])
                    .map_err(|e| RecordingError::Aec(e.to_string()))?;
                clean.extend_from_slice(&frame_clean);
            }
            if remainder > 0 {
                let start = num_frames * BLOCK_SHIFT;
                let mut near_pad = vec![0i16; BLOCK_SHIFT];
                let mut far_pad = vec![0i16; BLOCK_SHIFT];
                near_pad[..remainder].copy_from_slice(&mic[start..start + remainder]);
                far_pad[..remainder].copy_from_slice(&sys[start..start + remainder]);
                let frame_clean = canc
                    .process(&near_pad, &far_pad)
                    .map_err(|e| RecordingError::Aec(e.to_string()))?;
                clean.extend_from_slice(&frame_clean[..remainder]);
            }
            Ok(clean)
        })?
    };

    let aec_rel = PathBuf::from(format!("chunks/{:04}/mic_aec.wav", chunk.index));
    write_wav_i16(&root.join(&aec_rel), sample_rate, &clean)?;
    Ok(Some((i, aec_rel)))
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
    // across the rayon pool.
    let updates: Vec<(usize, PathBuf)> = chunks
        .par_iter()
        .enumerate()
        .filter_map(|(i, chunk)| {
            if chunk.ended_at_unix_seconds.is_none() {
                return None; // chunk never closed
            }
            if let Some(rel) = &chunk.mic_dn_wav_relative {
                if root_buf.join(rel).is_file() {
                    return None;
                }
            }
            let in_rel = match chunk.mic_aec_wav_relative.as_ref() {
                Some(p) if root_buf.join(p).is_file() => p.clone(),
                _ => chunk.mic_wav_relative.clone(),
            };
            // The path is built with an explicit forward-slash literal
            // (matching mic_aec), not with_file_name(), which reuses the
            // platform separator.
            let out_rel = PathBuf::from(format!("chunks/{:04}/mic_dn.wav", chunk.index));
            match denoise::denoise_wav(&root_buf.join(&in_rel), &root_buf.join(&out_rel)) {
                Ok(()) => Some((i, out_rel)),
                Err(e) => {
                    log::warn!("denoise chunk {i} failed (falling back to un-denoised): {e}");
                    None
                }
            }
        })
        .collect();

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

/// Repair an orphaned session at `root`: fill in any missing chunk
/// `ended_at_unix_seconds`/`duration_seconds` from WAV lengths, run AEC
/// finalize if the manifest's aec_mode is Always, and stamp finalization.
///
/// Refuses if a heartbeat exists and is fresher than `heartbeat_max_age_secs`.
pub fn finalize_orphan(root: &Path, heartbeat_max_age_secs: u64) -> Result<()> {
    let hb_path = root.join("heartbeat");
    if hb_path.exists() && Heartbeat::is_alive(&hb_path, heartbeat_max_age_secs) {
        let snap = Heartbeat::read(&hb_path)?;
        return Err(RecordingError::SessionStillLive {
            pid: snap.pid,
            age_secs: snap.age_seconds(),
        });
    }

    let mut session = Session::load(root)?;
    let chunks_snapshot = session.manifest().chunks.clone();
    let aec_mode = session.manifest().aec_mode;
    let sr = session.manifest().sample_rate as u64;

    let mut updates: Vec<(usize, u64)> = Vec::new();
    for (i, c) in chunks_snapshot.iter().enumerate() {
        // Repair any chunk missing a DURATION — not only those missing ended_at.
        // A force-quit (and a partial regen) can leave ended_at set but
        // duration_seconds None, which the UI reads as a 0:00 recording even
        // though the audio is intact on disk. Backfill from the WAV length.
        if c.duration_seconds.is_some() {
            continue;
        }
        let mic_abs = root.join(&c.mic_wav_relative);
        let r = hound::WavReader::open(&mic_abs)?;
        let n = r.duration() as u64; // frames, mono
        let dur = n / sr.max(1);
        updates.push((i, dur));
    }
    session.update_manifest(|m| {
        for (i, dur) in updates {
            if let Some(c) = m.chunks.get_mut(i) {
                if c.ended_at_unix_seconds.is_none() {
                    c.ended_at_unix_seconds = Some(c.started_at_unix_seconds + dur as i64);
                }
                c.duration_seconds = Some(dur);
            }
        }
    })?;

    if aec_mode == AecMode::Always {
        run_aec_finalize(&mut session)?;
    }
    // Stamp finalized_at_unix_seconds AFTER the AEC pass — using a `now` snapshotted
    // at function entry produced a misleading stamp that read 12 min before the
    // file mtime in user-facing logs. The stamp should reflect when finalize
    // actually completed.
    let finalized_at = now_unix();
    session.update_manifest(|m| {
        m.finalized_at_unix_seconds = Some(finalized_at);
        // Mark as recovered-from-interruption: this path only runs for orphaned
        // sessions (force-quit / crash), never a clean Stop.
        m.interrupted = true;
    })?;
    let _ = syncsafe::remove_file(&hb_path);
    Ok(())
}

fn read_wav_i16(path: &Path) -> Result<Vec<i16>> {
    let mut r = hound::WavReader::open(path)?;
    let samples: std::result::Result<Vec<i16>, hound::Error> = r.samples::<i16>().collect();
    Ok(samples?)
}

fn write_wav_i16(path: &Path, sample_rate: u32, data: &[i16]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in data {
        w.write_sample(s)?;
    }
    w.finalize()?;
    Ok(())
}
