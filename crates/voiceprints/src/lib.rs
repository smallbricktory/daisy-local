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
pub mod speakrs_diar;

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

/// Resolve the voiceprint model directory. Precedence:
///   1. the `DAISY_VOICEPRINT_DIR` env var
///   2. `models/voiceprints/` next to the executable
///   3. repo-relative `models/voiceprints/`
pub fn model_dir() -> PathBuf {
    if let Ok(p) = std::env::var("DAISY_VOICEPRINT_DIR") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("models/voiceprints");
            if cand.is_dir() {
                return cand;
            }
        }
    }
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("models/voiceprints"))
        .unwrap_or_else(|| PathBuf::from("models/voiceprints"))
}

pub struct Encoder {
    session: ort::session::Session,
}

impl Encoder {
    pub fn load() -> Result<Self> {
        let dir = model_dir();
        let model_path = dir.join("model.onnx");
        if !model_path.is_file() {
            return Err(VpError::ModelMissing(format!(
                "{} missing (run scripts/download-voiceprints.sh or rebuild the AppImage)",
                model_path.display()
            )));
        }
        let session = ort::session::Session::builder()?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?
            .commit_from_file(&model_path)?;
        Ok(Self { session })
    }

    /// Take a mono 16 kHz i16 PCM buffer and return an L2-normalized
    /// speaker embedding. Caller is responsible for resampling /
    /// downmixing if the source audio doesn't already match.
    pub fn encode_pcm(&mut self, samples: &[i16]) -> Result<Vec<f32>> {
        if samples.is_empty() {
            return Err(VpError::Decode("encode_pcm: empty input".into()));
        }
        // The WeSpeaker ResNet34 model takes kaldi-Fbank features (`feats`,
        // rank-3 [batch, frames, 80]), not raw audio. Samples are fed at
        // kaldi's int16 magnitude scale (raw i16 as f32); the front-end's CMN
        // removes any constant log-offset.
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
// k-means runs at a chosen k. Unsupervised count estimation over-counts a
// lone speaker (minimum 2 once there is stable structure); identity merging
// (`merge_by_gallery`) folds those modes back together downstream. Passing
// `known_count = Some(n)` skips estimation.

const MIN_EMB_FOR_SPLIT: usize = 8; // below this, don't attempt >1 speaker
const EST_CAP: usize = 400; // subsample cap for the O(k·n²) silhouette sweep
const KMAX: usize = 8; // most speakers the estimator will propose

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

/// Estimate speaker count via tolerance-silhouette: sweep k=2..=KMAX and pick
/// the smallest k whose silhouette is within `SIL_TOL` of the peak. Returns 1
/// for too-few embeddings; once there is stable structure it returns at
/// least 2, including for a lone speaker.
pub fn estimate_speaker_count(embeddings: &[Vec<f32>]) -> usize {
    const SIL_TOL: f32 = 0.05;
    if embeddings.len() < MIN_EMB_FOR_SPLIT {
        return 1;
    }
    let sub = subsample(embeddings, EST_CAP);
    let kmax = (sub.len() / 3).clamp(2, KMAX);
    let mut sils: Vec<(usize, f32)> = Vec::new();
    for k in 2..=kmax {
        let labels = spherical_kmeans(&sub, k);
        sils.push((k, silhouette(&sub, &labels, k)));
    }
    let peak = sils.iter().cloned().fold((1usize, f32::MIN), |a, b| if b.1 > a.1 { b } else { a });
    sils.iter()
        .filter(|(_, s)| *s >= peak.1 - SIL_TOL)
        .map(|(k, _)| *k)
        .min()
        .unwrap_or(1)
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

/// Mean silhouette over a (subsampled, for large n) set; cosine distance.
fn silhouette(emb: &[Vec<f32>], labels: &[u32], k: usize) -> f32 {
    if k <= 1 {
        return 0.0;
    }
    let n = emb.len();
    let stride = (n / 600).max(1);
    let (mut total, mut count) = (0f32, 0usize);
    for i in (0..n).step_by(stride) {
        let li = labels[i] as usize;
        let mut sums = vec![0f64; k];
        let mut cnts = vec![0usize; k];
        for j in 0..n {
            if i == j {
                continue;
            }
            let d = (1.0 - cosine(&emb[i], &emb[j])) as f64;
            sums[labels[j] as usize] += d;
            cnts[labels[j] as usize] += 1;
        }
        let a = if cnts[li] > 0 { (sums[li] / cnts[li] as f64) as f32 } else { 0.0 };
        let b = (0..k)
            .filter(|&c| c != li && cnts[c] > 0)
            .map(|c| (sums[c] / cnts[c] as f64) as f32)
            .fold(f32::INFINITY, f32::min);
        if b.is_finite() {
            total += (b - a) / a.max(b).max(1e-9);
            count += 1;
        }
    }
    if count > 0 {
        total / count as f32
    } else {
        0.0
    }
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
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        {
            let mut w = hound::WavWriter::create(&path, spec).unwrap();
            for i in 0..32_000i32 {
                w.write_sample(i as i16).unwrap(); // value == frame index (< 32768)
            }
            w.finalize().unwrap();
        }
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
