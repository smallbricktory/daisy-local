// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Thin wrapper over the speakrs pipeline (pyannote community-1): mono-16k
//! audio → speaker turns. The caller builds the per-track audio and maps
//! turns onto transcript segments; the k-means path in `lib.rs` is the
//! default and fallback.
use speakrs::pipeline::{PipelineBuilder, PipelineConfig, FRAME_DURATION_SECONDS, FRAME_STEP_SECONDS};
use speakrs::{ExecutionMode, RuntimeConfig};

/// Embedding-inference worker count. Parallelizes the per-chunk embedding
/// pass on CoreML/CUDA; no-op on the CPU provider.
const EMB_WORKERS: usize = 8;

/// A speaker turn: `[start, end]` seconds + speakrs label (e.g. `"SPEAKER_00"`).
#[derive(Debug, Clone)]
pub struct Turn {
    pub start: f64,
    pub end: f64,
    pub speaker: String,
}

/// Execution provider per platform: CoreML on macOS, CPU elsewhere.
pub fn exec_mode() -> ExecutionMode {
    if cfg!(target_os = "macos") {
        ExecutionMode::CoreMl
    } else {
        ExecutionMode::Cpu
    }
}

/// Speakrs runs on a hardware accelerator here.
pub fn accelerated() -> bool {
    !matches!(exec_mode(), ExecutionMode::Cpu)
}

/// Run speakrs on mono-16k audio, returning merged speaker turns. `known_count`
/// forces that many speakers: the training embeddings are re-clustered into
/// exactly that many groups when AHC + VBx land on a different number, so the
/// count is honoured in both directions. `None` leaves the count automatic.
/// Failures are returned as errors, never panics.
pub fn diarize_audio(audio: &[f32], known_count: Option<usize>) -> anyhow::Result<Vec<Turn>> {
    let ep = exec_mode();
    let mut config = PipelineConfig::for_mode(ep);
    config.vbx.fb = 3.0;
    config.num_speakers = known_count.filter(|n| *n > 0);
    // Models load from SPEAKRS_MODELS_DIR (ONNX for CPU, .mlmodelc for
    // CoreML); there is no download path. A missing dir returns an error.
    let dir = std::env::var("SPEAKRS_MODELS_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .ok_or_else(|| anyhow::anyhow!("SPEAKRS_MODELS_DIR unset (bundled models missing?)"))?;
    let mut pipeline = PipelineBuilder::from_dir(dir, ep)
        .runtime(RuntimeConfig { chunk_emb_workers: EMB_WORKERS, ..Default::default() })
        .build()
        .map_err(|e| anyhow::anyhow!("speakrs build: {e}"))?;
    let result = pipeline
        .run_with_config(audio, "session", &config)
        .map_err(|e| anyhow::anyhow!("speakrs run: {e}"))?;
    Ok(result
        .discrete_diarization
        .to_segments(FRAME_STEP_SECONDS, FRAME_DURATION_SECONDS)
        .into_iter()
        .map(|s| Turn { start: s.start, end: s.end, speaker: s.speaker })
        .collect())
}
