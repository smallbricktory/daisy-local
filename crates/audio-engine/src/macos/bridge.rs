//! C-ABI bridge to shim.swift. Rust never touches CoreAudio/AVAudioEngine
//! directly; Swift never touches files. PCM arrives via `on_pcm`, tagged by track.

use std::os::raw::c_void;

pub const TRACK_SYSTEM: i32 = 0;
pub const TRACK_MIC: i32 = 1;

pub const STATE_RUNNING: i32 = 0;
pub const STATE_STOPPED: i32 = 1;
pub const STATE_ERROR: i32 = 2;
pub const STATE_PERM_DENIED: i32 = 3;

pub type PcmCallback = extern "C" fn(
    ctx: *mut c_void,
    track: i32,
    samples: *const f32,
    frame_count: i32,
    channel_count: i32,
    sample_rate: i32,
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
    pub fn daisy_capture_start(
        want_mic: i32,
        mic_device_id: u32,
        ctx: *mut c_void,
        on_pcm: PcmCallback,
        on_state: StateCallback,
    ) -> i32;
    pub fn daisy_capture_stop() -> i32;
    pub fn daisy_permission_status() -> i32;
    /// Start a mic-only level meter on the given CoreAudio device id. Calls
    /// `on_rms(ctx, peak)` from the audio thread until `daisy_mic_meter_stop`.
    pub fn daisy_mic_meter_start(device_id: u32, ctx: *mut c_void, on_rms: RmsCallback) -> i32;
    pub fn daisy_mic_meter_stop();
    /// Classify the current default OUTPUT device by CoreAudio transport
    /// type. Returns one of `routing::macos_transport::*`.
    pub fn daisy_default_output_class() -> i32;
    /// Classify a specific INPUT (mic) device by transport type (same
    /// codes). `device_id` 0 = default input.
    pub fn daisy_input_class(device_id: u32) -> i32;
}
