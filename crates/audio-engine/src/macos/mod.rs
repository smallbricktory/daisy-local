//! macOS capture backend (Apple Silicon, macOS 15+).
//!
//! Audio-only: a Core Audio process tap captures the system-audio mix and an
//! AVAudioEngine input tap captures the microphone, both in a Swift shim
//! (`shim.swift`) that hands PCM to Rust via a C-ABI callback (`bridge.rs`).
//! The PCM is resampled to 16 kHz mono i16 (`resample.rs`) and written through
//! the shared `WavFrameWriter`, driven by the same `StreamingControl` loop the
//! Windows backend uses. Uses only the microphone permission — no screen
//! recording.

mod bridge;

/// Classify the current default OUTPUT device (CoreAudio transport type) into a
/// `routing::macos_transport` code. Used by `routing::detect_routing()` to skip
/// AEC on headphones/Bluetooth.
pub(crate) fn default_output_class() -> i32 {
    unsafe { bridge::daisy_default_output_class() }
}

/// Classify an INPUT (mic) device by CoreAudio transport type (`device_id` 0 =
/// default input). Used by `routing::mic_is_headphone` to skip AEC on a
/// Bluetooth/headset mic.
pub(crate) fn input_class(device_id: u32) -> i32 {
    unsafe { bridge::daisy_input_class(device_id) }
}
mod resample;
mod source;

use super::{
    DualCaptureOutputs, DualCaptureRequest, Source, StreamingCaptureRequest, StreamingControl,
    StreamingHandle,
};
use crate::capture::TeeSender;
use crate::error::{Error, Result};
use crate::macos::resample::FrameResampler;
use crate::wav::WavFrameWriter;
use std::os::raw::{c_char, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, Once};
use std::time::Duration;

// ── Shim log bridge ─────────────────────────────────────────────────────────
// The Swift shim emits its diagnostic trail through this callback; its
// CoreAudio/AVAudioEngine errors land in daisy.log tagged
// `audio_engine::macos` → "shim:".
extern "C" fn shim_log(level: i32, msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    let text = unsafe { std::ffi::CStr::from_ptr(msg) }.to_string_lossy();
    match level {
        2 => log::error!("shim: {text}"),
        1 => log::warn!("shim: {text}"),
        _ => log::info!("shim: {text}"),
    }
}

/// Install the shim log sink exactly once (idempotent across capture/meter starts).
fn ensure_shim_log() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe { bridge::daisy_set_log_callback(shim_log) });
}

// ── Per-track capture state shared between the SCK callback and control loop ──

struct TrackState {
    writer: Mutex<Option<WavFrameWriter>>,
    tee: Mutex<Option<TeeSender>>,
    /// Stream clock: samples delivered to the tee (see `TeeFrame`).
    tee_sent: AtomicU64,
    resampler: Mutex<FrameResampler>,
    /// Upward makeup-gain for the system track. Unity/no-op for the mic and
    /// for already-loud system audio.
    sysnorm: Mutex<crate::sysgain::SystemNormalizer>,
    /// Number of PCM callbacks seen (diagnostics: distinguishes "no buffers
    /// arriving" from "buffers arriving but silent").
    cb_count: AtomicU64,
}

impl TrackState {
    fn new() -> Self {
        Self {
            writer: Mutex::new(None),
            tee: Mutex::new(None),
            tee_sent: AtomicU64::new(0),
            // Device rate corrected on the first callback via reconfigure_if_needed.
            resampler: Mutex::new(FrameResampler::new(48_000, 1)),
            sysnorm: Mutex::new(crate::sysgain::SystemNormalizer::new()),
            cb_count: AtomicU64::new(0),
        }
    }
}

struct CaptureCtx {
    mic: TrackState,
    system: TrackState,
    state_tx: Mutex<Option<mpsc::Sender<i32>>>,
    /// Software mute for the mic track. OS input-gain mute does not affect
    /// the AVAudioEngine capture on macOS; the mic frames are zeroed here.
    mic_muted: AtomicBool,
}

extern "C" fn on_pcm(
    ctx: *mut c_void,
    track: i32,
    samples: *const f32,
    frame_count: i32,
    channel_count: i32,
    sample_rate: i32,
) {
    if ctx.is_null() || samples.is_null() || frame_count <= 0 {
        return;
    }
    let cx = unsafe { &*(ctx as *const CaptureCtx) };
    let is_mic = track == bridge::TRACK_MIC;
    let ts = if is_mic { &cx.mic } else { &cx.system };
    let total = frame_count as usize * channel_count.max(1) as usize;
    let slice = unsafe { std::slice::from_raw_parts(samples, total) };

    // Diagnostics: logs the first callback (buffers + negotiated format) and
    // a periodic peak. The peak is only computed on the logged callbacks.
    let n = ts.cb_count.fetch_add(1, Ordering::Relaxed);
    let label = if is_mic { "mic" } else { "system" };
    if n == 0 || n % 250 == 0 {
        let peak = slice.iter().fold(0.0f32, |a, &s| a.max(s.abs()));
        log::info!(
            "macos capture[{label}]: cb #{n} frames={frame_count} ch={channel_count} sr={sample_rate} peak={peak:.4}"
        );
    }

    // Software mute: silence feeds into the mic resampler; both the recorded
    // wav and the live tee go silent. OS input-gain mute is a no-op against
    // the AVAudioEngine tap on macOS.
    let muted = is_mic && cx.mic_muted.load(Ordering::Relaxed);
    let mut frames = {
        let mut r = ts.resampler.lock().unwrap();
        r.reconfigure_if_needed(sample_rate as u32, channel_count as u16);
        if muted {
            r.push(&vec![0.0f32; slice.len()])
        } else {
            r.push(slice)
        }
    };
    // System track only: lift a too-quiet far-end (BT-output tap) toward an
    // audible level before it's written/teed. No-op for the mic and for
    // already-loud or silent audio.
    if !is_mic {
        let mut sn = ts.sysnorm.lock().unwrap();
        for frame in frames.iter_mut() {
            sn.process(frame);
        }
    }
    for frame in frames {
        if let Some(w) = ts.writer.lock().unwrap().as_mut() {
            let _ = w.write_frame(&frame);
        }
        // The clock counts only what was delivered — pause detaches the tee
        // and freezes it, matching the consumer's view.
        if let Some(tee) = ts.tee.lock().unwrap().as_ref() {
            let at = ts.tee_sent.load(Ordering::Relaxed);
            let n = frame.len() as u64;
            if tee
                .send(crate::capture::TeeFrame { start_sample: at, samples: frame })
                .is_ok()
            {
                ts.tee_sent.store(at + n, Ordering::Relaxed);
            }
        }
    }
}

extern "C" fn on_state(ctx: *mut c_void, state: i32) {
    if ctx.is_null() {
        return;
    }
    let cx = unsafe { &*(ctx as *const CaptureCtx) };
    if let Some(tx) = cx.state_tx.lock().unwrap().as_ref() {
        let _ = tx.send(state);
    }
}

fn rotate(ts: &TrackState, path: &Path) {
    let mut g = ts.writer.lock().unwrap();
    if let Some(old) = g.take() {
        let _ = old.close();
    }
    match WavFrameWriter::create(path, 16_000) {
        Ok(w) => *g = Some(w),
        Err(e) => log::error!("open chunk {:?}: {e}", path),
    }
}

fn close(ts: &TrackState) {
    if let Some(w) = ts.writer.lock().unwrap().take() {
        let _ = w.close();
    }
}

fn flush_close(ts: &TrackState) {
    let tail = ts.resampler.lock().unwrap().flush();
    if let Some(w) = ts.writer.lock().unwrap().as_mut() {
        for f in tail {
            let _ = w.write_frame(&f);
        }
    }
    close(ts);
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 0 = not determined, 1 = granted, 2 = denied.
pub(crate) fn permission_status() -> i32 {
    unsafe { bridge::daisy_permission_status() }
}

pub(crate) fn list_sources_blocking() -> Result<Vec<Source>> {
    source::list_sources_blocking()
}

pub(crate) fn capture_one_blocking(_source: &Source, _duration: Duration, _out: &Path) -> Result<()> {
    // Single-source capture is unused on the recording path (always dual).
    Err(Error::Macos("capture_one not supported on macOS; use capture_dual".into()))
}

pub(crate) fn capture_dual_blocking(
    req: DualCaptureRequest,
    out_dir: &Path,
) -> Result<DualCaptureOutputs> {
    use crate::manifest::{ChannelManifest, RecordingManifest};
    std::fs::create_dir_all(out_dir)?;
    let mic_wav = out_dir.join("mic.wav");
    let sys_wav = out_dir.join("system.wav");
    let manifest_path = out_dir.join("manifest.json");
    let dur = req.duration;

    let stream_req = StreamingCaptureRequest {
        mic_source_id: req.mic_source_id,
        system_source_id: req.system_source_id,
        sample_rate: 16_000,
    };
    let mic_p = mic_wav.clone();
    let sys_p = sys_wav.clone();
    let handle_slot: Arc<Mutex<Option<StreamingHandle>>> = Arc::new(Mutex::new(None));
    let hs = Arc::clone(&handle_slot);

    let join = std::thread::spawn(move || -> Result<()> {
        run_dual_streaming_impl(stream_req, move |h| {
            let _ = h.open_chunk(&mic_p, &sys_p);
            *hs.lock().unwrap() = Some(h);
        })
    });
    std::thread::sleep(dur);
    if let Some(h) = handle_slot.lock().unwrap().take() {
        let _ = h.stop();
    }
    join.join().map_err(|_| Error::Macos("capture thread panicked".into()))??;

    let now = now_unix();
    let manifest = RecordingManifest {
        schema_version: RecordingManifest::SCHEMA,
        started_at_unix_seconds: now,
        duration_seconds: dur.as_secs(),
        sample_rate: 16_000,
        channels: 1,
        mic: ChannelManifest {
            source_id: req.mic_source_id,
            source_node_name: "mic".into(),
            source_description: "Microphone".into(),
            wav_path: mic_wav.clone(),
            captured_at_unix_seconds: now,
        },
        system: ChannelManifest {
            source_id: req.system_source_id,
            source_node_name: "system-audio".into(),
            source_description: "System audio".into(),
            wav_path: sys_wav.clone(),
            captured_at_unix_seconds: now,
        },
    };
    manifest.write(&manifest_path)?;
    Ok(DualCaptureOutputs {
        mic_wav,
        system_wav: sys_wav,
        manifest_json: manifest_path,
    })
}

pub(crate) fn run_dual_streaming_impl(
    req: StreamingCaptureRequest,
    on_ready: impl FnOnce(StreamingHandle),
) -> Result<()> {
    let _ = req.system_source_id; // tap captures the system mix; id is the sentinel.
    let (state_tx, state_rx) = mpsc::channel::<i32>();
    let ctx = Arc::new(CaptureCtx {
        mic: TrackState::new(),
        system: TrackState::new(),
        state_tx: Mutex::new(Some(state_tx)),
        mic_muted: AtomicBool::new(false),
    });
    let ctx_ptr = Arc::as_ptr(&ctx) as *mut c_void;

    ensure_shim_log();
    // Log the default OUTPUT transport class; ties a silent system track to
    // the device it was routed to.
    // Codes: 0 unknown · 1 builtin-speaker · 2 builtin-headphones ·
    //        3 bluetooth · 4 usb · 5 hdmi/displayport · 6 virtual/aggregate.
    let out_class = unsafe { bridge::daisy_default_output_class() };
    log::info!(
        "macos capture: starting (mic_source_id={}, 0=system-default) default_output_class={out_class}",
        req.mic_source_id
    );
    let rc = unsafe {
        bridge::daisy_capture_start(1, req.mic_source_id, ctx_ptr, on_pcm, on_state)
    };
    if rc != 0 {
        return Err(Error::Macos(format!(
            "daisy_capture_start rc={rc} (mic_source_id={})",
            req.mic_source_id
        )));
    }
    match state_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(bridge::STATE_RUNNING) => {}
        Ok(bridge::STATE_PERM_DENIED) => {
            unsafe { bridge::daisy_capture_stop() };
            return Err(Error::Macos("microphone permission denied".into()));
        }
        Ok(s) => {
            unsafe { bridge::daisy_capture_stop() };
            return Err(Error::Macos(format!(
                "system-audio tap / mic failed to start (state={s})"
            )));
        }
        Err(_) => {
            unsafe { bridge::daisy_capture_stop() };
            return Err(Error::Macos("capture start timed out".into()));
        }
    }

    // Verify the system tap is actually delivering frames before reporting
    // the capture as live: AudioDeviceStart returning noErr does not mean
    // the IOProc is scheduled. Waits briefly for the first system callback;
    // logs if it never comes, then proceeds (the mic keeps recording).
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        while ctx.system.cb_count.load(Ordering::Relaxed) == 0
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        if ctx.system.cb_count.load(Ordering::Relaxed) == 0 {
            log::warn!(
                "macos capture: system tap delivered NO frames within 6s of start — \
                 proceeding (mic recording; system audio delayed/contended)"
            );
        } else {
            log::info!("macos capture: system tap delivering frames");
        }
    }

    let (control_tx, control_rx) = mpsc::channel::<StreamingControl>();
    on_ready(StreamingHandle::new_macos(control_tx));

    // The shim captures mic + system together; a mic switch is a full
    // stop+restart with the same ctx (writers/tees reused). Fail-safe: if
    // the restart on the new device fails, restart on the previous device.
    let mut current_mic_id = req.mic_source_id;

    while let Ok(cmd) = control_rx.recv() {
        match cmd {
            StreamingControl::OpenChunk { mic_wav, system_wav } => {
                rotate(&ctx.mic, &mic_wav);
                rotate(&ctx.system, &system_wav);
            }
            StreamingControl::CloseChunk => {
                close(&ctx.mic);
                close(&ctx.system);
            }
            StreamingControl::SetMicTee(tx) => {
                *ctx.mic.tee.lock().unwrap() = tx;
            }
            StreamingControl::SetSystemTee(tx) => {
                *ctx.system.tee.lock().unwrap() = tx;
            }
            StreamingControl::SwitchMic(source) => {
                let new_id = source.id;
                log::info!(
                    "SwitchMic(macos): requested {} ({new_id}) — was ({current_mic_id})",
                    source.node_name
                );
                // Reset the mic callback counter; the first post-switch
                // callback (n==0) logs the new device's negotiated format.
                ctx.mic.cb_count.store(0, Ordering::Relaxed);
                unsafe { bridge::daisy_capture_stop() };
                let _ = state_rx.recv_timeout(Duration::from_secs(2)); // drain stop
                let started = |id: u32| -> bool {
                    let rc = unsafe {
                        bridge::daisy_capture_start(1, id, ctx_ptr, on_pcm, on_state)
                    };
                    if rc != 0 {
                        // rc distinguishes the failing layer: -2/-4 = mic
                        // channels/permission, other = system-tap CoreAudio
                        // error (e.g. aggregate recreation on rapid restart).
                        log::warn!("SwitchMic(macos): daisy_capture_start({id}) rc={rc}");
                        return false;
                    }
                    // Wait for the RUNNING handshake, tolerating a late
                    // STOPPED from the daisy_capture_stop above (the 2s
                    // drain races that stop's onState). Stray STOPPED is
                    // skipped; only RUNNING = started, ERROR/PERM_DENIED =
                    // failed.
                    let deadline = std::time::Instant::now() + Duration::from_secs(10);
                    loop {
                        let remaining =
                            deadline.saturating_duration_since(std::time::Instant::now());
                        if remaining.is_zero() {
                            log::warn!("SwitchMic(macos): start({id}) timed out waiting for RUNNING");
                            return false;
                        }
                        match state_rx.recv_timeout(remaining) {
                            Ok(bridge::STATE_RUNNING) => return true,
                            Ok(bridge::STATE_STOPPED) => continue, // stale stop — keep waiting
                            Ok(other) => {
                                log::warn!(
                                    "SwitchMic(macos): start({id}) got state={other}, not RUNNING"
                                );
                                return false;
                            }
                            Err(_) => {
                                log::warn!("SwitchMic(macos): start({id}) recv timed out");
                                return false;
                            }
                        }
                    }
                };
                if started(new_id) {
                    current_mic_id = new_id;
                    log::info!("SwitchMic(macos): mic now {} ({new_id})", source.node_name);
                } else {
                    log::error!(
                        "SwitchMic(macos): start on {} failed — recovering previous mic",
                        source.node_name
                    );
                    if !started(current_mic_id) {
                        log::error!("SwitchMic(macos): recovery start failed — capture stopped");
                    }
                }
            }
            StreamingControl::SetMicMuted(m) => {
                ctx.mic_muted.store(m, Ordering::Relaxed);
                log::info!(
                    "macos capture: mic {} (software mute)",
                    if m { "MUTED" } else { "unmuted" }
                );
            }
            StreamingControl::Stop => {
                flush_close(&ctx.mic);
                flush_close(&ctx.system);
                break;
            }
        }
    }

    unsafe { bridge::daisy_capture_stop() };
    // Keep ctx alive until SCK has fully stopped (a callback may fire once more).
    let _ = state_rx.recv_timeout(Duration::from_secs(2));
    drop(ctx);
    Ok(())
}

/// Mic level meter — AVAudioEngine mic tap in the Swift shim (shares
/// DaisyMicEngine). Mic-only: needs just the microphone permission.
pub(crate) fn run_level_meter_impl(
    source_id: u32,
    on_rms: impl FnMut(f32) + Send + 'static,
    stop_rx: std::sync::mpsc::Receiver<()>,
) -> Result<()> {
    ensure_shim_log();
    log::info!("mic meter (macos): starting for source_id={source_id}");
    // Box the callback and hand the pointer to Swift as the meter ctx; an
    // extern "C" trampoline calls it from the audio-tap thread. Reclaimed only
    // after daisy_mic_meter_stop() guarantees no further callbacks.
    type BoxedCb = Box<dyn FnMut(f32) + Send>;
    let cb_ptr = Box::into_raw(Box::new(Box::new(on_rms) as BoxedCb)) as *mut c_void;

    extern "C" fn trampoline(ctx: *mut c_void, rms: f32) {
        if ctx.is_null() {
            return;
        }
        let cb = unsafe { &mut *(ctx as *mut BoxedCb) };
        cb(rms);
    }

    let rc = unsafe { bridge::daisy_mic_meter_start(source_id, cb_ptr, trampoline) };
    if rc != 0 {
        unsafe { drop(Box::from_raw(cb_ptr as *mut BoxedCb)) };
        return Err(Error::Macos(format!("mic meter start rc={rc}")));
    }

    // Block until the caller signals stop (channel closed or a unit sent).
    loop {
        match stop_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(()) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }

    unsafe { bridge::daisy_mic_meter_stop() };
    // SAFETY: stop() removed the tap + stopped the engine; no callback can
    // still be running against cb_ptr.
    unsafe { drop(Box::from_raw(cb_ptr as *mut BoxedCb)) };
    Ok(())
}

