//! Engine-wide error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("audio I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("WAV: {0}")]
    Wav(#[from] hound::Error),

    #[error("invalid frame: {0}")]
    InvalidFrame(String),

    #[error("PipeWire: {0}")]
    PipeWire(String),

    #[error("macOS audio: {0}")]
    Macos(String),

    #[error("subprocess: {0}")]
    Subprocess(String),

    #[error("source not found: {0}")]
    SourceNotFound(String),

    /// Returned by every audio operation on platforms without a native
    /// backend.
    #[error("audio backend not yet implemented on this platform: {0}")]
    NotSupported(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;
