//! Recording lifecycle: session-on-disk + state machine + controllable Recorder.

pub mod audio_import;
pub mod compress;
pub mod error;
pub mod heartbeat;
pub(crate) mod inhibit;
pub mod live_pipeline;
pub mod live_transcript;
pub mod manifest;
pub mod manifest_ops;
pub mod flight_recorder;
pub mod mixdown;
pub mod recorder;
pub mod speech_levels;
pub mod session;
pub mod state;
pub(crate) mod worker;

pub use error::{RecordingError, Result};
pub use live_pipeline::LiveMode;
pub use manifest::{AecMode, ChunkManifest, SessionManifest};
pub use recorder::{apply_aec, apply_denoise, finalize_orphan, Recorder, RecorderConfig};
pub use session::Session;
pub use state::State;
