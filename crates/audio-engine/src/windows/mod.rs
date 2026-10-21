// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! WASAPI-backed audio capture for Windows.
//!
//! Mirrors the public-API contract of the Linux `pipewire_impl` module;
//! `capture.rs` / `source.rs` / `virtual_sink.rs` dispatch by target_os and
//! callers (recording, tauri-app) need no cfg gates.
//!
//! ## Threading model
//!
//! WASAPI's `IAudioCaptureClient::GetBuffer` is blocking; one native thread
//! runs per stream. Shared state between the control thread and each capture
//! thread:
//!
//! * `Arc<Mutex<Option<WavFrameWriter>>>` — the current chunk writer slot.
//!   `None` means paused; capture threads silently drop frames.
//! * `Arc<Mutex<Option<TeeSender>>>` — optional live-audio sink (best-effort
//!   `try_send`, drops if full).
//! * `Arc<AtomicBool>` — the stop flag. Capture threads poll once per buffer
//!   pull and exit when set.
//!
//! ## Sample-rate path
//!
//! WASAPI delivers the device-native mix format (commonly 48 kHz f32 stereo).
//! Each capture thread downmixes to mono f32, feeds 1024-frame chunks through
//! a `rubato::FastFixedIn` resampler, converts to `i16`, and drains the
//! result into the standard 320-sample 20 ms frame buffer that
//! `WavFrameWriter` expects. Device frames lost between packets (a jump in
//! the packet's device position) enter the resampler as the same number of
//! silent frames.

#![allow(unsafe_code)]

pub(crate) mod topology;
mod source;
pub(crate) mod sessions;
mod system;
pub(crate) use source::LOOPBACK_NODE;

use crate::capture::{StreamingCaptureRequest, StreamingControl, StreamingHandle, TeeSender};
use crate::error::{Error, Result};
use crate::packet_gap::{Gap, MAX_FILL_SECS};
use crate::source::{Source, SourceKind};
use crate::wav::{WavFrameWriter, FRAME_SAMPLES};

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::core::{Interface, GUID, PCWSTR};
use windows::Win32::Media::Audio::{
    AudioCategory_Communications, AudioClientProperties, IAcousticEchoCancellationControl,
    IAudioCaptureClient, IAudioClient, IAudioClient2, IAudioEffectsManager, IAudioRenderClient,
    IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMOPTIONS_NONE,
    AUDIO_EFFECT, AUDIO_EFFECT_STATE_ON, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM,
};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};

pub(crate) const OUTPUT_SAMPLE_RATE: u32 = 16_000;

/// Shared-mode client buffer requested at `Initialize`, in milliseconds.
const CLIENT_BUFFER_MS: i64 = 200;

/// Filled gaps logged one by one per stream; the rest are summed in one line
/// when the stream ends.
const MAX_FILL_LOGS: u64 = 20;

/// Classify the default output (render) device as headphone- vs speaker-class,
/// for `routing::detect_routing()`. Skips AEC on headphones/Bluetooth.
pub(crate) fn default_output_class() -> Result<crate::routing::OutputClass> {
    system::default_output_class()
}

// SubFormat GUIDs for WAVEFORMATEXTENSIBLE.
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID = GUID::from_values(
    0x0000_0003,
    0x0000,
    0x0010,
    [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
);
const KSDATAFORMAT_SUBTYPE_PCM: GUID = GUID::from_values(
    0x0000_0001,
    0x0000,
    0x0010,
    [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
);

// ksmedia.h AUDIO_EFFECT_TYPE_ACOUSTIC_ECHO_CANCELLATION.
const AUDIO_EFFECT_TYPE_AEC: GUID = GUID::from_u128(0x6f64adbe_8211_11e2_8c70_2c27d7f001fa);

/// Mic-stream voice-processing plumbing: `flag` is the shared
/// [`StreamingHandle::mic_voice_processed`] bit, written by the capture
/// thread each time it (re)opens; `aec_render_endpoint` pins the OS echo
/// canceller's reference to the endpoint the system track loopback-captures,
/// `None` when that is the default render endpoint.
#[derive(Clone)]
struct VoiceMode {
    flag: Arc<AtomicBool>,
    aec_render_endpoint: Option<String>,
}

// ─── public-from-parent entry points ──────────────────────────────────────────

pub(crate) fn list_sources_blocking() -> Result<Vec<Source>> {
    let _com = ComGuard::init()?;
    source::enumerate_sources()
}

/// WASAPI level-meter stub. Returns NotSupported; the command layer falls
/// back to a different code path (Web Audio in the frontend) on Windows.
pub(crate) fn run_level_meter_impl(
    _source_id: u32,
    _on_rms: impl FnMut(f32) + Send + 'static,
    _stop_rx: std::sync::mpsc::Receiver<()>,
) -> Result<()> {
    Err(Error::NotSupported("run_level_meter on Windows"))
}

pub(crate) fn capture_one_blocking(
    source: &Source,
    duration: Duration,
    out_path: &Path,
) -> Result<()> {
    let _com = ComGuard::init()?;
    let device = resolve_device(source)?;
    let loopback = source.kind == SourceKind::Monitor;

    let writer_slot: Arc<Mutex<Option<WavFrameWriter>>> = Arc::new(Mutex::new(Some(
        WavFrameWriter::create(out_path, OUTPUT_SAMPLE_RATE)?,
    )));
    let tee_slot: Arc<Mutex<Option<TeeSender>>> = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));

    let stop_for_timer = Arc::clone(&stop);
    let timer = thread::spawn(move || {
        thread::sleep(duration);
        stop_for_timer.store(true, Ordering::SeqCst);
    });

    let res = run_capture_thread(
        device,
        loopback,
        Arc::clone(&writer_slot),
        Arc::clone(&tee_slot),
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
        // One-shot capture to a file: no chunk is open, so frames carry no
        // chunk stamp. Always a raw client.
        Arc::new(Mutex::new(None)),
        Arc::new(crate::capture::ChunkClock::default()),
        None,
        Arc::clone(&stop),
        None,
        None,
    );

    let _ = timer.join();

    // Finalize the writer regardless of capture result.
    if let Some(w) = writer_slot.lock().unwrap().take() {
        let _ = w.close();
    }

    res
}

pub(crate) fn run_dual_streaming_impl(
    req: StreamingCaptureRequest,
    on_ready: impl FnOnce(StreamingHandle),
) -> Result<()> {
    // Validate IDs (and resolve to Source structs) on this thread before
    // committing to spawning capture workers.
    let _com = ComGuard::init()?;
    let all = source::enumerate_sources()?;
    let find = |id: u32| -> Result<Source> {
        all.iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| Error::SourceNotFound(format!("id={id}")))
    };
    let mic_source = find(req.mic_source_id)?;
    let system_source = find(req.system_source_id)?;

    let mic_voice_processed = Arc::new(AtomicBool::new(false));
    let voice = req.allow_os_voice_processing.then(|| VoiceMode {
        flag: Arc::clone(&mic_voice_processed),
        aec_render_endpoint: aec_reference_endpoint(&system_source),
    });

    // Index of the chunk currently being written; None between CloseChunk
    // and the next OpenChunk, where audio belongs to no file.
    let open_chunk: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    let mut mic = StreamParts::spawn(
        "daisy-wasapi-mic",
        mic_source,
        crate::topology::Track::Mic,
        req.topology_tx.clone(),
        Arc::clone(&open_chunk),
        Arc::clone(&req.mic_clock),
        voice,
    )?;
    let mut sys = StreamParts::spawn(
        "daisy-wasapi-system",
        system_source,
        crate::topology::Track::System,
        req.topology_tx.clone(),
        Arc::clone(&open_chunk),
        Arc::clone(&req.system_clock),
        None,
    )?;

    let (control_tx, control_rx) = mpsc::channel::<StreamingControl>();
    let handle = StreamingHandle::new_windows(control_tx, mic_voice_processed);
    on_ready(handle);

    // Drive the control loop until Stop.
    let sample_rate = req.sample_rate;
    while let Ok(cmd) = control_rx.recv() {
        match cmd {
            StreamingControl::OpenChunk { index, mic_wav, system_wav, done } => {
                if let Some((mic_w, sys_w)) = done.writers(index, &mic_wav, &system_wav, sample_rate) {
                    *open_chunk.lock().unwrap() = Some(index);
                    req.mic_clock.reset();
                    req.system_clock.reset();
                    rotate_writer(&mic.writer_slot, mic_w);
                    rotate_writer(&sys.writer_slot, sys_w);
                    done.opened();
                }
            }
            StreamingControl::CloseChunk => {
                *open_chunk.lock().unwrap() = None;
                close_writer(&mic.writer_slot);
                close_writer(&sys.writer_slot);
            }
            StreamingControl::SetMicTee(tx) => {
                *mic.tee_slot.lock().unwrap() = tx;
            }
            StreamingControl::SetSystemTee(tx) => {
                *sys.tee_slot.lock().unwrap() = tx;
            }
            StreamingControl::SwitchMic(source) => {
                // Respawn on the new device sharing the SAME writer + tee
                // (chunk continues); system thread untouched. Fail-safe: if
                // the new device won't start, recover the previous one.
                let prev = mic.source.clone();
                match mic.respawn(source.clone()) {
                    Ok(()) => log::info!("SwitchMic(win): mic now {}", source.node_name),
                    Err(e) => {
                        log::error!(
                            "SwitchMic(win): start on {} failed — recovering previous mic: {e}",
                            source.node_name
                        );
                        if let Err(e2) = mic.respawn(prev) {
                            log::error!("SwitchMic(win): recovery failed — mic stopped: {e2}");
                        }
                    }
                }
            }
            StreamingControl::ReopenSystem => {
                // Respawn the loopback thread on its own source.
                // Writers/tees are shared; the chunk continues.
                let src = sys.source.clone();
                match sys.respawn(src) {
                    Ok(()) => log::info!(
                        "ReopenSystem(win): loopback re-bound to current default output"
                    ),
                    Err(e) => log::error!(
                        "ReopenSystem(win): respawn failed — system silent, recording continues: {e}"
                    ),
                }
            }
            StreamingControl::Stop => {
                close_writer(&mic.writer_slot);
                close_writer(&sys.writer_slot);
                break;
            }
            StreamingControl::SetMicMuted(_) => {
                // Windows mutes via the OS input gain (WASAPI honors it); the
                // recorder's set_input_gain call handles it.
            }
        }
    }

    // Tell both capture threads to exit before joining either, then surface
    // any errors.
    mic.stop.store(true, Ordering::SeqCst);
    sys.stop.store(true, Ordering::SeqCst);
    let mic_res = mic.shutdown("mic");
    let sys_res = sys.shutdown("system");

    mic_res?;
    sys_res?;
    Ok(())
}

/// One capture stream's shared handles + lifecycle, as the control loop sees
/// them: everything a respawn must thread back into the fresh thread.
struct StreamParts {
    name: &'static str,
    source: Source,
    track: crate::topology::Track,
    /// Sink for un-commanded capture-thread deaths; None in one-shot mode.
    death_tx: Option<std::sync::mpsc::Sender<crate::topology::TopologyEvent>>,
    writer_slot: Arc<Mutex<Option<WavFrameWriter>>>,
    tee_slot: Arc<Mutex<Option<TeeSender>>>,
    tee_sent: Arc<std::sync::atomic::AtomicU64>,
    open_chunk: Arc<Mutex<Option<u32>>>,
    clock: Arc<crate::capture::ChunkClock>,
    /// Mic streams only; a respawned thread re-runs the communications-mode
    /// ladder for the new device and re-stores the shared flag.
    voice: Option<VoiceMode>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<Result<()>>>,
}

impl StreamParts {
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        name: &'static str,
        source: Source,
        track: crate::topology::Track,
        death_tx: Option<std::sync::mpsc::Sender<crate::topology::TopologyEvent>>,
        open_chunk: Arc<Mutex<Option<u32>>>,
        clock: Arc<crate::capture::ChunkClock>,
        voice: Option<VoiceMode>,
    ) -> Result<Self> {
        let writer_slot: Arc<Mutex<Option<WavFrameWriter>>> = Arc::new(Mutex::new(None));
        let tee_slot: Arc<Mutex<Option<TeeSender>>> = Arc::new(Mutex::new(None));
        let tee_sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let join = spawn_capture_thread(
            name,
            source.clone(),
            track,
            death_tx.clone(),
            Arc::clone(&writer_slot),
            Arc::clone(&tee_slot),
            Arc::clone(&tee_sent),
            Arc::clone(&open_chunk),
            Arc::clone(&clock),
            voice.clone(),
            Arc::clone(&stop),
        )?;
        Ok(Self {
            name,
            source,
            track,
            death_tx,
            writer_slot,
            tee_slot,
            tee_sent,
            open_chunk,
            clock,
            voice,
            stop,
            join: Some(join),
        })
    }

    /// Stop + join the current thread, then start a fresh one on `source`
    /// sharing the same writer/tee/clock — the chunk continues. On error the
    /// stream is left stopped (`source` unchanged); a second respawn can
    /// recover it.
    fn respawn(&mut self, source: Source) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = join_capture(self.name, j);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let join = spawn_capture_thread(
            self.name,
            source.clone(),
            self.track,
            self.death_tx.clone(),
            Arc::clone(&self.writer_slot),
            Arc::clone(&self.tee_slot),
            Arc::clone(&self.tee_sent),
            Arc::clone(&self.open_chunk),
            Arc::clone(&self.clock),
            self.voice.clone(),
            Arc::clone(&stop),
        )?;
        self.stop = stop;
        self.join = Some(join);
        self.source = source;
        Ok(())
    }

    fn shutdown(&mut self, label: &str) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        match self.join.take() {
            Some(j) => join_capture(label, j),
            None => Ok(()),
        }
    }
}

/// Puts `next` in place of the slot's writer, closing the previous one.
fn rotate_writer(slot: &Arc<Mutex<Option<WavFrameWriter>>>, next: WavFrameWriter) {
    let mut guard = slot.lock().unwrap();
    if let Some(old) = guard.replace(next) {
        if let Err(e) = old.close() {
            log::warn!("writer close on rotate: {e}");
        }
    }
}

fn close_writer(slot: &Arc<Mutex<Option<WavFrameWriter>>>) {
    if let Some(w) = slot.lock().unwrap().take() {
        if let Err(e) = w.close() {
            log::warn!("writer close: {e}");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_capture_thread(
    name: &str,
    source: Source,
    track: crate::topology::Track,
    death_tx: Option<std::sync::mpsc::Sender<crate::topology::TopologyEvent>>,
    writer_slot: Arc<Mutex<Option<WavFrameWriter>>>,
    tee_slot: Arc<Mutex<Option<TeeSender>>>,
    tee_sent: Arc<std::sync::atomic::AtomicU64>,
    open_chunk: Arc<Mutex<Option<u32>>>,
    chunk_clock: Arc<crate::capture::ChunkClock>,
    voice: Option<VoiceMode>,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<Result<()>>> {
    let loopback = source.kind == SourceKind::Monitor;
    let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
    let stop_for_timeout = Arc::clone(&stop);
    let join = thread::Builder::new()
        .name(name.to_string())
        .spawn(move || -> Result<()> {
            let com = match ComGuard::init() {
                Ok(c) => c,
                Err(e) => {
                    let _ = ready_tx.send(Err(clone_err(&e)));
                    return Err(e);
                }
            };
            let device = match resolve_device(&source) {
                Ok(d) => d,
                Err(e) => {
                    let _ = ready_tx.send(Err(clone_err(&e)));
                    drop(com);
                    return Err(e);
                }
            };
            let stop_for_death = Arc::clone(&stop);
            let ready_sent = Arc::new(AtomicBool::new(false));
            let ready_sent_in = Arc::clone(&ready_sent);
            let r = run_capture_thread(
                device, loopback, writer_slot, tee_slot, tee_sent, open_chunk, chunk_clock,
                voice, stop, Some(ready_tx), Some(ready_sent_in),
            );
            // An un-commanded post-ready exit is reported to the supervisor.
            if let Err(e) = &r {
                if ready_sent.load(Ordering::SeqCst) && !stop_for_death.load(Ordering::SeqCst) {
                    if let Some(tx) = &death_tx {
                        let _ = tx.send(crate::topology::TopologyEvent::StreamDied {
                            track,
                            reason: crate::topology::StreamDeathReason::Error(e.to_string()),
                        });
                    }
                }
            }
            r
        })
        .map_err(|e| Error::Wasapi(format!("spawn capture thread: {e}")))?;

    // Block until the capture loop confirms init success or surfaces an error.
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(join),
        Ok(Err(e)) => {
            // The thread already returned its own copy of this error.
            let _ = join.join();
            Err(e)
        }
        Err(_) => {
            // No ready signal in 5 s: set the stop flag and abort.
            stop_for_timeout.store(true, Ordering::SeqCst);
            Err(Error::Wasapi(format!(
                "{name}: WASAPI init timed out (no ready signal in 5 s)"
            )))
        }
    }
}

fn join_capture(label: &str, join: JoinHandle<Result<()>>) -> Result<()> {
    match join.join() {
        Ok(r) => r,
        Err(_) => Err(Error::Wasapi(format!("{label} capture thread panicked"))),
    }
}

// ─── COM init guard ───────────────────────────────────────────────────────────

struct ComGuard;

impl ComGuard {
    fn init() -> Result<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // S_OK or S_FALSE (already initialized on this thread) are both OK.
        // RPC_E_CHANGED_MODE means this thread was previously initialized as
        // STA; in that rare case fall through and let later calls fail.
        if hr.is_err() {
            let code = hr.0;
            // RPC_E_CHANGED_MODE = 0x80010106 is treated as usable: the
            // existing apartment accepts WASAPI calls.
            if code as u32 != 0x80010106 {
                return Err(Error::Wasapi(format!(
                    "CoInitializeEx failed: 0x{code:08x}"
                )));
            }
        }
        Ok(Self)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

// ─── Source → IMMDevice resolution ────────────────────────────────────────────

fn resolve_device(source: &Source) -> Result<IMMDevice> {
    // `LOOPBACK_NODE` means "the default render endpoint, captured in
    // loopback mode." Any other Monitor goes through the listed-device
    // lookup path.
    if source.kind == SourceKind::Monitor && source.node_name == LOOPBACK_NODE {
        return system::default_render_device();
    }
    match source.kind {
        SourceKind::Mic => source::find_device(windows::Win32::Media::Audio::eCapture, source.id, "mic"),
        SourceKind::Monitor => source::find_device(windows::Win32::Media::Audio::eRender, source.id, "system"),
    }
}

// ─── Client open (raw vs communications mode) ─────────────────────────────────

/// The stream's system-audio endpoint for the OS echo canceller's reference:
/// a concrete monitor source pins to that device; `LOOPBACK_NODE` returns
/// `None`.
fn aec_reference_endpoint(system_source: &Source) -> Option<String> {
    if system_source.node_name == LOOPBACK_NODE {
        return None;
    }
    let device = resolve_device(system_source).ok()?;
    let id = unsafe { device.GetId().ok()? };
    let s = pcwstr_to_string(PCWSTR(id.0));
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(id.0 as *const _)) };
    (!s.is_empty()).then_some(s)
}

/// Opens the capture client for a stream. A mic stream with voice processing
/// allowed first tries a communications-mode client and keeps it only when
/// the OS confirms an active echo canceller on it; everything else — loopback
/// streams, declined comms mode, pre-22000 Windows with no effects manager —
/// gets the plain client. Returns `(client, format, os_voice_processed)`.
fn open_capture_client(
    device: &IMMDevice,
    loopback: bool,
    voice: Option<&VoiceMode>,
) -> Result<(IAudioClient, InputFormat, bool)> {
    if let Some(v) = voice {
        if !loopback {
            match open_comms_client(device, v) {
                Ok((client, format)) => return Ok((client, format, true)),
                Err(e) => log::info!(
                    "communications mode declined — raw capture, software AEC stays on: {e}"
                ),
            }
        }
    }
    let client: IAudioClient = unsafe {
        device
            .Activate::<IAudioClient>(CLSCTX_ALL, None)
            .map_err(|e| Error::Wasapi(format!("IMMDevice::Activate(IAudioClient): {e}")))?
    };
    let format = initialize_client(&client, loopback)?;
    Ok((client, format, false))
}

/// Opens an `AudioCategory_Communications` client on the device and verifies
/// the OS attached an active echo canceller to it. Any failure — activation,
/// initialization, no `IAudioEffectsManager` (pre-Win11-22000), no active AEC
/// effect — drops the client and errors, so the caller falls back to raw.
fn open_comms_client(device: &IMMDevice, voice: &VoiceMode) -> Result<(IAudioClient, InputFormat)> {
    let client2: IAudioClient2 = unsafe {
        device
            .Activate::<IAudioClient2>(CLSCTX_ALL, None)
            .map_err(|e| Error::Wasapi(format!("Activate(IAudioClient2): {e}")))?
    };
    let props = AudioClientProperties {
        cbSize: std::mem::size_of::<AudioClientProperties>() as u32,
        bIsOffload: false.into(),
        eCategory: AudioCategory_Communications,
        Options: AUDCLNT_STREAMOPTIONS_NONE,
    };
    unsafe {
        client2
            .SetClientProperties(&props)
            .map_err(|e| Error::Wasapi(format!("SetClientProperties(Communications): {e}")))?
    };
    let client: IAudioClient = client2
        .cast()
        .map_err(|e| Error::Wasapi(format!("IAudioClient2→IAudioClient: {e}")))?;
    let format = initialize_client(&client, false)?;

    let effects = query_effects(&client)?;
    if !os_aec_active(&effects) {
        return Err(Error::Wasapi(
            "no active echo-cancellation effect on the communications stream".into(),
        ));
    }

    // Best-effort: point the canceller's reference at the endpoint the system
    // track records. Failure keeps the stream — the OS falls back to its
    // default reference.
    if let Some(ep) = &voice.aec_render_endpoint {
        match unsafe { client.GetService::<IAcousticEchoCancellationControl>() } {
            Ok(ctl) => {
                let wide: Vec<u16> = ep.encode_utf16().chain(std::iter::once(0)).collect();
                if let Err(e) = unsafe { ctl.SetEchoCancellationRenderEndpoint(PCWSTR(wide.as_ptr())) }
                {
                    log::warn!("AEC reference pin failed (OS default reference stays): {e}");
                }
            }
            Err(e) => log::warn!("IAcousticEchoCancellationControl unavailable: {e}"),
        }
    }
    Ok((client, format))
}

/// `GetMixFormat` + shared-mode `Initialize`, freeing the mix format either
/// way. Any of PCM int16 / IEEE float32, mono or stereo, is accepted and
/// converted by the capture loop.
fn initialize_client(client: &IAudioClient, loopback: bool) -> Result<InputFormat> {
    let mix_format_ptr = unsafe {
        client
            .GetMixFormat()
            .map_err(|e| Error::Wasapi(format!("IAudioClient::GetMixFormat: {e}")))?
    };
    let input_format = parse_wave_format(mix_format_ptr);

    let mut stream_flags: u32 = 0;
    if loopback {
        stream_flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
    }

    let buffer_duration_hns: i64 = CLIENT_BUFFER_MS * 10_000;

    let init_result = unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            stream_flags,
            buffer_duration_hns,
            0,
            mix_format_ptr,
            None,
        )
    };
    // Free the WAVEFORMATEX that GetMixFormat allocated via CoTaskMemAlloc.
    unsafe {
        windows::Win32::System::Com::CoTaskMemFree(Some(mix_format_ptr as *const _));
    }
    let input_format = input_format?;
    init_result.map_err(|e| Error::Wasapi(format!("IAudioClient::Initialize: {e}")))?;
    Ok(input_format)
}

/// The stream's current effect list, via `IAudioEffectsManager` (Windows 11
/// build 22000+; errors on older Windows).
fn query_effects(client: &IAudioClient) -> Result<Vec<AUDIO_EFFECT>> {
    let mgr: IAudioEffectsManager = unsafe {
        client
            .GetService()
            .map_err(|e| Error::Wasapi(format!("IAudioEffectsManager unavailable: {e}")))?
    };
    let mut ptr: *mut AUDIO_EFFECT = std::ptr::null_mut();
    let mut count: u32 = 0;
    unsafe {
        mgr.GetAudioEffects(&mut ptr, &mut count)
            .map_err(|e| Error::Wasapi(format!("GetAudioEffects: {e}")))?
    };
    let list = if ptr.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(ptr, count as usize) }.to_vec()
    };
    if !ptr.is_null() {
        unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(ptr as *const _)) };
    }
    Ok(list)
}

fn os_aec_active(effects: &[AUDIO_EFFECT]) -> bool {
    effects
        .iter()
        .any(|e| e.id == AUDIO_EFFECT_TYPE_AEC && e.state == AUDIO_EFFECT_STATE_ON)
}

// ─── Capture loop (shared between mic + loopback) ─────────────────────────────

#[allow(clippy::too_many_arguments)]
fn run_capture_thread(
    device: IMMDevice,
    loopback: bool,
    writer_slot: Arc<Mutex<Option<WavFrameWriter>>>,
    tee_slot: Arc<Mutex<Option<TeeSender>>>,
    tee_sent: Arc<std::sync::atomic::AtomicU64>,
    open_chunk: Arc<Mutex<Option<u32>>>,
    chunk_clock: Arc<crate::capture::ChunkClock>,
    voice: Option<VoiceMode>,
    stop: Arc<AtomicBool>,
    ready_tx: Option<mpsc::Sender<Result<()>>>,
    // ready_sent: set once WASAPI init succeeded and the ready signal was
    // sent; the spawner uses it to distinguish a post-ready death (report
    // to the supervisor) from an init failure (handled synchronously).
    ready_sent: Option<Arc<AtomicBool>>,
) -> Result<()> {
    let (audio_client, input_format, os_voice_processed) =
        open_capture_client(&device, loopback, voice.as_ref())?;
    if let Some(v) = &voice {
        v.flag.store(os_voice_processed, Ordering::SeqCst);
        log::info!(
            "WASAPI mic capture path: {}",
            if os_voice_processed {
                "OS voice-processed (communications mode, AEC active)"
            } else {
                "raw"
            }
        );
    }

    let capture_client: IAudioCaptureClient = unsafe {
        audio_client
            .GetService::<IAudioCaptureClient>()
            .map_err(|e| Error::Wasapi(format!("IAudioClient::GetService: {e}")))?
    };

    unsafe {
        audio_client
            .Start()
            .map_err(|e| Error::Wasapi(format!("IAudioClient::Start: {e}")))?;
    }

    // Loopback streams start a render client that pushes silence.
    // Failure to set up this renderer is non-fatal and is logged.
    let silence = if loopback {
        match SilenceRenderer::setup(&device) {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!(
                    "WASAPI silence renderer setup failed; loopback may stall on silent endpoints: {e}"
                );
                None
            }
        }
    } else {
        None
    };

    if let Some(tx) = ready_tx.as_ref() {
        let _ = tx.send(Ok(()));
    }
    if let Some(flag) = ready_sent.as_ref() {
        flag.store(true, Ordering::SeqCst);
    }

    let mut engine =
        ResampleEngine::new(input_format.sample_rate, tee_sent, open_chunk, chunk_clock);

    // Poll loop at a 5 ms interval.
    let poll = Duration::from_millis(5);
    let mut last_capture: Instant = Instant::now();
    // Frame-delivery diagnostic: tracks the first real frame and warns once
    // if it is slow.
    let started_at = Instant::now();
    let track_label = if loopback { "system" } else { "mic" };
    let mut first_data_seen = false;
    // Reused per-packet decode scratch; the capture loop must not allocate.
    let mut mono_scratch: Vec<f32> = Vec::new();
    let mut warned_slow_start = false;
    let device_rate = input_format.sample_rate.max(1) as u64;
    let mut packet_gap = crate::packet_gap::PacketGap::new(input_format.sample_rate);
    let mut uncorroborated_jumps: u64 = 0;
    // Gaps filled this stream and their total device frames.
    let mut fills: u64 = 0;
    let mut filled_frames: u64 = 0;
    // Longest pause between capture-loop service points since the last packet.
    let mut last_poll = Instant::now();
    let mut longest_poll_pause = Duration::ZERO;
    // Mic frames in the current run of SILENT-flagged packets.
    let mut silent_run_frames: u64 = 0;
    while !stop.load(Ordering::SeqCst) {
        longest_poll_pause = longest_poll_pause.max(last_poll.elapsed());
        last_poll = Instant::now();
        // Keeps the render endpoint's clock ticking on loopback streams.
        // Pushed via AUDCLNT_BUFFERFLAGS_SILENT; WASAPI ignores buffer
        // contents. Errors here degrade timing, never abort the capture.
        if let Some(s) = silence.as_ref() {
            if let Err(e) = s.push() {
                log::debug!("silence push: {e}");
            }
        }

        let packet_size = unsafe {
            capture_client
                .GetNextPacketSize()
                .map_err(|e| Error::Wasapi(format!("GetNextPacketSize: {e}")))?
        };
        if packet_size == 0 {
            // No data — give the audio driver time to fill the buffer.
            thread::sleep(poll);
            // Warn once if nothing has arrived within 6 s of start.
            if !first_data_seen && !warned_slow_start && started_at.elapsed() > Duration::from_secs(6)
            {
                log::warn!(
                    "WASAPI {track_label} capture: no frames within 6s of start — \
                     proceeding (delayed/contended)"
                );
                warned_slow_start = true;
            }
            // Failsafe: 30 s without data is logged as a dead device.
            if last_capture.elapsed() > Duration::from_secs(30) {
                log::warn!("WASAPI capture: 30 s without data; continuing");
                last_capture = Instant::now();
            }
            continue;
        }

        // Drain everything currently available before sleeping again.
        loop {
            longest_poll_pause = longest_poll_pause.max(last_poll.elapsed());
            last_poll = Instant::now();
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut num_frames: u32 = 0;
            let mut flags: u32 = 0;
            let mut device_pos: u64 = 0;
            let mut qpc_pos: u64 = 0;
            let hr = unsafe {
                capture_client.GetBuffer(
                    &mut data,
                    &mut num_frames,
                    &mut flags,
                    Some(&mut device_pos),
                    Some(&mut qpc_pos),
                )
            };
            if let Err(e) = hr {
                return Err(Error::Wasapi(format!("GetBuffer: {e}")));
            }
            if num_frames == 0 {
                break;
            }
            if !first_data_seen {
                first_data_seen = true;
                log::info!(
                    "WASAPI {track_label} capture: first frames after {}ms",
                    started_at.elapsed().as_millis()
                );
            }

            // Capture time of the packet's first frame; 0 when WASAPI flags
            // it unreliable. Device frames lost before this packet are filled
            // with silence.
            let qpc_ns = if (flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32) != 0 {
                0
            } else {
                qpc_pos.saturating_mul(100)
            };
            let discontinuity = (flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32) != 0;
            let pause_ms = longest_poll_pause.as_millis();
            match packet_gap.observe(device_pos, num_frames, qpc_ns) {
                Gap::None => {}
                Gap::Lost(lost) => {
                    let secs = lost as f64 / device_rate as f64;
                    if lost <= MAX_FILL_SECS * device_rate {
                        if fills < MAX_FILL_LOGS {
                            log::warn!(
                                "WASAPI {track_label} capture: {lost} device frames ({secs:.2}s) lost before this packet; filled with silence (discontinuity flag: {discontinuity}, longest capture-loop pause: {pause_ms} ms, client buffer: {CLIENT_BUFFER_MS} ms)"
                            );
                        }
                        fills += 1;
                        filled_frames += lost;
                        engine.push_silence(lost);
                    } else {
                        log::warn!(
                            "WASAPI {track_label} capture: {lost} device frames ({secs:.0}s) lost before this packet; over the {MAX_FILL_SECS}s fill cap, not filled (discontinuity flag: {discontinuity}, longest capture-loop pause: {pause_ms} ms)"
                        );
                    }
                }
                Gap::Uncorroborated(jump) => {
                    if uncorroborated_jumps == 0 {
                        log::warn!(
                            "WASAPI {track_label} capture: device position jumped {jump} frames past the previous packet without matching QPC time; not filled (discontinuity flag: {discontinuity})"
                        );
                    }
                    uncorroborated_jumps += 1;
                }
            }
            longest_poll_pause = Duration::ZERO;

            let silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
            if !loopback {
                if silent {
                    silent_run_frames += num_frames as u64;
                } else {
                    log_silent_run(silent_run_frames, device_rate);
                    silent_run_frames = 0;
                }
            }
            decode_buffer(&input_format, data, num_frames, silent, &mut mono_scratch);

            // Feed the resampler chunk-by-chunk; spillover is buffered.
            engine.push_samples(&mono_scratch);
            engine.drain_into(&writer_slot, &tee_slot, qpc_ns);

            let release = unsafe { capture_client.ReleaseBuffer(num_frames) };
            if let Err(e) = release {
                return Err(Error::Wasapi(format!("ReleaseBuffer: {e}")));
            }

            last_capture = Instant::now();

            let next = unsafe {
                capture_client
                    .GetNextPacketSize()
                    .map_err(|e| Error::Wasapi(format!("GetNextPacketSize: {e}")))?
            };
            if next == 0 {
                break;
            }
        }
    }

    log_silent_run(silent_run_frames, device_rate);
    if fills > MAX_FILL_LOGS {
        log::warn!(
            "WASAPI {track_label} capture: {fills} device-position gaps filled with {:.2}s of silence this stream",
            filled_frames as f64 / device_rate as f64
        );
    }
    if uncorroborated_jumps > 1 {
        log::warn!(
            "WASAPI {track_label} capture: {uncorroborated_jumps} device-position jumps without matching QPC time this stream; none filled"
        );
    }
    // Flush whatever resampled output is still queued; the WAV ends cleanly.
    engine.flush_into(&writer_slot, &tee_slot);

    unsafe {
        let _ = audio_client.Stop();
    }
    drop(silence); // Stops the render-silence client (see SilenceRenderer::Drop).
    Ok(())
}

/// Logs a mic run of SILENT-flagged packets lasting at least one second.
fn log_silent_run(frames: u64, device_rate: u64) {
    if frames >= device_rate {
        log::info!(
            "WASAPI mic capture: {:.1}s of SILENT-flagged packets",
            frames as f64 / device_rate as f64
        );
    }
}

// ─── Loopback silence renderer ───────────────────────────────────────────────
//
// A render IAudioClient on the same device pushes silence-tagged buffers
// each capture-poll iteration. This client has no audio category.

struct SilenceRenderer {
    client: IAudioClient,
    renderer: IAudioRenderClient,
    buffer_frames: u32,
}

impl SilenceRenderer {
    fn setup(device: &IMMDevice) -> Result<Self> {
        let client: IAudioClient = unsafe {
            device
                .Activate::<IAudioClient>(CLSCTX_ALL, None)
                .map_err(|e| Error::Wasapi(format!("silence Activate: {e}")))?
        };
        let mix_fmt_ptr = unsafe {
            client
                .GetMixFormat()
                .map_err(|e| Error::Wasapi(format!("silence GetMixFormat: {e}")))?
        };

        // 100 ms render buffer, topped up every 5 ms of capture polling.
        let buffer_duration_hns: i64 = 100 * 10_000;
        let init_result = unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                0,
                buffer_duration_hns,
                0,
                mix_fmt_ptr,
                None,
            )
        };
        unsafe {
            windows::Win32::System::Com::CoTaskMemFree(Some(mix_fmt_ptr as *const _));
        }
        init_result.map_err(|e| Error::Wasapi(format!("silence Initialize: {e}")))?;

        let renderer: IAudioRenderClient = unsafe {
            client
                .GetService::<IAudioRenderClient>()
                .map_err(|e| Error::Wasapi(format!("silence GetService: {e}")))?
        };
        let buffer_frames = unsafe {
            client
                .GetBufferSize()
                .map_err(|e| Error::Wasapi(format!("silence GetBufferSize: {e}")))?
        };
        unsafe {
            client
                .Start()
                .map_err(|e| Error::Wasapi(format!("silence Start: {e}")))?;
        }

        Ok(Self {
            client,
            renderer,
            buffer_frames,
        })
    }

    fn push(&self) -> Result<()> {
        let padding = unsafe {
            self.client
                .GetCurrentPadding()
                .map_err(|e| Error::Wasapi(format!("GetCurrentPadding: {e}")))?
        };
        let avail = self.buffer_frames.saturating_sub(padding);
        if avail == 0 {
            return Ok(());
        }
        let _buf = unsafe {
            self.renderer
                .GetBuffer(avail)
                .map_err(|e| Error::Wasapi(format!("silence GetBuffer: {e}")))?
        };
        unsafe {
            self.renderer
                .ReleaseBuffer(avail, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)
                .map_err(|e| Error::Wasapi(format!("silence ReleaseBuffer: {e}")))?;
        }
        Ok(())
    }
}

impl Drop for SilenceRenderer {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

// ─── Wave format parsing ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct InputFormat {
    sample_rate: u32,
    channels: u16,
    sample_kind: SampleKind,
    bytes_per_frame: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SampleKind {
    Float32,
    Pcm16,
}

fn parse_wave_format(ptr: *const WAVEFORMATEX) -> Result<InputFormat> {
    // Each field of the packed WAVEFORMATEX is read through a raw pointer with
    // `read_unaligned`, then operated on as an aligned local copy.
    use std::ptr::{addr_of, read_unaligned};
    let tag = unsafe { read_unaligned(addr_of!((*ptr).wFormatTag)) } as u32;
    let channels = unsafe { read_unaligned(addr_of!((*ptr).nChannels)) };
    let sample_rate = unsafe { read_unaligned(addr_of!((*ptr).nSamplesPerSec)) };
    let bits = unsafe { read_unaligned(addr_of!((*ptr).wBitsPerSample)) };
    let bytes_per_frame = unsafe { read_unaligned(addr_of!((*ptr).nBlockAlign)) } as usize;

    let sample_kind = if tag == WAVE_FORMAT_IEEE_FLOAT && bits == 32 {
        SampleKind::Float32
    } else if tag == WAVE_FORMAT_PCM && bits == 16 {
        SampleKind::Pcm16
    } else if tag == WAVE_FORMAT_EXTENSIBLE {
        // Same packed-struct dance for the WAVEFORMATEXTENSIBLE.SubFormat GUID.
        let wext_ptr = ptr as *const WAVEFORMATEXTENSIBLE;
        let sub_format: GUID = unsafe { read_unaligned(addr_of!((*wext_ptr).SubFormat)) };
        if sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT && bits == 32 {
            SampleKind::Float32
        } else if sub_format == KSDATAFORMAT_SUBTYPE_PCM && bits == 16 {
            SampleKind::Pcm16
        } else {
            return Err(Error::Wasapi(format!(
                "unsupported WAVEFORMATEXTENSIBLE: SubFormat={sub_format:?} bits={bits}"
            )));
        }
    } else {
        return Err(Error::Wasapi(format!(
            "unsupported wave format: tag=0x{tag:04x} bits={bits}"
        )));
    };

    Ok(InputFormat {
        sample_rate,
        channels,
        sample_kind,
        bytes_per_frame,
    })
}

/// Decode `num_frames` of interleaved input bytes into a Vec<f32> of mono
/// samples (channel-average if multichannel). Returns silence when the
/// AUDCLNT_BUFFERFLAGS_SILENT flag is set.
/// Decode one WASAPI packet into `out` (cleared first): downmixed mono f32.
/// `out` is a caller-owned scratch buffer.
fn decode_buffer(
    fmt: &InputFormat,
    data: *const u8,
    num_frames: u32,
    silent: bool,
    out: &mut Vec<f32>,
) {
    out.clear();
    let n_samples_per_chan = num_frames as usize;
    // A null data pointer is treated as silence regardless of the flag.
    if silent || data.is_null() {
        out.resize(n_samples_per_chan, 0.0);
        return;
    }
    let n_total_samples = n_samples_per_chan * fmt.channels as usize;
    out.reserve(n_samples_per_chan);
    let inv_chans = 1.0 / fmt.channels as f32;

    match fmt.sample_kind {
        SampleKind::Float32 => {
            let total_bytes = n_total_samples * 4;
            let slice = unsafe { std::slice::from_raw_parts(data as *const f32, n_total_samples) };
            // Only the bytes the format declared are read; total_bytes is
            // implied by num_frames * nBlockAlign, which equals num_frames *
            // channels * sizeof(f32) for a 32-bit float stream.
            debug_assert_eq!(total_bytes, n_samples_per_chan * fmt.bytes_per_frame);
            for frame_idx in 0..n_samples_per_chan {
                let base = frame_idx * fmt.channels as usize;
                let mut sum = 0.0f32;
                for c in 0..fmt.channels as usize {
                    sum += slice[base + c];
                }
                out.push(sum * inv_chans);
            }
        }
        SampleKind::Pcm16 => {
            let slice = unsafe { std::slice::from_raw_parts(data as *const i16, n_total_samples) };
            let denom = i16::MAX as f32;
            for frame_idx in 0..n_samples_per_chan {
                let base = frame_idx * fmt.channels as usize;
                let mut sum = 0.0f32;
                for c in 0..fmt.channels as usize {
                    sum += slice[base + c] as f32 / denom;
                }
                out.push(sum * inv_chans);
            }
        }
    }
}

// ─── Resample + frame-accumulate engine ───────────────────────────────────────

struct ResampleEngine {
    /// The shared device-rate→16 kHz mono frame core (same one the macOS
    /// backend uses).
    core: crate::resample::FrameResampler,
    /// Whole frames from the core awaiting emission.
    pending: Vec<Vec<i16>>,
    /// Per-chunk capture timing, shared with the recorder.
    chunk_clock: Arc<crate::capture::ChunkClock>,
    /// Monotonic nanoseconds for the next frame `emit_frames` will produce.
    /// Advanced per frame; zero when the device reports no QPC position.
    stamp_ns: u64,
    /// Stream clock: samples delivered to the tee (see `TeeFrame`). Shared
    /// with the control loop.
    tee_sent: Arc<std::sync::atomic::AtomicU64>,
    open_chunk: Arc<Mutex<Option<u32>>>,
}

impl ResampleEngine {
    fn new(
        input_rate: u32,
        tee_sent: Arc<std::sync::atomic::AtomicU64>,
        open_chunk: Arc<Mutex<Option<u32>>>,
        chunk_clock: Arc<crate::capture::ChunkClock>,
    ) -> Self {
        Self {
            // Mono: decode_buffer downmixes before the engine.
            core: crate::resample::FrameResampler::new(input_rate, 1),
            pending: Vec::new(),
            chunk_clock,
            stamp_ns: 0,
            tee_sent,
            open_chunk,
        }
    }

    fn push_samples(&mut self, mono: &[f32]) {
        self.pending.extend(self.core.push(mono));
    }

    /// Pushes `frames` device-rate frames of silence.
    fn push_silence(&mut self, frames: u64) {
        const ZEROS: [f32; 1024] = [0.0; 1024];
        let mut left = frames;
        while left > 0 {
            let n = left.min(ZEROS.len() as u64) as usize;
            self.pending.extend(self.core.push(&ZEROS[..n]));
            left -= n as u64;
        }
    }

    /// `host_ns` stamps the first sample of the buffer that produced this
    /// drain; each emitted frame advances by its own duration. Zero when the
    /// device reported no QPC position.
    fn drain_into(
        &mut self,
        writer_slot: &Arc<Mutex<Option<WavFrameWriter>>>,
        tee_slot: &Arc<Mutex<Option<TeeSender>>>,
        host_ns: u64,
    ) {
        if host_ns != 0 && self.stamp_ns == 0 {
            self.stamp_ns = host_ns;
        }
        self.emit_frames(writer_slot, tee_slot);
    }

    /// Drain any held whole frames without waiting for more input.
    fn flush_into(
        &mut self,
        writer_slot: &Arc<Mutex<Option<WavFrameWriter>>>,
        tee_slot: &Arc<Mutex<Option<TeeSender>>>,
    ) {
        self.pending.extend(self.core.flush());
        self.emit_frames(writer_slot, tee_slot);
    }

    fn emit_frames(
        &mut self,
        writer_slot: &Arc<Mutex<Option<WavFrameWriter>>>,
        tee_slot: &Arc<Mutex<Option<TeeSender>>>,
    ) {
        for frame in self.pending.drain(..) {
            // Write primary path (WAV). The tee stamp is taken from the
            // writer in the same step.
            let mut frame_chunk: Option<(u32, u64)> = None;
            {
                let mut guard = writer_slot.lock().unwrap();
                if let Some(w) = guard.as_mut() {
                    if let Err(e) = w.write_frame_stamped(&frame, self.stamp_ns) {
                        log::warn!("frame write failed: {e}");
                    } else if let Some(idx) = *self.open_chunk.lock().unwrap() {
                        frame_chunk =
                            Some((idx, w.samples_written().saturating_sub(frame.len() as u64)));
                        self.chunk_clock.note(self.stamp_ns, &frame);
                    }
                }
            }
            // Tee to live consumer (unbounded: non-blocking, never drops).
            // The clock counts only what was delivered — pause detaches the
            // tee and freezes it, matching the consumer's view.
            {
                let guard = tee_slot.lock().unwrap();
                if let Some(tx) = guard.as_ref() {
                    let at = self.tee_sent.load(Ordering::Relaxed);
                    let n = frame.len() as u64;
                    if tx
                        .send(crate::capture::TeeFrame {
                            start_sample: at,
                            host_ns: self.stamp_ns,
                            samples: frame,
                            chunk: frame_chunk,
                        })
                        .is_ok()
                    {
                        self.tee_sent.store(at + n, Ordering::Relaxed);
                    }
                }
            }
            if self.stamp_ns != 0 {
                self.stamp_ns += FRAME_SAMPLES as u64 * 1_000_000_000 / 16_000;
            }
        }
    }
}

// ─── helpers ──────────────────────────────────────────────────────────────────

pub(crate) fn pcwstr_to_string(p: PCWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { p.to_string().unwrap_or_default() }
}

pub(crate) fn hash_device_id(s: &str) -> u32 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish() as u32
}

pub(crate) fn create_enumerator() -> Result<IMMDeviceEnumerator> {
    unsafe {
        CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|e| Error::Wasapi(format!("CoCreateInstance(MMDeviceEnumerator): {e}")))
    }
}

fn clone_err(e: &Error) -> Error {
    // Reconstruct a similar variant for cross-thread reporting.
    Error::Wasapi(e.to_string())
}

// ─── Mic endpoint volume (auto-gain) ──────────────────────────────────────────
// Each call runs on a short-lived thread that initializes COM itself.

pub(crate) fn endpoint_volume(source_id: u32) -> Option<f32> {
    std::thread::spawn(move || -> Option<f32> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let device = source::find_device(windows::Win32::Media::Audio::eCapture, source_id, "mic").ok()?;
            let vol: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
            vol.GetMasterVolumeLevelScalar().ok()
        }
    })
    .join()
    .ok()
    .flatten()
}

pub(crate) fn set_endpoint_volume(source_id: u32, scalar: f32) -> bool {
    std::thread::spawn(move || -> bool {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let Ok(device) = source::find_device(windows::Win32::Media::Audio::eCapture, source_id, "mic") else { return false };
            let Ok(vol) = device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) else {
                return false;
            };
            vol.SetMasterVolumeLevelScalar(scalar, std::ptr::null()).is_ok()
        }
    })
    .join()
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wfx(tag: u16, channels: u16, sample_rate: u32, bits: u16) -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: tag,
            nChannels: channels,
            nSamplesPerSec: sample_rate,
            nAvgBytesPerSec: sample_rate * channels as u32 * (bits / 8) as u32,
            nBlockAlign: channels * (bits / 8),
            wBitsPerSample: bits,
            cbSize: 0,
        }
    }

    fn effect(id: GUID, on: bool) -> AUDIO_EFFECT {
        AUDIO_EFFECT {
            id,
            canSetState: false.into(),
            state: if on {
                AUDIO_EFFECT_STATE_ON
            } else {
                windows::Win32::Media::Audio::AUDIO_EFFECT_STATE_OFF
            },
        }
    }

    #[test]
    fn os_aec_active_needs_the_aec_effect_present_and_on() {
        assert!(os_aec_active(&[effect(AUDIO_EFFECT_TYPE_AEC, true)]));
        assert!(os_aec_active(&[
            effect(GUID::zeroed(), true),
            effect(AUDIO_EFFECT_TYPE_AEC, true),
        ]));
    }

    #[test]
    fn os_aec_active_rejects_off_absent_and_lookalike_effects() {
        assert!(!os_aec_active(&[]));
        assert!(!os_aec_active(&[effect(AUDIO_EFFECT_TYPE_AEC, false)]));
        // Same family, different effect (noise suppression).
        let ns = GUID::from_u128(0x6f64adbf_8211_11e2_8c70_2c27d7f001fa);
        assert!(!os_aec_active(&[effect(ns, true)]));
    }

    fn extensible(channels: u16, sample_rate: u32, bits: u16, sub: GUID) -> WAVEFORMATEXTENSIBLE {
        let mut fmt = wfx(WAVE_FORMAT_EXTENSIBLE as u16, channels, sample_rate, bits);
        fmt.cbSize = 22;
        WAVEFORMATEXTENSIBLE {
            Format: fmt,
            Samples: windows::Win32::Media::Audio::WAVEFORMATEXTENSIBLE_0 {
                wValidBitsPerSample: bits,
            },
            dwChannelMask: 0,
            SubFormat: sub,
        }
    }

    #[test]
    fn plain_float32_and_pcm16_tags_are_recognised() {
        let f = wfx(WAVE_FORMAT_IEEE_FLOAT as u16, 2, 48000, 32);
        let got = parse_wave_format(&f).expect("float32");
        assert_eq!(got.sample_kind, SampleKind::Float32);
        assert_eq!(got.channels, 2);
        assert_eq!(got.sample_rate, 48000);
        assert_eq!(got.bytes_per_frame, 8);

        let p = wfx(WAVE_FORMAT_PCM as u16, 1, 16000, 16);
        let got = parse_wave_format(&p).expect("pcm16");
        assert_eq!(got.sample_kind, SampleKind::Pcm16);
        assert_eq!(got.bytes_per_frame, 2);
    }

    /// Shared-mode capture usually hands back WAVEFORMATEXTENSIBLE, where the
    /// tag says nothing and only the SubFormat GUID identifies the samples.
    #[test]
    fn extensible_is_resolved_by_its_subformat() {
        let e = extensible(2, 48000, 32, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
        let got = parse_wave_format(&e as *const _ as *const WAVEFORMATEX).expect("float32");
        assert_eq!(got.sample_kind, SampleKind::Float32);

        let e = extensible(1, 44100, 16, KSDATAFORMAT_SUBTYPE_PCM);
        let got = parse_wave_format(&e as *const _ as *const WAVEFORMATEX).expect("pcm16");
        assert_eq!(got.sample_kind, SampleKind::Pcm16);
        assert_eq!(got.sample_rate, 44100);
    }

    /// A format we cannot decode is an error.
    #[test]
    fn unsupported_formats_are_rejected() {
        assert!(parse_wave_format(&wfx(WAVE_FORMAT_PCM as u16, 2, 48000, 24)).is_err());
        assert!(parse_wave_format(&wfx(WAVE_FORMAT_IEEE_FLOAT as u16, 2, 48000, 64)).is_err());
        let e = extensible(2, 48000, 24, KSDATAFORMAT_SUBTYPE_PCM);
        assert!(parse_wave_format(&e as *const _ as *const WAVEFORMATEX).is_err());
    }

    #[test]
    fn float32_stereo_is_averaged_to_mono() {
        let fmt = InputFormat {
            sample_rate: 48000,
            channels: 2,
            sample_kind: SampleKind::Float32,
            bytes_per_frame: 8,
        };
        let frames: [f32; 6] = [1.0, 0.0, 0.5, 0.5, -1.0, 1.0];
        let mut out = Vec::new();
        decode_buffer(&fmt, frames.as_ptr() as *const u8, 3, false, &mut out);
        assert_eq!(out, vec![0.5, 0.5, 0.0]);
    }

    #[test]
    fn pcm16_is_scaled_to_unit_range() {
        let fmt = InputFormat {
            sample_rate: 16000,
            channels: 1,
            sample_kind: SampleKind::Pcm16,
            bytes_per_frame: 2,
        };
        let frames: [i16; 3] = [i16::MAX, 0, -i16::MAX];
        let mut out = Vec::new();
        decode_buffer(&fmt, frames.as_ptr() as *const u8, 3, false, &mut out);
        assert_eq!(out, vec![1.0, 0.0, -1.0]);
    }

    /// WASAPI may hand back a null pointer for an empty packet without setting
    /// the SILENT flag; decode treats it as silence.
    #[test]
    fn null_or_silent_packets_decode_to_silence() {
        let fmt = InputFormat {
            sample_rate: 48000,
            channels: 2,
            sample_kind: SampleKind::Float32,
            bytes_per_frame: 8,
        };
        let mut out = Vec::new();
        decode_buffer(&fmt, std::ptr::null(), 4, false, &mut out);
        assert_eq!(out, vec![0.0; 4], "null pointer must not be dereferenced");

        let frames = [1.0f32; 8];
        decode_buffer(&fmt, frames.as_ptr() as *const u8, 4, true, &mut out);
        assert_eq!(out, vec![0.0; 4], "SILENT must not read the buffer");
    }

    /// `out` is a reused scratch buffer.
    #[test]
    fn decode_clears_the_scratch_buffer() {
        let fmt = InputFormat {
            sample_rate: 48000,
            channels: 1,
            sample_kind: SampleKind::Float32,
            bytes_per_frame: 4,
        };
        let mut out = vec![9.0; 64];
        let frames = [0.25f32; 2];
        decode_buffer(&fmt, frames.as_ptr() as *const u8, 2, false, &mut out);
        assert_eq!(out, vec![0.25, 0.25]);
    }

    #[test]
    fn filled_silence_reaches_the_wav_the_chunk_clock_and_the_tee() {
        // The manifest's audio_samples (WAV), mic_samples (chunk clock) and
        // the live decode (tee) must all count the filled frames.
        let dir = tempfile::tempdir().unwrap();
        let writer_slot = Arc::new(Mutex::new(Some(
            WavFrameWriter::create(&dir.path().join("mic.wav"), 16_000).unwrap(),
        )));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let tee_slot = Arc::new(Mutex::new(Some(tx)));
        let clock = Arc::new(crate::capture::ChunkClock::default());
        let mut e = ResampleEngine::new(
            48_000,
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            Arc::new(Mutex::new(Some(0))),
            Arc::clone(&clock),
        );
        e.push_samples(&[0.5; 4_800]);
        e.push_silence(48_000);
        e.push_samples(&[0.5; 4_800]);
        e.flush_into(&writer_slot, &tee_slot);
        let written = writer_slot.lock().unwrap().as_ref().unwrap().samples_written();
        let mut teed = 0u64;
        while let Ok(f) = rx.try_recv() {
            teed += f.samples.len() as u64;
        }
        let counted = clock.samples.load(Ordering::Relaxed);
        assert!(counted >= 19_200, "the chunk clock counts the 1.2 s, got {counted}");
        assert_eq!(teed, counted);
        // The WAV holds every counted frame plus the unstamped lead pad for the
        // wall time between create and the first write.
        assert!(
            (counted..counted + 16_000).contains(&written),
            "{written} vs {counted}"
        );
    }

    #[test]
    fn push_silence_holds_the_lost_frames_place() {
        // 0.1 s of signal, 1 s lost, 0.1 s of signal at 48 kHz.
        let mut e = ResampleEngine::new(
            48_000,
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            Arc::new(Mutex::new(None)),
            Arc::new(crate::capture::ChunkClock::default()),
        );
        e.push_samples(&[0.5; 4_800]);
        e.push_silence(48_000);
        e.push_samples(&[0.5; 4_800]);
        let mut out: Vec<i16> = e.pending.drain(..).flatten().collect();
        out.extend(e.core.flush().into_iter().flatten());
        assert!(
            (19_200..=19_200 + 2 * FRAME_SAMPLES).contains(&out.len()),
            "16 kHz length {} must cover 1.2 s",
            out.len()
        );
        assert!(out[3_200..16_000].iter().all(|&s| s == 0), "the lost second is silence");
        assert!(out[16_400..19_000].iter().any(|&s| s != 0), "signal resumes after it");
    }
}
