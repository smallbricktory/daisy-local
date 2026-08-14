//! Vendored parakeet TDT decode core. Origin: parakeet-rs fork
//! (MIT/Apache-2.0, see `LICENSE-parakeet-rs`), trimmed for Daisy: CPU
//! execution provider only, session tuning via [`ParakeetOpts`] instead of
//! env vars, encoder-output debug entry point removed. Model layout is the
//! istupakov split-file ONNX export (`encoder.int8.onnx`, `decoder.int8.onnx`,
//! `joint.*.int8.onnx`, `nemo128.onnx`, `tokens.txt`).

pub mod asr;
pub mod batch;
pub mod committer;
pub mod error;
pub mod model;
pub mod realtime;

pub use asr::ParakeetASR;
pub use error::ParakeetError;

use crate::streaming::agreement::StreamToken;

/// Session configuration for the ONNX pipeline. Only the int8-quantized model
/// files are loaded (the fp32 export is not shipped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParakeetOpts {
    /// Intra-op threads for the encoder session (the dominant cost).
    pub encoder_threads: usize,
    /// Allow ONNX Runtime intra-op spin-waiting.
    pub spin: bool,
}

impl Default for ParakeetOpts {
    fn default() -> Self {
        Self { encoder_threads: 4, spin: false }
    }
}

impl ParakeetASR {
    /// Decode a 16 kHz mono buffer into word tokens with window-relative
    /// millisecond spans.
    pub fn decode(&mut self, samples: &[f32]) -> Result<Vec<StreamToken>, ParakeetError> {
        let mut res = self.infer_buffer(samples)?;
        res.generate_word_records();
        Ok(res
            .get_word_records()
            .iter()
            .map(|w| StreamToken {
                text: w.word.trim().to_string(),
                start_ms: (w.start_time as f64 * 1000.0) as i64,
                end_ms: (w.end_time as f64 * 1000.0) as i64,
            })
            .collect())
    }
}
