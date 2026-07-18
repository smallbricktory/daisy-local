//! Error type for the recording lifecycle layer.

use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum RecordingError {
    #[error("invalid state transition: {from:?} -> {to:?}")]
    InvalidTransition { from: &'static str, to: &'static str },

    #[error("session directory already exists: {0}")]
    SessionExists(PathBuf),

    #[error("session directory not found: {0}")]
    SessionMissing(PathBuf),

    #[error("session is still live (heartbeat from pid {pid} within {age_secs}s)")]
    SessionStillLive { pid: u32, age_secs: u64 },

    #[error("worker thread panicked or exited unexpectedly")]
    WorkerGone,

    #[error("audio-engine error: {0}")]
    AudioEngine(#[from] audio_engine::error::Error),

    #[error("aec error: {0}")]
    Aec(String),

    #[error("io error at {path}: {source}")]
    Io { path: PathBuf, #[source] source: io::Error },

    #[error("manifest decode/encode error: {0}")]
    Manifest(#[from] serde_json::Error),

    #[error("hound (wav) error: {0}")]
    Wav(#[from] hound::Error),
}

pub type Result<T> = std::result::Result<T, RecordingError>;
