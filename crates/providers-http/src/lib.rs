// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The batch `Transcriber` trait + session orchestrator, and HTTP model
//! discovery for the (summarization) providers. The sole transcriber
//! implementation is on-device Whisper in `providers-local`.

pub mod error;
pub mod models;
pub mod orchestrator;

pub use error::{ProviderError, Result};
pub use models::{list_models, lm_studio_loaded_context, normalize_compat_base, probe_chat_path};
pub use orchestrator::{clear_chunk_transcript_checkpoints, transcribe_session};
use std::path::Path;
use transcript::Segment;

/// 16 kHz mono audio as `f32` samples in `[-1, 1]`: what every
/// [`Transcriber`] takes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pcm16k(Vec<f32>);

impl Pcm16k {
    pub const SAMPLE_RATE: u32 = 16_000;

    pub fn new(samples: Vec<f32>) -> Self {
        Self(samples)
    }

    /// 16-bit samples scaled to `[-1, 1]`.
    pub fn from_i16(samples: &[i16]) -> Self {
        Self(samples.iter().map(|&s| f32::from(s) / 32_768.0).collect())
    }

    /// Decodes a WAV file, a sealed session stream or a plaintext file
    /// (`transcript::wav_io`). The rate must be 16 kHz and is checked before
    /// any samples are decoded; multi-channel input is downmixed by
    /// averaging.
    pub fn read_wav(path: &Path) -> Result<Self> {
        let (sample_rate, _) = transcript::wav_io::wav_spec(path).map_err(|e| ProviderError::Other(e.to_string()))?;
        if sample_rate != Self::SAMPLE_RATE {
            return Err(ProviderError::Other(format!(
                "on-device transcription expects 16 kHz audio; got {} Hz ({})",
                sample_rate,
                path.display()
            )));
        }
        let (samples, _) = transcript::wav_io::read_wav_mono_f32(path).map_err(|e| ProviderError::Other(e.to_string()))?;
        Ok(Self(samples))
    }

    pub fn samples(&self) -> &[f32] {
        &self.0
    }

    pub fn into_samples(self) -> Vec<f32> {
        self.0
    }
}

/// A blocking transcription provider: 16 kHz mono samples in, time-stamped
/// segments in chronological order out.
pub trait Transcriber: Send + Sync {
    /// Stable string identifying the provider (e.g. `"whisper_local"`).
    fn name(&self) -> &'static str;

    /// Stable string identifying the model in use (e.g. a ggml file name).
    fn model(&self) -> &str;

    /// Transcribe `audio`. `language_hint` of `Some("en")` accelerates
    /// Whisper-family models; `None` lets the model auto-detect.
    fn transcribe(&self, audio: &Pcm16k, language_hint: Option<&str>) -> Result<Vec<Segment>>;

    /// Calls to `transcribe` this provider runs at once from separate
    /// threads without slowing any of them. Callers with independent audio
    /// to decode run up to this many in parallel.
    fn parallelism(&self) -> usize {
        1
    }
}
