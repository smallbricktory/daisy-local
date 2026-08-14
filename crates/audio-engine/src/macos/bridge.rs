//! C-ABI bridge to shim.swift. Rust never touches CoreAudio directly; Swift
//! never touches files. PCM arrives via `on_pcm`, tagged by track.

use std::os::raw::c_void;

/// Track tag on `on_pcm`: 1 = mic, 0 = system tap.
pub const TRACK_MIC: i32 = 1;

/// Shim state values (see `DAISY_STATE_*` in shim.swift): 0 running,
/// 1 stopped, 2 error, 3 permission denied. Only the two the start
/// handshake branches on are named; others hit its catch-all error arm.
pub const STATE_RUNNING: i32 = 0;
pub const STATE_PERM_DENIED: i32 = 3;

/// Capture backend a started mic runs on (`DAISY_MIC_BACKEND_*` in
/// shim.swift). VPIO capture is voice-processed by the OS — already
/// echo-cancelled — so software AEC is skipped for it. AUHAL capture is raw.
pub const MIC_BACKEND_AUHAL: i32 = 0;
pub const MIC_BACKEND_VPIO: i32 = 1;

pub fn is_voice_processed(backend: i32) -> bool {
    backend == MIC_BACKEND_VPIO
}

pub fn backend_name(backend: i32) -> &'static str {
    if is_voice_processed(backend) { "VoiceProcessingIO" } else { "AUHAL" }
}

pub type PcmCallback = extern "C" fn(
    ctx: *mut c_void,
    track: i32,
    samples: *const f32,
    frame_count: i32,
    channel_count: i32,
    sample_rate: i32,
    // Monotonic nanoseconds (mach host time, converted) for the newest
    // sample in this buffer. Zero when unavailable.
    host_ns: u64,
);
pub type StateCallback = extern "C" fn(ctx: *mut c_void, state: i32);
/// Mic level meter callback: opaque ctx + peak amplitude (0..1).
pub type RmsCallback = extern "C" fn(ctx: *mut c_void, rms: f32);
/// Log callback handed to the Swift shim.
/// `level`: 0 info · 1 warn · 2 error. `msg` is a UTF-8 C string.
pub type LogCallback = extern "C" fn(level: i32, msg: *const std::os::raw::c_char);

extern "C" {
    /// Install the log sink the shim writes its diagnostic trail to. Call once
    /// before any capture/meter start.
    pub fn daisy_set_log_callback(cb: LogCallback);
    /// Restart only the mic on the given device; the tap keeps running.
    /// `*out_backend` receives a `MIC_BACKEND_*` value on success (the
    /// replacement device may not take the same backend as the old one).
    pub fn daisy_mic_restart(mic_device_id: u32, out_backend: *mut i32) -> i32;
    /// Restart only the system tap; the mic keeps running.
    pub fn daisy_tap_restart() -> i32;
    /// Restart the session mic with voice processing preferred, after a
    /// zero-run proved another process's VPIO client silenced the raw
    /// capture. One-way for the session: later device switches keep VPIO.
    /// `*out_backend` receives a `MIC_BACKEND_*` value on success.
    pub fn daisy_mic_escalate(out_backend: *mut i32) -> i32;
    /// Pids of OTHER processes currently running audio input. Writes up to
    /// `cap` pids and returns the total count (a return above `cap` means
    /// the buffer was too small).
    pub fn daisy_capturing_pids(buf: *mut i32, cap: i32) -> i32;
    /// Devices whose input scope exists only because one of our
    /// VoiceProcessingIO units pinned them as its output. Writes up to `cap`
    /// ids and returns how many are held; a return above `cap` means the
    /// buffer was too small.
    pub fn daisy_phantom_input_devices(out: *mut u32, cap: i32) -> i32;
    /// `*out_backend` receives a `MIC_BACKEND_*` value when the return is 0
    /// and a mic was requested; it is left untouched otherwise.
    pub fn daisy_capture_start(
        want_mic: i32,
        mic_device_id: u32,
        ctx: *mut c_void,
        on_pcm: PcmCallback,
        on_state: StateCallback,
        out_backend: *mut i32,
    ) -> i32;
    pub fn daisy_capture_stop() -> i32;
    /// Start a mic-only preview (no system tap) on the given CoreAudio device
    /// id: the picker's level meter passes `on_rms`, the one-shot calibration
    /// capture passes `on_pcm`, either may be null. Callbacks run on the audio
    /// thread until stopped by the returned token. Concurrent previews (a
    /// caller switching devices before the prior stop lands) are independent
    /// by token — a stop for one can never tear down a different, newer one.
    /// The preview runs on the same backend capture would choose, so its
    /// levels match what a recording will see. `*out_token` and `*out_backend`
    /// are written on success; ignore both when the return is nonzero.
    pub fn daisy_mic_preview_start(
        device_id: u32,
        ctx: *mut c_void,
        on_pcm: Option<PcmCallback>,
        on_rms: Option<RmsCallback>,
        out_token: *mut i64,
        out_backend: *mut i32,
    ) -> i32;
    pub fn daisy_mic_preview_stop(token: i64);
    /// Classify the current default OUTPUT device by CoreAudio transport
    /// type. Returns one of `routing::macos_transport::*`.
    pub fn daisy_default_output_class() -> i32;
    /// Classify a specific INPUT (mic) device by transport type (same
    /// codes). `device_id` 0 = default input.
    pub fn daisy_input_class(device_id: u32) -> i32;
}
