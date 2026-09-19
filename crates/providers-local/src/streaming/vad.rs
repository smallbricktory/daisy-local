// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Silero VAD (v5) as a faint-audio *detector* — never a decode gate. When an
//! energy-positive window decodes to zero words, the detector decides whether
//! to raise a "possible missed speech" marker. Decoding is never gated on it
//! (a VAD gate silenced quiet real speech twice; annotation fails quiet).
//!
//! ONNX contract (Silero VAD v5): inputs `input`[1,512] f32, `state`[2,1,128]
//! f32, `sr` i64 scalar → outputs `output`[1,1] (speech prob), `stateN`[2,1,128].
//! Uses the shared ONNX Runtime (`ort`). Ships win/linux only; macOS has no
//! marker VAD (the OS `SpeechDetector` never reports results, so the marker
//! fails quiet there).

use std::path::Path;

use ndarray::{Array0, Array3, ArrayView2};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::TensorRef;

const FRAME_SAMPLES: usize = 512; // 32ms @ 16kHz (v5 16k context window)
/// Speech-probability threshold (Silero-recommended ~0.3–0.5).
const DEFAULT_THRESHOLD: f32 = 0.4;
/// Scan at most ~1.5s per check — bounding cost; recent audio carries the
/// signal. Callers hand `confirms` at most this much tail audio.
pub const SCAN_SAMPLES: usize = 24_000;

/// Lazy-loading detector handle shared by the streaming backends. Load
/// happens on the first zero-word window; a failed load stays failed (no
/// marker) without retry spam.
pub struct MarkerDetector {
    path: std::path::PathBuf,
    vad: std::sync::Mutex<Option<Option<SileroVad>>>,
}

impl MarkerDetector {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path, vad: std::sync::Mutex::new(None) }
    }

    /// `Some(true)` = speech-like tail, `Some(false)` = non-speech, `None` =
    /// detector unavailable (model missing/broken → marker fails quiet).
    pub fn confirms(&self, samples: &[f32]) -> Option<bool> {
        let mut slot = self.vad.lock().ok()?;
        if slot.is_none() {
            *slot = Some(match SileroVad::load(&self.path) {
                Ok(v) => Some(v),
                Err(e) => {
                    log::warn!("faint-audio marker: silero load failed ({e}); markers off");
                    None
                }
            });
        }
        slot.as_mut()?.as_mut().map(|v| v.speech_in_tail(samples))
    }
}

static LOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RESIDENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Silero sessions this process has built since it started.
pub fn silero_loads() -> u64 {
    LOADS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Silero sessions alive right now.
pub fn silero_resident() -> u64 {
    RESIDENT.load(std::sync::atomic::Ordering::Relaxed)
}

pub struct SileroVad {
    session: Session,
    sr: Array0<i64>,
    threshold: f32,
}

impl Drop for SileroVad {
    fn drop(&mut self) {
        RESIDENT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl SileroVad {
    /// Load the Silero v5 ONNX model. Err (logged by the caller) → no marker.
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self, String> {
        let session = Session::builder()
            .map_err(|e| e.to_string())?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| e.to_string())?
            .with_intra_threads(1)
            .map_err(|e| e.to_string())?
            .commit_from_file(model_path)
            .map_err(|e| e.to_string())?;
        LOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        RESIDENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Self { session, sr: Array0::from_elem((), 16000i64), threshold: DEFAULT_THRESHOLD })
    }

    /// True if the most-recent ~1.5s of `samples` contains speech. Scans
    /// fresh (state reset each call) and early-returns on the first speech
    /// frame. As a *detector*, errors return `false` — no marker rather than
    /// a false alarm.
    pub fn speech_in_tail(&mut self, samples: &[f32]) -> bool {
        let start = samples.len().saturating_sub(SCAN_SAMPLES);
        let mut state = Array3::<f32>::zeros((2, 1, 128));
        for frame in samples[start..].chunks_exact(FRAME_SAMPLES) {
            match self.speech_prob(frame, &mut state) {
                Ok(p) if p > self.threshold => return true,
                Ok(_) => {}
                Err(e) => {
                    log::warn!("silero vad: {e}; skipping marker");
                    return false;
                }
            }
        }
        false
    }

    fn speech_prob(&mut self, frame: &[f32], state: &mut Array3<f32>) -> Result<f32, String> {
        let input = ArrayView2::from_shape((1, FRAME_SAMPLES), frame).map_err(|e| e.to_string())?;
        let t_input = TensorRef::from_array_view(input.into_dyn()).map_err(|e| e.to_string())?;
        let t_state = TensorRef::from_array_view(state.view().into_dyn()).map_err(|e| e.to_string())?;
        let t_sr = TensorRef::from_array_view(self.sr.view().into_dyn()).map_err(|e| e.to_string())?;
        let out = self
            .session
            .run(ort::inputs!["input" => t_input, "state" => t_state, "sr" => t_sr])
            .map_err(|e| e.to_string())?;
        let next = out
            .get("stateN")
            .ok_or("missing stateN")?
            .try_extract_array::<f32>()
            .map_err(|e| e.to_string())?;
        *state = next.to_owned().into_dimensionality().map_err(|e| e.to_string())?;
        let prob = out
            .get("output")
            .ok_or("missing output")?
            .try_extract_array::<f32>()
            .map_err(|e| e.to_string())?;
        Ok(prob.iter().next().copied().unwrap_or(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Contract check against a real Silero v5 model. Ignored by default (needs
    // the file): `DAISY_VAD_TEST_MODEL=/path/silero_vad.onnx cargo test -p \
    // providers-local vad -- --ignored`. Silence must return false — if the
    // ONNX contract (names/shapes incl. the scalar `sr`) is wrong it errors →
    // false too, so pair this with a speech wav when validating a new model.
    #[test]
    #[ignore]
    fn loads_and_runs_real_v5_model() {
        let path = std::env::var("DAISY_VAD_TEST_MODEL").expect("set DAISY_VAD_TEST_MODEL");
        let mut vad = SileroVad::load(&path).expect("load v5 model");
        let silence = vec![0.0f32; SCAN_SAMPLES];
        assert!(!vad.speech_in_tail(&silence), "silence must not be speech (contract OK)");
    }
}
