// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Recording lifecycle: session-on-disk + state machine + controllable Recorder.

pub mod audio_import;
pub mod compress;
pub mod error;
pub(crate) mod aec_gate;
pub mod heartbeat;
pub mod health;
pub mod jsonl_sink;
pub mod supervisor;
pub mod live_aec_wav;
pub mod live_pipeline;
pub mod live_transcript;
pub mod manifest;
pub mod manifest_ops;
pub mod flight_recorder;
pub mod mixdown;
pub mod notes;
pub mod proc_stats;
pub mod recorder;
pub mod speech_levels;
pub mod session;
pub mod session_meta;
pub mod speakers;
pub mod state;
pub mod telemetry;
pub(crate) mod worker;

pub use error::{RecordingError, Result};
pub use live_pipeline::LiveMode;
pub use manifest::{AecMode, ChunkManifest, SessionManifest};
pub use recorder::{apply_aec, apply_denoise, AecOverride, Recorder, RecorderConfig};
pub use transcript::wav_io::{
    read_wav_mono_f32, read_wav_mono_i16, write_wav_mono_i16, WavIoError,
};
pub use session::{chunk_dir, chunk_rel, Session};
pub use state::State;
