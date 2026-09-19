// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! ONNX session management for the five-model parakeet TDT pipeline plus the
//! mel-feature preprocessor and the SentencePiece token table.

use super::error::ParakeetError;
use super::ParakeetOpts;
use ndarray::{ArrayD, ArrayViewD, Axis, IxDyn};
use ort::execution_providers::{CPUExecutionProvider, ExecutionProviderDispatch};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::{Tensor, TensorRef};
use std::io::{BufRead, BufReader};
use std::ops::Add;
use std::path::Path;

/// Token-vocab size of the exported TDT graphs (incl. trailing blank); the
/// joint-net logit block `[..VOCAB_SIZE]` is scored against it and the
/// remainder holds the duration bins.
pub(crate) const VOCAB_SIZE: usize = 1025;

fn missing_output(name: &str) -> ParakeetError {
    ParakeetError::Model(format!("graph output {name:?} not found"))
}

pub struct ParakeetModel {
    encoder: Session,
    decoder: Session,
    joint_pred: Session,
    joint_enc: Session,
    joint_net: Session,
    preprocessor: Session,
}

fn log_softmax(input: &ArrayViewD<f32>, axis: Axis) -> Result<ArrayD<f32>, ParakeetError> {
    let max = input.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let x_minus_max = input.mapv(|x| x - max);
    let exps = x_minus_max.mapv(f32::exp);
    let sum_of_exps = exps.sum_axis(axis);
    let log_sum_of_exps = sum_of_exps.mapv(f32::ln).insert_axis(axis);
    Ok(x_minus_max - &log_sum_of_exps)
}

fn cpu_provider() -> Vec<ExecutionProviderDispatch> {
    vec![CPUExecutionProvider::default().build()]
}

static LOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RESIDENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Parakeet models this process has built since it started. One model is six
/// ONNX sessions.
pub fn loads() -> u64 {
    LOADS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Parakeet models alive right now.
pub fn resident() -> u64 {
    RESIDENT.load(std::sync::atomic::Ordering::Relaxed)
}

impl Drop for ParakeetModel {
    fn drop(&mut self) {
        RESIDENT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl ParakeetModel {
    pub fn new(model_dir: &Path, opts: &ParakeetOpts) -> Result<Self, ParakeetError> {
        let model = ParakeetModel {
            encoder: Self::init_encoder_session(model_dir, opts)?,
            decoder: Self::init_decoder_session(model_dir)?,
            joint_pred: Self::init_joint_pred_session(model_dir)?,
            joint_enc: Self::init_joint_enc_session(model_dir)?,
            joint_net: Self::init_joint_net_session(model_dir)?,
            preprocessor: Self::init_preprocessor_session(model_dir)?,
        };
        LOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        RESIDENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(model)
    }

    /// The encoder dominates decode cost; it gets the configurable thread
    /// budget and spin policy. The other sessions keep the per-component
    /// threading the probe measured as fastest.
    fn init_encoder_session(model_dir: &Path, opts: &ParakeetOpts) -> Result<Session, ParakeetError> {
        let name = "encoder.int8.onnx";
        log::info!("parakeet: loading {name}");
        let encoder = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_execution_providers(cpu_provider())?
            .with_parallel_execution(true)?
            .with_intra_threads(opts.encoder_threads.max(1))?
            .with_inter_threads(4)?
            .with_config_entry(
                "session.intra_op.allow_spinning",
                if opts.spin { "1" } else { "0" },
            )?
            .commit_from_file(model_dir.join(name))?;
        Ok(encoder)
    }

    fn init_decoder_session(model_dir: &Path) -> Result<Session, ParakeetError> {
        let name = "decoder.int8.onnx";
        log::info!("parakeet: loading {name}");
        let decoder = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_execution_providers(cpu_provider())?
            .with_parallel_execution(false)?
            .with_intra_threads(1)?
            .with_inter_threads(1)?
            .commit_from_file(model_dir.join(name))?;
        Ok(decoder)
    }

    fn init_joint_pred_session(model_dir: &Path) -> Result<Session, ParakeetError> {
        let name = "joint.pred.int8.onnx";
        log::info!("parakeet: loading {name}");
        let joint_pred = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_execution_providers(cpu_provider())?
            .with_parallel_execution(false)?
            .with_intra_threads(1)?
            .with_inter_threads(1)?
            .commit_from_file(model_dir.join(name))?;
        Ok(joint_pred)
    }

    fn init_joint_enc_session(model_dir: &Path) -> Result<Session, ParakeetError> {
        let name = "joint.enc.int8.onnx";
        log::info!("parakeet: loading {name}");
        let joint_enc = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_execution_providers(cpu_provider())?
            .with_parallel_execution(true)?
            .with_intra_threads(4)?
            .with_inter_threads(2)?
            .commit_from_file(model_dir.join(name))?;
        Ok(joint_enc)
    }

    fn init_joint_net_session(model_dir: &Path) -> Result<Session, ParakeetError> {
        let name = "joint.joint_net.int8.onnx";
        log::info!("parakeet: loading {name}");
        let joint_net = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_execution_providers(cpu_provider())?
            .with_parallel_execution(false)?
            .with_intra_threads(1)?
            .with_inter_threads(1)?
            .commit_from_file(model_dir.join(name))?;
        Ok(joint_net)
    }

    fn init_preprocessor_session(model_dir: &Path) -> Result<Session, ParakeetError> {
        let name = "nemo128.onnx";
        log::info!("parakeet: loading {name}");
        let preprocessor = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_execution_providers(cpu_provider())?
            .with_parallel_execution(true)?
            .with_intra_threads(4)?
            .with_inter_threads(4)?
            .commit_from_file(model_dir.join(name))?;
        Ok(preprocessor)
    }

    pub fn fbank_features(
        &mut self,
        audio: &ArrayViewD<f32>,
        audio_len: i64,
    ) -> Result<ArrayD<f32>, ParakeetError> {
        let inputs = ort::inputs![
            "waveforms" => TensorRef::from_array_view(audio.view())?,
            "waveforms_lens" => Tensor::from_array(([1], vec![audio_len].into_boxed_slice()))?,
        ];
        let outputs = self.preprocessor.run(inputs)?;
        let features = outputs.get("features").ok_or_else(|| missing_output("features"))?.try_extract_array()?;
        Ok(features.to_owned())
    }

    pub fn encoder_infer(&mut self, features: &ArrayViewD<f32>) -> Result<ArrayD<f32>, ParakeetError> {
        let input_length = features.shape()[2] as i64;
        let inputs = ort::inputs![
            "audio_signal" => TensorRef::from_array_view(features.view())?,
            "length" => Tensor::from_array(([1], vec![input_length].into_boxed_slice()))?,
        ];
        let outputs = self.encoder.run(inputs)?;
        let encoder_output = outputs.get("outputs").ok_or_else(|| missing_output("outputs"))?.try_extract_array()?;
        let encoder_output = encoder_output.permuted_axes(IxDyn(&[0, 2, 1]));
        Ok(encoder_output.to_owned())
    }

    pub fn joint_enc_infer(
        &mut self,
        encoder_output: &ArrayViewD<f32>,
    ) -> Result<ArrayD<f32>, ParakeetError> {
        let contiguous_encoder_output = encoder_output.as_standard_layout();
        let inputs = ort::inputs![
            "input" => TensorRef::from_array_view(contiguous_encoder_output.view())?
        ];
        let outputs = self.joint_enc.run(inputs)?;
        let res = outputs.get("output").ok_or_else(|| missing_output("output"))?.try_extract_array()?;
        Ok(res.to_owned())
    }

    /// LSTM state shape is fixed by the exported decoder graph.
    pub fn get_init_decoder_state(&self) -> (ArrayD<f32>, ArrayD<f32>) {
        let state0 = ArrayD::zeros(IxDyn(&[2, 1, 640]));
        let state1 = ArrayD::zeros(IxDyn(&[2, 1, 640]));
        (state0, state1)
    }

    pub fn decoder_infer(
        &mut self,
        target: i64,
        state0: &ArrayViewD<f32>,
        state1: &ArrayViewD<f32>,
    ) -> Result<(ArrayD<f32>, ArrayD<f32>, ArrayD<f32>), ParakeetError> {
        let inputs = ort::inputs![
            "targets" => Tensor::from_array(([1, 1], vec![target as i32].into_boxed_slice()))?,
            "target_length" => Tensor::from_array(([1], vec![1i32].into_boxed_slice()))?,
            "states.1" => TensorRef::from_array_view(state0.view())?,
            "onnx::Slice_3" => TensorRef::from_array_view(state1.view())?,
        ];
        let outputs = self.decoder.run(inputs)?;
        let decoder_output = outputs.get("outputs").ok_or_else(|| missing_output("outputs"))?.try_extract_array()?;
        let decoder_output = decoder_output.permuted_axes(IxDyn(&[0, 2, 1]));
        let state0_next: ArrayViewD<f32> = outputs.get("states").ok_or_else(|| missing_output("states"))?.try_extract_array()?;
        let state1_next: ArrayViewD<f32> = outputs.get("162").ok_or_else(|| missing_output("162"))?.try_extract_array()?;
        Ok((
            decoder_output.to_owned(),
            state0_next.to_owned(),
            state1_next.to_owned(),
        ))
    }

    pub fn joint_pred_infer(
        &mut self,
        decoder_output: &ArrayViewD<f32>,
    ) -> Result<ArrayD<f32>, ParakeetError> {
        let inputs = ort::inputs![
            "onnx::MatMul_0" => TensorRef::from_array_view(decoder_output.view())?,
        ];
        let outputs = self.joint_pred.run(inputs)?;
        let res = outputs.get("5").ok_or_else(|| missing_output("5"))?.try_extract_array()?;
        Ok(res.to_owned())
    }

    pub fn joint_net_infer(
        &mut self,
        f: &ArrayViewD<f32>,
        g: &ArrayViewD<f32>,
    ) -> Result<ArrayD<f32>, ParakeetError> {
        let ff = f.to_shape(IxDyn(&[1, 1, 1, 640]))?;
        let gg = g.to_shape(IxDyn(&[1, 1, 1, 640]))?;
        let input = ff.add(gg);
        let inputs = ort::inputs![
            "input.1" => TensorRef::from_array_view(input.view())?,
        ];
        let outputs = self.joint_net.run(inputs)?;
        let res = outputs.get("6").ok_or_else(|| missing_output("6"))?.try_extract_array()?;
        // The decode loop slices [..VOCAB_SIZE] and [VOCAB_SIZE..] out of the
        // last axis; a narrower graph would panic there.
        let width = res.shape().last().copied().unwrap_or(0);
        if width <= VOCAB_SIZE {
            return Err(ParakeetError::Model(format!(
                "joint logits width {width}, expected > {VOCAB_SIZE}"
            )));
        }
        let res = log_softmax(&res, Axis(3))?;
        Ok(res)
    }
}

pub struct ParakeetTokenizer {
    tokens: Vec<String>,
}

impl ParakeetTokenizer {
    pub fn new(model_dir: &Path) -> Result<Self, ParakeetError> {
        let file = std::fs::File::open(model_dir.join("tokens.txt"))?;
        let reader = BufReader::new(file);
        let mut tokens: Vec<String> = Vec::with_capacity(VOCAB_SIZE);
        for (i, line) in reader.lines().enumerate() {
            let line = line?;
            let tok = line.split_whitespace().next().ok_or_else(|| {
                ParakeetError::Model(format!("tokens.txt line {} is empty", i + 1))
            })?;
            tokens.push(tok.to_string());
        }
        // The decode loop indexes by argmax over the fixed logit block, so a
        // short table would be an out-of-bounds read.
        if tokens.len() != VOCAB_SIZE {
            return Err(ParakeetError::Model(format!(
                "tokens.txt has {} rows, expected {VOCAB_SIZE}",
                tokens.len()
            )));
        }
        Ok(ParakeetTokenizer { tokens })
    }

    /// Blank is the last row of the token table by NeMo convention.
    pub fn blank_id(&self) -> i64 {
        self.tokens.len() as i64 - 1
    }

    pub fn decode(&self, idx: i64) -> &str {
        &self.tokens[idx as usize]
    }
}
