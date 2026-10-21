// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Streaming Nemotron 3 Diarization: the ASR's `MelStream` front end, a
//! chunker with left context 0, the exported step graph on ONNX Runtime, and
//! the host-side streaming state update.
//!
//! `LiveDiarizer::push` takes mono 16 kHz f32 in any size and records, per
//! 10 ms frame, a bitmask of the speaker slots whose probability exceeds 0.5.
//! `turns` reads runs of at least 0.2 s from those frames.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use ndarray::ArrayView3;
use ort::ep::CPU as CPUExecutionProvider;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::{Tensor, TensorRef};

use crate::nemotron::features::{MelStream, CONTEXT_FRAMES, N_MELS, WINDOW_FRAMES};

const SUB: usize = 8;
const D: usize = 512;
const S: usize = 8;
const UP: usize = 8;

const PRED_SCORE_THRESHOLD: f32 = 0.25;
const SCORES_BOOST_LATEST: f32 = 0.05;
const STRONG_BOOST_RATE: f32 = 0.75;
const WEAK_BOOST_RATE: f32 = 1.5;
const MIN_POS_SCORES_RATE: f32 = 0.5;
const SIL_FRAMES_PER_SPK: usize = 1;
const MAX_INDEX: usize = 99_999;

/// File names of the exported `fast32` preset inside the model directory.
pub const MODEL_FILE: &str = "nemotron3_diar_fast32.onnx";
pub const SIL_FILE: &str = "learnable_sil_emb.bin";
/// The overlay's `model` value.
pub const MODEL_NAME: &str = "nemotron3/fast32";
/// Samples per 10 ms output frame at 16 kHz.
pub const FRAME_SAMPLES: usize = 160;
/// Probability above which a slot counts as speaking in a frame.
pub const SPEECH_PROB: f32 = 0.5;
/// Shortest run read as a turn, in 10 ms frames.
pub const MIN_TURN_FRAMES: usize = 20;

#[derive(Clone, Copy, Debug)]
struct Preset {
    chunk_len: usize,
    rc: usize,
    fifo: usize,
    spkcache: usize,
    update_period: usize,
}

const FAST32: Preset = Preset { chunk_len: 32, rc: 4, fifo: 40, spkcache: 264, update_period: 40 };

impl Preset {
    fn chunk_enc(&self) -> usize {
        self.chunk_len + self.rc
    }
    fn chunk_mel(&self) -> usize {
        self.chunk_enc() * SUB
    }
    fn core_mel(&self) -> usize {
        self.chunk_len * SUB
    }
    fn packed(&self) -> usize {
        self.spkcache + self.fifo + self.chunk_enc()
    }
}

trait OrtCtx<T> {
    fn ort(self) -> Result<T>;
}

impl<T, R> OrtCtx<T> for std::result::Result<T, ort::Error<R>> {
    fn ort(self) -> Result<T> {
        self.map_err(|e| anyhow!("onnxruntime: {e}"))
    }
}

// ---------------------------------------------------------------------------

struct State {
    spkcache: Vec<f32>,
    spkcache_len: usize,
    spkcache_preds: Vec<f32>,
    compressed: bool,
    fifo: Vec<f32>,
    fifo_len: usize,
    fifo_preds: Vec<f32>,
}

impl State {
    fn new(p: &Preset) -> Self {
        Self {
            spkcache: vec![0.0; p.spkcache * D],
            spkcache_len: 0,
            spkcache_preds: vec![0.0; p.spkcache * S],
            compressed: false,
            fifo: vec![0.0; p.fifo * D],
            fifo_len: 0,
            fifo_preds: vec![0.0; p.fifo * S],
        }
    }
}

fn replace_region(buf: &mut [f32], content: &[f32]) {
    buf[..content.len()].copy_from_slice(content);
    buf[content.len()..].iter_mut().for_each(|v| *v = 0.0);
}

/// One streaming update; returns this chunk's 10 ms probabilities
/// (`frames * S`).
#[allow(clippy::too_many_arguments)]
fn update(
    p: &Preset,
    sil: &[f32],
    st: &mut State,
    chunk_embs: &[f32],
    chunk_enc_len: usize,
    preds: &[f32],
    hi: &[f32],
    lc: usize,
    rc: usize,
) -> Vec<f32> {
    let max_chunk = p.chunk_enc() - lc - rc;
    let (fifo_cap, sc_cap) = (p.fifo, p.spkcache);
    let (sc_len, fifo_len) = (st.spkcache_len, st.fifo_len);
    let chunk_len = chunk_enc_len.saturating_sub(lc).min(max_chunk);

    let spk_preds_cur = preds[..sc_len * S].to_vec();
    let fifo_preds_cur = &preds[sc_len * S..(sc_len + fifo_len) * S];
    let cps = (sc_len + fifo_len + lc) * S;
    let chunk_preds_cur = &preds[cps..cps + chunk_len * S];

    let hs = (sc_len + fifo_len + lc) * UP * S;
    let probs = hi[hs..hs + chunk_len * UP * S].to_vec();

    let combined = fifo_len + chunk_len;
    let mut pop = 0;
    if combined > fifo_cap {
        pop = combined.min(p.update_period.max(combined - fifo_cap));
    }
    if chunk_len == 0 {
        pop = fifo_len;
    }

    let mut logical_embs = Vec::with_capacity(combined * D);
    logical_embs.extend_from_slice(&st.fifo[..fifo_len * D]);
    logical_embs.extend_from_slice(&chunk_embs[lc * D..(lc + chunk_len) * D]);
    let mut logical_preds = Vec::with_capacity(combined * S);
    logical_preds.extend_from_slice(fifo_preds_cur);
    logical_preds.extend_from_slice(chunk_preds_cur);

    let pop_embs = logical_embs[..pop * D].to_vec();
    let pop_preds = logical_preds[..pop * S].to_vec();
    replace_region(&mut st.fifo, &logical_embs[pop * D..combined * D]);
    replace_region(&mut st.fifo_preds, &logical_preds[pop * S..combined * S]);
    st.fifo_len = combined - pop;

    let updated = sc_len + pop;
    if updated > sc_cap {
        let mut cand_embs = st.spkcache[..sc_len * D].to_vec();
        cand_embs.extend_from_slice(&pop_embs);
        let mut cand_preds = if st.compressed { st.spkcache_preds[..sc_len * S].to_vec() } else { spk_preds_cur };
        cand_preds.extend_from_slice(&pop_preds);
        let (cache, cache_preds) = compress(p, sil, &cand_embs, &cand_preds, updated);
        replace_region(&mut st.spkcache, &cache);
        replace_region(&mut st.spkcache_preds, &cache_preds);
        st.spkcache_len = sc_cap;
        st.compressed = true;
    } else if pop > 0 {
        st.spkcache[sc_len * D..updated * D].copy_from_slice(&pop_embs);
        st.spkcache_preds[sc_len * S..updated * S].copy_from_slice(&pop_preds);
        st.spkcache_len = updated;
    }
    probs
}

fn compress(p: &Preset, sil: &[f32], embs: &[f32], preds: &[f32], frames: usize) -> (Vec<f32>, Vec<f32>) {
    let sc_cap = p.spkcache;
    let per_spk = sc_cap / S - SIL_FRAMES_PER_SPK;
    let strong = (per_spk as f32 * STRONG_BOOST_RATE) as usize;
    let weak = (per_spk as f32 * WEAK_BOOST_RATE) as usize;
    let min_pos = (per_spk as f32 * MIN_POS_SCORES_RATE) as usize;

    let mut scores = log_pred_scores(preds, frames);
    disable_low_scores(preds, &mut scores, frames, min_pos);
    if SCORES_BOOST_LATEST > 0.0 && frames > sc_cap {
        for v in scores[sc_cap * S..frames * S].iter_mut() {
            if *v != f32::NEG_INFINITY {
                *v += SCORES_BOOST_LATEST;
            }
        }
    }
    boost_topk(&mut scores, frames, strong, 2.0);
    boost_topk(&mut scores, frames, weak, 1.0);

    let total = frames + SIL_FRAMES_PER_SPK;
    scores.resize(total * S, f32::INFINITY);
    let (topk, disabled) = topk_indices(&scores, total, sc_cap);

    let mut cache = vec![0.0f32; sc_cap * D];
    let mut cache_preds = vec![0.0f32; sc_cap * S];
    for (i, &f) in topk.iter().enumerate() {
        if disabled[i] {
            cache[i * D..(i + 1) * D].copy_from_slice(sil);
        } else {
            cache[i * D..(i + 1) * D].copy_from_slice(&embs[f * D..(f + 1) * D]);
            cache_preds[i * S..(i + 1) * S].copy_from_slice(&preds[f * S..(f + 1) * S]);
        }
    }
    (cache, cache_preds)
}

/// score = ln p - ln(1-p) + sum_spk ln(1-p) + ln 2, both logs floored at the
/// prediction score threshold.
fn log_pred_scores(preds: &[f32], frames: usize) -> Vec<f32> {
    let mut scores = vec![0.0f32; frames * S];
    for f in 0..frames {
        let row = &preds[f * S..(f + 1) * S];
        let log1p: Vec<f32> = row.iter().map(|&v| (1.0 - v).max(PRED_SCORE_THRESHOLD).ln()).collect();
        let sum: f32 = log1p.iter().sum();
        for s in 0..S {
            scores[f * S + s] = row[s].max(PRED_SCORE_THRESHOLD).ln() - log1p[s] + sum + 2f32.ln();
        }
    }
    scores
}

fn disable_low_scores(preds: &[f32], scores: &mut [f32], frames: usize, min_pos: usize) {
    let mut pos = [0usize; S];
    for f in 0..frames {
        for (s, count) in pos.iter_mut().enumerate() {
            let i = f * S + s;
            if preds[i] > 0.5 && scores[i] > 0.0 {
                *count += 1;
            }
        }
    }
    for f in 0..frames {
        for (s, &count) in pos.iter().enumerate() {
            let i = f * S + s;
            if preds[i] <= 0.5 || (scores[i] <= 0.0 && count >= min_pos) {
                scores[i] = f32::NEG_INFINITY;
            }
        }
    }
}

/// Adds `scale * ln 2` to each speaker's top-k finite scores; ties keep the
/// earlier frame.
fn boost_topk(scores: &mut [f32], frames: usize, k: usize, scale: f32) {
    if k == 0 || frames == 0 {
        return;
    }
    let delta = scale * 2f32.ln();
    for s in 0..S {
        let mut finite: Vec<(usize, f32)> =
            (0..frames).map(|f| (f, scores[f * S + s])).filter(|(_, v)| *v != f32::NEG_INFINITY).collect();
        finite.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        for &(f, _) in finite.iter().take(k) {
            scores[f * S + s] += delta;
        }
    }
}

/// Top-k over the speaker-major index (`spk * frames + frame`), value
/// descending then index ascending, sorted back to ascending index, folded to
/// frames; silence pads and -inf picks are flagged disabled.
fn topk_indices(scores: &[f32], frames: usize, k: usize) -> (Vec<usize>, Vec<bool>) {
    let n_no_sil = frames - SIL_FRAMES_PER_SPK;
    let mut all: Vec<(usize, f32)> =
        (0..S).flat_map(|s| (0..frames).map(move |f| (s * frames + f, s, f))).map(|(i, s, f)| (i, scores[f * S + s])).collect();
    all.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
    let mut topk = vec![MAX_INDEX; k];
    for (slot, &(i, v)) in topk.iter_mut().zip(all.iter()) {
        if v != f32::NEG_INFINITY {
            *slot = i;
        }
    }
    topk.sort_unstable();
    let mut disabled = vec![false; k];
    for (i, t) in topk.iter_mut().enumerate() {
        if *t == MAX_INDEX {
            disabled[i] = true;
            *t = 0;
            continue;
        }
        *t %= frames;
        if *t >= n_no_sil {
            disabled[i] = true;
            *t = 0;
        }
    }
    (topk, disabled)
}

// ---------------------------------------------------------------------------
// The stream: mel front end, chunker, graph, update.

/// A run of one slot speaking, in seconds on the stream's own clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Turn {
    pub slot: u8,
    pub start_s: f64,
    pub end_s: f64,
}

/// Turns (runs of at least 0.2 s) over a frame-mask sequence; frame `i` covers
/// stream seconds `i * 0.01 ..= (i + 1) * 0.01`. Ordered by start.
pub fn turns_from_frames(frames: &[u8]) -> Vec<Turn> {
    let mut out = Vec::new();
    for slot in 0..S as u8 {
        let bit = 1u8 << slot;
        let mut run_start: Option<usize> = None;
        for (i, &m) in frames.iter().chain(std::iter::once(&0)).enumerate() {
            match (m & bit != 0, run_start) {
                (true, None) => run_start = Some(i),
                (false, Some(s)) => {
                    if i - s >= MIN_TURN_FRAMES {
                        out.push(Turn { slot, start_s: s as f64 / 100.0, end_s: i as f64 / 100.0 });
                    }
                    run_start = None;
                }
                _ => {}
            }
        }
    }
    out.sort_by(|a, b| a.start_s.total_cmp(&b.start_s).then(a.slot.cmp(&b.slot)));
    out
}

pub struct LiveDiarizer {
    p: Preset,
    session: Session,
    sil: Vec<f32>,
    st: State,
    mel: MelStream,
    frames: Vec<f32>,
    cache_start: usize,
    total_frames: usize,
    next_core: usize,
    fed: u64,
    active: Vec<u8>,
}

fn build_session(path: &Path, threads: usize) -> Result<Session> {
    let b = Session::builder()
        .ort()?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .ort()?
        .with_execution_providers(vec![CPUExecutionProvider::default().build()])
        .ort()?
        .with_intra_threads(threads.max(1))
        .ort()?
        .with_inter_threads(1)
        .ort()?
        .with_parallel_execution(false)
        .ort()?
        .with_config_entry("session.intra_op.allow_spinning", "0")
        .ort()?
        .with_thread_manager(crate::ort_threads::BackgroundThreads)
        .ort()?;
    let mut b = b;
    b.commit_from_file(path).ort().with_context(|| format!("loading {}", path.display()))
}

/// True when `dir` holds the files a `fast32` stream loads.
pub fn model_present(dir: &Path) -> bool {
    dir.join(MODEL_FILE).is_file() && dir.join(SIL_FILE).is_file()
}

impl LiveDiarizer {
    /// Loads the `fast32` graph and silence embedding from `dir`; ONNX Runtime
    /// pool threads run at background priority.
    pub fn load(dir: &Path, threads: usize) -> Result<Self> {
        let sil_bytes = std::fs::read(dir.join(SIL_FILE)).with_context(|| SIL_FILE.to_string())?;
        if sil_bytes.len() != D * 4 {
            bail!("{SIL_FILE} holds {} bytes, expected {}", sil_bytes.len(), D * 4);
        }
        let sil = sil_bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        let p = FAST32;
        Ok(Self {
            p,
            session: build_session(&dir.join(MODEL_FILE), threads)?,
            sil,
            st: State::new(&p),
            mel: MelStream::new(),
            frames: Vec::new(),
            cache_start: 0,
            total_frames: 0,
            next_core: 0,
            fed: 0,
            active: Vec::new(),
        })
    }

    /// Seconds of audio pushed so far.
    pub fn audio_seconds(&self) -> f64 {
        self.fed as f64 / 16_000.0
    }

    /// One mask byte per 10 ms frame decided so far: bit `s` set when slot `s`
    /// is above 0.5.
    pub fn frames(&self) -> &[u8] {
        &self.active
    }

    /// Feeds samples and runs every step they complete; returns how many new
    /// frames `frames()` gained.
    pub fn push(&mut self, samples: &[f32]) -> Result<usize> {
        let before = self.active.len();
        self.fed += samples.len() as u64;
        self.feed(samples)?;
        Ok(self.active.len() - before)
    }

    /// Pads with silence until every pushed sample has a decided frame, then
    /// drops the frames past the audio. Ends the stream.
    pub fn finish(&mut self) -> Result<usize> {
        let before = self.active.len();
        let want = (self.fed as usize).div_ceil(FRAME_SAMPLES);
        let zeros = vec![0.0f32; 16_000];
        while self.active.len() < want {
            self.feed(&zeros)?;
        }
        self.active.truncate(want);
        Ok(self.active.len().saturating_sub(before))
    }

    fn feed(&mut self, samples: &[f32]) -> Result<()> {
        self.mel.push(samples);
        while let Some(w) = self.mel.next_chunk() {
            for col in CONTEXT_FRAMES..WINDOW_FRAMES {
                for m in 0..N_MELS {
                    self.frames.push(w[[0, m, col]]);
                }
                self.total_frames += 1;
            }
        }
        while self.next_core + self.p.core_mel() + self.p.rc * SUB <= self.total_frames {
            self.step()?;
        }
        Ok(())
    }

    fn step(&mut self) -> Result<()> {
        let p = self.p;
        let lo = (self.next_core - self.cache_start) * N_MELS;
        let chunk = self.frames[lo..lo + p.chunk_mel() * N_MELS].to_vec();
        let (preds, hi, embs) = self.run(&chunk, p.chunk_mel())?;
        let probs = update(&p, &self.sil, &mut self.st, &embs, p.chunk_mel().div_ceil(SUB), &preds, &hi, 0, p.rc);
        for row in probs.chunks_exact(S) {
            let mut mask = 0u8;
            for (s, &v) in row.iter().enumerate() {
                if v > SPEECH_PROB {
                    mask |= 1 << s;
                }
            }
            self.active.push(mask);
        }
        self.next_core += p.core_mel();
        let drop = self.next_core - self.cache_start;
        self.frames.drain(..drop * N_MELS);
        self.cache_start = self.next_core;
        Ok(())
    }

    fn run(&mut self, chunk: &[f32], chunk_len_mel: usize) -> Result<(Vec<f32>, Vec<f32>, Vec<f32>)> {
        let p = self.p;
        let chunk_v = ArrayView3::from_shape((1, p.chunk_mel(), N_MELS), chunk)?;
        let sc_v = ArrayView3::from_shape((1, p.spkcache, D), &self.st.spkcache[..])?;
        let ff_v = ArrayView3::from_shape((1, p.fifo, D), &self.st.fifo[..])?;
        let i32x1 = |v: usize| Tensor::from_array(([1usize], vec![v as i32].into_boxed_slice()));
        let outputs = self
            .session
            .run(ort::inputs![
                "chunk" => TensorRef::from_array_view(chunk_v).ort()?,
                "chunk_lengths" => i32x1(chunk_len_mel).ort()?,
                "spkcache" => TensorRef::from_array_view(sc_v).ort()?,
                "spkcache_lengths" => i32x1(self.st.spkcache_len).ort()?,
                "fifo" => TensorRef::from_array_view(ff_v).ort()?,
                "fifo_lengths" => i32x1(self.st.fifo_len).ort()?,
            ])
            .ort()?;
        let get = |name: &str, len: usize| -> Result<Vec<f32>> {
            let v: Vec<f32> = outputs
                .get(name)
                .ok_or_else(|| anyhow!("graph output {name:?} not found"))?
                .try_extract_array::<f32>()
                .ort()?
                .iter()
                .copied()
                .collect();
            if v.len() != len {
                bail!("graph output {name} holds {} values, expected {len}", v.len());
            }
            Ok(v)
        };
        Ok((
            get("speaker_preds", p.packed() * S)?,
            get("speaker_preds_10ms", p.packed() * UP * S)?,
            get("chunk_pre_encode_embs", p.chunk_enc() * D)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_need_twenty_frames_and_split_per_slot() {
        let mut f = vec![0u8; 100];
        for m in f.iter_mut().take(40).skip(10) {
            *m |= 1;
        }
        for m in f.iter_mut().take(60).skip(45) {
            *m |= 2; // 15 frames: dropped
        }
        for m in f.iter_mut().take(100).skip(30) {
            *m |= 2; // overlaps slot 0
        }
        let t = turns_from_frames(&f);
        assert_eq!(
            t,
            vec![
                Turn { slot: 0, start_s: 0.10, end_s: 0.40 },
                Turn { slot: 1, start_s: 0.30, end_s: 1.00 },
            ]
        );
    }

    #[test]
    fn a_run_open_at_the_end_is_closed() {
        let f = vec![4u8; 25];
        assert_eq!(turns_from_frames(&f), vec![Turn { slot: 2, start_s: 0.0, end_s: 0.25 }]);
    }

    /// Needs the exported graph; runs only when `NEMOTRON3_DIAR_DIR` names it.
    #[test]
    fn streams_a_clip_in_odd_sized_pushes() {
        let Ok(dir) = std::env::var("NEMOTRON3_DIAR_DIR") else { return };
        let mut d = LiveDiarizer::load(Path::new(&dir), 2).unwrap();
        let clip: Vec<f32> = (0..16_000 * 8).map(|i| 0.1 * ((i as f32) * 0.05).sin()).collect();
        for part in clip.chunks(7_777) {
            d.push(part).unwrap();
        }
        d.finish().unwrap();
        assert_eq!(d.frames().len(), clip.len().div_ceil(FRAME_SAMPLES));
    }
}
