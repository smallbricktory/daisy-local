// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! pyannote segmentation-3.0 (ONNX) runner: 10 s windows at a caller-chosen
//! step, streaming per-window local-speaker activity.
//!
//! Window layout, tensor handling and the output decode are adapted from
//! speakrs (<https://github.com/avencera/speakrs>, Apache-2.0):
//! `inference/segmentation.rs`, `inference/segmentation/run.rs` and
//! `inference/segmentation/tensor.rs`. Attribution and the Apache-2.0 text
//! are in THIRD-PARTY-LICENSES.txt (source: `scripts/lib/model-notices.txt`).

use std::path::PathBuf;

use ndarray::Array3;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::TensorRef;

use crate::{model_dir, require_model_file, BackgroundThreads, Result, VpError};

pub const SAMPLE_RATE: usize = 16_000;
/// Samples in one model window (10 s).
pub const WINDOW_SAMPLES: usize = 10 * SAMPLE_RATE;
/// Local speakers the model tracks per window.
pub const LOCAL_SPEAKERS: usize = 3;
/// Powerset classes per frame: `{}`, `{0}`, `{1}`, `{2}`, `{0,1}`, `{0,2}`, `{1,2}`.
pub const POWERSET_CLASSES: usize = 7;

const MEMBERS: [[bool; LOCAL_SPEAKERS]; POWERSET_CLASSES] = [
    [false, false, false],
    [true, false, false],
    [false, true, false],
    [false, false, true],
    [true, true, false],
    [true, false, true],
    [false, true, true],
];

/// Path of the segmentation weights.
pub fn segmentation_model_path() -> PathBuf {
    model_dir().join("segmentation-3.0.onnx")
}

/// Segmentation-3.0 session.
pub struct SegmentationModel {
    session: ort::session::Session,
    input: Array3<f32>,
}

impl SegmentationModel {
    /// Load for background finalize work: at most four intra-op threads,
    /// within the finalize budget, pool threads at background priority.
    pub fn load_background() -> Result<Self> {
        let path = segmentation_model_path();
        require_model_file(&path)?;
        let threads = transcript::cpu::finalize_cpu_budget().min(4);
        let session = ort::session::Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level1)?
            .with_thread_manager(BackgroundThreads)?
            .with_intra_threads(threads)?
            .with_inter_threads(1)?
            .with_memory_pattern(true)?
            .commit_from_file(&path)?;
        crate::LOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::RESIDENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Self { session, input: Array3::zeros((1, 1, WINDOW_SAMPLES)) })
    }

    /// One window's per-frame local-speaker probabilities into `out`.
    /// `samples` shorter than a window are zero-padded; longer ones are
    /// cut to one window.
    pub fn window_probs(
        &mut self,
        samples: &[f32],
        out: &mut Vec<[f32; LOCAL_SPEAKERS]>,
    ) -> Result<()> {
        let samples = &samples[..samples.len().min(WINDOW_SAMPLES)];
        self.window(samples, 0, &mut |_, logits| marginals(logits, out))
    }

    /// Run every window of `audio` (mono 16 kHz) at `step_s` and hand each
    /// window's raw logits (`frames * 7`, row-major) to `on_logits` with its
    /// index. Audio shorter than one window runs as a single zero-padded
    /// window. Returns the window count.
    pub fn run_logits(
        &mut self,
        audio: &[f32],
        step_s: f32,
        mut on_logits: impl FnMut(usize, &[f32]),
    ) -> Result<usize> {
        let step = ((step_s * SAMPLE_RATE as f32) as usize).max(1);
        let mut index = 0;
        let mut offset = 0;
        while offset + WINDOW_SAMPLES <= audio.len() {
            self.window(&audio[offset..offset + WINDOW_SAMPLES], index, &mut on_logits)?;
            index += 1;
            offset += step;
        }
        // Remainder past the last full window, zero-padded; a clip shorter
        // than one window is that remainder from offset 0.
        if offset < audio.len() {
            self.window(&audio[offset..], index, &mut on_logits)?;
            index += 1;
        }
        Ok(index)
    }

    /// As [`Self::run_logits`], delivering each window's per-frame local
    /// speaker probabilities (powerset softmax, summed per member speaker).
    pub fn run_windows(
        &mut self,
        audio: &[f32],
        step_s: f32,
        mut on_window: impl FnMut(usize, &[[f32; LOCAL_SPEAKERS]]),
    ) -> Result<usize> {
        let mut probs = Vec::new();
        self.run_logits(audio, step_s, |i, logits| {
            marginals(logits, &mut probs);
            on_window(i, &probs);
        })
    }

    fn window(
        &mut self,
        samples: &[f32],
        index: usize,
        sink: &mut impl FnMut(usize, &[f32]),
    ) -> Result<()> {
        self.input.fill(0.0);
        self.input
            .slice_mut(ndarray::s![0, 0, ..samples.len()])
            .assign(&ndarray::ArrayView1::from(samples));
        let tensor = TensorRef::from_array_view(self.input.view())?;
        let outputs = self.session.run(ort::inputs![tensor])?;
        let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        if shape.len() != 3 || shape[2] as usize != POWERSET_CLASSES {
            return Err(VpError::Decode(format!("segmentation output shape {shape:?}")));
        }
        sink(index, data);
        Ok(())
    }
}

/// Speaker turns of one clip whose audio arrives in pieces: the same windows
/// [`SegmentationModel::run_windows`] takes over the whole clip (full windows
/// at every step, then the zero-padded remainder), run as soon as each full
/// window is in.
pub struct StreamSegmenter {
    step: usize,
    buf: Vec<f32>,
    probs: Vec<[f32; LOCAL_SPEAKERS]>,
    builder: crate::turns::TurnBuilder,
    samples: u64,
}

impl StreamSegmenter {
    pub fn new(step_s: f32) -> Self {
        Self {
            step: ((step_s * SAMPLE_RATE as f32) as usize).max(1),
            buf: Vec::with_capacity(WINDOW_SAMPLES),
            probs: Vec::new(),
            builder: crate::turns::TurnBuilder::new(step_s as f64),
            samples: 0,
        }
    }

    /// Samples pushed so far.
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Append mono 16 kHz samples; every window they complete runs now.
    pub fn push(&mut self, model: &mut SegmentationModel, mut samples: &[f32]) -> Result<()> {
        self.samples += samples.len() as u64;
        while !samples.is_empty() {
            let take = (WINDOW_SAMPLES - self.buf.len()).min(samples.len());
            self.buf.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.buf.len() == WINDOW_SAMPLES {
                model.window_probs(&self.buf, &mut self.probs)?;
                self.builder.push_window(self.builder.windows(), &self.probs);
                self.buf.drain(..self.step);
            }
        }
        Ok(())
    }

    /// Turns so far that later audio cannot move by much; see
    /// [`crate::turns::TurnBuilder::finish_through`].
    pub fn provisional(&self) -> crate::turns::Turns {
        self.builder.finish_through()
    }

    /// The clip, `audio_s` long, is complete: run the zero-padded remainder
    /// and resolve.
    pub fn finish(mut self, model: &mut SegmentationModel, audio_s: f64) -> Result<crate::turns::Turns> {
        // After a full window the buffer keeps its overlap with the next one,
        // so a clip of at least one window always ends in a remainder window,
        // as in `run_logits`.
        if !self.buf.is_empty() {
            model.window_probs(&self.buf, &mut self.probs)?;
            self.builder.push_window(self.builder.windows(), &self.probs);
        }
        Ok(self.builder.finish(audio_s))
    }
}

impl Drop for SegmentationModel {
    fn drop(&mut self) {
        crate::RESIDENT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Per-frame local-speaker marginals from powerset logits: softmax, then the
/// sum of the classes containing each speaker. Fills `out`, one entry per frame.
pub fn marginals(logits: &[f32], out: &mut Vec<[f32; LOCAL_SPEAKERS]>) {
    out.clear();
    for row in logits.chunks_exact(POWERSET_CLASSES) {
        let mx = row.iter().copied().fold(f32::MIN, f32::max);
        let e: Vec<f32> = row.iter().map(|v| (v - mx).exp()).collect();
        let z: f32 = e.iter().sum();
        let mut p = [0f32; LOCAL_SPEAKERS];
        for (c, ec) in e.iter().enumerate() {
            for (s, slot) in p.iter_mut().enumerate() {
                if MEMBERS[c][s] {
                    *slot += ec / z;
                }
            }
        }
        out.push(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marginals_sum_the_classes_containing_each_speaker() {
        // Class {0,2} dominant: speakers 0 and 2 near 1, speaker 1 near 0.
        let mut logits = [-20.0f32; POWERSET_CLASSES];
        logits[5] = 20.0;
        let mut out = Vec::new();
        marginals(&logits, &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0][0] > 0.99 && out[0][2] > 0.99 && out[0][1] < 0.01, "{:?}", out[0]);
    }
}
