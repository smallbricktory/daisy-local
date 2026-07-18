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
//! `WavFrameWriter` expects.

#![allow(unsafe_code)]

mod mic;
mod source;
mod system;

use crate::capture::{
    DualCaptureOutputs, DualCaptureRequest, StreamingCaptureRequest, StreamingControl,
    StreamingHandle, TeeSender,
};
use crate::error::{Error, Result};
use crate::manifest::{ChannelManifest, RecordingManifest};
use crate::source::{Source, SourceKind};
use crate::wav::{WavFrameWriter, FRAME_SAMPLES};

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::core::{GUID, PCWSTR};
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM,
};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};

pub(crate) const OUTPUT_SAMPLE_RATE: u32 = 16_000;

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
        Arc::clone(&stop),
        None,
    );

    let _ = timer.join();

    // Finalize the writer regardless of capture result.
    if let Some(w) = writer_slot.lock().unwrap().take() {
        let _ = w.close();
    }

    res
}

pub(crate) fn capture_dual_blocking(
    req: DualCaptureRequest,
    out_dir: &Path,
) -> Result<DualCaptureOutputs> {
    std::fs::create_dir_all(out_dir)?;
    let mic_wav = out_dir.join("mic.wav");
    let system_wav = out_dir.join("system.wav");
    let manifest_path = out_dir.join("manifest.json");

    let started = unix_now();

    // Validate IDs and snapshot Source metadata before opening any WASAPI resources.
    let all = list_sources_blocking()?;
    let find = |id: u32| -> Result<Source> {
        all.iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| Error::SourceNotFound(format!("id={id}")))
    };
    let mic_source = find(req.mic_source_id)?;
    let system_source = find(req.system_source_id)?;

    let streaming_req = StreamingCaptureRequest {
        mic_source_id: req.mic_source_id,
        system_source_id: req.system_source_id,
        sample_rate: req.sample_rate,
    };

    let mic_path = mic_wav.clone();
    let sys_path = system_wav.clone();
    let duration = req.duration;

    run_dual_streaming_impl(streaming_req, move |handle| {
        let _ = handle.open_chunk(&mic_path, &sys_path);
        let h = handle.clone();
        thread::spawn(move || {
            thread::sleep(duration);
            let _ = h.stop();
        });
    })?;

    let now_unix = unix_now();
    let manifest = RecordingManifest {
        schema_version: RecordingManifest::SCHEMA,
        started_at_unix_seconds: started,
        duration_seconds: req.duration.as_secs(),
        sample_rate: req.sample_rate,
        channels: 1,
        mic: ChannelManifest {
            source_id: mic_source.id,
            source_node_name: mic_source.node_name.clone(),
            source_description: mic_source.description.clone(),
            wav_path: mic_wav.clone(),
            captured_at_unix_seconds: now_unix,
        },
        system: ChannelManifest {
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

    // Shared per-stream state.
    let mic_writer_slot: Arc<Mutex<Option<WavFrameWriter>>> = Arc::new(Mutex::new(None));
    let sys_writer_slot: Arc<Mutex<Option<WavFrameWriter>>> = Arc::new(Mutex::new(None));
    let mic_tee_slot: Arc<Mutex<Option<TeeSender>>> = Arc::new(Mutex::new(None));
    let sys_tee_slot: Arc<Mutex<Option<TeeSender>>> = Arc::new(Mutex::new(None));
    let mut mic_stop = Arc::new(AtomicBool::new(false));
    let sys_stop = Arc::new(AtomicBool::new(false));
    // Tracked; a failed mic switch recovers the previous device.
    let mut current_mic_source = mic_source.clone();

    let mic_tee_sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let sys_tee_sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut mic_join: Option<JoinHandle<Result<()>>> = Some(spawn_capture_thread(
        "daisy-wasapi-mic",
        mic_source.clone(),
        Arc::clone(&mic_writer_slot),
        Arc::clone(&mic_tee_slot),
        Arc::clone(&mic_tee_sent),
        Arc::clone(&mic_stop),
    )?);
    let sys_join = spawn_capture_thread(
        "daisy-wasapi-system",
        system_source.clone(),
        Arc::clone(&sys_writer_slot),
        Arc::clone(&sys_tee_slot),
        Arc::clone(&sys_tee_sent),
        Arc::clone(&sys_stop),
    )?;

    let (control_tx, control_rx) = mpsc::channel::<StreamingControl>();
    let handle = StreamingHandle::new_windows(control_tx);
    on_ready(handle);

    // Drive the control loop until Stop.
    let sample_rate = req.sample_rate;
    while let Ok(cmd) = control_rx.recv() {
        match cmd {
            StreamingControl::OpenChunk { mic_wav, system_wav } => {
                rotate_writer(&mic_writer_slot, &mic_wav, sample_rate);
                rotate_writer(&sys_writer_slot, &system_wav, sample_rate);
            }
            StreamingControl::CloseChunk => {
                close_writer(&mic_writer_slot);
                close_writer(&sys_writer_slot);
            }
            StreamingControl::SetMicTee(tx) => {
                *mic_tee_slot.lock().unwrap() = tx;
            }
            StreamingControl::SetSystemTee(tx) => {
                *sys_tee_slot.lock().unwrap() = tx;
            }
            StreamingControl::SwitchMic(source) => {
                // Stop + join the old mic thread, then start a new one on the new
                // device sharing the SAME writer + tee (chunk continues). System
                // thread untouched. Fail-safe: if the new device won't start,
                // recover the previous one.
                mic_stop.store(true, Ordering::SeqCst);
                if let Some(j) = mic_join.take() {
                    let _ = join_capture("mic(switch)", j);
                }
                let spawn = |src: Source, stop: &Arc<AtomicBool>| {
                    spawn_capture_thread(
                        "daisy-wasapi-mic",
                        src,
                        Arc::clone(&mic_writer_slot),
                        Arc::clone(&mic_tee_slot),
                        Arc::clone(&mic_tee_sent),
                        Arc::clone(stop),
                    )
                };
                let new_stop = Arc::new(AtomicBool::new(false));
                match spawn(source.clone(), &new_stop) {
                    Ok(j) => {
                        mic_join = Some(j);
                        mic_stop = new_stop;
                        current_mic_source = source.clone();
                        log::info!("SwitchMic(win): mic now {}", source.node_name);
                    }
                    Err(e) => {
                        log::error!(
                            "SwitchMic(win): start on {} failed — recovering previous mic: {e}",
                            source.node_name
                        );
                        let rec_stop = Arc::new(AtomicBool::new(false));
                        match spawn(current_mic_source.clone(), &rec_stop) {
                            Ok(j) => {
                                mic_join = Some(j);
                                mic_stop = rec_stop;
                            }
                            Err(e2) => {
                                log::error!("SwitchMic(win): recovery failed — mic stopped: {e2}");
                            }
                        }
                    }
                }
            }
            StreamingControl::Stop => {
                close_writer(&mic_writer_slot);
                close_writer(&sys_writer_slot);
                break;
            }
            StreamingControl::SetMicMuted(_) => {
                // Windows mutes via the OS input gain (WASAPI honors it); the
                // recorder's set_input_gain call handles it.
            }
        }
    }

    // Tell capture threads to exit, join them, surface any errors.
    mic_stop.store(true, Ordering::SeqCst);
    sys_stop.store(true, Ordering::SeqCst);
    let mic_res = match mic_join.take() {
        Some(j) => join_capture("mic", j),
        None => Ok(()),
    };
    let sys_res = join_capture("system", sys_join);

    mic_res?;
    sys_res?;
    Ok(())
}

fn rotate_writer(
    slot: &Arc<Mutex<Option<WavFrameWriter>>>,
    path: &Path,
    sample_rate: u32,
) {
    let mut guard = slot.lock().unwrap();
    if let Some(old) = guard.take() {
        if let Err(e) = old.close() {
            log::warn!("close old writer: {e}");
        }
    }
    match WavFrameWriter::create(path, sample_rate) {
        Ok(w) => *guard = Some(w),
        Err(e) => log::error!("open chunk {:?}: {e}", path),
    }
}

fn close_writer(slot: &Arc<Mutex<Option<WavFrameWriter>>>) {
    if let Some(w) = slot.lock().unwrap().take() {
        if let Err(e) = w.close() {
            log::warn!("close writer: {e}");
        }
    }
}

fn spawn_capture_thread(
    name: &str,
    source: Source,
    writer_slot: Arc<Mutex<Option<WavFrameWriter>>>,
    tee_slot: Arc<Mutex<Option<TeeSender>>>,
    tee_sent: Arc<std::sync::atomic::AtomicU64>,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<Result<()>>> {
    let loopback = source.kind == SourceKind::Monitor;
    let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
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
            run_capture_thread(device, loopback, writer_slot, tee_slot, tee_sent, stop, Some(ready_tx))
        })
        .map_err(|e| Error::PipeWire(format!("spawn capture thread: {e}")))?;

    // Block until the capture loop confirms init success or surfaces an error.
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(join),
        Ok(Err(e)) => {
            // The thread already returned its own copy of this error.
            let _ = join.join();
            Err(e)
        }
        Err(_) => {
            // No ready signal in 5 s — capture thread is wedged. Best-effort abort.
            Err(Error::PipeWire(format!(
                "{name}: WASAPI init timed out (no ready signal in 5 s)"
            )))
        }
    }
}

fn join_capture(label: &str, join: JoinHandle<Result<()>>) -> Result<()> {
    match join.join() {
        Ok(r) => r,
        Err(_) => Err(Error::PipeWire(format!("{label} capture thread panicked"))),
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
                return Err(Error::PipeWire(format!(
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
    // The "wasapi-loopback" sentinel (from `VirtualSink::monitor_source_name`
    // on Windows) means "the default render endpoint, captured in loopback
    // mode."  Any other Monitor goes through the listed-device lookup path.
    if source.kind == SourceKind::Monitor && source.node_name == "wasapi-loopback" {
        return system::default_render_device();
    }
    match source.kind {
        SourceKind::Mic => mic::find_capture_device(source.id),
        SourceKind::Monitor => system::find_render_device(source.id),
    }
}

// ─── Capture loop (shared between mic + loopback) ─────────────────────────────

fn run_capture_thread(
    device: IMMDevice,
    loopback: bool,
    writer_slot: Arc<Mutex<Option<WavFrameWriter>>>,
    tee_slot: Arc<Mutex<Option<TeeSender>>>,
    tee_sent: Arc<std::sync::atomic::AtomicU64>,
    stop: Arc<AtomicBool>,
    ready_tx: Option<mpsc::Sender<Result<()>>>,
) -> Result<()> {
    // Activate IAudioClient.
    let audio_client: IAudioClient = unsafe {
        device
            .Activate::<IAudioClient>(CLSCTX_ALL, None)
            .map_err(|e| Error::PipeWire(format!("IMMDevice::Activate(IAudioClient): {e}")))?
    };

    // Query the device's native mix format. Any of PCM int16 / IEEE float32,
    // mono or stereo, is accepted and converted here.
    let mix_format_ptr = unsafe {
        audio_client
            .GetMixFormat()
            .map_err(|e| Error::PipeWire(format!("IAudioClient::GetMixFormat: {e}")))?
    };
    let input_format = parse_wave_format(mix_format_ptr)?;

    let mut stream_flags: u32 = 0;
    if loopback {
        stream_flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
    }

    // 200 ms WASAPI ring buffer.  Long enough to absorb scheduling jitter on
    // a desktop Windows box; short enough that the buffer-overflow path is
    // unreachable under normal load.
    let buffer_duration_hns: i64 = 200 * 10_000;

    let init_result = unsafe {
        audio_client.Initialize(
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
    init_result.map_err(|e| Error::PipeWire(format!("IAudioClient::Initialize: {e}")))?;

    let capture_client: IAudioCaptureClient = unsafe {
        audio_client
            .GetService::<IAudioCaptureClient>()
            .map_err(|e| Error::PipeWire(format!("IAudioClient::GetService: {e}")))?
    };

    unsafe {
        audio_client
            .Start()
            .map_err(|e| Error::PipeWire(format!("IAudioClient::Start: {e}")))?;
    }

    // Loopback streams need a co-existing render client pushing silence to
    // keeps WASAPI's clock alive; loopback capture delivers zero buffers
    // when nothing else is rendering on the endpoint. Failure to set up
    // this renderer is non-fatal and is logged.
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

    let mut engine = ResampleEngine::new(input_format.sample_rate, OUTPUT_SAMPLE_RATE, tee_sent)?;

    // Poll loop. WASAPI buffers fill on ~10 ms cadence; 5 ms keeps latency
    // tight without spinning.
    let poll = Duration::from_millis(5);
    let mut last_capture: Instant = Instant::now();
    // Frame-delivery diagnostic (parity with the macOS tap): the ready signal
    // fires after Start(), but Start() succeeding doesn't guarantee frames are
    // flowing. Track the first real frame and warn (once, non-blocking) if it's
    // slow — the back-to-back-capture contention signature.
    let started_at = Instant::now();
    let track_label = if loopback { "system" } else { "mic" };
    let mut first_data_seen = false;
    let mut warned_slow_start = false;
    while !stop.load(Ordering::SeqCst) {
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
                .map_err(|e| Error::PipeWire(format!("GetNextPacketSize: {e}")))?
        };
        if packet_size == 0 {
            // No data — give the audio driver time to fill the buffer.
            thread::sleep(poll);
            // First-frame contention signal: warn once if nothing has arrived
            // within 6 s of start (a busy/contended audio stack delaying the
            // stream — the back-to-back capture starvation).
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
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut num_frames: u32 = 0;
            let mut flags: u32 = 0;
            let hr = unsafe {
                capture_client.GetBuffer(&mut data, &mut num_frames, &mut flags, None, None)
            };
            if let Err(e) = hr {
                return Err(Error::PipeWire(format!("GetBuffer: {e}")));
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

            let silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
            let mono = decode_buffer(&input_format, data, num_frames, silent);

            // Feed the resampler chunk-by-chunk; spillover is buffered.
            engine.push_samples(&mono);
            engine.drain_into(&writer_slot, &tee_slot)?;

            let release = unsafe { capture_client.ReleaseBuffer(num_frames) };
            if let Err(e) = release {
                return Err(Error::PipeWire(format!("ReleaseBuffer: {e}")));
            }

            last_capture = Instant::now();

            let next = unsafe {
                capture_client
                    .GetNextPacketSize()
                    .map_err(|e| Error::PipeWire(format!("GetNextPacketSize: {e}")))?
            };
            if next == 0 {
                break;
            }
        }
    }

    // Flush whatever resampled output is still queued; the WAV ends cleanly.
    engine.flush_into(&writer_slot, &tee_slot)?;

    unsafe {
        let _ = audio_client.Stop();
    }
    drop(silence); // Stops the render-silence client (see SilenceRenderer::Drop).
    Ok(())
}

// ─── Loopback silence renderer ───────────────────────────────────────────────
//
// WASAPI loopback only delivers capture buffers when something is rendering
// on the endpoint. A co-existing render IAudioClient on the same device
// pushes silence-tagged buffers each capture-poll iteration, keeping buffers
// (and the WASAPI clock) flowing on otherwise silent endpoints. With the
// SILENT flag, WASAPI substitutes silence regardless of buffer contents.

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
                .map_err(|e| Error::PipeWire(format!("silence Activate: {e}")))?
        };
        let mix_fmt_ptr = unsafe {
            client
                .GetMixFormat()
                .map_err(|e| Error::PipeWire(format!("silence GetMixFormat: {e}")))?
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
        init_result.map_err(|e| Error::PipeWire(format!("silence Initialize: {e}")))?;

        let renderer: IAudioRenderClient = unsafe {
            client
                .GetService::<IAudioRenderClient>()
                .map_err(|e| Error::PipeWire(format!("silence GetService: {e}")))?
        };
        let buffer_frames = unsafe {
            client
                .GetBufferSize()
                .map_err(|e| Error::PipeWire(format!("silence GetBufferSize: {e}")))?
        };
        unsafe {
            client
                .Start()
                .map_err(|e| Error::PipeWire(format!("silence Start: {e}")))?;
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
                .map_err(|e| Error::PipeWire(format!("GetCurrentPadding: {e}")))?
        };
        let avail = self.buffer_frames.saturating_sub(padding);
        if avail == 0 {
            return Ok(());
        }
        let _buf = unsafe {
            self.renderer
                .GetBuffer(avail)
                .map_err(|e| Error::PipeWire(format!("silence GetBuffer: {e}")))?
        };
        unsafe {
            self.renderer
                .ReleaseBuffer(avail, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)
                .map_err(|e| Error::PipeWire(format!("silence ReleaseBuffer: {e}")))?;
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
    // WAVEFORMATEX is `#[repr(C, packed)]` in windows-rs; `&wfx.field`
    // references are unsound. Each field is read through a raw pointer with
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
            return Err(Error::PipeWire(format!(
                "unsupported WAVEFORMATEXTENSIBLE: SubFormat={sub_format:?} bits={bits}"
            )));
        }
    } else {
        return Err(Error::PipeWire(format!(
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
/// AUDCLNT_BUFFERFLAGS_SILENT flag is set rather than reading the buffer
/// (per WASAPI docs the buffer contents are undefined on silence).
fn decode_buffer(
    fmt: &InputFormat,
    data: *const u8,
    num_frames: u32,
    silent: bool,
) -> Vec<f32> {
    let n_samples_per_chan = num_frames as usize;
    // WASAPI can hand back a NULL data pointer for an empty/silent packet
    // without always setting AUDCLNT_BUFFERFLAGS_SILENT — driver-dependent, and
    // common on loopback when nothing is playing. Reading it dereferences null
    // (observed crash: 0xc0000005 at 0x8 = `slice[ch]` off a null base a frame
    // into capture). Treat a null pointer as silence regardless of the flag.
    if silent || data.is_null() {
        return vec![0.0; n_samples_per_chan];
    }
    let n_total_samples = n_samples_per_chan * fmt.channels as usize;
    let mut out = Vec::with_capacity(n_samples_per_chan);
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
    out
}

// ─── Resample + frame-accumulate engine ───────────────────────────────────────

struct ResampleEngine {
    resampler: Option<rubato::FastFixedIn<f32>>,
    /// Pending mono f32 input samples at the device-native sample rate,
    /// buffered in front of the resampler until a full `chunk_size_in` is
    /// available.
    input_buf: Vec<f32>,
    /// Pending i16 mono samples at OUTPUT_SAMPLE_RATE waiting to be emitted
    /// as 320-sample frames.
    output_accum: Vec<i16>,
    /// Stream clock: samples delivered to the tee (see `TeeFrame`). Shared
    /// with the control loop so a mid-call mic switch (new thread, new
    /// engine) keeps the clock monotonic.
    tee_sent: Arc<std::sync::atomic::AtomicU64>,
    input_chunk_size: usize,
    /// True when input rate already equals output rate; rubato is bypassed.
    passthrough: bool,
}

impl ResampleEngine {
    fn new(input_rate: u32, output_rate: u32, tee_sent: Arc<std::sync::atomic::AtomicU64>) -> Result<Self> {
        if input_rate == output_rate {
            return Ok(Self {
                resampler: None,
                input_buf: Vec::new(),
                output_accum: Vec::with_capacity(FRAME_SAMPLES * 4),
                tee_sent: Arc::clone(&tee_sent),
                input_chunk_size: 0,
                passthrough: true,
            });
        }
        let chunk = 1024usize;
        let resampler = rubato::FastFixedIn::<f32>::new(
            output_rate as f64 / input_rate as f64,
            1.0,
            rubato::PolynomialDegree::Linear,
            chunk,
            1,
        )
        .map_err(|e| Error::PipeWire(format!("FastFixedIn::new: {e}")))?;
        Ok(Self {
            resampler: Some(resampler),
            input_buf: Vec::with_capacity(chunk * 4),
            output_accum: Vec::with_capacity(FRAME_SAMPLES * 4),
            tee_sent,
            input_chunk_size: chunk,
            passthrough: false,
        })
    }

    fn push_samples(&mut self, mono: &[f32]) {
        if self.passthrough {
            // Convert f32 directly to i16; no buffering at the resample stage.
            for &s in mono {
                self.output_accum.push(f32_to_i16(s));
            }
        } else {
            self.input_buf.extend_from_slice(mono);
        }
    }

    fn drain_into(
        &mut self,
        writer_slot: &Arc<Mutex<Option<WavFrameWriter>>>,
        tee_slot: &Arc<Mutex<Option<TeeSender>>>,
    ) -> Result<()> {
        if let Some(rs) = self.resampler.as_mut() {
            use rubato::Resampler;
            while self.input_buf.len() >= self.input_chunk_size {
                let mut chunk: Vec<f32> = self.input_buf.drain(..self.input_chunk_size).collect();
                // Process. process() takes &[V] where V: AsRef<[f32]>.
                let waves_out = rs
                    .process(&[chunk.as_slice()], None)
                    .map_err(|e| Error::PipeWire(format!("resample: {e}")))?;
                for &s in &waves_out[0] {
                    self.output_accum.push(f32_to_i16(s));
                }
                chunk.clear();
            }
        }
        self.emit_frames(writer_slot, tee_slot);
        Ok(())
    }

    /// Drain any held output without trying to resample partial input.
    fn flush_into(
        &mut self,
        writer_slot: &Arc<Mutex<Option<WavFrameWriter>>>,
        tee_slot: &Arc<Mutex<Option<TeeSender>>>,
    ) -> Result<()> {
        self.emit_frames(writer_slot, tee_slot);
        Ok(())
    }

    fn emit_frames(
        &mut self,
        writer_slot: &Arc<Mutex<Option<WavFrameWriter>>>,
        tee_slot: &Arc<Mutex<Option<TeeSender>>>,
    ) {
        while self.output_accum.len() >= FRAME_SAMPLES {
            let frame: Vec<i16> = self.output_accum.drain(..FRAME_SAMPLES).collect();
            // Write primary path (WAV).
            {
                let mut guard = writer_slot.lock().unwrap();
                if let Some(w) = guard.as_mut() {
                    if let Err(e) = w.write_frame(&frame) {
                        log::warn!("frame write failed: {e}");
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
                        .send(crate::capture::TeeFrame { start_sample: at, samples: frame })
                        .is_ok()
                    {
                        self.tee_sent.store(at + n, Ordering::Relaxed);
                    }
                }
            }
        }
    }
}

#[inline]
fn f32_to_i16(s: f32) -> i16 {
    let c = s.clamp(-1.0, 1.0);
    (c * i16::MAX as f32) as i16
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
            .map_err(|e| Error::PipeWire(format!("CoCreateInstance(MMDeviceEnumerator): {e}")))
    }
}

fn clone_err(e: &Error) -> Error {
    // Error is not Clone; reconstruct a similar variant for cross-thread reporting.
    Error::PipeWire(e.to_string())
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ─── Mic endpoint volume (auto-gain) ──────────────────────────────────────────
// Each call runs on a short-lived thread that initializes COM itself; the
// caller is the live-pipeline async task, whose tokio worker thread may not
// have COM initialized (and can move between awaits).

pub(crate) fn endpoint_volume(source_id: u32) -> Option<f32> {
    std::thread::spawn(move || -> Option<f32> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let device = mic::find_capture_device(source_id).ok()?;
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
            let Ok(device) = mic::find_capture_device(source_id) else { return false };
            let Ok(vol) = device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) else {
                return false;
            };
            vol.SetMasterVolumeLevelScalar(scalar, std::ptr::null()).is_ok()
        }
    })
    .join()
    .unwrap_or(false)
}
