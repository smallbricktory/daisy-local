// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Recording lifecycle: session-on-disk + state machine + controllable Recorder.

pub mod audio_import;
pub mod compress;
pub mod error;
pub mod heartbeat;
pub(crate) mod inhibit;
pub mod health;
pub mod supervisor;
pub mod live_pipeline;
pub mod live_transcript;
pub mod manifest;
pub mod manifest_ops;
pub mod flight_recorder;
pub mod mixdown;
pub mod notes;
pub mod recorder;
pub mod speech_levels;
pub mod session;
pub mod speakers;
pub mod state;
pub(crate) mod worker;

pub use error::{RecordingError, Result};
pub use live_pipeline::LiveMode;
pub use manifest::{AecMode, ChunkManifest, SessionManifest};
pub use recorder::{apply_aec, apply_denoise, AecOverride, Recorder, RecorderConfig};
pub use transcript::wav_io::{
    read_wav_mono_f32, read_wav_mono_i16, write_wav_mono_i16, WavIoError,
};
pub use session::{chunk_dir, chunk_file, Session};
pub use state::State;
