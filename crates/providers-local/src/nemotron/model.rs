// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! ONNX sessions of the Nemotron export: cache-aware encoder (65 mel frames
//! in, 7 encoder frames out per call), LSTM prediction network and joiner,
//! plus the 1025-row token table (`<blk>` last).

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use ndarray::{s, Array1, Array3, Array4, Ix3, Ix4};
use ort::ep::CPU as CPUExecutionProvider;
use ort::session::builder::{GraphOptimizationLevel, SessionBuilder};
use ort::session::{Session, SessionOutputs};
use ort::value::{DynValue, Tensor, TensorRef};

use super::features::WINDOW_FRAMES;
use super::{NemotronError, NemotronOpts};

pub const BLANK: usize = 1024;
pub const VOCAB: usize = 1025;
pub const FRAME_MS: i64 = 80;
pub const FRAMES_PER_CHUNK: usize = 7;
pub(crate) const HIDDEN: usize = 1024;
const LAYERS: usize = 24;
const LEFT_CONTEXT: usize = 70;
const CONV_CONTEXT: usize = 8;
const PRED_LAYERS: usize = 2;
const PRED_HIDDEN: usize = 640;

static LOADS: AtomicU64 = AtomicU64::new(0);
static RESIDENT: AtomicU64 = AtomicU64::new(0);

/// Nemotron models this process has built since it started.
pub fn loads() -> u64 {
    LOADS.load(Ordering::Relaxed)
}

/// Nemotron models alive right now.
pub fn resident() -> u64 {
    RESIDENT.load(Ordering::Relaxed)
}

/// Encoder attention and convolution caches carried between chunks.
pub struct EncoderCache {
    channel: Array4<f32>,
    time: Array4<f32>,
    len: Array1<i64>,
}

impl Default for EncoderCache {
    fn default() -> Self {
        Self::new()
    }
}

impl EncoderCache {
    pub fn new() -> Self {
        Self {
            channel: Array4::zeros((1, LAYERS, LEFT_CONTEXT, HIDDEN)),
            time: Array4::zeros((1, LAYERS, HIDDEN, CONV_CONTEXT)),
            len: Array1::zeros(1),
        }
    }
}

/// Prediction-network output and LSTM state after the last emitted token.
#[derive(Clone)]
pub struct DecoderState {
    out: Array3<f32>,
    h: Array3<f32>,
    c: Array3<f32>,
}

impl DecoderState {
    pub fn zeros() -> Self {
        Self {
            out: Array3::zeros((1, PRED_HIDDEN, 1)),
            h: Array3::zeros((PRED_LAYERS, 1, PRED_HIDDEN)),
            c: Array3::zeros((PRED_LAYERS, 1, PRED_HIDDEN)),
        }
    }
}

pub struct NemotronModel {
    encoder: Session,
    decoder: Session,
    joiner: Session,
    tokens: Vec<String>,
}

impl Drop for NemotronModel {
    fn drop(&mut self) {
        RESIDENT.fetch_sub(1, Ordering::Relaxed);
    }
}

fn builder(opts: &NemotronOpts, intra: usize) -> Result<SessionBuilder, NemotronError> {
    let b = Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .with_execution_providers(vec![CPUExecutionProvider::default().build()])?
        .with_intra_threads(intra.max(1))?
        .with_inter_threads(1)?
        .with_parallel_execution(false)?
        .with_config_entry("session.intra_op.allow_spinning", "0")?;
    if opts.background {
        Ok(b.with_thread_manager(crate::ort_threads::BackgroundThreads)?)
    } else {
        Ok(b)
    }
}

/// Fails unless the encoder's metadata describes the 560 ms, 128-bin,
/// subsampling-8 export this decoder drives.
pub(crate) fn check_encoder_metadata(get: impl Fn(&str) -> Option<String>) -> Result<(), NemotronError> {
    for (key, want) in [
        ("window_size", "65"),
        ("chunk_shift", "56"),
        ("subsampling_factor", "8"),
        ("feat_dim", "128"),
        ("vocab_size", "1024"),
        ("cache_last_channel_dim2", "70"),
    ] {
        let got = get(key);
        if got.as_deref() != Some(want) {
            return Err(NemotronError::Model(format!("encoder metadata {key} = {got:?}, expected {want}")));
        }
    }
    Ok(())
}

/// `tokens.txt` rows are `<piece> <id>`; the table must hold VOCAB rows with
/// `<blk>` at BLANK.
pub(crate) fn read_tokens(dir: &Path) -> Result<Vec<String>, NemotronError> {
    let path = dir.join("tokens.txt");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| NemotronError::Load { path: path.clone(), reason: e.to_string() })?;
    let tokens: Vec<String> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.rsplit_once(' ').map_or(l, |(p, _)| p).to_string())
        .collect();
    if tokens.len() != VOCAB || tokens[BLANK] != "<blk>" {
        return Err(NemotronError::Model(format!(
            "tokens.txt has {} rows (blank row {:?}), expected {VOCAB} ending in <blk>",
            tokens.len(),
            tokens.get(BLANK)
        )));
    }
    Ok(tokens)
}

/// Builds a session from `path`; a missing, unreadable or rejected file is
/// reported with its path.
fn load_session(mut b: SessionBuilder, path: &Path) -> Result<Session, NemotronError> {
    if let Err(e) = std::fs::metadata(path) {
        return Err(NemotronError::Load { path: path.to_path_buf(), reason: e.to_string() });
    }
    b.commit_from_file(path).map_err(|e| NemotronError::Load { path: path.to_path_buf(), reason: e.to_string() })
}

fn output<'a>(o: &'a SessionOutputs<'_>, name: &str) -> Result<&'a DynValue, NemotronError> {
    o.get(name).ok_or_else(|| NemotronError::Model(format!("graph output {name:?} not found")))
}

fn array3(o: &SessionOutputs<'_>, name: &str) -> Result<Array3<f32>, NemotronError> {
    Ok(output(o, name)?.try_extract_array::<f32>()?.into_dimensionality::<Ix3>()?.to_owned())
}

fn array4(o: &SessionOutputs<'_>, name: &str) -> Result<Array4<f32>, NemotronError> {
    Ok(output(o, name)?.try_extract_array::<f32>()?.into_dimensionality::<Ix4>()?.to_owned())
}

impl NemotronModel {
    pub fn new(dir: &Path, opts: &NemotronOpts) -> Result<Self, NemotronError> {
        let tokens = read_tokens(dir)?;
        log::info!("nemotron: loading encoder ({} threads)", opts.threads);
        let encoder = load_session(builder(opts, opts.threads)?, &dir.join("encoder.int8.onnx"))?;
        let meta = encoder.metadata()?;
        check_encoder_metadata(|k| meta.custom(k))?;
        drop(meta);
        let decoder = load_session(builder(opts, 1)?, &dir.join("decoder.onnx"))?;
        let joiner = load_session(builder(opts, 1)?, &dir.join("joiner.onnx"))?;
        LOADS.fetch_add(1, Ordering::Relaxed);
        RESIDENT.fetch_add(1, Ordering::Relaxed);
        Ok(Self { encoder, decoder, joiner, tokens })
    }

    pub fn token(&self, id: usize) -> &str {
        &self.tokens[id]
    }

    /// One 65-frame window through the encoder; updates `cache` and returns
    /// `[1, 1024, frames]`.
    pub fn encode(&mut self, window: &Array3<f32>, cache: &mut EncoderCache) -> Result<Array3<f32>, NemotronError> {
        let outputs = self.encoder.run(ort::inputs![
            "audio_signal" => TensorRef::from_array_view(window.view())?,
            "length" => Tensor::from_array(([1usize], vec![WINDOW_FRAMES as i64].into_boxed_slice()))?,
            "cache_last_channel" => TensorRef::from_array_view(cache.channel.view())?,
            "cache_last_time" => TensorRef::from_array_view(cache.time.view())?,
            "cache_last_channel_len" => TensorRef::from_array_view(cache.len.view())?,
        ])?;
        let frames = output(&outputs, "encoded_lengths")?
            .try_extract_array::<i64>()?
            .iter()
            .next()
            .copied()
            .unwrap_or(0)
            .max(0) as usize;
        let enc = array3(&outputs, "outputs")?;
        let enc = enc.slice(s![.., .., ..frames.min(enc.shape()[2])]).to_owned();
        cache.channel = array4(&outputs, "cache_last_channel_next")?;
        cache.time = array4(&outputs, "cache_last_time_next")?;
        cache.len = output(&outputs, "cache_last_channel_next_len")?
            .try_extract_array::<i64>()?
            .iter()
            .copied()
            .collect::<Vec<_>>()
            .into();
        Ok(enc)
    }

    /// Advance the prediction network by one emitted token.
    pub fn predict(&mut self, token: usize, state: &DecoderState) -> Result<DecoderState, NemotronError> {
        let outputs = self.decoder.run(ort::inputs![
            "targets" => Tensor::from_array(([1usize, 1], vec![token as i32].into_boxed_slice()))?,
            "target_length" => Tensor::from_array(([1usize], vec![1i32].into_boxed_slice()))?,
            "states.1" => TensorRef::from_array_view(state.h.view())?,
            "onnx::Slice_3" => TensorRef::from_array_view(state.c.view())?,
        ])?;
        Ok(DecoderState { out: array3(&outputs, "outputs")?, h: array3(&outputs, "states")?, c: array3(&outputs, "162")? })
    }

    /// Most likely token id at encoder frame `frame` given the decoder state.
    pub fn joint(&mut self, enc: &Array3<f32>, frame: usize, state: &DecoderState) -> Result<usize, NemotronError> {
        let f = enc.slice(s![.., .., frame..frame + 1]).to_owned();
        let outputs = self.joiner.run(ort::inputs![
            "encoder_outputs" => TensorRef::from_array_view(f.view())?,
            "decoder_outputs" => TensorRef::from_array_view(state.out.view())?,
        ])?;
        let logits = output(&outputs, "outputs")?.try_extract_array::<f32>()?;
        if logits.len() != VOCAB {
            return Err(NemotronError::Model(format!("joiner logits width {}, expected {VOCAB}", logits.len())));
        }
        let mut best = (0usize, f32::NEG_INFINITY);
        for (i, &v) in logits.iter().enumerate() {
            if v > best.1 {
                best = (i, v);
            }
        }
        Ok(best.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nemotron::features::{N_MELS, SILENT_BIN};

    fn test_model_dir() -> Option<std::path::PathBuf> {
        std::env::var_os("DAISY_NEMOTRON_TEST_MODEL_DIR").map(Into::into)
    }

    #[test]
    fn metadata_check_names_the_mismatch() {
        let good = |k: &str| {
            Some(match k {
                "window_size" => "65", "chunk_shift" => "56", "subsampling_factor" => "8",
                "feat_dim" => "128", "vocab_size" => "1024", "cache_last_channel_dim2" => "70",
                _ => return None,
            }.to_string())
        };
        assert!(check_encoder_metadata(good).is_ok());
        let err = check_encoder_metadata(|k| if k == "window_size" { Some("57".into()) } else { good(k) })
            .unwrap_err();
        assert!(err.to_string().contains("window_size"), "{err}");
        let err = check_encoder_metadata(|_| None).unwrap_err();
        assert!(err.to_string().contains("None"), "{err}");
    }

    #[test]
    fn tokens_table_must_end_in_blank() {
        let dir = tempfile::tempdir().unwrap();
        let mut rows: Vec<String> = (0..BLANK).map(|i| format!("p{i} {i}")).collect();
        rows.push(format!("<blk> {BLANK}"));
        std::fs::write(dir.path().join("tokens.txt"), rows.join("\n")).unwrap();
        let t = read_tokens(dir.path()).unwrap();
        assert_eq!(t.len(), VOCAB);
        rows.pop();
        std::fs::write(dir.path().join("tokens.txt"), rows.join("\n")).unwrap();
        assert!(read_tokens(dir.path()).is_err());
    }

    #[test]
    fn load_errors_name_the_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let err = NemotronModel::new(dir.path(), &NemotronOpts::live()).err().expect("no tokens.txt");
        assert!(err.to_string().contains("tokens.txt"), "{err}");
        let mut rows: Vec<String> = (0..BLANK).map(|i| format!("p{i} {i}")).collect();
        rows.push(format!("<blk> {BLANK}"));
        std::fs::write(dir.path().join("tokens.txt"), rows.join("\n")).unwrap();
        let err = NemotronModel::new(dir.path(), &NemotronOpts::live()).err().expect("no encoder");
        assert!(err.to_string().contains("encoder.int8.onnx"), "{err}");
    }

    #[test]
    fn encoder_emits_seven_frames_per_chunk() {
        let Some(dir) = test_model_dir() else { return eprintln!("skip: DAISY_NEMOTRON_TEST_MODEL_DIR not set") };
        let mut model = NemotronModel::new(&dir, &NemotronOpts::live()).unwrap();
        let mut cache = EncoderCache::new();
        let window = ndarray::Array3::from_elem((1, N_MELS, WINDOW_FRAMES), SILENT_BIN);
        for _ in 0..3 {
            let enc = model.encode(&window, &mut cache).unwrap();
            assert_eq!(enc.shape(), &[1, HIDDEN, FRAMES_PER_CHUNK]);
        }
    }

    #[test]
    fn background_model_spawns_its_ort_pools_through_the_manager() {
        let Some(dir) = test_model_dir() else { return eprintln!("skip: DAISY_NEMOTRON_TEST_MODEL_DIR not set") };
        let budget = transcript::cpu::finalize_cpu_budget();
        if budget < 2 {
            return eprintln!("skip: a one-thread budget spawns no pool threads");
        }
        let before = crate::ort_threads::SPAWNED.load(std::sync::atomic::Ordering::Relaxed);
        let mut model = NemotronModel::new(&dir, &NemotronOpts::background()).unwrap();
        let window = ndarray::Array3::from_elem((1, N_MELS, WINDOW_FRAMES), SILENT_BIN);
        model.encode(&window, &mut EncoderCache::new()).unwrap();
        let spawned = crate::ort_threads::SPAWNED.load(std::sync::atomic::Ordering::Relaxed) - before;
        assert!(spawned >= budget - 1, "{spawned} < {}", budget - 1);
    }
}
