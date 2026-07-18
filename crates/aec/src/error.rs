//! AEC engine error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("model file not found: {0}")]
    ModelNotFound(String),

    #[error("ONNX runtime: {0}")]
    Onnx(String),

    #[error("invalid frame: expected {expected} samples of {dtype}, got {actual}")]
    InvalidFrame {
        expected: usize,
        actual: usize,
        dtype: &'static str,
    },

    #[error("WAV: {0}")]
    Wav(#[from] hound::Error),

    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

impl<R> From<ort::Error<R>> for Error {
    fn from(value: ort::Error<R>) -> Self {
        Self::Onnx(format!("{value}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
