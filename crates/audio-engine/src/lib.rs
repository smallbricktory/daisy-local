//! Daisy audio engine: source enumeration, dual-stream capture, virtual sink.

pub mod autogain;
pub mod bt_profile;
pub mod capture;
pub mod correlation;
pub mod error;
pub mod gain;
pub mod manifest;
pub mod routing;
pub mod source;
pub mod sysgain;
pub mod virtual_sink;
pub mod wav;

// The internal module is aliased `wasapi`, apart from the `windows` crate
// name (the WASAPI bindings). Files live in `src/windows/`.
#[cfg(target_os = "windows")]
#[path = "windows/mod.rs"]
mod wasapi;

// macOS capture backend (Core Audio tap + AVAudioEngine). Standard module name —
// `crate::macos::*` dispatch calls in capture.rs/source.rs resolve to this.
#[cfg(target_os = "macos")]
mod macos;

pub use capture::{
    capture_dual, capture_one, run_dual_streaming, DualCaptureOutputs, DualCaptureRequest,
    StreamingCaptureRequest, StreamingControl, StreamingHandle,
};
pub use error::{Error, Result};
pub use manifest::{ChannelManifest, RecordingManifest};
pub use source::{list_sources, Source, SourceKind};
pub use virtual_sink::VirtualSink;

/// Microphone capture permission status: 0 not-determined, 1 granted,
/// 2 denied. Non-macOS platforms always return 1 (no equivalent OS gate).
pub fn capture_permission_status() -> i32 {
    #[cfg(target_os = "macos")]
    {
        return macos::permission_status();
    }
    #[cfg(not(target_os = "macos"))]
    {
        1
    }
}
