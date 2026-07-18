//! Thin wrappers around `ort::session::Session` for the two DTLN-aec stages.
//!
//! Each stage's I/O contract:
//!
//! Stage 1 (frequency-domain mask):
//!   IN   slot 0  (1, 1, FFT_BINS) f32   — mic magnitude spectrum
//!   IN   slot 1  STATE_SHAPE      f32   — LSTM state
//!   IN   slot 2  (1, 1, FFT_BINS) f32   — far magnitude spectrum
//!   OUT  slot 0  (1, 1, FFT_BINS) f32   — suppression mask
//!   OUT  slot 1  STATE_SHAPE      f32   — new LSTM state
//!
//! Stage 2 (time-domain refinement):
//!   IN   slot 0  (1, 1, BLOCK_SIZE) f32 — mic time-domain block (post-mask)
//!   IN   slot 1  STATE_SHAPE        f32 — LSTM state
//!   IN   slot 2  (1, 1, BLOCK_SIZE) f32 — far time-domain block
//!   OUT  slot 0  (1, 1, BLOCK_SIZE) f32 — clean audio block
//!   OUT  slot 1  STATE_SHAPE        f32 — new LSTM state
//!
//! The auto-numbered names (`input_3`, `input_4`, `input_5`, etc.) are
//! positional in the ONNX file; inputs and outputs are addressed by slot
//! index, not by lexical name.

use crate::constants::intra_op_threads;
use crate::error::{Error, Result};
use std::path::Path;

#[derive(Debug)]
pub struct Stage1Session {
    inner: ort::session::Session,
}

#[derive(Debug)]
pub struct Stage2Session {
    inner: ort::session::Session,
}

impl Stage1Session {
    pub fn load(path: &Path) -> Result<Self> {
        load_session(path).map(|inner| Self { inner })
    }

    pub fn input_count(&self) -> usize {
        self.inner.inputs().len()
    }

    pub fn output_count(&self) -> usize {
        self.inner.outputs().len()
    }

    pub fn inner(&self) -> &ort::session::Session {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut ort::session::Session {
        &mut self.inner
    }
}

impl Stage2Session {
    pub fn load(path: &Path) -> Result<Self> {
        load_session(path).map(|inner| Self { inner })
    }

    pub fn input_count(&self) -> usize {
        self.inner.inputs().len()
    }

    pub fn output_count(&self) -> usize {
        self.inner.outputs().len()
    }

    pub fn inner(&self) -> &ort::session::Session {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut ort::session::Session {
        &mut self.inner
    }
}

fn load_session(path: &Path) -> Result<ort::session::Session> {
    if !path.exists() {
        return Err(Error::ModelNotFound(format!(
            "model file not found at {}",
            path.display()
        )));
    }
    let session = ort::session::Session::builder()?
        .with_intra_threads(intra_op_threads())?
        .commit_from_file(path)?;
    Ok(session)
}
