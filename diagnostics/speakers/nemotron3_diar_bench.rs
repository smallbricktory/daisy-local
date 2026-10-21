// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The CPU cost of one Nemotron 3 Diarization stream on this machine, through
//! the app's own `ort` (ONNX Runtime 1.28.0).
//!
//! Runs the streaming loop: the ASR's `MelStream` front end, the chunker with
//! left context 0, the exported step graph, and the host-side state update.
//!
//!   cargo run --release -p providers-local --example nemotron3_diar_bench -- \
//!       --i-know-recordings-are-done --model-dir DIR --preset low|fast32 \
//!       --threads N --mode solo|asr-concurrent [--seconds S] \
//!       [--baseline-seconds S] [--asr-model-dir DIR] [--wav PATH] \
//!       [--max-load X] --out FILE.json
//!   ... --write-clip PATH.wav [--seconds S]   (writes the synthetic clip and exits)
//!
//! `solo` feeds the clip as fast as the stream runs. `asr-concurrent` first
//! runs two Nemotron ASR live tracks at real-time pace for the baseline, then
//! the same two tracks plus the diarizer, also at real-time pace, for
//! `--seconds`. The clip is a deterministic synthetic four-voice signal unless
//! `--wav` names a 16 kHz mono file. One JSON object is written to `--out`.
//!

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use ndarray::ArrayView3;
use ort::ep::CPU as CPUExecutionProvider;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::{Tensor, TensorRef};
use providers_local::nemotron::features::{MelStream, CONTEXT_FRAMES, N_MELS, WINDOW_FRAMES};
use sha2::{Digest, Sha256};

const SR: usize = 16_000;
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

const GATE_RTF: f64 = 0.25;
const GATE_ASR_LAG_RATIO: f64 = 1.10;
const GATE_ADDED_RSS_MB: f64 = 500.0;

#[derive(Clone, Copy, Debug)]
struct Preset {
    name: &'static str,
    chunk_len: usize,
    rc: usize,
    fifo: usize,
    spkcache: usize,
    update_period: usize,
}

const PRESETS: [Preset; 2] = [
    Preset { name: "low", chunk_len: 9, rc: 4, fifo: 264, spkcache: 264, update_period: 222 },
    Preset { name: "fast32", chunk_len: 32, rc: 4, fifo: 40, spkcache: 264, update_period: 40 },
];

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
    fn step_seconds(&self) -> f64 {
        self.chunk_len as f64 * 0.08
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

#[derive(Default)]
struct Timing {
    mel_s: f64,
    steps: Vec<f64>,
    update_s: f64,
    frames_out: usize,
    active_frames: usize,
}

struct Diarizer {
    p: Preset,
    session: Session,
    sil: Vec<f32>,
    st: State,
    mel: MelStream,
    frames: Vec<f32>,
    cache_start: usize,
    total_frames: usize,
    next_core: usize,
    timing: Timing,
}

fn session(path: &Path, threads: usize) -> Result<Session> {
    let mut b = Session::builder()
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
        .ort()?;
    b.commit_from_file(path).ort().with_context(|| format!("loading {}", path.display()))
}

impl Diarizer {
    fn new(p: Preset, model: &Path, sil: Vec<f32>, threads: usize) -> Result<Self> {
        Ok(Self {
            p,
            session: session(model, threads)?,
            sil,
            st: State::new(&p),
            mel: MelStream::new(),
            frames: Vec::new(),
            cache_start: 0,
            total_frames: 0,
            next_core: 0,
            timing: Timing::default(),
        })
    }

    /// Feeds samples and runs every chunk they complete; returns the audio
    /// seconds the run chunks cover.
    fn push(&mut self, samples: &[f32]) -> Result<f64> {
        let t = Instant::now();
        self.mel.push(samples);
        while let Some(w) = self.mel.next_chunk() {
            for col in CONTEXT_FRAMES..WINDOW_FRAMES {
                for m in 0..N_MELS {
                    self.frames.push(w[[0, m, col]]);
                }
                self.total_frames += 1;
            }
        }
        self.timing.mel_s += t.elapsed().as_secs_f64();
        let mut covered = 0.0;
        while self.next_core + self.p.core_mel() + self.p.rc * SUB <= self.total_frames {
            self.step()?;
            covered += self.p.step_seconds();
        }
        Ok(covered)
    }

    fn step(&mut self) -> Result<()> {
        let p = self.p;
        let lo = (self.next_core - self.cache_start) * N_MELS;
        let chunk = self.frames[lo..lo + p.chunk_mel() * N_MELS].to_vec();
        let t = Instant::now();
        let (preds, hi, embs) = self.run(&chunk, p.chunk_mel())?;
        let model_s = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let probs = update(&p, &self.sil, &mut self.st, &embs, p.chunk_mel().div_ceil(SUB), &preds, &hi, 0, p.rc);
        self.timing.update_s += t.elapsed().as_secs_f64();
        self.timing.steps.push(model_s + t.elapsed().as_secs_f64());
        self.timing.frames_out += probs.len() / S;
        self.timing.active_frames += probs.chunks(S).filter(|r| r.iter().any(|&v| v > 0.5)).count();
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

// ---------------------------------------------------------------------------
// Clip, host facts, memory.

/// Deterministic speech-like audio: four fictional synthetic voices
/// (harmonic stacks at distinct pitches, syllable-rate envelopes) taking turns
/// with short gaps. xorshift64* drives every random choice.
fn synthetic_clip(seconds: f64) -> Vec<f32> {
    let mut seed: u64 = 0x2026_1001_dead_beef;
    let mut rnd = move || {
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        (seed.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64
    };
    let n = (seconds * SR as f64) as usize;
    let mut out = vec![0.0f32; n];
    let pitches = [110.0f64, 155.0, 205.0, 260.0];
    let (mut t0, mut voice) = (0usize, 0usize);
    while t0 < n {
        let len = (((1.5 + 2.5 * rnd()) * SR as f64) as usize).min(n - t0);
        let syll = 3.0 + 2.0 * rnd();
        let mut phase = 0.0f64;
        for i in 0..len {
            let t = i as f64 / SR as f64;
            let f0 = pitches[voice] * (1.0 + 0.04 * (2.0 * std::f64::consts::PI * 0.7 * t).sin());
            phase += 2.0 * std::f64::consts::PI * f0 / SR as f64;
            let sig: f64 = (1..=8).map(|k| (0.6 / k as f64) * (k as f64 * phase).sin()).sum();
            let env = (0.5 * (1.0 + (2.0 * std::f64::consts::PI * syll * t).sin())).powi(2);
            out[t0 + i] += (0.08 * sig * env) as f32;
        }
        t0 += len + ((0.15 + 0.35 * rnd()) * SR as f64) as usize;
        voice = (voice + 1 + (rnd() * 3.0) as usize) % 4;
    }
    for v in out.iter_mut() {
        *v += (0.004 * (rnd() - 0.5)) as f32;
    }
    out
}

fn write_wav(path: &Path, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate: SR as u32, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        w.write_sample((s * 32767.0).clamp(-32768.0, 32767.0) as i16)?;
    }
    w.finalize()?;
    Ok(())
}

fn command_out(cmd: &str, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new(cmd).args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn cpu_model() -> String {
    #[cfg(target_os = "macos")]
    let v = command_out("sysctl", &["-n", "machdep.cpu.brand_string"]);
    #[cfg(target_os = "linux")]
    let v = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("model name")).and_then(|l| l.split_once(':')).map(|(_, v)| v.trim().to_string()));
    #[cfg(windows)]
    let v = command_out("reg", &["query", r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0", "/v", "ProcessorNameString"])
        .and_then(|s| s.lines().find(|l| l.contains("ProcessorNameString")).and_then(|l| l.split("REG_SZ").nth(1)).map(|v| v.trim().to_string()));
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    let v: Option<String> = None;
    v.unwrap_or_else(|| "unknown".into())
}

fn host_name() -> String {
    std::env::var("COMPUTERNAME").ok().or_else(|| command_out("hostname", &[])).unwrap_or_else(|| "unknown".into())
}

/// 1-minute load average where the OS reports one.
#[cfg(target_os = "linux")]
fn load_average() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg").ok()?.split_whitespace().next()?.parse().ok()
}

#[cfg(target_os = "macos")]
fn load_average() -> Option<f64> {
    command_out("sysctl", &["-n", "vm.loadavg"])?
        .trim_matches(|c| c == '{' || c == '}' || c == ' ')
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Windows has no load average.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn load_average() -> Option<f64> {
    None
}

/// Peak resident set of this process, MB.
#[cfg(target_os = "linux")]
fn peak_rss_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<f64>().ok())
        .map_or(0.0, |kb| kb / 1024.0)
}

#[cfg(target_os = "macos")]
fn peak_rss_mb() -> f64 {
    #[repr(C)]
    #[allow(dead_code)]
    struct Rusage {
        utime: [i64; 2],
        stime: [i64; 2],
        maxrss: i64,
        rest: [i64; 13],
    }
    extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }
    let mut r: Rusage = unsafe { std::mem::zeroed() };
    // macOS reports ru_maxrss in bytes.
    if unsafe { getrusage(0, &mut r) } == 0 {
        r.maxrss as f64 / 1_048_576.0
    } else {
        0.0
    }
}

#[cfg(windows)]
fn peak_rss_mb() -> f64 {
    #[repr(C)]
    #[allow(dead_code)]
    struct Pmc {
        cb: u32,
        faults: u32,
        peak_ws: usize,
        ws: usize,
        rest: [usize; 6],
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(h: isize, p: *mut Pmc, cb: u32) -> i32;
    }
    let mut p: Pmc = unsafe { std::mem::zeroed() };
    p.cb = std::mem::size_of::<Pmc>() as u32;
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut p, p.cb) };
    p.peak_ws as f64 / 1_048_576.0
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn peak_rss_mb() -> f64 {
    0.0
}

fn file_sha256(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect())
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v
}

// ---------------------------------------------------------------------------
// Runs.

struct Args {
    model_dir: PathBuf,
    preset: Preset,
    threads: usize,
    mode: String,
    seconds: f64,
    baseline_seconds: f64,
    asr_model_dir: Option<PathBuf>,
    wav: Option<PathBuf>,
    out: PathBuf,
    max_load: Option<f64>,
}

fn usage() -> ! {
    eprintln!(
        "usage: nemotron3_diar_bench --i-know-recordings-are-done --model-dir DIR --preset low|fast32 \
         --threads N --mode solo|asr-concurrent [--seconds S] [--baseline-seconds S] \
         [--asr-model-dir DIR] [--wav PATH] [--max-load X] --out FILE.json\n\
         \x20      nemotron3_diar_bench --write-clip PATH.wav [--seconds S]"
    );
    std::process::exit(2)
}

fn parse() -> Result<Option<Args>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let val = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    if let Some(path) = val("--write-clip") {
        let secs: f64 = val("--seconds").map(|s| s.parse()).transpose()?.unwrap_or(600.0);
        write_wav(Path::new(&path), &synthetic_clip(secs))?;
        eprintln!("wrote {secs} s synthetic clip to {path}");
        return Ok(None);
    }
    if !a.iter().any(|x| x == "--i-know-recordings-are-done") {
        bail!("refusing to start: pass --i-know-recordings-are-done once no test machine is recording");
    }
    let preset_name = val("--preset").unwrap_or_else(|| usage());
    let preset = *PRESETS.iter().find(|p| p.name == preset_name).ok_or_else(|| anyhow!("unknown preset {preset_name}"))?;
    let mode = val("--mode").unwrap_or_else(|| "solo".into());
    if mode != "solo" && mode != "asr-concurrent" {
        bail!("unknown mode {mode}");
    }
    let default_secs = if mode == "solo" { 300.0 } else { 1800.0 };
    Ok(Some(Args {
        model_dir: val("--model-dir").map(PathBuf::from).unwrap_or_else(|| usage()),
        preset,
        threads: val("--threads").map(|s| s.parse()).transpose()?.unwrap_or(2),
        seconds: val("--seconds").map(|s| s.parse()).transpose()?.unwrap_or(default_secs),
        baseline_seconds: val("--baseline-seconds").map(|s| s.parse()).transpose()?.unwrap_or(300.0),
        asr_model_dir: val("--asr-model-dir").map(PathBuf::from),
        wav: val("--wav").map(PathBuf::from),
        out: val("--out").map(PathBuf::from).unwrap_or_else(|| usage()),
        max_load: val("--max-load").map(|s| s.parse()).transpose()?,
        mode,
    }))
}

fn check_load(max: Option<f64>) -> Result<Option<f64>> {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) as f64;
    let limit = max.unwrap_or((0.25 * cores).max(1.0));
    let load = load_average();
    if let Some(l) = load {
        if l > limit {
            bail!("refusing to start: 1-minute load average {l:.2} is above {limit:.2}; wait for the machine to idle or pass --max-load");
        }
    }
    Ok(load)
}

fn clip(args: &Args, seconds: f64) -> Result<Vec<f32>> {
    let one = match &args.wav {
        Some(p) => providers_local::wav::decode_wav_16k_mono_f32(p).map_err(|e| anyhow!("{}: {e}", p.display()))?,
        None => synthetic_clip(seconds.min(600.0)),
    };
    if one.is_empty() {
        bail!("empty clip");
    }
    let n = (seconds * SR as f64) as usize;
    Ok(one.iter().copied().cycle().take(n).collect())
}

fn load_diarizer(args: &Args) -> Result<(Diarizer, f64, String)> {
    let model = args.model_dir.join(format!("nemotron3_diar_{}.onnx", args.preset.name));
    let sil_bytes = std::fs::read(args.model_dir.join("learnable_sil_emb.bin")).context("learnable_sil_emb.bin")?;
    if sil_bytes.len() != D * 4 {
        bail!("learnable_sil_emb.bin holds {} bytes, expected {}", sil_bytes.len(), D * 4);
    }
    let sil: Vec<f32> = sil_bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    let sha = file_sha256(&model)?;
    let t = Instant::now();
    let d = Diarizer::new(args.preset, &model, sil, args.threads)?;
    Ok((d, t.elapsed().as_secs_f64(), sha))
}

fn diarizer_json(d: &Diarizer, audio_s: f64, wall_s: f64) -> serde_json::Value {
    let step_rtf = sorted(d.timing.steps.iter().map(|s| s / d.p.step_seconds()).collect());
    let compute = d.timing.steps.iter().sum::<f64>() + d.timing.mel_s;
    serde_json::json!({
        "audio_seconds": audio_s,
        "wall_seconds": wall_s,
        "steps": d.timing.steps.len(),
        "rtf": if audio_s > 0.0 { compute / audio_s } else { 0.0 },
        "step_rtf_p50": pct(&step_rtf, 0.5),
        "step_rtf_p95": pct(&step_rtf, 0.95),
        "step_rtf_max": pct(&step_rtf, 1.0),
        "step_ms_p50": pct(&step_rtf, 0.5) * d.p.step_seconds() * 1000.0,
        "step_ms_p95": pct(&step_rtf, 0.95) * d.p.step_seconds() * 1000.0,
        "mel_seconds": d.timing.mel_s,
        "update_seconds": d.timing.update_s,
        "frames_out": d.timing.frames_out,
        "active_frame_share": if d.timing.frames_out > 0 { d.timing.active_frames as f64 / d.timing.frames_out as f64 } else { 0.0 },
    })
}

fn run_solo(args: &Args) -> Result<serde_json::Value> {
    let audio = clip(args, args.seconds)?;
    let rss_before = peak_rss_mb();
    let (mut d, load_s, sha) = load_diarizer(args)?;
    let t = Instant::now();
    let mut covered = 0.0;
    for buf in audio.chunks(SR / 10) {
        covered += d.push(buf)?;
    }
    let wall = t.elapsed().as_secs_f64();
    let peak = peak_rss_mb();
    let mut j = diarizer_json(&d, covered, wall);
    let rtf = j["rtf"].as_f64().unwrap_or(f64::MAX);
    j["load_seconds"] = load_s.into();
    j["model_sha256"] = sha.into();
    j["rss_before_mb"] = rss_before.into();
    j["peak_rss_mb"] = peak.into();
    j["added_rss_mb"] = (peak - rss_before).into();
    j["gate"] = serde_json::json!({
        "rtf_le_0_25": rtf <= GATE_RTF,
        "added_rss_le_500mb": peak - rss_before <= GATE_ADDED_RSS_MB,
        "note": "solo run: ASR lag and queue growth are measured in asr-concurrent mode",
    });
    Ok(j)
}

#[derive(Default, Clone)]
struct AsrStats {
    decode_ms: Vec<u64>,
    lags: Vec<i64>,
    finals: u64,
    behind: u64,
    errors: u64,
    backlog_max_ms: u64,
}

/// Two Nemotron ASR live tracks at real-time pace for `seconds`, as a
/// recording runs them; `during` starts with the feed and runs alongside.
fn asr_phase(
    rt: &tokio::runtime::Runtime,
    asr_dir: &Path,
    audio: &[f32],
    seconds: f64,
    during: Option<Box<dyn FnOnce(Instant) + Send>>,
) -> Result<AsrStats> {
    use providers_local::nemotron::realtime::NemotronRealtime;
    use providers_local::streaming::live_metrics;
    use providers_realtime::{RealtimeEvent, RealtimeTranscriber};

    let n = (seconds * SR as f64) as usize;
    let pcm: Vec<i16> = audio.iter().cycle().take(n).map(|&s| (s * 32768.0).clamp(-32768.0, 32767.0) as i16).collect();
    let offset = pcm.len() / 3;
    let tracks = [pcm.clone(), pcm[offset..].iter().chain(pcm[..offset].iter()).copied().collect::<Vec<i16>>()];
    let stats = Arc::new(Mutex::new(AsrStats::default()));
    let asr_dir = asr_dir.to_path_buf();
    let during = Arc::new(Mutex::new(during));
    rt.block_on(async {
        live_metrics::reset();
        let live = Arc::new(NemotronRealtime::new(asr_dir.clone(), "nemotron (diar bench)"));
        let resident_before = providers_local::nemotron::model::resident();
        let mut runs = Vec::new();
        let mut feeds = Vec::new();
        for pcm in tracks {
            let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
            let (ev_tx, ev_rx) = tokio::sync::mpsc::channel::<RealtimeEvent>(1024);
            feeds.push((pcm, audio_tx, ev_rx));
            let live = Arc::clone(&live);
            runs.push(tokio::spawn(async move { live.run(16_000, audio_rx, ev_tx).await }));
        }
        while providers_local::nemotron::model::resident() < resident_before + 2 {
            if runs.iter().any(|r| r.is_finished()) {
                return Err(anyhow!("an ASR track ended before its model loaded"));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        live_metrics::reset();
        let start = tokio::time::Instant::now();
        if let Some(f) = during.lock().unwrap().take() {
            let std_start = Instant::now();
            std::thread::spawn(move || f(std_start));
        }
        for (pcm, audio_tx, mut ev_rx) in feeds {
            tokio::spawn(async move {
                for (i, frame) in pcm.chunks(320).enumerate() {
                    tokio::time::sleep_until(start + Duration::from_millis(i as u64 * 20)).await;
                    if audio_tx.send(frame.to_vec()).is_err() {
                        return;
                    }
                }
            });
            let st = Arc::clone(&stats);
            tokio::spawn(async move {
                while let Some(ev) = ev_rx.recv().await {
                    let mut m = st.lock().unwrap();
                    match ev {
                        RealtimeEvent::Final { segment, .. } => {
                            m.finals += 1;
                            m.lags.push(start.elapsed().as_millis() as i64 - segment.end_ms as i64);
                        }
                        RealtimeEvent::FallingBehind => m.behind += 1,
                        RealtimeEvent::Error(e) => {
                            m.errors += 1;
                            eprintln!("asr error: {e}");
                        }
                        _ => {}
                    }
                }
            });
        }
        let st = Arc::clone(&stats);
        let poll = tokio::spawn(async move {
            let mut seen = 0u64;
            loop {
                tokio::time::sleep(Duration::from_millis(40)).await;
                let snap = live_metrics::snapshot();
                if snap.decodes != seen {
                    seen = snap.decodes;
                    st.lock().unwrap().decode_ms.push(snap.last_decode_ms);
                }
            }
        });
        tokio::time::sleep_until(start + Duration::from_secs_f64(seconds)).await;
        poll.abort();
        stats.lock().unwrap().backlog_max_ms = live_metrics::snapshot().max_backlog_ms;
        for r in runs {
            r.abort();
        }
        Ok(())
    })?;
    let s = stats.lock().unwrap().clone();
    Ok(s)
}

fn asr_json(s: &AsrStats) -> serde_json::Value {
    let mut d = s.decode_ms.clone();
    d.sort_unstable();
    let mut l = s.lags.clone();
    l.sort_unstable();
    let p = |v: &[u64], q: f64| if v.is_empty() { 0 } else { v[((v.len() - 1) as f64 * q).round() as usize] };
    let pl = |v: &[i64], q: f64| if v.is_empty() { -1 } else { v[((v.len() - 1) as f64 * q).round() as usize] };
    serde_json::json!({
        "decode_ms_p50": p(&d, 0.5), "decode_ms_p90": p(&d, 0.9), "decodes": d.len(),
        "final_lag_ms_p50": pl(&l, 0.5), "final_lag_ms_p90": pl(&l, 0.9), "finals": s.finals,
        "falling_behind": s.behind, "errors": s.errors, "backlog_ms_max": s.backlog_max_ms,
    })
}

fn run_concurrent(args: &Args) -> Result<serde_json::Value> {
    let asr_dir = args.asr_model_dir.clone().ok_or_else(|| anyhow!("asr-concurrent needs --asr-model-dir"))?;
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
    let audio = clip(args, args.seconds.max(args.baseline_seconds))?;

    eprintln!("baseline: ASR alone for {} s", args.baseline_seconds);
    let base = asr_phase(&rt, &asr_dir, &audio, args.baseline_seconds, None)?;
    let rss_baseline = peak_rss_mb();

    let (d, load_s, sha) = load_diarizer(args)?;
    let d = Arc::new(Mutex::new(d));
    let done = Arc::new(AtomicBool::new(false));
    let backlog_ms_max = Arc::new(AtomicU64::new(0));
    let backlog_trace: Arc<Mutex<Vec<(f64, u64)>>> = Arc::new(Mutex::new(Vec::new()));
    let covered_total = Arc::new(Mutex::new(0.0f64));
    let (tx, rx) = mpsc::channel::<Vec<f32>>();
    let feed_audio: Vec<f32> = audio.iter().copied().cycle().take((args.seconds * SR as f64) as usize).collect();

    // Diarizer worker: drains the queue, records the backlog it sees.
    let worker = {
        let d = Arc::clone(&d);
        let done = Arc::clone(&done);
        let covered_total = Arc::clone(&covered_total);
        std::thread::spawn(move || -> Result<()> {
            while let Ok(buf) = rx.recv() {
                let c = d.lock().unwrap().push(&buf)?;
                *covered_total.lock().unwrap() += c;
            }
            done.store(true, Ordering::SeqCst);
            Ok(())
        })
    };
    let feeder: Box<dyn FnOnce(Instant) + Send> = {
        let backlog_ms_max = Arc::clone(&backlog_ms_max);
        let backlog_trace = Arc::clone(&backlog_trace);
        let covered_total = Arc::clone(&covered_total);
        Box::new(move |start: Instant| {
            for (i, buf) in feed_audio.chunks(SR / 10).enumerate() {
                let due = start + Duration::from_millis(i as u64 * 100);
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                if tx.send(buf.to_vec()).is_err() {
                    return;
                }
                let fed_ms = (i as u64 + 1) * 100;
                let covered_ms = (*covered_total.lock().unwrap() * 1000.0) as u64;
                let backlog = fed_ms.saturating_sub(covered_ms);
                backlog_ms_max.fetch_max(backlog, Ordering::SeqCst);
                if i % 10 == 0 {
                    backlog_trace.lock().unwrap().push((start.elapsed().as_secs_f64(), backlog));
                }
            }
        })
    };

    eprintln!("diarizer + ASR for {} s", args.seconds);
    let t = Instant::now();
    let with = asr_phase(&rt, &asr_dir, &audio, args.seconds, Some(feeder))?;
    worker.join().map_err(|_| anyhow!("diarizer worker panicked"))??;
    let wall = t.elapsed().as_secs_f64();
    let peak = peak_rss_mb();

    let d = d.lock().unwrap();
    let covered = *covered_total.lock().unwrap();
    let mut j = diarizer_json(&d, covered, wall);
    let rtf = j["rtf"].as_f64().unwrap_or(f64::MAX);
    let trace = backlog_trace.lock().unwrap().clone();
    let third = trace.len() / 3;
    let first_max = trace[..third].iter().map(|x| x.1).max().unwrap_or(0);
    let last_max = trace[trace.len() - third..].iter().map(|x| x.1).max().unwrap_or(0);
    let step_ms = (args.preset.step_seconds() * 1000.0) as u64;
    let lag_ok = if base.finals > 0 && with.finals > 0 {
        let (mut a, mut b) = (base.lags.clone(), with.lags.clone());
        a.sort_unstable();
        b.sort_unstable();
        let p90 = |v: &[i64]| v[((v.len() - 1) as f64 * 0.9).round() as usize] as f64;
        Some(p90(&b) <= GATE_ASR_LAG_RATIO * p90(&a).max(1.0))
    } else {
        None
    };
    let mut bd = base.decode_ms.clone();
    bd.sort_unstable();
    let mut wd = with.decode_ms.clone();
    wd.sort_unstable();
    let p90u = |v: &[u64]| if v.is_empty() { 0.0 } else { v[((v.len() - 1) as f64 * 0.9).round() as usize] as f64 };
    j["load_seconds"] = load_s.into();
    j["model_sha256"] = sha.into();
    j["rss_baseline_peak_mb"] = rss_baseline.into();
    j["peak_rss_mb"] = peak.into();
    j["added_rss_mb"] = (peak - rss_baseline).into();
    j["backlog_ms_max"] = backlog_ms_max.load(Ordering::SeqCst).into();
    j["backlog_ms_max_first_third"] = first_max.into();
    j["backlog_ms_max_last_third"] = last_max.into();
    j["asr_baseline"] = asr_json(&base);
    j["asr_with_diarizer"] = asr_json(&with);
    j["gate"] = serde_json::json!({
        "rtf_le_0_25": rtf <= GATE_RTF,
        "asr_final_lag_p90_within_10pct": lag_ok,
        "asr_decode_p90_within_10pct": p90u(&wd) <= GATE_ASR_LAG_RATIO * p90u(&bd).max(1.0),
        "no_queue_growth": last_max <= first_max + step_ms,
        "added_rss_le_500mb": peak - rss_baseline <= GATE_ADDED_RSS_MB,
        "note": "asr_final_lag_p90_within_10pct is null when the clip yields no ASR finals (synthetic audio); pass --wav with speech to measure it",
    });
    Ok(j)
}

/// Opts the process out of Windows execution-speed throttling, as the app
/// does at startup, unless `BENCH_NO_OPTOUT=1`. Returns the readback line, or
/// what was skipped.
fn opt_out() -> String {
    if std::env::var_os("BENCH_NO_OPTOUT").is_some_and(|v| v == "1") {
        return "skipped (BENCH_NO_OPTOUT=1)".into();
    }
    daisy_llm_protocol::process_power::opt_out_of_execution_throttling().unwrap_or_else(|| "n/a (not windows)".into())
}

fn main() -> Result<()> {
    let Some(args) = parse()? else { return Ok(()) };
    let optout = opt_out();
    eprintln!("ecoqos opt-out: {optout}");
    let load = check_load(args.max_load)?;
    let started = chrono_like_now();
    let mut j = match args.mode.as_str() {
        "solo" => run_solo(&args)?,
        _ => run_concurrent(&args)?,
    };
    let pass = j["gate"].as_object().map(|g| g.values().all(|v| v.as_bool() != Some(false))).unwrap_or(false);
    j["pass"] = pass.into();
    j["host"] = host_name().into();
    j["os"] = std::env::consts::OS.into();
    j["arch"] = std::env::consts::ARCH.into();
    j["ecoqos_opt_out"] = optout.into();
    j["cpu_model"] = cpu_model().into();
    j["logical_cores"] = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0).into();
    j["threads"] = args.threads.into();
    j["preset"] = args.preset.name.into();
    j["mode"] = args.mode.clone().into();
    j["clip"] = args.wav.as_ref().map_or_else(|| "synthetic".to_string(), |p| p.display().to_string()).into();
    j["load_average_at_start"] = load.into();
    j["started_unix"] = started.into();
    j["onnxruntime"] = "1.28.0 (ort 2.0.0-rc.13)".into();
    j["thresholds"] = serde_json::json!({
        "rtf_max": GATE_RTF, "asr_lag_ratio_max": GATE_ASR_LAG_RATIO, "added_rss_mb_max": GATE_ADDED_RSS_MB,
        "gate": "gate 2",
    });
    if let Some(dir) = args.out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&args.out, serde_json::to_string_pretty(&j)? + "\n")?;
    println!("{} {} t{} {}: rtf {:.3} pass {}", args.preset.name, args.mode, args.threads, args.out.display(), j["rtf"].as_f64().unwrap_or(-1.0), pass);
    Ok(())
}

fn chrono_like_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topk_orders_by_score_then_index_and_flags_silence() {
        // 3 frames + 1 silence frame, 8 speakers, k = 4.
        let frames = 4;
        let mut scores = vec![f32::NEG_INFINITY; frames * S];
        scores[0] = 1.0; // frame 0 spk 0
        scores[2 * S] = 2.0; // frame 2 spk 0
        scores[S + 1] = 2.0; // frame 1 spk 1
        for s in 0..S {
            scores[3 * S + s] = f32::INFINITY;
        }
        let (idx, dis) = topk_indices(&scores, frames, 4);
        // +inf picks: spk 0 frame 3 (index 3) and spk 1 frame 3 (index 7) etc.;
        // the four best are the +inf silence frames of speakers 0..3.
        assert_eq!(idx, vec![0, 0, 0, 0]);
        assert!(dis.iter().all(|&d| d));
    }

    #[test]
    fn fifo_fills_before_anything_pops() {
        let p = PRESETS[1];
        let mut st = State::new(&p);
        let sil = vec![0.0; D];
        let embs = vec![0.5; p.chunk_enc() * D];
        let preds = vec![0.0; p.packed() * S];
        let hi = vec![0.0; p.packed() * UP * S];
        let out = update(&p, &sil, &mut st, &embs, p.chunk_enc(), &preds, &hi, 0, p.rc);
        assert_eq!(out.len(), p.chunk_len * UP * S);
        assert_eq!(st.fifo_len, p.chunk_len.min(p.fifo));
        assert_eq!(st.spkcache_len, 0);
    }
}
