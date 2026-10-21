// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Speaker-embedding (voiceprint) extraction + cross-session matching.
//!
//! Given a per-session diarization (`Segment::speaker_id` in
//! `transcript.dedup.json`), the raw system audio (`system.wav` per chunk),
//! and the bundled WeSpeaker/ECAPA-TDNN ONNX model, extracts one mean-pooled,
//! L2-normalized embedding per speaker cluster. Embeddings can be saved into
//! the encrypted vault and matched against future sessions.
//!
//! Matching uses cosine similarity (vectors are L2-normalized; computed as a
//! dot product) against `match_threshold()`; when multiple candidates pass,
//! the highest-scoring one is returned.

use ndarray::{Array3, Axis};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub mod fbank;
pub mod segmentation;
pub mod turns;

pub const EMBED_DIM: usize = 256;
pub const SAMPLE_RATE: u32 = 16_000;

/// Minimum cosine similarity for a match. The `DAISY_VOICEPRINT_THRESHOLD`
/// env var overrides the default.
pub fn match_threshold() -> f32 {
    std::env::var("DAISY_VOICEPRINT_THRESHOLD")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.62)
}

#[derive(Debug, Error)]
pub enum VpError {
    #[error("voiceprint model missing: {0}")]
    ModelMissing(String),
    /// The model file may be present but could not be read; carries the
    /// path and the OS error.
    #[error("voiceprint model unreadable: {0}")]
    ModelUnreadable(String),
    #[error("ort: {0}")]
    Ort(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("hound: {0}")]
    Hound(#[from] hound::Error),
    #[error("decode: {0}")]
    Decode(String),
}

// Converts every `ort::Error<R>` recover variant via Display.
impl<R> From<ort::Error<R>> for VpError {
    fn from(value: ort::Error<R>) -> Self {
        Self::Ort(format!("{value}"))
    }
}

pub type Result<T> = std::result::Result<T, VpError>;

/// Resolve the voiceprint model directory: the `DAISY_VOICEPRINT_DIR` env var,
/// else `models/voiceprints/` next to the executable.
pub fn model_dir() -> PathBuf {
    if let Ok(p) = std::env::var("DAISY_VOICEPRINT_DIR") {
        return PathBuf::from(p);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default()
        .join("models")
        .join("voiceprints")
}

/// Path of the speaker-encoder weights.
pub fn model_path() -> PathBuf {
    model_dir().join("model.onnx")
}

/// Ok when `path` can be read; otherwise the missing/unreadable model error
/// the app maps to `diarization_unavailable`.
pub fn require_model_file(path: &Path) -> Result<()> {
    match syncsafe::file_state(path) {
        syncsafe::FileState::Present => Ok(()),
        syncsafe::FileState::Missing => Err(VpError::ModelMissing(format!(
            "{} missing (run scripts/download-voiceprints.sh or rebuild the AppImage)",
            path.display()
        ))),
        syncsafe::FileState::Unreadable(e) => {
            Err(VpError::ModelUnreadable(format!("{}: {e}", path.display())))
        }
    }
}

/// Whether the diarizer's model files can be loaded: the first of the
/// speaker encoder ([`model_path`]) and the segmentation network that is
/// absent or unreadable, with its path and the OS error; otherwise the
/// encoder's path and `Present`.
pub fn model_state() -> (PathBuf, syncsafe::FileState) {
    for path in [model_path(), segmentation::segmentation_model_path()] {
        match syncsafe::file_state(&path) {
            syncsafe::FileState::Present => {}
            other => return (path, other),
        }
    }
    (model_path(), syncsafe::FileState::Present)
}

static LOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RESIDENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Speaker-encoder and segmentation sessions this process has built since
/// it started.
pub fn loads() -> u64 {
    LOADS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Speaker-encoder and segmentation sessions alive right now. One model's
/// weights are resident per count.
pub fn resident() -> u64 {
    RESIDENT.load(std::sync::atomic::Ordering::Relaxed)
}

/// ONNX Runtime thread manager whose threads drop to background priority
/// before running their work.
struct BackgroundThreads;

impl ort::environment::ThreadManager for BackgroundThreads {
    type Thread = std::thread::JoinHandle<()>;

    fn create(&self, work: impl FnOnce() + Send + 'static) -> ort::Result<Self::Thread> {
        std::thread::Builder::new()
            .name("voiceprint-ort".into())
            .spawn(move || {
                transcript::cpu::lower_to_background_priority();
                work()
            })
            .map_err(|e| ort::Error::new(format!("spawn ORT thread: {e}")))
    }

    fn join(thread: Self::Thread) -> ort::Result<()> {
        thread.join().map_err(|_| ort::Error::new("ORT thread panicked"))
    }
}

/// Speaker encoder.
pub struct Encoder {
    session: ort::session::Session,
}

impl Drop for Encoder {
    fn drop(&mut self) {
        RESIDENT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Encoder {
    /// Load for interactive use: ONNX Runtime's default threading at the
    /// caller's priority.
    pub fn load() -> Result<Self> {
        Self::load_with(false)
    }

    /// Load for background finalize work: intra-op threads within the
    /// finalize budget, pool threads at background priority.
    pub fn load_background() -> Result<Self> {
        Self::load_with(true)
    }

    fn load_with(background: bool) -> Result<Self> {
        let model_path = model_path();
        require_model_file(&model_path)?;
        let builder = ort::session::Session::builder()?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?;
        let mut builder = if background {
            builder
                .with_thread_manager(BackgroundThreads)?
                .with_intra_threads(transcript::cpu::finalize_cpu_budget())?
        } else {
            builder
        };
        let session = builder.commit_from_file(&model_path)?;
        LOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        RESIDENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Self { session })
    }

    /// Take a mono 16 kHz i16 PCM buffer and return an L2-normalized
    /// speaker embedding. Caller is responsible for resampling /
    /// downmixing if the source audio doesn't already match.
    pub fn encode_pcm(&mut self, samples: &[i16]) -> Result<Vec<f32>> {
        if samples.is_empty() {
            return Err(VpError::Decode("encode_pcm: empty input".into()));
        }
        // Kaldi-Fbank features of the samples (int16 scale) feed the model.
        let audio_f32: Vec<f32> = samples.iter().map(|s| *s as f32).collect();
        let feats = fbank::compute(&audio_f32);
        if feats.shape()[0] == 0 {
            return Err(VpError::Decode(
                "encode_pcm: too little audio to form a frame".into(),
            ));
        }
        // [frames, 80] -> [1, frames, 80].
        let feats3: Array3<f32> = feats.insert_axis(Axis(0));
        let in_name = self
            .session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .unwrap_or_else(|| "feats".to_string());
        let in_tensor = ort::value::TensorRef::from_array_view(feats3.view())?;
        let inputs: Vec<(&str, ort::session::SessionInputValue)> =
            vec![(in_name.as_str(), in_tensor.into())];
        let outputs = self.session.run(inputs)?;
        let first_pair = outputs
            .iter()
            .next()
            .ok_or_else(|| VpError::Decode("no output tensor".into()))?;
        let first = first_pair.1.try_extract_array::<f32>()?;
        let view = first.view();
        // Output is typically [1, EMBED_DIM] or [1, EMBED_DIM, 1]. Flatten
        // and L2-normalize.
        let flat: Vec<f32> = view.iter().copied().collect();
        let vec = if flat.len() == EMBED_DIM {
            flat
        } else if flat.len() % EMBED_DIM == 0 && flat.len() > EMBED_DIM {
            // Multi-frame output — average across frames.
            let n = flat.len() / EMBED_DIM;
            let mut acc = vec![0f32; EMBED_DIM];
            for chunk in flat.chunks(EMBED_DIM) {
                for (a, v) in acc.iter_mut().zip(chunk) {
                    *a += *v;
                }
            }
            for a in acc.iter_mut() {
                *a /= n as f32;
            }
            acc
        } else {
            return Err(VpError::Decode(format!(
                "unexpected output len {} (expected multiple of {})",
                flat.len(),
                EMBED_DIM
            )));
        };
        Ok(l2_normalize(vec))
    }
}

fn l2_normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    for x in v.iter_mut() {
        *x /= norm;
    }
    v
}

// ===== Speaker clustering: spherical k-means + unsupervised count estimate =====
//
// k-means runs at a chosen k; `known_count = Some(n)` skips count estimation.

/// Fewest embeddings the count estimate splits into more than one speaker.
pub const MIN_EMB_FOR_SPLIT: usize = 8;
const EST_CAP: usize = 400; // subsample cap for the count sweep
const KMAX: usize = 16; // most speakers the estimator will propose

/// Two k-means clusters whose centroids sit at this cosine or above read as
/// one voice split in two.
const DISTINCT_VOICES_MAX_COSINE: f32 = 0.55;

/// A k-means cluster smaller than this many embeddings is an outlier.
const MIN_SPEAKER_EMBEDDINGS: usize = 3;

/// Cluster per-segment speaker embeddings into ids `0..k`. `known_count` is
/// used as k when `Some` (clamped to `1..=n`); otherwise k is estimated.
pub fn cluster_speakers(embeddings: &[Vec<f32>], known_count: Option<usize>) -> Vec<u32> {
    let n = embeddings.len();
    if n == 0 {
        return Vec::new();
    }
    let k = match known_count {
        Some(c) if c >= 1 => c.min(n),
        _ => estimate_speaker_count(embeddings),
    };
    if k <= 1 {
        return vec![0; n];
    }
    spherical_kmeans(embeddings, k)
}

/// Estimate the speaker count: the most clusters k-means makes, stepping k up
/// from 2, before two of its centroids reach `DISTINCT_VOICES_MAX_COSINE` or a
/// cluster holds fewer than `MIN_SPEAKER_EMBEDDINGS` embeddings. Returns 1
/// below `MIN_EMB_FOR_SPLIT` embeddings, otherwise at least 2 and at most
/// `KMAX`.
pub fn estimate_speaker_count(embeddings: &[Vec<f32>]) -> usize {
    if embeddings.len() < MIN_EMB_FOR_SPLIT {
        return 1;
    }
    let sub = subsample(embeddings, EST_CAP);
    let kmax = (sub.len() / MIN_SPEAKER_EMBEDDINGS).clamp(2, KMAX);
    let mut count = 2;
    let mut steps: Vec<String> = Vec::new();
    for k in 3..=kmax {
        let labels = spherical_kmeans(&sub, k);
        let (closest, smallest) = closest_centroids_and_smallest(&sub, &labels, k);
        steps.push(format!("k{k}: closest {closest:.2}, smallest {smallest}"));
        if closest >= DISTINCT_VOICES_MAX_COSINE || smallest < MIN_SPEAKER_EMBEDDINGS {
            break;
        }
        count = k;
    }
    log::info!(
        "speaker count estimate over {} embedding(s): {count} ({})",
        sub.len(),
        steps.join("; ")
    );
    count
}

/// The highest cosine between two of the `k` clusters' centroids, and the
/// size of the smallest cluster.
fn closest_centroids_and_smallest(emb: &[Vec<f32>], labels: &[u32], k: usize) -> (f32, usize) {
    let centers = centroids_of(emb, labels, k);
    let mut closest = f32::MIN;
    for i in 0..k {
        for j in (i + 1)..k {
            closest = closest.max(cosine(&centers[i], &centers[j]));
        }
    }
    let mut sizes = vec![0usize; k];
    for &l in labels {
        sizes[l as usize] += 1;
    }
    (closest, sizes.into_iter().min().unwrap_or(0))
}

/// Deterministically subsample to at most `max` evenly-spaced embeddings.
fn subsample(emb: &[Vec<f32>], max: usize) -> Vec<Vec<f32>> {
    if emb.len() <= max {
        return emb.to_vec();
    }
    let stride = emb.len() as f64 / max as f64;
    (0..max).map(|i| emb[(i as f64 * stride) as usize].clone()).collect()
}

/// Spherical k-means (cosine): k-means++ init, best of several deterministic
/// restarts (lowest cosine-distance inertia). Embeddings are unit-norm: a
/// centroid is the L2-normalized mean and similarity is a dot product.
fn spherical_kmeans(emb: &[Vec<f32>], k: usize) -> Vec<u32> {
    if k <= 1 || emb.len() <= k {
        // trivial: each its own cluster when n<=k, else single cluster
        return if emb.len() <= k {
            (0..emb.len() as u32).collect()
        } else {
            vec![0; emb.len()]
        };
    }
    let mut best_labels = vec![0u32; emb.len()];
    let mut best_inertia = f32::INFINITY;
    for restart in 0..6u64 {
        let seed = 0x9E3779B9u64.wrapping_add(restart.wrapping_mul(0x1234567));
        let labels = kmeans_once(emb, k, seed);
        let inrt = inertia(emb, &labels, k);
        if inrt < best_inertia {
            best_inertia = inrt;
            best_labels = labels;
        }
    }
    best_labels
}

fn kmeans_once(emb: &[Vec<f32>], k: usize, mut seed: u64) -> Vec<u32> {
    let n = emb.len();
    let mut rng = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 33) as f32 / (1u64 << 31) as f32
    };
    // k-means++: first center random, the rest by D² (cosine-distance) weighting.
    let mut centers: Vec<Vec<f32>> = vec![emb[(rng() * n as f32) as usize % n].clone()];
    while centers.len() < k {
        let d2: Vec<f32> = emb
            .iter()
            .map(|e| {
                let nearest = centers.iter().map(|c| 1.0 - cosine(e, c)).fold(f32::INFINITY, f32::min);
                nearest * nearest
            })
            .collect();
        let sum: f32 = d2.iter().sum();
        let mut target = rng() * sum;
        let mut pick = n - 1;
        for (i, &d) in d2.iter().enumerate() {
            target -= d;
            if target <= 0.0 {
                pick = i;
                break;
            }
        }
        centers.push(emb[pick].clone());
    }
    let dim = emb[0].len();
    let mut labels = vec![0u32; n];
    for _ in 0..30 {
        let mut changed = false;
        for (i, e) in emb.iter().enumerate() {
            let mut bl = 0u32;
            let mut bs = f32::NEG_INFINITY;
            for (c, ctr) in centers.iter().enumerate() {
                let s = cosine(e, ctr);
                if s > bs {
                    bs = s;
                    bl = c as u32;
                }
            }
            if labels[i] != bl {
                labels[i] = bl;
                changed = true;
            }
        }
        for c in 0..k {
            let mut acc = vec![0f32; dim];
            for (i, e) in emb.iter().enumerate() {
                if labels[i] as usize == c {
                    for (a, v) in acc.iter_mut().zip(e) {
                        *a += v;
                    }
                }
            }
            let norm = acc.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 1e-9 {
                for a in acc.iter_mut() {
                    *a /= norm;
                }
                centers[c] = acc;
            }
        }
        if !changed {
            break;
        }
    }
    labels
}

fn inertia(emb: &[Vec<f32>], labels: &[u32], k: usize) -> f32 {
    let centers = centroids_of(emb, labels, k);
    emb.iter()
        .enumerate()
        .map(|(i, e)| 1.0 - cosine(e, &centers[labels[i] as usize]))
        .sum()
}

/// L2-normalized mean embedding per cluster id `0..k`.
fn centroids_of(emb: &[Vec<f32>], labels: &[u32], k: usize) -> Vec<Vec<f32>> {
    let dim = emb.first().map(|v| v.len()).unwrap_or(0);
    let mut acc = vec![vec![0f32; dim]; k];
    for (i, e) in emb.iter().enumerate() {
        let c = labels[i] as usize;
        for (x, v) in acc[c].iter_mut().zip(e) {
            *x += v;
        }
    }
    acc.into_iter().map(l2_normalize).collect()
}

/// Merge clusters that match the same enrolled identity. For each current
/// cluster, find the gallery identity whose nearest sample best matches the
/// cluster centroid (cosine ≥ `match_thr`); clusters mapping to the same
/// identity collapse into one. Clusters matching nothing keep their own id.
/// Returns compacted labels `0..k'`, ordered by first appearance.
///
/// `gallery`: `(identity_id, sample_embedding)` pairs; one identity may appear
/// multiple times (its gallery samples). Samples must be L2-normalized.
pub fn merge_by_gallery(
    labels: &[u32],
    embeddings: &[Vec<f32>],
    gallery: &[(u32, Vec<f32>)],
    match_thr: f32,
) -> Vec<u32> {
    if labels.is_empty() {
        return Vec::new();
    }
    if gallery.is_empty() {
        return compact(labels);
    }
    let k = labels.iter().copied().max().unwrap() as usize + 1;
    let centroids = centroids_of(embeddings, labels, k);

    // Best-matching gallery identity per cluster (None if below threshold).
    let ident: Vec<Option<u32>> = centroids
        .iter()
        .map(|c| {
            let mut best: (Option<u32>, f32) = (None, match_thr);
            for (gid, gemb) in gallery {
                let s = cosine(c, gemb);
                if s >= best.1 {
                    best = (Some(*gid), s);
                }
            }
            best.0
        })
        .collect();

    // Union clusters sharing an identity onto the first cluster that claimed it.
    let mut rep_for_ident: std::collections::HashMap<u32, usize> = Default::default();
    let mut remap: Vec<usize> = (0..k).collect();
    for (c, id) in ident.iter().enumerate() {
        if let Some(id) = id {
            let rep = *rep_for_ident.entry(*id).or_insert(c);
            remap[c] = rep;
        }
    }
    compact(&labels.iter().map(|&l| remap[l as usize] as u32).collect::<Vec<_>>())
}

/// Renumber labels to contiguous `0..k'` in order of first appearance.
fn compact(labels: &[u32]) -> Vec<u32> {
    let mut map: std::collections::HashMap<u32, u32> = Default::default();
    let mut next = 0u32;
    labels
        .iter()
        .map(|&l| {
            *map.entry(l).or_insert_with(|| {
                let v = next;
                next += 1;
                v
            })
        })
        .collect()
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Pull i16 mono samples for a chunk's system audio between
/// `start_ms..end_ms` (relative to that WAV's start). Returns at most
/// `cap_ms` worth of audio.
pub fn read_pcm_window(wav_path: &Path, start_ms: u32, end_ms: u32, cap_ms: u32) -> Result<Vec<i16>> {
    let mut reader = hound::WavReader::open(wav_path)?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE {
        return Err(VpError::Decode(format!(
            "{}: sample rate {} != {}",
            wav_path.display(),
            spec.sample_rate,
            SAMPLE_RATE
        )));
    }
    let start = (start_ms as u64 * spec.sample_rate as u64 / 1000) as u32;
    let raw_end = (end_ms.min(start_ms.saturating_add(cap_ms)) as u64
        * spec.sample_rate as u64
        / 1000) as u32;
    if raw_end <= start {
        return Ok(Vec::new());
    }
    let want = (raw_end - start) as usize;
    let chan = spec.channels.max(1) as usize;
    // Seek directly to the window start; `seek` is per-frame.
    reader
        .seek(start)
        .map_err(|e| VpError::Decode(format!("{}: seek: {e}", wav_path.display())))?;
    let mut iter = reader.into_samples::<i16>();
    let mut out: Vec<i16> = Vec::with_capacity(want);
    for _ in 0..want {
        let mut sum = 0i32;
        for _ in 0..chan {
            match iter.next() {
                Some(Ok(s)) => sum += s as i32,
                _ => return Ok(out),
            }
        }
        let mono = (sum / chan as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        out.push(mono);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_preserves_direction() {
        let v = vec![3.0_f32, 4.0, 0.0];
        let n = l2_normalize(v);
        assert!((n[0] - 0.6).abs() < 1e-6);
        assert!((n[1] - 0.8).abs() < 1e-6);
        let sq: f32 = n.iter().map(|x| x * x).sum();
        assert!((sq - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_identity() {
        let v = l2_normalize(vec![1.0, 1.0, 1.0]);
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    // Build n noisy unit vectors around an axis given by `base`.
    fn cloud(base: &[f32], n: usize, jitter: f32) -> Vec<Vec<f32>> {
        (0..n)
            .map(|i| {
                let mut v: Vec<f32> = base.to_vec();
                // deterministic small perpendicular wobble
                let w = ((i as f32 * 12.9898).sin() * 43758.5453).fract() - 0.5;
                for (d, x) in v.iter_mut().enumerate() {
                    *x += jitter * w * if d % 2 == 0 { 1.0 } else { -1.0 };
                }
                l2_normalize(v)
            })
            .collect()
    }

    #[test]
    fn estimate_and_cluster_two_distinct_voices() {
        // Two well-separated clouds → estimator finds 2, k-means splits cleanly.
        let mut embs = cloud(&[1.0, 0.0, 0.0, 0.0], 8, 0.05);
        embs.extend(cloud(&[0.0, 1.0, 0.0, 0.0], 8, 0.05));
        assert_eq!(estimate_speaker_count(&embs), 2, "two clouds → k=2");
        let ids = cluster_speakers(&embs, None);
        let n0 = ids[..8].iter().filter(|&&l| l == ids[0]).count();
        assert_eq!(n0, 8, "first cloud is one cluster");
        assert!(ids[8..].iter().all(|&l| l != ids[0]), "second cloud is the other cluster");
    }

    #[test]
    fn known_count_is_honored() {
        // Three distinct directions, but caller pins 2 → exactly 2 clusters.
        let mut embs = cloud(&[1.0, 0.0, 0.0, 0.0], 5, 0.03);
        embs.extend(cloud(&[0.0, 1.0, 0.0, 0.0], 5, 0.03));
        embs.extend(cloud(&[0.0, 0.0, 1.0, 0.0], 5, 0.03));
        let ids = cluster_speakers(&embs, Some(2));
        assert_eq!(ids.iter().copied().max().unwrap() + 1, 2, "pinned to 2");
        let ids3 = cluster_speakers(&embs, Some(3));
        assert_eq!(ids3.iter().copied().max().unwrap() + 1, 3, "pinned to 3");
    }

    /// Deterministic standard-normal draws (xorshift + Box-Muller).
    struct Draws(u64);

    impl Draws {
        fn uniform(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            ((self.0 >> 40) as f32 + 0.5) / (1u64 << 24) as f32
        }

        fn normal(&mut self) -> f32 {
            let (u, v) = (self.uniform(), self.uniform());
            (-2.0 * u.ln()).sqrt() * (2.0 * std::f32::consts::PI * v).cos()
        }

        fn unit(&mut self, dim: usize) -> Vec<f32> {
            l2_normalize((0..dim).map(|_| self.normal()).collect())
        }
    }

    const VOICE_DIM: usize = 64;

    /// `n` embeddings of one voice centred on `centre`, each sitting near
    /// cosine 0.75 to it.
    fn voice(centre: &[f32], n: usize, draws: &mut Draws) -> Vec<Vec<f32>> {
        let sigma = 0.11;
        (0..n)
            .map(|_| l2_normalize(centre.iter().map(|c| c + sigma * draws.normal()).collect()))
            .collect()
    }

    #[test]
    fn estimate_counts_quiet_voices_next_to_a_dominant_one() {
        let mut draws = Draws(0x5EED_0001);
        let mut embs = voice(&draws.unit(VOICE_DIM), 200, &mut draws);
        for _ in 0..3 {
            let centre = draws.unit(VOICE_DIM);
            embs.extend(voice(&centre, 20, &mut draws));
        }
        assert_eq!(estimate_speaker_count(&embs), 4, "one dominant voice and three quiet ones");
    }

    #[test]
    fn estimate_counts_equal_speakers() {
        let mut draws = Draws(0x5EED_0002);
        let mut embs = Vec::new();
        for _ in 0..4 {
            let centre = draws.unit(VOICE_DIM);
            embs.extend(voice(&centre, 40, &mut draws));
        }
        assert_eq!(estimate_speaker_count(&embs), 4);
    }

    #[test]
    fn estimate_keeps_a_lone_speaker_at_two() {
        let mut draws = Draws(0x5EED_0003);
        let embs = voice(&draws.unit(VOICE_DIM), 300, &mut draws);
        assert_eq!(estimate_speaker_count(&embs), 2);
    }

    #[test]
    fn estimate_skips_outliers_as_speakers() {
        let mut draws = Draws(0x5EED_0004);
        let mut embs = Vec::new();
        for _ in 0..2 {
            let centre = draws.unit(VOICE_DIM);
            embs.extend(voice(&centre, 60, &mut draws));
        }
        // Two lone embeddings pointing nowhere near either voice.
        for _ in 0..2 {
            embs.push(draws.unit(VOICE_DIM));
        }
        assert_eq!(estimate_speaker_count(&embs), 2);
    }

    #[test]
    fn estimate_with_too_few_embeddings_to_split_is_one() {
        let mut draws = Draws(0x5EED_0005);
        let mut embs = Vec::new();
        for _ in 0..(MIN_EMB_FOR_SPLIT - 1) {
            embs.push(draws.unit(VOICE_DIM));
        }
        assert_eq!(estimate_speaker_count(&embs), 1);
    }

    #[test]
    fn estimate_with_few_embeddings_stops_at_two() {
        // 8 embeddings allow k up to 8 / MIN_SPEAKER_EMBEDDINGS = 2.
        let mut draws = Draws(0x5EED_0006);
        let mut embs = Vec::new();
        for _ in 0..4 {
            let centre = draws.unit(VOICE_DIM);
            embs.extend(voice(&centre, 2, &mut draws));
        }
        assert_eq!(embs.len(), MIN_EMB_FOR_SPLIT);
        assert_eq!(estimate_speaker_count(&embs), 2);
    }

    #[test]
    fn estimate_stops_at_kmax() {
        let mut draws = Draws(0x5EED_0007);
        let mut embs = Vec::new();
        for _ in 0..(KMAX + 4) {
            let centre = draws.unit(VOICE_DIM);
            embs.extend(voice(&centre, 25, &mut draws));
        }
        assert_eq!(estimate_speaker_count(&embs), KMAX);
    }

    #[test]
    fn too_few_embeddings_stay_one_cluster() {
        let embs = cloud(&[1.0, 0.0, 0.0, 0.0], 4, 0.05);
        assert_eq!(estimate_speaker_count(&embs), 1, "below MIN_EMB_FOR_SPLIT → 1");
        assert!(cluster_speakers(&embs, None).iter().all(|&l| l == 0));
    }

    #[test]
    fn read_pcm_window_seeks_to_exact_offset() {
        // Ramp where each frame's value == its index; asserts the seek lands
        // on the exact frame.
        let dir = std::env::temp_dir().join(format!("vp-rpw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ramp.wav");
        // Value == frame index (< 32768).
        let samples: Vec<i16> = (0..32_000).map(|i| i as i16).collect();
        transcript::wav_io::write_wav_mono_i16(&path, &samples, SAMPLE_RATE).unwrap();
        // Window at 1000ms (frame 16000) for 100ms (1600 frames).
        let out = read_pcm_window(&path, 1000, 1100, 100).unwrap();
        assert_eq!(out.len(), 1600);
        assert_eq!(out[0], 16000); // seek landed on the exact frame
        assert_eq!(out[1], 16001);
        assert_eq!(*out.last().unwrap(), 17599);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gallery_merge_folds_same_identity() {
        // Two clusters that are really the same enrolled person's two acoustic
        // modes (both ≈ axis A) + one distinct cluster (axis B). A gallery with
        // identity 7 ≈ A and identity 9 ≈ B collapses the two A-clusters into one.
        let a = l2_normalize(vec![1.0, 0.0, 0.0, 0.0]);
        let a2 = l2_normalize(vec![0.9, 0.1, 0.0, 0.0]);
        let b = l2_normalize(vec![0.0, 1.0, 0.0, 0.0]);
        let embeddings = vec![a.clone(), a.clone(), a2.clone(), a2.clone(), b.clone(), b.clone()];
        let labels = vec![0u32, 0, 1, 1, 2, 2];
        let gallery = vec![(7u32, a.clone()), (7u32, a2.clone()), (9u32, b.clone())];
        let merged = merge_by_gallery(&labels, &embeddings, &gallery, 0.62);
        // clusters 0 and 1 (both identity 7) collapse; cluster 2 (identity 9) stays
        assert_eq!(merged.iter().copied().max().unwrap() + 1, 2, "A-modes merge → 2 ids");
        assert_eq!(merged[0], merged[2], "both A clusters share an id");
        assert_ne!(merged[0], merged[4], "B stays separate");
    }

    #[test]
    fn gallery_merge_keeps_distinct_and_unmatched() {
        let a = l2_normalize(vec![1.0, 0.0, 0.0, 0.0]);
        let b = l2_normalize(vec![0.0, 1.0, 0.0, 0.0]);
        let c = l2_normalize(vec![0.0, 0.0, 1.0, 0.0]); // matches nothing in gallery
        let embeddings = vec![a.clone(), b.clone(), c.clone()];
        let labels = vec![0u32, 1, 2];
        let gallery = vec![(1u32, a.clone()), (2u32, b.clone())];
        let merged = merge_by_gallery(&labels, &embeddings, &gallery, 0.62);
        assert_eq!(merged, vec![0, 1, 2], "distinct identities + unmatched cluster all stay");
    }

    #[test]
    fn gallery_merge_noop_without_gallery() {
        let labels = vec![0u32, 1, 1, 2];
        let embeddings = cloud(&[1.0, 0.0, 0.0, 0.0], 4, 0.0);
        assert_eq!(merge_by_gallery(&labels, &embeddings, &[], 0.62), vec![0, 1, 1, 2]);
    }
}
