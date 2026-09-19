// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
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

mod source;
pub(crate) use source::SYSTEM_TAP_NODE;
pub(crate) mod topology;

use crate::capture::{
    DuckDetector, DuckTick, StreamingCaptureRequest, StreamingControl, StreamingHandle,
    DUCK_ESCALATE_TICKS,
};
use crate::source::Source;
use crate::capture::TeeSender;
use crate::error::{Error, Result};
use crate::resample::FrameResampler;
use crate::wav::WavFrameWriter;
use std::os::raw::{c_char, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
    /// Index of the chunk currently being written; None between CloseChunk
    /// and the next OpenChunk, where audio belongs to no file.
    open_chunk: Mutex<Option<u32>>,
    resampler: Mutex<FrameResampler>,
    /// Upward makeup-gain for the system track. Unity/no-op for the mic and
    /// for already-loud system audio.
    sysnorm: Mutex<crate::sysgain::SystemNormalizer>,
    /// Number of PCM callbacks seen (diagnostics: distinguishes "no buffers
    /// arriving" from "buffers arriving but silent").
    cb_count: AtomicU64,
    /// Per-chunk capture timing, shared with the recorder.
    chunk_clock: std::sync::Arc<crate::capture::ChunkClock>,
}

impl TrackState {
    fn new(chunk_clock: std::sync::Arc<crate::capture::ChunkClock>) -> Self {
        Self {
            writer: Mutex::new(None),
            tee: Mutex::new(None),
            tee_sent: AtomicU64::new(0),
            open_chunk: Mutex::new(None),
            // Device rate corrected on the first callback via reconfigure_if_needed.
            resampler: Mutex::new(FrameResampler::new(48_000, 1)),
            sysnorm: Mutex::new(crate::sysgain::SystemNormalizer::new()),
            cb_count: AtomicU64::new(0),
            chunk_clock,
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
    /// True while the mic runs on VoiceProcessingIO, whose output the OS has
    /// already echo-cancelled. Shared with the handle the recorder reads when
    /// it opens a chunk, and flipped by a mid-session escalation to VPIO or a
    /// switch that falls back to AUHAL, so chunks opened after either are
    /// marked correctly for software AEC.
    mic_voice_processed: Arc<AtomicBool>,
    /// Consecutive exact-zero mic samples (device rate). Fed by `on_pcm`,
    /// read by the control loop's escalation check. Counted only while the
    /// mic is raw and unmuted — VPIO legitimately gates zeros, and software
    /// mute injects them.
    mic_zero_run: AtomicU64,
    /// Max |sample| of raw mic callbacks since the control loop last drained
    /// it (f32 bits). Stays 0 while the mic is muted or voice-processed, so
    /// the duck detector skips those ticks. Feeds the loop's duck-escalation
    /// check.
    mic_tick_peak: AtomicU32,
}

extern "C" fn on_pcm(
    ctx: *mut c_void,
    track: i32,
    samples: *const f32,
    frame_count: i32,
    channel_count: i32,
    sample_rate: i32,
    host_ns: u64,
) {
    if ctx.is_null() || samples.is_null() || frame_count <= 0 {
        return;
    }
    let cx = unsafe { &*(ctx as *const CaptureCtx) };
    let is_mic = track == bridge::TRACK_MIC;
    let ts = if is_mic { &cx.mic } else { &cx.system };
    let total = frame_count as usize * channel_count.max(1) as usize;
    let slice = unsafe { std::slice::from_raw_parts(samples, total) };

    // Peak feeds the duck detector (mic) and the periodic diagnostics.
    let peak = slice.iter().fold(0.0f32, |a, &s| a.max(s.abs()));
    let n = ts.cb_count.fetch_add(1, Ordering::Relaxed);
    let label = if is_mic { "mic" } else { "system" };
    if n == 0 || n % 250 == 0 {
        // First callback stays at info (the delivery-started marker the
        // field checks look for); the recurring counter is debug-only.
        if n == 0 {
            log::info!(
                "macos capture[{label}]: cb #{n} frames={frame_count} ch={channel_count} sr={sample_rate} peak={peak:.4}"
            );
        } else {
            log::debug!(
                "macos capture[{label}]: cb #{n} frames={frame_count} ch={channel_count} sr={sample_rate} peak={peak:.4}"
            );
        }
    }

    // Software mute: silence feeds into the mic resampler; both the recorded
    // wav and the live tee go silent. OS input-gain mute is a no-op against
    // the AVAudioEngine tap on macOS.
    let muted = is_mic && cx.mic_muted.load(Ordering::Relaxed);
    if is_mic {
        // Single writer (this callback), so load/store is safe.
        if muted || cx.mic_voice_processed.load(Ordering::Relaxed) {
            cx.mic_zero_run.store(0, Ordering::Relaxed);
        } else {
            let run = crate::capture::zero_run_update(
                cx.mic_zero_run.load(Ordering::Relaxed),
                slice,
            );
            cx.mic_zero_run.store(run, Ordering::Relaxed);
            // Max since the control loop's last drain. The loop's swap can
            // drop at most one callback's peak — harmless at the 500 ms tick.
            if peak > f32::from_bits(cx.mic_tick_peak.load(Ordering::Relaxed)) {
                cx.mic_tick_peak.store(peak.to_bits(), Ordering::Relaxed);
            }
        }
    }
    // `host_ns` stamps the newest input sample this callback delivered;
    // `push_stamped` maps it to the first emitted sample, subtracting the
    // audio still buffered inside the resampler.
    let (mut frames, mut frame_ns) = {
        let mut r = ts.resampler.lock().unwrap();
        r.reconfigure_if_needed(sample_rate as u32, channel_count as u16);
        if muted {
            r.push_stamped(&vec![0.0f32; slice.len()], host_ns)
        } else {
            r.push_stamped(slice, host_ns)
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
        let this_ns = frame_ns;
        if frame_ns != 0 {
            frame_ns += frame.len() as u64 * 1_000_000_000 / 16_000;
        }
        // The tee stamp is taken from the writer in the same step, so a live
        // consumer can place this audio at exactly the same file offset —
        // including the clock-derived silence pads, which cannot be
        // recomputed later.
        let mut frame_chunk: Option<(u32, u64)> = None;
        if let Some(w) = ts.writer.lock().unwrap().as_mut() {
            if w.write_frame_stamped(&frame, this_ns).is_ok() {
                if let Some(idx) = *ts.open_chunk.lock().unwrap() {
                    frame_chunk =
                        Some((idx, w.samples_written().saturating_sub(frame.len() as u64)));
                    ts.chunk_clock.note(this_ns, &frame);
                }
            }
        }
        // The clock counts only what was delivered — pause detaches the tee
        // and freezes it, matching the consumer's view.
        if let Some(tee) = ts.tee.lock().unwrap().as_ref() {
            let at = ts.tee_sent.load(Ordering::Relaxed);
            let n = frame.len() as u64;
            if tee
                .send(crate::capture::TeeFrame {
                    start_sample: at,
                    host_ns: this_ns,
                    samples: frame,
                    chunk: frame_chunk,
                })
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

/// Pids of other processes currently running audio input, per the shim's
/// CoreAudio process-object walk.
fn capturing_pids() -> std::collections::HashSet<i32> {
    let mut buf = [0i32; 64];
    let n = unsafe { bridge::daisy_capturing_pids(buf.as_mut_ptr(), buf.len() as i32) };
    buf[..(n.max(0) as usize).min(buf.len())].iter().copied().collect()
}

extern "C" {
    fn proc_name(pid: i32, buffer: *mut std::os::raw::c_void, buffersize: u32) -> i32;
}

/// Executable name of a pid, or `pid <n>` when the name is unreadable.
fn process_name(pid: i32) -> String {
    let mut buf = [0u8; 256];
    let n = unsafe { proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return format!("pid {pid}");
    }
    String::from_utf8_lossy(&buf[..(n as usize).min(buf.len())]).into_owned()
}

pub(crate) fn mic_clients_support() -> crate::mic_clients::MicClientsSupport {
    if unsafe { bridge::daisy_mic_clients_supported() } == 1 {
        crate::mic_clients::MicClientsSupport::Yes
    } else {
        crate::mic_clients::MicClientsSupport::No("needs macOS 14")
    }
}

/// Parent pid and start time (microseconds since the epoch) of a pid.
pub(crate) fn proc_info(pid: u32) -> Option<crate::mic_clients::ProcInfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    let n = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if n != size {
        return None;
    }
    Some(crate::mic_clients::ProcInfo {
        ppid: info.pbi_ppid,
        started: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
    })
}

pub(crate) fn mic_clients() -> Vec<crate::mic_clients::MicClient> {
    capturing_pids()
        .into_iter()
        .map(|pid| crate::mic_clients::MicClient {
            pid: pid as u32,
            name: process_name(pid),
        })
        .collect()
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

pub(crate) fn list_sources_blocking() -> Result<Vec<Source>> {
    source::list_sources_blocking()
}

/// Mic-only capture to a 16 kHz mono WAV, through the same start ladder a
/// recording uses — so a clip recorded here is in the capture domain the
/// recording will be in. Returns whether it came up voice-processed.
pub(crate) fn capture_one_blocking(source: &Source, duration: Duration, out: &Path) -> Result<bool> {
    ensure_shim_log();
    let (state_tx, _state_rx) = mpsc::channel::<i32>();
    let ctx = Arc::new(CaptureCtx {
        mic: TrackState::new(Arc::new(crate::capture::ChunkClock::default())),
        system: TrackState::new(Arc::new(crate::capture::ChunkClock::default())),
        state_tx: Mutex::new(Some(state_tx)),
        mic_muted: AtomicBool::new(false),
        mic_voice_processed: Arc::new(AtomicBool::new(false)),
        mic_zero_run: AtomicU64::new(0),
        mic_tick_peak: AtomicU32::new(0),
    });
    *ctx.mic.writer.lock().unwrap() = Some(WavFrameWriter::create(out, 16_000)?);
    let ctx_ptr = Arc::as_ptr(&ctx) as *mut c_void;

    let mut token: i64 = 0;
    let mut backend = bridge::MIC_BACKEND_AUHAL;
    let rc = unsafe {
        bridge::daisy_mic_preview_start(
            source.id,
            ctx_ptr,
            Some(on_pcm),
            None,
            &mut token,
            &mut backend,
        )
    };
    if rc != 0 {
        close(&ctx.mic);
        let _ = std::fs::remove_file(out);
        return Err(Error::Macos(format!(
            "one-shot mic capture rc={rc} (source_id={})",
            source.id
        )));
    }
    log::info!(
        "one-shot mic capture: {}s on {} for source_id={}",
        duration.as_secs(),
        bridge::backend_name(backend),
        source.id
    );
    std::thread::sleep(duration);
    // Token-scoped stop: no callback can still be running against ctx_ptr
    // after it returns, so the writer can be finalized here.
    unsafe { bridge::daisy_mic_preview_stop(token) };
    flush_close(&ctx.mic);
    Ok(bridge::is_voice_processed(backend))
}

pub(crate) fn run_dual_streaming_impl(
    req: StreamingCaptureRequest,
    on_ready: impl FnOnce(StreamingHandle),
) -> Result<()> {
    let _ = req.system_source_id; // tap captures the system mix; id is the sentinel.
    let (state_tx, state_rx) = mpsc::channel::<i32>();
    let mic_voice_processed = Arc::new(AtomicBool::new(false));
    let ctx = Arc::new(CaptureCtx {
        mic: TrackState::new(Arc::clone(&req.mic_clock)),
        system: TrackState::new(Arc::clone(&req.system_clock)),
        state_tx: Mutex::new(Some(state_tx)),
        mic_muted: AtomicBool::new(false),
        mic_voice_processed: Arc::clone(&mic_voice_processed),
        mic_zero_run: AtomicU64::new(0),
        mic_tick_peak: AtomicU32::new(0),
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
    let mut backend = bridge::MIC_BACKEND_AUHAL;
    let rc = unsafe {
        bridge::daisy_capture_start(1, req.mic_source_id, ctx_ptr, on_pcm, on_state, &mut backend)
    };
    if rc != 0 {
        return Err(Error::Macos(format!(
            "daisy_capture_start rc={rc} (mic_source_id={})",
            req.mic_source_id
        )));
    }
    ctx.mic_voice_processed
        .store(bridge::is_voice_processed(backend), Ordering::Relaxed);
    log::info!("macos capture: mic backend = {}", bridge::backend_name(backend));
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

    // The tap aggregate's IOProc delivers nothing until some process renders
    // audio, then wakes within ~200ms — so an absence of system callbacks
    // here is silence, not failure. Don't wait on it.

    let (control_tx, control_rx) = mpsc::channel::<StreamingControl>();
    on_ready(StreamingHandle::new_macos(control_tx, mic_voice_processed));

    // Mic and tap restart independently: a mic switch never touches the
    // system tap, a system reopen never touches the mic.
    let mut current_mic_id = req.mic_source_id;

    // Escalation guard state. Two independent triggers share the pid check:
    // a zero-run (another VPIO client synthesizing exact zeros into this raw
    // capture) and a duck run (another VPIO client attenuating it ~35 dB —
    // FaceTime/phone calls duck, they never zero). Either alone is ambiguous
    // (hardware mute also yields exact zeros; a quiet room is also quiet);
    // paired with an input-capturing process that was NOT there while our
    // mic was healthy, it is another process's VPIO client degrading this
    // raw capture — the one case escalating to VPIO fixes.
    //
    // The pid baseline is a ~5 s delayed snapshot: a call app starts
    // capturing an instant before its VPIO unit ducks us, and an undelayed
    // baseline would absorb its pid in the ticks between the two.
    let mut pids = crate::capture::DelayedPidBaseline::new(PID_BASELINE_TICKS, capturing_pids());
    let mut duck = DuckDetector::new();
    let mut escalate_attempts = 0u32;
    let mut last_attempt: Option<std::time::Instant> = None;
    const ESCALATE_MAX_ATTEMPTS: u32 = 3;
    const ESCALATE_RETRY_SPACING: Duration = Duration::from_secs(30);
    const PID_BASELINE_TICKS: usize = 10; // ~5 s at the 500 ms tick
    // Mic callbacks deliver at the device rate; the run is counted in
    // samples. 48 kHz is the fleet-wide device rate — a different rate only
    // scales the threshold by its ratio, which the 1 s margin absorbs.
    const ZERO_RUN_THRESHOLD: u64 = 48_000 * crate::capture::ZERO_RUN_ESCALATE_SECS;

    loop {
        let cmd = match control_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(cmd) => cmd,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if ctx.mic_voice_processed.load(Ordering::Relaxed) {
                    // Already voice-processed; nothing to escalate to.
                    continue;
                }
                let run = ctx.mic_zero_run.load(Ordering::Relaxed);
                let tick_peak = f32::from_bits(ctx.mic_tick_peak.swap(0, Ordering::Relaxed));
                let tick = duck.note_tick(tick_peak);
                let duck_ticks = match tick {
                    DuckTick::Collapsed(n) => n,
                    _ => 0,
                };
                if run < ZERO_RUN_THRESHOLD && duck_ticks < DUCK_ESCALATE_TICKS {
                    if run == 0 && tick == DuckTick::Healthy {
                        pids.note_healthy(capturing_pids());
                    }
                } else if escalate_attempts < ESCALATE_MAX_ATTEMPTS
                    && last_attempt.is_none_or(|t| t.elapsed() >= ESCALATE_RETRY_SPACING)
                {
                    let cause = if run >= ZERO_RUN_THRESHOLD {
                        format!("zero-run ({}s)", run / 48_000)
                    } else {
                        format!(
                            "duck ({}s at peak {tick_peak:.5}, learned floor {:.5})",
                            duck_ticks / 2,
                            duck.floor().unwrap_or(0.0)
                        )
                    };
                    let now_pids = capturing_pids();
                    let new: Vec<i32> =
                        now_pids.difference(pids.baseline()).copied().collect();
                    if new.is_empty() {
                        log::info!(
                            "macos capture: mic {cause} with no new input-capturing process — hardware mute, dead device or pre-existing capturer, not escalating"
                        );
                        last_attempt = Some(std::time::Instant::now());
                    } else {
                        log::warn!(
                            "macos capture: mic {cause} and new input-capturing process(es) {new:?} — another VPIO client has degraded the raw capture, escalating to VoiceProcessingIO"
                        );
                        escalate_attempts += 1;
                        last_attempt = Some(std::time::Instant::now());
                        let mut backend = bridge::MIC_BACKEND_AUHAL;
                        let rc = unsafe { bridge::daisy_mic_escalate(&mut backend) };
                        ctx.mic.cb_count.store(0, Ordering::Relaxed);
                        ctx.mic_zero_run.store(0, Ordering::Relaxed);
                        ctx.mic_tick_peak.store(0, Ordering::Relaxed);
                        duck.reset_run();
                        if rc == 0 {
                            ctx.mic_voice_processed
                                .store(bridge::is_voice_processed(backend), Ordering::Relaxed);
                            log::info!(
                                "macos capture: mic escalated to {} — OS echo cancellation active for the rest of the session",
                                bridge::backend_name(backend)
                            );
                        } else {
                            log::error!(
                                "macos capture: mic escalation failed (rc={rc}) — next attempt in {}s",
                                ESCALATE_RETRY_SPACING.as_secs()
                            );
                        }
                    }
                }
                continue;
            }
        };
        match cmd {
            StreamingControl::OpenChunk { index, mic_wav, system_wav } => {
                *ctx.mic.open_chunk.lock().unwrap() = Some(index);
                *ctx.system.open_chunk.lock().unwrap() = Some(index);
                ctx.mic.chunk_clock.reset();
                ctx.system.chunk_clock.reset();
                rotate(&ctx.mic, &mic_wav);
                rotate(&ctx.system, &system_wav);
            }
            StreamingControl::CloseChunk => {
                *ctx.mic.open_chunk.lock().unwrap() = None;
                *ctx.system.open_chunk.lock().unwrap() = None;
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
                ctx.mic_zero_run.store(0, Ordering::Relaxed);
                ctx.mic_tick_peak.store(0, Ordering::Relaxed);
                // A different device has a different noise floor.
                duck.reset();
                let mut backend = bridge::MIC_BACKEND_AUHAL;
                let rc = unsafe { bridge::daisy_mic_restart(new_id, &mut backend) };
                if rc == 0 {
                    current_mic_id = new_id;
                    ctx.mic_voice_processed
                        .store(bridge::is_voice_processed(backend), Ordering::Relaxed);
                    log::info!(
                        "SwitchMic(macos): mic now {} ({new_id}) backend={}",
                        source.node_name,
                        bridge::backend_name(backend)
                    );
                } else {
                    log::error!(
                        "SwitchMic(macos): start on {} failed (rc={rc}) — recovering previous mic",
                        source.node_name
                    );
                    let mut recovered = bridge::MIC_BACKEND_AUHAL;
                    if unsafe { bridge::daisy_mic_restart(current_mic_id, &mut recovered) } != 0 {
                        log::error!("SwitchMic(macos): recovery start failed — mic silent, system continues");
                    } else {
                        ctx.mic_voice_processed
                            .store(bridge::is_voice_processed(recovered), Ordering::Relaxed);
                    }
                }
            }
            StreamingControl::ReopenSystem => {
                ctx.system.cb_count.store(0, Ordering::Relaxed);
                let rc = unsafe { bridge::daisy_tap_restart() };
                if rc == 0 {
                    log::info!("ReopenSystem(macos): system tap restarted (mic untouched)");
                } else {
                    log::error!("ReopenSystem(macos): tap restart failed (rc={rc}) — system silent, recording continues");
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
    log::info!("mic meter: starting for source_id={source_id}");
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

    let mut token: i64 = 0;
    let mut backend = bridge::MIC_BACKEND_AUHAL;
    let rc = unsafe {
        bridge::daisy_mic_preview_start(
            source_id,
            cb_ptr,
            None,
            Some(trampoline),
            &mut token,
            &mut backend,
        )
    };
    if rc != 0 {
        unsafe { drop(Box::from_raw(cb_ptr as *mut BoxedCb)) };
        return Err(Error::Macos(format!("mic meter start rc={rc}")));
    }
    log::info!(
        "mic meter: running on {} for source_id={source_id}",
        bridge::backend_name(backend)
    );

    // Block until the caller signals stop (channel closed or a unit sent).
    loop {
        match stop_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(()) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }

    // Token-scoped: tears down exactly this call's meter, never a newer one
    // that may already have replaced it (device switched again mid-stop).
    unsafe { bridge::daisy_mic_preview_stop(token) };
    // SAFETY: stop() removed the tap + stopped the engine; no callback can
    // still be running against cb_ptr.
    unsafe { drop(Box::from_raw(cb_ptr as *mut BoxedCb)) };
    Ok(())
}

