// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! PipeWire-driven audio capture.
//!
//! `capture_one(&Source, duration, &Path)` opens a single capture stream
//! against a PipeWire source — either a real input (`SourceKind::Mic`) or a
//! synthesized monitor entry produced by `source::list_sources` for an
//! `Audio/Sink` (`SourceKind::Monitor`). Mic streams connect via numeric
//! `target.object`; monitor streams connect via `node_name` (which ends in
//! `.monitor`).
//!
//! `capture_dual` runs two streams in parallel on one MainLoop, each writing
//! its own WAV file, with a JSON manifest.
//!
//! `run_dual_streaming` runs both streams until a `StreamingControl::Stop`
//! is received, with `OpenChunk`/`CloseChunk` commands that swap WAV writers
//! mid-flight without disturbing the PipeWire streams.
//!
//! # Wake mechanism
//!
//! Control uses `pipewire::channel::channel::<StreamingControl>()`, the
//! inter-thread channel for pipewire-rs. The `Sender` is `Send + Clone`
//! (wraps `Arc<Mutex<...>>`) and works as the waker. The `Receiver` is
//! attached to the loop via `add_io`; when the sender writes a message the
//! loop wakes and calls the registered callback on the loop thread.

use crate::error::Result;
use crate::source::Source;
#[cfg(target_os = "linux")]
use crate::source::SourceKind;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
#[cfg(target_os = "windows")]
use crate::wasapi as wasapi_impl;

/// Capture from a single source for `duration`, writing 16 kHz mono int16
/// WAV via `WavFrameWriter`. Returns whether the capture was OS
/// voice-processed — the clip's capture domain, which a caller storing
/// learned levels must record with it.
pub fn capture_one(
    source: &Source,
    duration: Duration,
    out_path: &Path,
) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        pipewire_impl::capture_one_blocking(source, duration, out_path)?;
        Ok(false)
    }
    #[cfg(target_os = "windows")]
    {
        wasapi_impl::capture_one_blocking(source, duration, out_path)?;
        Ok(false)
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos::capture_one_blocking(source, duration, out_path)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = (source, duration, out_path);
        Err(crate::error::Error::NotSupported("capture_one"))
    }
}

/// Request parameters for a dual-source capture.
#[derive(Debug, Clone, Copy)]
pub struct DualCaptureRequest {
    pub mic_source_id: u32,
    pub system_source_id: u32,
    pub duration: Duration,
    /// Sample rate (16_000).
    pub sample_rate: u32,
}

/// Paths produced by `capture_dual`.
pub struct DualCaptureOutputs {
    pub mic_wav: std::path::PathBuf,
    pub system_wav: std::path::PathBuf,
    pub manifest_json: std::path::PathBuf,
}

/// Capture mic + system loopback to two WAV files plus a JSON manifest.
///
/// Platform-neutral: source validation, the timed open-chunk/stop dance, and
/// the manifest are identical everywhere; only `run_dual_streaming` (which
/// this calls) dispatches per OS.
pub fn capture_dual(
    req: DualCaptureRequest,
    out_dir: &Path,
) -> Result<DualCaptureOutputs> {
    std::fs::create_dir_all(out_dir)?;

    let mic_wav = out_dir.join("mic.wav");
    let system_wav = out_dir.join("system.wav");
    let manifest_path = out_dir.join("manifest.json");

    let started = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Validate both source IDs before opening any capture resources.
    let all = crate::source::list_sources()?;
    let find = |id: u32| -> Result<crate::source::Source> {
        all.iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| crate::error::Error::SourceNotFound(format!("id={id}")))
    };
    let mic_source = find(req.mic_source_id)?;
    let system_source = find(req.system_source_id)?;

    let streaming_req = StreamingCaptureRequest {
        mic_source_id: req.mic_source_id,
        system_source_id: req.system_source_id,
        sample_rate: req.sample_rate,
        mic_clock: std::sync::Arc::new(ChunkClock::default()),
        system_clock: std::sync::Arc::new(ChunkClock::default()),
        topology_tx: None,
        allow_os_voice_processing: false,
    };

    let mic_path = mic_wav.clone();
    let sys_path = system_wav.clone();
    let duration = req.duration;

    run_dual_streaming(streaming_req, move |handle| {
        // Open one chunk pointing at the output paths, then schedule a
        // controller thread to stop after `duration`.
        let _ = handle.open_chunk(1, &mic_path, &sys_path);
        let h = handle.clone();
        std::thread::spawn(move || {
            std::thread::sleep(duration);
            let _ = h.stop();
        });
    })?;

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let manifest = crate::manifest::RecordingManifest {
        schema_version: crate::manifest::RecordingManifest::SCHEMA,
        started_at_unix_seconds: started,
        duration_seconds: req.duration.as_secs(),
        sample_rate: req.sample_rate,
        channels: 1,
        mic: crate::manifest::ChannelManifest {
            source_id: mic_source.id,
            source_node_name: mic_source.node_name.clone(),
            source_description: mic_source.description.clone(),
            wav_path: mic_wav.clone(),
            captured_at_unix_seconds: now_unix,
        },
        system: crate::manifest::ChannelManifest {
            source_id: system_source.id,
            source_node_name: system_source.node_name.clone(),
            source_description: system_source.description.clone(),
            wav_path: system_wav.clone(),
            captured_at_unix_seconds: now_unix,
        },
    };
    manifest.write(&manifest_path)?;

    Ok(DualCaptureOutputs {
        mic_wav,
        system_wav,
        manifest_json: manifest_path,
    })
}

// ─── Streaming API ────────────────────────────────────────────────────────────

/// Capture timing for the chunk currently open, one per stream. The capture
/// callback fills these; the recorder reads them when it closes the chunk and
/// writes them into the manifest. Reset on `OpenChunk`.
#[derive(Debug, Default)]
pub struct ChunkClock {
    /// Monotonic nanoseconds of the first sample written into the open chunk.
    /// Zero when the stream carries no stamp.
    pub first_ns: std::sync::atomic::AtomicU64,
    /// Samples written into the open chunk.
    pub samples: std::sync::atomic::AtomicU64,
    /// Exactly-zero samples written into the open chunk, and the longest
    /// unbroken run of them.
    pub zero_samples: std::sync::atomic::AtomicU64,
    pub longest_zero_run: std::sync::atomic::AtomicU64,
    /// Trailing zero run carried across frame boundaries.
    zero_run: std::sync::atomic::AtomicU64,
}

impl ChunkClock {
    /// Records one frame. The first stamped frame of a chunk sets `first_ns`.
    pub fn note(&self, host_ns: u64, frame: &[i16]) {
        use std::sync::atomic::Ordering;
        if host_ns != 0 {
            let _ = self.first_ns.compare_exchange(
                0, host_ns, Ordering::Relaxed, Ordering::Relaxed,
            );
        }
        self.samples.fetch_add(frame.len() as u64, Ordering::Relaxed);
        let mut run = self.zero_run.load(Ordering::Relaxed);
        let mut longest = self.longest_zero_run.load(Ordering::Relaxed);
        let mut zeros = 0u64;
        for &s in frame {
            if s == 0 {
                run += 1;
                zeros += 1;
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
        self.zero_samples.fetch_add(zeros, Ordering::Relaxed);
        self.zero_run.store(run, Ordering::Relaxed);
        self.longest_zero_run.store(longest, Ordering::Relaxed);
    }

    /// Clears every counter for a newly opened chunk.
    pub fn reset(&self) {
        use std::sync::atomic::Ordering;
        self.first_ns.store(0, Ordering::Relaxed);
        self.samples.store(0, Ordering::Relaxed);
        self.zero_samples.store(0, Ordering::Relaxed);
        self.longest_zero_run.store(0, Ordering::Relaxed);
        self.zero_run.store(0, Ordering::Relaxed);
    }

    /// `(zero samples, longest unbroken zero run)` in the open chunk.
    pub fn zeros(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering;
        (
            self.zero_samples.load(Ordering::Relaxed),
            self.longest_zero_run.load(Ordering::Relaxed),
        )
    }

    /// `(first_ns, samples)` — `first_ns` is `None` when unstamped.
    pub fn read(&self) -> (Option<u64>, u64) {
        use std::sync::atomic::Ordering;
        let ns = self.first_ns.load(Ordering::Relaxed);
        (if ns == 0 { None } else { Some(ns) }, self.samples.load(Ordering::Relaxed))
    }
}

/// Consecutive exact-zero samples ending at the last sample of `samples`,
/// continuing `prev` from earlier callbacks. Any nonzero sample resets the run.
pub fn zero_run_update(prev: u64, samples: &[f32]) -> u64 {
    samples
        .iter()
        .fold(prev, |run, &s| if s == 0.0 { run + 1 } else { 0 })
}

/// Seconds of exact-zero mic before the macOS backend considers escalating
/// the capture to VoiceProcessingIO.
pub const ZERO_RUN_ESCALATE_SECS: u64 = 1;

/// Sustained-collapse detector for the raw mic capture, fed one peak per
/// control-loop tick (~500 ms). It learns a noise floor from healthy ticks
/// and reports a collapse run while ticks sit below that floor and under an
/// absolute ceiling.
pub struct DuckDetector {
    /// Peaks of recent healthy ticks; the min is the learned noise floor.
    healthy_peaks: std::collections::VecDeque<f32>,
    collapsed_ticks: u32,
}

/// One tick's classification from [`DuckDetector::note_tick`].
#[derive(Debug, PartialEq)]
pub enum DuckTick {
    /// Peak at or above the collapse threshold (or still learning the floor).
    Healthy,
    /// Peak collapsed; carries the consecutive-collapsed-tick count.
    Collapsed(u32),
    /// No usable peak this tick (no callbacks ran, or the mic was muted).
    Skipped,
}

/// Learned-floor window: 120 ticks ≈ 60 s of healthy audio.
const DUCK_FLOOR_WINDOW_TICKS: usize = 120;
/// Healthy ticks required before collapse detection arms (~10 s).
const DUCK_FLOOR_ARM_TICKS: usize = 20;
/// Collapse must sit >12 dB under the learned floor…
const DUCK_FLOOR_DROP: f32 = 0.25;
/// …and under −45 dBFS outright.
const DUCK_ABS_CEILING: f32 = 0.0056;
/// Consecutive collapsed ticks before escalation (~12 s at the 500 ms tick).
pub const DUCK_ESCALATE_TICKS: u32 = 24;

/// Delayed baseline of input-capturing pids for the escalation guard's
/// "new capturer" check.
/// Snapshots are recorded only on healthy ticks; the comparison baseline
/// is the oldest retained snapshot.
pub struct DelayedPidBaseline {
    history: std::collections::VecDeque<std::collections::HashSet<i32>>,
    cap: usize,
}

impl DelayedPidBaseline {
    pub fn new(cap: usize, seed: std::collections::HashSet<i32>) -> Self {
        let mut history = std::collections::VecDeque::with_capacity(cap + 1);
        history.push_back(seed);
        Self { history, cap }
    }

    /// Record a snapshot taken on a healthy tick.
    pub fn note_healthy(&mut self, pids: std::collections::HashSet<i32>) {
        self.history.push_back(pids);
        if self.history.len() > self.cap {
            self.history.pop_front();
        }
    }

    /// The comparison baseline: the oldest retained healthy snapshot.
    pub fn baseline(&self) -> &std::collections::HashSet<i32> {
        self.history.front().expect("seeded at construction")
    }
}

impl Default for DuckDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl DuckDetector {
    pub fn new() -> Self {
        Self { healthy_peaks: std::collections::VecDeque::new(), collapsed_ticks: 0 }
    }

    /// Feed the max |sample| seen since the previous tick.
    pub fn note_tick(&mut self, peak: f32) -> DuckTick {
        if peak <= 0.0 {
            return DuckTick::Skipped;
        }
        if self.healthy_peaks.len() < DUCK_FLOOR_ARM_TICKS {
            self.healthy_peaks.push_back(peak);
            return DuckTick::Healthy;
        }
        let floor = self.healthy_peaks.iter().copied().fold(f32::INFINITY, f32::min);
        if peak < (floor * DUCK_FLOOR_DROP).min(DUCK_ABS_CEILING) {
            self.collapsed_ticks += 1;
            DuckTick::Collapsed(self.collapsed_ticks)
        } else {
            self.collapsed_ticks = 0;
            self.healthy_peaks.push_back(peak);
            if self.healthy_peaks.len() > DUCK_FLOOR_WINDOW_TICKS {
                self.healthy_peaks.pop_front();
            }
            DuckTick::Healthy
        }
    }

    /// Learned noise floor, once armed. For logging.
    pub fn floor(&self) -> Option<f32> {
        (self.healthy_peaks.len() >= DUCK_FLOOR_ARM_TICKS)
            .then(|| self.healthy_peaks.iter().copied().fold(f32::INFINITY, f32::min))
    }

    /// Drop the collapse run but keep the learned floor.
    pub fn reset_run(&mut self) {
        self.collapsed_ticks = 0;
    }

    /// Forget everything.
    pub fn reset(&mut self) {
        self.healthy_peaks.clear();
        self.collapsed_ticks = 0;
    }
}

/// Parameters for the streaming capture API.
#[derive(Debug, Clone)]
pub struct StreamingCaptureRequest {
    pub mic_source_id: u32,
    pub system_source_id: u32,
    pub sample_rate: u32,
    /// Per-chunk capture timing, filled by the capture callbacks.
    pub mic_clock: std::sync::Arc<ChunkClock>,
    pub system_clock: std::sync::Arc<ChunkClock>,
    /// Sink for stream-death events; `None` runs without topology reporting.
    pub topology_tx: Option<std::sync::mpsc::Sender<crate::topology::TopologyEvent>>,
    /// Let the OS voice-process the mic stream. On Windows this requests
    /// `AudioCategory_Communications`; the stream reports back through
    /// [`StreamingHandle::mic_voice_processed`] once the OS confirms echo
    /// cancellation, and falls back to a raw stream otherwise. macOS starts raw
    /// and escalates to VoiceProcessingIO on capture loss. Linux has no OS path.
    pub allow_os_voice_processing: bool,
}

/// A tee sender for live consumption of audio samples.
///
/// An **unbounded** `tokio::sync::mpsc::UnboundedSender<Vec<i16>>`: `send`
/// is non-blocking and never drops.
pub type TeeSender = tokio::sync::mpsc::UnboundedSender<TeeFrame>;

/// One tee'd audio frame, stamped with its position in the stream.
///
/// `start_sample` is the absolute index of `samples[0]` (samples since the
/// tee was first attached, at the capture rate), counted per stream over
/// everything delivered to the tee.
#[derive(Debug, Clone)]
pub struct TeeFrame {
    pub start_sample: u64,
    /// Monotonic nanoseconds for this frame's first sample, read from the
    /// clock shared by both capture streams. Zero when the producer has no
    /// stamp; consumers fall back to pairing by `start_sample`.
    pub host_ns: u64,
    pub samples: Vec<i16>,
    /// Where this frame landed in the open chunk's WAV: `(chunk_index,
    /// offset_in_samples)`, offset measured from the file start so it
    /// includes the leading-silence pad. `None` when no chunk is open — that
    /// audio belongs to no track file and a consumer must drop it.
    pub chunk: Option<(u32, u64)>,
}

/// How long [`StreamingHandle::open_chunk`] waits for the capture loop to
/// take up the request.
const OPEN_CHUNK_TIMEOUT: Duration = Duration::from_secs(10);

/// Creates the mic and system writers for a chunk, mic first. An error names
/// the track whose file failed; a mic writer already created is closed.
pub(crate) fn create_chunk_writers(
    mic_wav: &Path,
    system_wav: &Path,
    sample_rate: u32,
) -> Result<(crate::wav::WavFrameWriter, crate::wav::WavFrameWriter)> {
    let open = |track: &'static str, path: &Path| {
        crate::wav::WavFrameWriter::create(path, sample_rate).map_err(|e| {
            let source = match e {
                crate::error::Error::Wav(hound::Error::IoError(io)) | crate::error::Error::Io(io) => io,
                other => std::io::Error::other(other.to_string()),
            };
            crate::error::Error::TrackFileOpen { track, path: path.to_path_buf(), source }
        })
    };
    let mic = open("mic", mic_wav)?;
    let system = open("system", system_wav)?;
    Ok((mic, system))
}

const OPEN_PENDING: u8 = 0;
const OPEN_COMMITTED: u8 = 1;
const OPEN_ABANDONED: u8 = 2;

/// The capture loop's half of an [`StreamingControl::OpenChunk`]: it creates
/// the chunk's writers and answers the requester.
#[derive(Debug)]
pub struct OpenChunkReply {
    tx: std::sync::mpsc::Sender<Result<()>>,
    state: Arc<AtomicU8>,
}

/// The requester's half of an [`OpenChunkReply`].
struct PendingOpen {
    rx: std::sync::mpsc::Receiver<Result<()>>,
    state: Arc<AtomicU8>,
}

fn open_chunk_request() -> (OpenChunkReply, PendingOpen) {
    let (tx, rx) = std::sync::mpsc::channel();
    let state = Arc::new(AtomicU8::new(OPEN_PENDING));
    (OpenChunkReply { tx, state: Arc::clone(&state) }, PendingOpen { rx, state })
}

impl OpenChunkReply {
    /// Creates chunk `index`'s two writers ([`create_chunk_writers`]) and
    /// commits to putting them in place. `None` when a file failed (the
    /// error goes to the requester) or the requester stopped waiting (the
    /// new writers are closed and the current chunk stays open); otherwise
    /// the caller swaps the writers in and then calls [`Self::opened`].
    pub(crate) fn writers(
        &self,
        index: u32,
        mic_wav: &Path,
        system_wav: &Path,
        sample_rate: u32,
    ) -> Option<(crate::wav::WavFrameWriter, crate::wav::WavFrameWriter)> {
        match create_chunk_writers(mic_wav, system_wav, sample_rate) {
            Err(e) => {
                log::error!("open chunk {index}: {e}");
                let _ = self.tx.send(Err(e));
                None
            }
            Ok((mic, system)) => {
                let committed = self
                    .state
                    .compare_exchange(OPEN_PENDING, OPEN_COMMITTED, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok();
                if committed {
                    return Some((mic, system));
                }
                log::warn!("open chunk {index}: its request timed out; the current chunk stays open");
                let _ = mic.close();
                let _ = system.close();
                None
            }
        }
    }

    /// Tells the requester the writers from [`Self::writers`] are in place.
    pub(crate) fn opened(self) {
        let _ = self.tx.send(Ok(()));
    }
}

impl PendingOpen {
    /// Waits up to `timeout` for the capture loop to take up the request.
    /// Past it, a request the loop has not committed to is abandoned (the
    /// loop then leaves the current chunk in place) and reported as timed
    /// out; one it has committed to is waited for to the end.
    fn wait(self, index: u32, timeout: Duration) -> Result<()> {
        use std::sync::mpsc::RecvTimeoutError;
        let gone = || crate::error::Error::Io(std::io::Error::other(format!("capture loop dropped the request to open chunk {index}")));
        match self.rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(RecvTimeoutError::Disconnected) => Err(gone()),
            Err(RecvTimeoutError::Timeout) => {
                let abandoned = self
                    .state
                    .compare_exchange(OPEN_PENDING, OPEN_ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok();
                if abandoned {
                    return Err(crate::error::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("capture loop did not open chunk {index} within {}s", timeout.as_secs()),
                    )));
                }
                self.rx.recv().unwrap_or_else(|_| Err(gone()))
            }
        }
    }
}

/// Commands sent from the controller thread to the PipeWire loop thread.
#[derive(Debug)]
pub enum StreamingControl {
    /// Open a new chunk: replace mic/system writers with the given paths.
    /// `index` is the manifest chunk index, echoed back on every tee'd frame
    /// so a live consumer knows which chunk its audio belongs to. Both files
    /// are created before either replaces the current writer; if one fails,
    /// the current writers and chunk index stay. The outcome goes to `done`.
    OpenChunk {
        index: u32,
        mic_wav: std::path::PathBuf,
        system_wav: std::path::PathBuf,
        done: OpenChunkReply,
    },
    /// Close current chunk: finalize the open writers; further audio is dropped.
    CloseChunk,
    /// Quit the MainLoop and return from `run_dual_streaming`.
    Stop,
    /// Replace the mic-stream tee. Pass `None` to detach.
    SetMicTee(Option<TeeSender>),
    /// Replace the system-stream tee. Pass `None` to detach.
    SetSystemTee(Option<TeeSender>),
    /// Mid-call mic switch: tear down the current mic stream and rebuild it on
    /// the given source, keeping the same writer + tee (the chunk continues).
    /// Fail-safe: if the rebuild fails the loop keeps running (mic may go silent,
    /// but recording + the system track are never interrupted). System track is
    /// untouched.
    SwitchMic(crate::source::Source),
    /// Tear down and rebuild the system stream on the current default
    /// output; the chunk continues.
    ReopenSystem,
    /// Software mute for the mic track. On macOS the callback zeroes mic
    /// frames before they reach the wav and the live tee. The Linux and
    /// Windows arms are no-ops. System track is untouched everywhere.
    SetMicMuted(bool),
}

/// A cheaply-cloneable handle that lets any thread drive the streaming loop.
///
/// The inner transport is platform-conditional: on Linux it wraps a
/// `pipewire::channel::Sender`, on other targets it's an opaque stub that
/// fails every dispatch. The public API (`open_chunk`, `stop`, etc.) is
/// uniform across platforms.
#[derive(Clone)]
pub struct StreamingHandle {
    #[cfg(target_os = "linux")]
    sender: pipewire::channel::Sender<StreamingControl>,
    #[cfg(target_os = "windows")]
    sender: std::sync::mpsc::Sender<StreamingControl>,
    #[cfg(target_os = "macos")]
    sender: std::sync::mpsc::Sender<StreamingControl>,
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    _marker: std::marker::PhantomData<StreamingControl>,
    /// True while the OS is voice-processing the mic capture (macOS
    /// VoiceProcessingIO).
    mic_voice_processed: Arc<AtomicBool>,
}

#[cfg(target_os = "windows")]
impl StreamingHandle {
    pub(crate) fn new_windows(
        sender: std::sync::mpsc::Sender<StreamingControl>,
        mic_voice_processed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            sender,
            mic_voice_processed,
        }
    }
}

#[cfg(target_os = "macos")]
impl StreamingHandle {
    pub(crate) fn new_macos(
        sender: std::sync::mpsc::Sender<StreamingControl>,
        mic_voice_processed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            sender,
            mic_voice_processed,
        }
    }
}

impl StreamingHandle {
    /// The live "OS is voice-processing the mic" flag: while set, capture
    /// arrives already echo-cancelled and software AEC is skipped.
    pub fn mic_voice_processed(&self) -> &Arc<AtomicBool> {
        &self.mic_voice_processed
    }

    /// Open (or replace) the current WAV chunk. Returns once the capture
    /// loop has created both track files, or with the error that stopped
    /// it ([`crate::error::Error::TrackFileOpen`] names the track and path).
    pub fn open_chunk(&self, index: u32, mic: &Path, system: &Path) -> Result<()> {
        let (done, pending) = open_chunk_request();
        self.send(StreamingControl::OpenChunk {
            index,
            mic_wav: mic.to_path_buf(),
            system_wav: system.to_path_buf(),
            done,
        })?;
        pending.wait(index, OPEN_CHUNK_TIMEOUT)
    }

    /// Finalize the current WAV chunk; audio is silently discarded until the
    /// next `open_chunk`.
    pub fn close_chunk(&self) -> Result<()> {
        self.send(StreamingControl::CloseChunk)
    }

    /// Stop the streaming loop. `run_dual_streaming` will return after this.
    pub fn stop(&self) -> Result<()> {
        self.send(StreamingControl::Stop)
    }

    /// Attach (or replace) a tee for the mic stream. Pass `None` to detach.
    pub fn set_mic_tee(&self, tx: Option<TeeSender>) -> Result<()> {
        self.send(StreamingControl::SetMicTee(tx))
    }

    /// Attach (or replace) a tee for the system stream. Pass `None` to detach.
    pub fn set_system_tee(&self, tx: Option<TeeSender>) -> Result<()> {
        self.send(StreamingControl::SetSystemTee(tx))
    }

    /// Switch the mic to a different source mid-recording (fail-safe — see
    /// `StreamingControl::SwitchMic`).
    pub fn switch_mic(&self, source: crate::source::Source) -> Result<()> {
        self.send(StreamingControl::SwitchMic(source))
    }

    /// Rebuild the system stream on the current default output.
    pub fn reopen_system(&self) -> Result<()> {
        self.send(StreamingControl::ReopenSystem)
    }

    /// Software-mute (or unmute) the mic track in the capture callback.
    pub fn set_mic_muted(&self, muted: bool) -> Result<()> {
        self.send(StreamingControl::SetMicMuted(muted))
    }

    #[cfg(target_os = "linux")]
    fn send(&self, cmd: StreamingControl) -> Result<()> {
        self.sender
            .send(cmd)
            .map_err(|_| crate::error::Error::PipeWire("control channel closed".into()))
    }

    #[cfg(target_os = "windows")]
    fn send(&self, cmd: StreamingControl) -> Result<()> {
        self.sender
            .send(cmd)
            .map_err(|_| crate::error::Error::PipeWire("control channel closed".into()))
    }

    #[cfg(target_os = "macos")]
    fn send(&self, cmd: StreamingControl) -> Result<()> {
        self.sender
            .send(cmd)
            .map_err(|_| crate::error::Error::Macos("control channel closed".into()))
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    fn send(&self, _cmd: StreamingControl) -> Result<()> {
        Err(crate::error::Error::NotSupported("streaming_handle.send"))
    }
}

/// Blocking entry point. Connects both capture streams and runs the PipeWire
/// MainLoop until [`StreamingHandle::stop`] is called.
///
/// `on_ready` is invoked (on the calling thread, before `mainloop.run()`)
/// after the streams have been registered; the caller may immediately send
/// [`StreamingHandle::open_chunk`] commands.
pub fn run_dual_streaming(
    req: StreamingCaptureRequest,
    on_ready: impl FnOnce(StreamingHandle),
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        pipewire_impl::run_dual_streaming_impl(req, on_ready)
    }
    #[cfg(target_os = "windows")]
    {
        wasapi_impl::run_dual_streaming_impl(req, on_ready)
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos::run_dual_streaming_impl(req, on_ready)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = (req, on_ready);
        Err(crate::error::Error::NotSupported("run_dual_streaming"))
    }
}

/// Run a non-recording level meter on a mic source. Opens a PipeWire (Linux)
/// or WASAPI (Windows) capture stream against the given source id, computes
/// RMS in the process callback, and invokes `on_rms` at ~20 Hz with the
/// most-recent value (0.0–1.0, where 1.0 ≈ full-scale int16).
///
/// Runs until `stop_rx` receives any message (or its sender is dropped). All
/// audio data is discarded — no WAV is written.
///
/// Uses the same audio backend as recording.
pub fn run_level_meter(
    source_id: u32,
    on_rms: impl FnMut(f32) + Send + 'static,
    stop_rx: std::sync::mpsc::Receiver<()>,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        pipewire_impl::run_level_meter_impl(source_id, on_rms, stop_rx)
    }
    #[cfg(target_os = "windows")]
    {
        wasapi_impl::run_level_meter_impl(source_id, on_rms, stop_rx)
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos::run_level_meter_impl(source_id, on_rms, stop_rx)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = (source_id, on_rms, stop_rx);
        Err(crate::error::Error::NotSupported("run_level_meter"))
    }
}

// ─── PipeWire implementation ──────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod pipewire_impl {
    use super::{Source, SourceKind, StreamingCaptureRequest, StreamingControl, StreamingHandle, TeeFrame, TeeSender};
    use crate::topology::{StreamDeathReason, Track, TopologyEvent};
    use crate::error::{Error, Result};
    use crate::wav::{WavFrameWriter, FRAME_SAMPLES};
    use pipewire as pw;
    use pw::spa;
    use spa::param::audio::{AudioFormat, AudioInfoRaw};
    use spa::pod::Pod;
    use std::cell::RefCell;
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::time::Duration;

    /// Locks `m`, taking the value back from a thread that panicked holding it.
    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Build a SPA S16LE format POD at the given sample rate (mono).
    fn make_format_pod(sample_rate: u32) -> Result<Vec<u8>> {
        let mut audio_info = AudioInfoRaw::new();
        audio_info.set_format(AudioFormat::S16LE);
        audio_info.set_rate(sample_rate);
        audio_info.set_channels(1);
        let obj = pw::spa::pod::Object {
            type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: pw::spa::param::ParamType::EnumFormat.as_raw(),
            properties: audio_info.into(),
        };
        let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &pw::spa::pod::Value::Object(obj),
        )
        .map_err(|e| Error::PipeWire(format!("serialize format pod: {e}")))?
        .0
        .into_inner();
        Ok(values)
    }

    /// Build and connect a single capture stream backed by a concrete writer.
    ///
    /// Returns `(stream, listener)` — both must stay alive for the duration of
    /// the MainLoop run. Dropping the listener handle unsubscribes it in
    /// `pipewire-rs 0.8`.
    type DeathTx = std::sync::mpsc::Sender<TopologyEvent>;

    /// Maps a stream state transition to a death reason. `stopping` is true
    /// once a Stop command was dispatched; commanded teardown is not a death.
    fn stream_death(
        old: &pw::stream::StreamState,
        new: &pw::stream::StreamState,
        stopping: bool,
    ) -> Option<StreamDeathReason> {
        use pw::stream::StreamState as S;
        match new {
            S::Error(e) => Some(StreamDeathReason::Error(e.clone())),
            S::Unconnected if matches!(old, S::Streaming | S::Paused) && !stopping => {
                Some(StreamDeathReason::TargetGone)
            }
            _ => None,
        }
    }

    /// Finalize and drop the writer in `slot`, if any. A close failure is
    /// logged and the slot still ends up empty.
    fn close_writer_slot(slot: &Mutex<Option<WavFrameWriter>>, track: &str, when: &str) {
        let writer = lock(slot).take();
        if let Some(w) = writer {
            if let Err(e) = w.close() {
                log::warn!("{track} writer close on {when}: {e}");
            }
        }
    }

    /// One capture track's shared handles, as the command dispatcher sees
    /// them: everything a rebuild must thread back into the fresh stream.
    struct TrackParts {
        node_name: &'static str,
        slot: Arc<Mutex<Option<WavFrameWriter>>>,
        tee: Arc<Mutex<Option<TeeSender>>>,
        sent: Arc<AtomicU64>,
        clock: std::sync::Arc<super::ChunkClock>,
        track: Track,
        stopping: Arc<AtomicBool>,
        stream_cell: Rc<RefCell<Option<(pw::stream::StreamRc, pw::stream::StreamListener<()>)>>>,
    }

    /// Drop the track's stream and rebuild it on `source`, sharing the same
    /// writer/tee/clock so the chunk continues.
    /// Returns whether the rebuild succeeded (failure leaves the track
    /// silent while the rest of the recording keeps running).
    fn rebuild_stream(
        core: &pw::core::CoreRc,
        parts: &TrackParts,
        source: &Source,
        sample_rate: u32,
        open_chunk: &Arc<Mutex<Option<u32>>>,
        death_tx: &Option<DeathTx>,
        what: &str,
    ) -> bool {
        parts.stopping.store(true, Ordering::SeqCst);
        parts.stream_cell.borrow_mut().take();
        let res = build_capture_stream_optional(
            core,
            source,
            sample_rate,
            parts.node_name,
            Arc::clone(&parts.slot),
            Arc::clone(&parts.tee),
            Arc::clone(&parts.sent),
            Arc::clone(open_chunk),
            std::sync::Arc::clone(&parts.clock),
            parts.track,
            death_tx.clone(),
            Arc::clone(&parts.stopping),
        );
        parts.stopping.store(false, Ordering::SeqCst);
        match res {
            Ok(pair) => {
                *parts.stream_cell.borrow_mut() = Some(pair);
                true
            }
            Err(e) => {
                log::error!(
                    "{what}: rebuild on {} failed — track silent, recording continues: {e}",
                    source.node_name
                );
                false
            }
        }
    }

    /// Stream properties for a capture on `source`: a name-addressed target and,
    /// for mic streams, a Communication role.
    fn capture_props(source: &Source, node_name: &str) -> pw::properties::PropertiesBox {
        let role = match source.kind {
            SourceKind::Mic => "Communication",
            SourceKind::Monitor => "Music",
        };
        let mut props = pw::properties::PropertiesBox::new();
        props.insert(*pw::keys::MEDIA_TYPE, "Audio");
        props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
        props.insert(*pw::keys::MEDIA_ROLE, role);
        props.insert(*pw::keys::NODE_NAME, node_name);
        props.insert("target.object", source.node_name.as_str());
        if source.kind == SourceKind::Monitor {
            props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
        }
        props
    }

    fn build_capture_stream(
        core: &pw::core::CoreRc,
        source: &Source,
        sample_rate: u32,
        node_name: &str,
        writer: Arc<Mutex<WavFrameWriter>>,
    ) -> Result<(
        pw::stream::StreamRc,
        pw::stream::StreamListener<()>,
    )> {
        let props = capture_props(source, node_name);
        let stream = pw::stream::StreamRc::new(core.clone(), node_name, props)
            .map_err(|e| Error::PipeWire(format!("Stream::new ({node_name}): {e}")))?;

        let mut accum: Vec<i16> = Vec::with_capacity(FRAME_SAMPLES * 4);
        let writer_for_cb = Arc::clone(&writer);

        let listener = stream
            .add_local_listener_with_user_data(())
            .process(move |stream, _| {
                let mut buf = match stream.dequeue_buffer() {
                    Some(b) => b,
                    None => return,
                };
                let datas = buf.datas_mut();
                let d = match datas.first_mut() {
                    Some(d) => d,
                    None => return,
                };
                let valid_bytes = d.chunk().size() as usize;
                if valid_bytes == 0 {
                    return;
                }
                let raw = match d.data() {
                    Some(b) => b,
                    None => return,
                };
                let bytes = &raw[..valid_bytes];
                let samples: &[i16] = bytemuck::cast_slice(bytes);
                accum.extend_from_slice(samples);
                let mut writer_borrow = lock(&writer_for_cb);
                while accum.len() >= FRAME_SAMPLES {
                    let frame: Vec<i16> = accum.drain(..FRAME_SAMPLES).collect();
                    if let Err(e) = writer_borrow.write_frame(&frame) {
                        log::warn!("frame write failed: {e}");
                    }
                }
            })
            .register()
            .map_err(|e| Error::PipeWire(format!("register listener ({node_name}): {e}")))?;

        let format_bytes = make_format_pod(sample_rate)?;
        let mut params = [Pod::from_bytes(&format_bytes)
            .ok_or_else(|| Error::PipeWire("invalid format pod bytes".into()))?];

        stream
            .connect(
                spa::utils::Direction::Input,
                None,
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut params,
            )
            .map_err(|e| Error::PipeWire(format!("Stream::connect ({node_name}): {e}")))?;

        Ok((stream, listener))
    }

    /// Variant of `build_capture_stream` where the writer is optional.
    ///
    /// The process callback skips writes when the slot holds `None` (paused).
    /// The same slot is shared with the command dispatcher, which swaps
    /// writers mid-flight on the loop thread.
    #[allow(clippy::too_many_arguments)]
    fn build_capture_stream_optional(
        core: &pw::core::CoreRc,
        source: &Source,
        sample_rate: u32,
        node_name: &str,
        writer: Arc<Mutex<Option<WavFrameWriter>>>,
        tee: Arc<Mutex<Option<TeeSender>>>,
        sent: Arc<AtomicU64>,
        chunk: Arc<Mutex<Option<u32>>>,
        clock: std::sync::Arc<super::ChunkClock>,
        track: Track,
        death_tx: Option<DeathTx>,
        stopping: Arc<AtomicBool>,
    ) -> Result<(pw::stream::StreamRc, pw::stream::StreamListener<()>)> {
        let props = capture_props(source, node_name);
        let stream = pw::stream::StreamRc::new(core.clone(), node_name, props)
            .map_err(|e| Error::PipeWire(format!("Stream::new ({node_name}): {e}")))?;

        let mut accum: Vec<i16> = Vec::with_capacity(FRAME_SAMPLES * 4);
        let writer_cb = Arc::clone(&writer);
        let tee_cb = Arc::clone(&tee);
        let sent_cb = Arc::clone(&sent);
        let chunk_cb = Arc::clone(&chunk);
        let clock_cb = std::sync::Arc::clone(&clock);
        let death_tx_cb = death_tx.clone();
        let stopping_cb = Arc::clone(&stopping);

        let listener = stream
            .add_local_listener_with_user_data(())
            .process(move |stream, _| {
                let mut buf = match stream.dequeue_buffer() {
                    Some(b) => b,
                    None => return,
                };
                let datas = buf.datas_mut();
                let d = match datas.first_mut() {
                    Some(d) => d,
                    None => return,
                };
                let valid = d.chunk().size() as usize;
                if valid == 0 {
                    return;
                }
                let raw = match d.data() {
                    Some(b) => b,
                    None => return,
                };
                let samples: &[i16] = bytemuck::cast_slice(&raw[..valid]);

                let rate_ns_div = sample_rate.max(1) as u64;
                let cycle_ns = stream
                    .time()
                    .map(|t| t.now().max(0) as u64)
                    .unwrap_or(0);

                accum.extend_from_slice(samples);
                // `cycle_ns` stamps the newest sample in the buffer, so the
                // oldest sample still queued sits that many samples earlier.
                let mut frame_ns = if cycle_ns == 0 {
                    0
                } else {
                    let back = (accum.len().saturating_sub(1)) as u64 * 1_000_000_000
                        / rate_ns_div;
                    cycle_ns.saturating_sub(back)
                };
                let frame_step_ns = FRAME_SAMPLES as u64 * 1_000_000_000 / rate_ns_div;

                let mut writer_b = lock(&writer_cb);
                let open_chunk = *lock(&chunk_cb);
                while accum.len() >= FRAME_SAMPLES {
                    let frame: Vec<i16> = accum.drain(..FRAME_SAMPLES).collect();
                    // Write to WAV (primary path); the tee stamp is taken from the writer in
                    // the same step.
                    let mut frame_chunk: Option<(u32, u64)> = None;
                    if let Some(w) = writer_b.as_mut() {
                        if let Err(e) = w.write_frame_stamped(&frame, frame_ns) {
                            log::warn!("frame write failed: {e}");
                        } else if let Some(idx) = open_chunk {
                            frame_chunk =
                                Some((idx, w.samples_written().saturating_sub(frame.len() as u64)));
                            clock_cb.note(frame_ns, &frame);
                        }
                    }
                    // Tee to live consumer (unbounded, non-blocking).
                    let tee_borrow = lock(&tee_cb);
                    if let Some(tx) = tee_borrow.as_ref() {
                        let at = sent_cb.load(Ordering::SeqCst);
                        let n = frame.len() as u64;
                        if tx
                            .send(TeeFrame {
                                start_sample: at,
                                host_ns: frame_ns,
                                samples: frame,
                                chunk: frame_chunk,
                            })
                            .is_ok()
                        {
                            sent_cb.store(at + n, Ordering::SeqCst);
                        }
                    }
                    if frame_ns != 0 {
                        frame_ns += frame_step_ns;
                    }
                }
            })
            .state_changed(move |_s, _ud, old, new| {
                log::debug!("pw stream {track:?}: {old:?} -> {new:?}");
                if let Some(reason) =
                    stream_death(&old, &new, stopping_cb.load(Ordering::SeqCst))
                {
                    log::warn!("pw stream {track:?} died: {reason:?}");
                    if let Some(tx) = &death_tx_cb {
                        let _ = tx.send(TopologyEvent::StreamDied { track, reason });
                    }
                }
            })
            .register()
            .map_err(|e| Error::PipeWire(format!("register listener ({node_name}): {e}")))?;

        let format_bytes = make_format_pod(sample_rate)?;
        let mut params = [Pod::from_bytes(&format_bytes)
            .ok_or_else(|| Error::PipeWire("invalid format pod bytes".into()))?];

        stream
            .connect(
                spa::utils::Direction::Input,
                None,
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut params,
            )
            .map_err(|e| Error::PipeWire(format!("Stream::connect ({node_name}): {e}")))?;

        Ok((stream, listener))
    }

    pub(super) fn capture_one_blocking(
        source: &Source,
        duration: Duration,
        out_path: &Path,
    ) -> Result<()> {
        pw::init();

        let mainloop = pw::main_loop::MainLoopRc::new(None)
            .map_err(|e| Error::PipeWire(format!("MainLoop::new: {e}")))?;
        let context = pw::context::ContextRc::new(&mainloop, None)
            .map_err(|e| Error::PipeWire(format!("ContextRc::new: {e}")))?;
        let core = context
            .connect_rc(None)
            .map_err(|e| Error::PipeWire(format!("connect: {e}")))?;

        let writer = Arc::new(Mutex::new(WavFrameWriter::create(out_path, 16_000)?));

        let (stream, _listener) =
            build_capture_stream(&core, source, 16_000, "daisy-capture-one", Arc::clone(&writer))?;

        let mainloop_for_timer = mainloop.clone();
        let timer = mainloop
            .loop_()
            .add_timer(move |_expirations| {
                mainloop_for_timer.quit();
            });
        timer
            .update_timer(Some(duration), None)
            .into_sync_result()
            .map_err(|e| Error::PipeWire(format!("update_timer: {e}")))?;

        mainloop.run();

        drop(_listener);
        drop(stream);

        let writer_owned = match Arc::try_unwrap(writer) {
            Ok(m) => m.into_inner().unwrap_or_else(|e| e.into_inner()),
            Err(_) => {
                return Err(Error::PipeWire(
                    "writer handle still held after stream drop".into(),
                ))
            }
        };
        writer_owned.close()?;
        Ok(())
    }

    pub(super) fn run_dual_streaming_impl(
        req: StreamingCaptureRequest,
        on_ready: impl FnOnce(StreamingHandle),
    ) -> Result<()> {
        pw::init();

        // Validate both source IDs before opening any PW resources.
        let all = crate::source::list_sources()?;
        let find = |id: u32| -> Result<crate::source::Source> {
            all.iter()
                .find(|s| s.id == id)
                .cloned()
                .ok_or_else(|| Error::SourceNotFound(format!("id={id}")))
        };
        let mic_source = find(req.mic_source_id)?;
        let system_source = find(req.system_source_id)?;

        let mainloop = pw::main_loop::MainLoopRc::new(None)
            .map_err(|e| Error::PipeWire(format!("MainLoop::new: {e}")))?;
        let context = pw::context::ContextRc::new(&mainloop, None)
            .map_err(|e| Error::PipeWire(format!("ContextRc::new: {e}")))?;
        let core = context
            .connect_rc(None)
            .map_err(|e| Error::PipeWire(format!("connect: {e}")))?;

        // Shared optional writer slots — None means paused (audio discarded).
        let mic_slot: Arc<Mutex<Option<WavFrameWriter>>> = Arc::new(Mutex::new(None));
        let sys_slot: Arc<Mutex<Option<WavFrameWriter>>> = Arc::new(Mutex::new(None));

        // Per-stream tee slots — None means no live consumer attached.
        let mic_tee: Arc<Mutex<Option<TeeSender>>> = Arc::new(Mutex::new(None));
        let sys_tee: Arc<Mutex<Option<TeeSender>>> = Arc::new(Mutex::new(None));
        // Stream clocks (samples delivered to each tee).
        let mic_sent: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
        let sys_sent: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
        // Index of the chunk currently being written; None between
        // CloseChunk and the next OpenChunk, where audio belongs to no file.
        let open_chunk: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
        let mic_clock_cmd = std::sync::Arc::clone(&req.mic_clock);
        let sys_clock_cmd = std::sync::Arc::clone(&req.system_clock);
        // True while a commanded teardown is in flight for that stream.
        let stopping_mic: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        let stopping_sys: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        let death_tx = req.topology_tx.clone();

        let (mic_stream, _mic_listener) = build_capture_stream_optional(
            &core,
            &mic_source,
            req.sample_rate,
            "daisy-stream-mic",
            Arc::clone(&mic_slot),
            Arc::clone(&mic_tee),
            Arc::clone(&mic_sent),
            Arc::clone(&open_chunk),
            std::sync::Arc::clone(&req.mic_clock),
            Track::Mic,
            death_tx.clone(),
            Arc::clone(&stopping_mic),
        )?;
        let (sys_stream, _sys_listener) = build_capture_stream_optional(
            &core,
            &system_source,
            req.sample_rate,
            "daisy-stream-system",
            Arc::clone(&sys_slot),
            Arc::clone(&sys_tee),
            Arc::clone(&sys_sent),
            Arc::clone(&open_chunk),
            std::sync::Arc::clone(&req.system_clock),
            Track::System,
            death_tx.clone(),
            Arc::clone(&stopping_sys),
        )?;

        // Control channel from the controller thread to the loop thread.
        let (pw_sender, pw_receiver) = pw::channel::channel::<StreamingControl>();

        let mic_tee_for_event = Arc::clone(&mic_tee);
        let sys_tee_for_event = Arc::clone(&sys_tee);
        let open_chunk_for_cmd = Arc::clone(&open_chunk);
        let mainloop_for_cmd = mainloop.clone();
        let sample_rate = req.sample_rate;

        // Streams live in cells; SwitchMic/ReopenSystem replace them on the
        // loop thread. Core is moved in (last use here) for the rebuilds.
        let mic_stream_cell = Rc::new(RefCell::new(Some((mic_stream, _mic_listener))));
        let sys_stream_cell = Rc::new(RefCell::new(Some((sys_stream, _sys_listener))));
        let stopping_mic_for_cmd = Arc::clone(&stopping_mic);
        let stopping_sys_for_cmd = Arc::clone(&stopping_sys);
        let mic_parts = TrackParts {
            node_name: "daisy-stream-mic",
            slot: Arc::clone(&mic_slot),
            tee: Arc::clone(&mic_tee),
            sent: Arc::clone(&mic_sent),
            clock: std::sync::Arc::clone(&req.mic_clock),
            track: Track::Mic,
            stopping: Arc::clone(&stopping_mic),
            stream_cell: Rc::clone(&mic_stream_cell),
        };
        let sys_parts = TrackParts {
            node_name: "daisy-stream-system",
            slot: Arc::clone(&sys_slot),
            tee: Arc::clone(&sys_tee),
            sent: Arc::clone(&sys_sent),
            clock: std::sync::Arc::clone(&req.system_clock),
            track: Track::System,
            stopping: Arc::clone(&stopping_sys),
            stream_cell: Rc::clone(&sys_stream_cell),
        };
        let system_source_for_reopen = system_source.clone();
        let death_tx_for_cmd = death_tx.clone();
        let core_for_cmd = core;

        let _receiver = pw_receiver.attach(mainloop.loop_(), move |cmd| {
            match cmd {
                StreamingControl::OpenChunk { index, mic_wav, system_wav, done } => {
                    if let Some((mic_w, sys_w)) = done.writers(index, &mic_wav, &system_wav, sample_rate) {
                        *lock(&open_chunk_for_cmd) = Some(index);
                        // Finalize the previous chunk's writers before the new ones take over.
                        close_writer_slot(&mic_parts.slot, "mic", "OpenChunk");
                        close_writer_slot(&sys_parts.slot, "system", "OpenChunk");
                        mic_clock_cmd.reset();
                        sys_clock_cmd.reset();
                        *lock(&mic_parts.slot) = Some(mic_w);
                        *lock(&sys_parts.slot) = Some(sys_w);
                        done.opened();
                    }
                }
                StreamingControl::CloseChunk => {
                    *lock(&open_chunk_for_cmd) = None;
                    close_writer_slot(&mic_parts.slot, "mic", "CloseChunk");
                    close_writer_slot(&sys_parts.slot, "system", "CloseChunk");
                }
                StreamingControl::Stop => {
                    stopping_mic_for_cmd.store(true, Ordering::SeqCst);
                    stopping_sys_for_cmd.store(true, Ordering::SeqCst);
                    // Close any open writers then quit.
                    close_writer_slot(&mic_parts.slot, "mic", "Stop");
                    close_writer_slot(&sys_parts.slot, "system", "Stop");
                    mainloop_for_cmd.quit();
                }
                StreamingControl::SetMicTee(tx) => {
                    *lock(&mic_tee_for_event) = tx;
                }
                StreamingControl::SetSystemTee(tx) => {
                    *lock(&sys_tee_for_event) = tx;
                }
                StreamingControl::SwitchMic(source) => {
                    // Rebuild on the new source; the chunk continues.
                    if rebuild_stream(
                        &core_for_cmd,
                        &mic_parts,
                        &source,
                        sample_rate,
                        &open_chunk_for_cmd,
                        &death_tx_for_cmd,
                        "SwitchMic",
                    ) {
                        log::info!("SwitchMic: mic now {}", source.node_name);
                    }
                }
                StreamingControl::ReopenSystem => {
                    // Rebuild on the current default output.
                    if rebuild_stream(
                        &core_for_cmd,
                        &sys_parts,
                        &system_source_for_reopen,
                        sample_rate,
                        &open_chunk_for_cmd,
                        &death_tx_for_cmd,
                        "ReopenSystem",
                    ) {
                        log::info!("ReopenSystem: system stream rebuilt");
                    }
                }
                StreamingControl::SetMicMuted(_) => {
                    // Linux mutes via the OS input gain.
                }
            }
        });

        let handle = StreamingHandle {
            sender: pw_sender,
            mic_voice_processed: Arc::new(AtomicBool::new(false)),
        };

        // Notify the caller that the loop is ready to receive commands.
        on_ready(handle);

        mainloop.run();

        // Drop the receiver before returning; both stream cells live in its
        // closure and drop with it.
        drop(_receiver);

        Ok(())
    }

    /// PipeWire level-meter impl. See the public `run_level_meter` wrapper for
    /// the contract. Lives on its own MainLoop and exits when `stop_rx`
    /// receives a message (or its sender disconnects).
    pub(super) fn run_level_meter_impl(
        source_id: u32,
        on_rms: impl FnMut(f32) + Send + 'static,
        stop_rx: std::sync::mpsc::Receiver<()>,
    ) -> Result<()> {
        // The user callback sits behind a RefCell.
        let on_rms = std::cell::RefCell::new(on_rms);
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        pw::init();

        // Resolve the source id once up front.
        let all = crate::source::list_sources()?;
        let source = all
            .into_iter()
            .find(|s| s.id == source_id)
            .ok_or_else(|| Error::SourceNotFound(format!("id={source_id}")))?;

        let mainloop = pw::main_loop::MainLoopRc::new(None)
            .map_err(|e| Error::PipeWire(format!("MainLoop::new: {e}")))?;
        let context = pw::context::ContextRc::new(&mainloop, None)
            .map_err(|e| Error::PipeWire(format!("ContextRc::new: {e}")))?;
        let core = context
            .connect_rc(None)
            .map_err(|e| Error::PipeWire(format!("connect: {e}")))?;

        // Latest RMS (f32, bit-cast into u32 for atomic transfer between the
        // realtime audio thread and the loop thread).
        let latest_bits = Arc::new(AtomicU32::new(0));
        let latest_for_cb = Arc::clone(&latest_bits);

        let props = capture_props(&source, "daisy-level-meter");
        let stream = pw::stream::StreamRc::new(core.clone(), "daisy-level-meter", props)
            .map_err(|e| Error::PipeWire(format!("Stream::new: {e}")))?;

        let _listener = stream
            .add_local_listener_with_user_data(())
            .process(move |stream, _| {
                let mut buf = match stream.dequeue_buffer() {
                    Some(b) => b,
                    None => return,
                };
                let datas = buf.datas_mut();
                let d = match datas.first_mut() {
                    Some(d) => d,
                    None => return,
                };
                let valid_bytes = d.chunk().size() as usize;
                if valid_bytes == 0 {
                    return;
                }
                let raw = match d.data() {
                    Some(b) => b,
                    None => return,
                };
                let bytes = &raw[..valid_bytes];
                let samples: &[i16] = bytemuck::cast_slice(bytes);
                if samples.is_empty() {
                    return;
                }
                // Peak amplitude normalised to [0, 1].
                let mut peak: i32 = 0;
                for &s in samples {
                    let a = (s as i32).abs();
                    if a > peak { peak = a; }
                }
                let level = (peak as f32) / (i16::MAX as f32);
                latest_for_cb.store(level.to_bits(), Ordering::Relaxed);
            })
            .state_changed(|_s, _ud, old, new| {
                log::debug!("pw meter stream: {old:?} -> {new:?}");
            })
            .register()
            .map_err(|e| Error::PipeWire(format!("register listener: {e}")))?;

        let format_bytes = make_format_pod(16_000)?;
        let mut params = [Pod::from_bytes(&format_bytes)
            .ok_or_else(|| Error::PipeWire("invalid format pod bytes".into()))?];

        stream
            .connect(
                spa::utils::Direction::Input,
                None,
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut params,
            )
            .map_err(|e| Error::PipeWire(format!("Stream::connect: {e}")))?;

        // Drive emission + stop polling from a 50ms loop timer (≈20 Hz).
        let mainloop_for_timer = mainloop.clone();
        let latest_for_timer = Arc::clone(&latest_bits);
        let timer = mainloop.loop_().add_timer(move |_| {
            // The stop signal is checked first.
            match stop_rx.try_recv() {
                Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    mainloop_for_timer.quit();
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            let bits = latest_for_timer.load(Ordering::Relaxed);
            let rms = f32::from_bits(bits);
            on_rms.borrow_mut()(rms);
        });
        timer
            .update_timer(Some(Duration::from_millis(50)), Some(Duration::from_millis(50)))
            .into_sync_result()
            .map_err(|e| Error::PipeWire(format!("update_timer: {e}")))?;

        mainloop.run();

        drop(_listener);
        drop(stream);
        Ok(())
    }
}

#[cfg(test)]
mod create_chunk_writers_tests {
    use super::create_chunk_writers;
    use crate::error::Error;

    #[test]
    fn a_system_file_that_cannot_be_created_names_the_system_track() {
        let td = tempfile::tempdir().unwrap();
        let mic = td.path().join("mic.wav");
        let sys = td.path().join("missing").join("system.wav");
        let err = create_chunk_writers(&mic, &sys, 16_000).err().expect("system create fails");
        match err {
            Error::TrackFileOpen { track, path, source } => {
                assert_eq!(track, "system");
                assert_eq!(path, sys);
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("unexpected {other}"),
        }
        // The mic writer created first is closed as a valid, empty WAV.
        assert_eq!(hound::WavReader::open(&mic).unwrap().duration(), 0);
    }

    #[test]
    fn a_mic_file_that_cannot_be_created_names_the_mic_track_and_skips_system() {
        let td = tempfile::tempdir().unwrap();
        let mic = td.path().join("missing").join("mic.wav");
        let sys = td.path().join("system.wav");
        let err = create_chunk_writers(&mic, &sys, 16_000).err().expect("mic create fails");
        assert!(matches!(err, Error::TrackFileOpen { track: "mic", .. }), "{err}");
        assert!(!sys.exists());
    }

    #[test]
    fn a_request_that_timed_out_leaves_the_current_chunk_in_place() {
        // The loop takes the request up after the requester gave up: the new
        // files are created and closed empty, and nothing is swapped in.
        let td = tempfile::tempdir().unwrap();
        let (reply, pending) = super::open_chunk_request();
        let err = pending.wait(2, std::time::Duration::from_millis(20)).unwrap_err();
        assert!(matches!(&err, Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut), "{err}");
        let (mic, sys) = (td.path().join("mic.wav"), td.path().join("system.wav"));
        assert!(reply.writers(2, &mic, &sys, 16_000).is_none());
        assert_eq!(hound::WavReader::open(&mic).unwrap().duration(), 0);
    }

    #[test]
    fn a_request_the_loop_committed_to_is_waited_for_past_the_timeout() {
        let td = tempfile::tempdir().unwrap();
        let (reply, pending) = super::open_chunk_request();
        let (mic, sys) = (td.path().join("mic.wav"), td.path().join("system.wav"));
        let writers = reply.writers(3, &mic, &sys, 16_000).expect("committed while the requester waits");
        let loop_side = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            reply.opened();
            writers
        });
        pending.wait(3, std::time::Duration::from_millis(20)).expect("the committed open succeeds");
        let (m, s) = loop_side.join().unwrap();
        m.close().unwrap();
        s.close().unwrap();
    }

    #[test]
    fn a_failed_file_is_reported_to_the_requester() {
        let td = tempfile::tempdir().unwrap();
        let (reply, pending) = super::open_chunk_request();
        let missing = td.path().join("missing");
        assert!(reply.writers(4, &missing.join("mic.wav"), &missing.join("system.wav"), 16_000).is_none());
        let err = pending.wait(4, std::time::Duration::from_secs(5)).unwrap_err();
        assert!(matches!(err, Error::TrackFileOpen { track: "mic", .. }), "{err}");
    }

    #[test]
    fn a_dropped_request_is_an_error_not_a_hang() {
        let (reply, pending) = super::open_chunk_request();
        drop(reply);
        assert!(pending.wait(5, std::time::Duration::from_secs(5)).is_err());
    }

    #[test]
    fn both_files_open() {
        let td = tempfile::tempdir().unwrap();
        let (m, s) = create_chunk_writers(&td.path().join("mic.wav"), &td.path().join("system.wav"), 16_000)
            .unwrap();
        m.close().unwrap();
        s.close().unwrap();
    }
}

#[cfg(test)]
mod zero_run_tests {
    use super::zero_run_update;

    #[test]
    fn counts_trailing_zeros_across_calls() {
        assert_eq!(zero_run_update(0, &[0.0, 0.0, 0.0]), 3);
        assert_eq!(zero_run_update(3, &[0.0, 0.0]), 5);
    }

    #[test]
    fn resets_on_signal() {
        assert_eq!(zero_run_update(100, &[0.0, 0.001, 0.0, 0.0]), 2);
        assert_eq!(zero_run_update(100, &[0.5]), 0);
    }

    #[test]
    fn empty_slice_keeps_the_run() {
        assert_eq!(zero_run_update(7, &[]), 7);
    }
}

#[cfg(test)]
mod duck_detector_tests {
    use super::{DuckDetector, DuckTick, DUCK_ESCALATE_TICKS};

    const QUIET_ROOM: f32 = 0.0032; // ≈ −50 dBFS
    const DUCKED: f32 = 0.0002; // ≈ −74 dBFS
    const SPEECH: f32 = 0.4; // ≈ −8 dBFS

    fn armed(floor: f32) -> DuckDetector {
        let mut d = DuckDetector::new();
        for _ in 0..30 {
            assert_eq!(d.note_tick(floor), DuckTick::Healthy);
        }
        d
    }

    #[test]
    fn detects_field_measured_duck_after_sustained_run() {
        let mut d = armed(QUIET_ROOM);
        for n in 1..=DUCK_ESCALATE_TICKS {
            assert_eq!(d.note_tick(DUCKED), DuckTick::Collapsed(n));
        }
    }

    #[test]
    fn quiet_room_alone_is_healthy_not_collapsed() {
        let mut d = armed(QUIET_ROOM);
        // Same level as the learned floor: quiet, but not a collapse.
        assert_eq!(d.note_tick(QUIET_ROOM), DuckTick::Healthy);
        assert_eq!(d.note_tick(QUIET_ROOM * 0.7), DuckTick::Healthy);
    }

    #[test]
    fn healthy_tick_resets_the_run() {
        let mut d = armed(QUIET_ROOM);
        for _ in 0..10 {
            d.note_tick(DUCKED);
        }
        assert_eq!(d.note_tick(SPEECH), DuckTick::Healthy);
        assert_eq!(d.note_tick(DUCKED), DuckTick::Collapsed(1));
    }

    #[test]
    fn loud_room_duck_is_caught_by_the_absolute_ceiling() {
        // Floor −20 dBFS; a −48 dBFS tick counts as collapse.
        let mut d = armed(0.1);
        assert_eq!(d.note_tick(0.004), DuckTick::Collapsed(1));
    }

    #[test]
    fn session_started_mid_duck_learns_the_ducked_floor_and_stays_quiet() {
        let mut d = armed(DUCKED);
        // Ducked level == learned floor: no collapse ever reported.
        assert_eq!(d.note_tick(DUCKED), DuckTick::Healthy);
        assert_eq!(d.floor(), Some(DUCKED));
    }

    #[test]
    fn no_signal_ticks_are_skipped_and_keep_the_run() {
        let mut d = armed(QUIET_ROOM);
        assert_eq!(d.note_tick(DUCKED), DuckTick::Collapsed(1));
        assert_eq!(d.note_tick(0.0), DuckTick::Skipped);
        assert_eq!(d.note_tick(DUCKED), DuckTick::Collapsed(2));
    }

    #[test]
    fn detection_stays_disarmed_until_enough_healthy_ticks() {
        let mut d = DuckDetector::new();
        // Below the absolute ceiling from the first tick, yet never
        // collapsed: the floor is still being learned.
        for _ in 0..10 {
            assert_eq!(d.note_tick(0.004), DuckTick::Healthy);
        }
        assert_eq!(d.floor(), None);
    }

    #[test]
    fn reset_run_keeps_the_floor() {
        let mut d = armed(QUIET_ROOM);
        for _ in 0..10 {
            d.note_tick(DUCKED);
        }
        d.reset_run();
        assert!(d.floor().is_some());
        assert_eq!(d.note_tick(DUCKED), DuckTick::Collapsed(1));
    }

    #[test]
    fn reset_forgets_the_floor() {
        let mut d = armed(QUIET_ROOM);
        d.reset();
        assert_eq!(d.floor(), None);
        assert_eq!(d.note_tick(DUCKED), DuckTick::Healthy);
    }
}

#[cfg(test)]
mod delayed_pid_baseline_tests {
    use super::DelayedPidBaseline;
    use std::collections::HashSet;

    fn pids(v: &[i32]) -> HashSet<i32> {
        v.iter().copied().collect()
    }

    #[test]
    fn call_pid_arriving_just_before_the_duck_is_still_new() {
        // Seed = capture start. The call app (pid 2) shows up on the last
        // healthy tick before the duck engages; the delayed baseline has
        // not absorbed it yet.
        let mut b = DelayedPidBaseline::new(3, pids(&[1]));
        b.note_healthy(pids(&[1]));
        b.note_healthy(pids(&[1]));
        b.note_healthy(pids(&[1, 2]));
        assert_eq!(b.baseline(), &pids(&[1]));
    }

    #[test]
    fn a_long_standing_capturer_ages_into_the_baseline() {
        let mut b = DelayedPidBaseline::new(3, pids(&[1]));
        for _ in 0..4 {
            b.note_healthy(pids(&[1, 2]));
        }
        assert_eq!(b.baseline(), &pids(&[1, 2]));
    }

    #[test]
    fn baseline_freezes_when_healthy_ticks_stop() {
        // Collapse = the caller stops noting; whatever was oldest stays.
        let mut b = DelayedPidBaseline::new(2, pids(&[]));
        b.note_healthy(pids(&[1]));
        assert_eq!(b.baseline(), &pids(&[]));
        assert_eq!(b.baseline(), &pids(&[]));
    }

    #[test]
    fn young_session_compares_against_the_seed() {
        let b = DelayedPidBaseline::new(10, pids(&[7]));
        assert_eq!(b.baseline(), &pids(&[7]));
    }
}

#[cfg(test)]
mod chunk_clock_tests {
    use super::ChunkClock;

    #[test]
    fn counts_zeros_and_the_longest_run_across_frames() {
        let c = ChunkClock::default();
        c.note(1_000, &[0, 5, 0, 0]);
        // The run continues across the frame boundary: 2 trailing + 3 leading
        // is one run of 5.
        c.note(2_000, &[0, 0, 0, 7]);
        let (_, samples) = c.read();
        assert_eq!(samples, 8);
        assert_eq!(c.zeros(), (6, 5));
    }

    #[test]
    fn a_fully_zeroed_chunk_reads_as_all_zeros() {
        let c = ChunkClock::default();
        c.note(1_000, &[0; 320]);
        c.note(2_000, &[0; 320]);
        let (_, samples) = c.read();
        assert_eq!(c.zeros(), (samples, samples));
    }

    #[test]
    fn reset_clears_the_run_carried_into_the_next_chunk() {
        let c = ChunkClock::default();
        c.note(1_000, &[0, 0, 0]);
        c.reset();
        c.note(2_000, &[0, 1]);
        // The carried run is cleared by reset.
        assert_eq!(c.zeros(), (1, 1));
    }

    #[test]
    fn a_signal_carrying_chunk_reports_nothing() {
        let c = ChunkClock::default();
        c.note(1_000, &[3, -4, 5]);
        assert_eq!(c.zeros(), (0, 0));
    }
}
