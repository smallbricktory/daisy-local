//! DTLN-aec acoustic echo cancellation (Rust port of Hyprnote `crates/aec`).
//!
//! Streaming AEC: feed `BLOCK_SHIFT` (128) samples of mic + far at a time;
//! output is `BLOCK_SHIFT` samples of echo-cancelled mic. Two-stage ONNX
//! pipeline (frequency-domain mask + time-domain refinement) with LSTM state
//! carried between calls.

pub mod constants;
pub mod echo_canceller;
pub mod error;
pub mod fft;
pub mod onnx;

pub use constants::{
    BLOCK_SHIFT, BLOCK_SIZE, FFT_BINS, SAMPLE_RATE, STATE_SHAPE, STATE_SIZE, model_dir,
};
pub use echo_canceller::AcousticEchoCanceller;
pub use error::{Error, Result};
