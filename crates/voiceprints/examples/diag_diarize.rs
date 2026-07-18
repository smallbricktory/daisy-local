//! Diarization diagnostic. Runs on a Daisy session dir (manifest.json +
//! transcript.dedup.json + chunks/NNNN/system.wav). For the chosen track it
//! builds embeddings two ways:
//!   A. per-ASR-segment (first min(dur,8s), one mean-pooled vec)
//!   B. sub-windowed (1.5s window / 0.75s hop)
//! Then reports, for each:
//!   * pairwise-cosine distribution
//!   * spherical k-means k=1..6 with mean silhouette + cluster sizes
//!   * what `cluster_speakers()` returns (auto and known-k params, and
//!     forced cluster-to-N).
//!
//! Usage:
//!   DAISY_VOICEPRINT_DIR=$PWD/models/voiceprints \
//!   cargo run --release --example diag_diarize -p voiceprints -- <session_dir> [track] [known_k]
//!   (track defaults to "system"; known_k defaults to 0/unknown)

use serde_json::Value;
use std::path::{Path, PathBuf};
use voiceprints::{cluster_speakers, cosine, read_pcm_window, Encoder};

fn min_ms() -> u32 {
    std::env::var("DAISY_DIAG_MINMS").ok().and_then(|s| s.parse().ok()).unwrap_or(1_200)
}
const EMBED_CAP_MS: u32 = 8_000;
const SUBWIN_MS: u32 = 1_500;
const SUBHOP_MS: u32 = 750;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: diag_diarize <session_dir> [track=system] [known_k=0]");
        std::process::exit(2);
    }
    let dir = PathBuf::from(&args[1]);
    let track = args.get(2).cloned().unwrap_or_else(|| "system".to_string());
    let known_k: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);

    let manifest: Value =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).expect("manifest.json"))
            .expect("parse manifest");
    let tpath = dir.join("transcript.dedup.json");
    let tpath = if tpath.is_file() { tpath } else { dir.join("transcript.json") };
    let transcript: Value =
        serde_json::from_slice(&std::fs::read(&tpath).expect("transcript")).expect("parse transcript");

    let mut enc = Encoder::load().expect("load encoder (set DAISY_VOICEPRINT_DIR)");

    let emb_a = collect_embeddings(&dir, &manifest, &transcript, &track, &mut enc);
    // Local-speaker embeddings come from mic_aec (or mic); the AEC-bleed
    // check compares system clusters against their centroid.
    let local_track = if has_track(&transcript, "mic_aec") { "mic_aec" } else { "mic" };
    let emb_local = collect_embeddings(&dir, &manifest, &transcript, local_track, &mut enc);

    eprintln!(
        "session {:?} | system emb={} local({})={}",
        dir.file_name().unwrap_or_default(),
        emb_a.len(),
        local_track,
        emb_local.len()
    );

    report("A: per-segment (current)", &emb_a, known_k);
    bakeoff(&emb_a, &emb_local, known_k);
    let _ = (SUBWIN_MS, SUBHOP_MS);
}

fn has_track(transcript: &Value, track: &str) -> bool {
    transcript["chunks"].as_array().map_or(false, |chs| {
        chs.iter().any(|ch| {
            ch["tracks"]
                .as_array()
                .map_or(false, |trs| trs.iter().any(|tr| tr["track"].as_str() == Some(track)))
        })
    })
}

// Per-segment embeddings for one track, current-pipeline style (>=1.2s, first 8s).
fn collect_embeddings(
    dir: &Path,
    manifest: &Value,
    transcript: &Value,
    track: &str,
    enc: &mut Encoder,
) -> Vec<Vec<f32>> {
    let rel_key = match track {
        "mic" => "mic_wav_relative",
        "mic_aec" => "mic_aec_wav_relative",
        _ => "system_wav_relative",
    };
    let wav_for = |idx: i64| -> Option<PathBuf> {
        manifest["chunks"].as_array()?.iter().find_map(|c| {
            if c["index"].as_i64() == Some(idx) {
                c[rel_key].as_str().map(|r| dir.join(r))
            } else {
                None
            }
        })
    };
    let mut emb: Vec<Vec<f32>> = Vec::new();
    for ch in transcript["chunks"].as_array().unwrap_or(&vec![]).iter() {
        let cidx = ch["chunk_index"].as_i64().unwrap_or(-1);
        for tr in ch["tracks"].as_array().unwrap_or(&vec![]).iter() {
            if tr["track"].as_str() != Some(track) {
                continue;
            }
            for s in tr["segments"].as_array().unwrap_or(&vec![]).iter() {
                let a = s["start_ms"].as_u64().unwrap_or(0) as u32;
                let b = s["end_ms"].as_u64().unwrap_or(0) as u32;
                if b <= a || b - a < min_ms() {
                    continue;
                }
                let Some(wav) = wav_for(cidx) else { continue };
                if !wav.is_file() {
                    continue;
                }
                let cap = (b - a).min(EMBED_CAP_MS);
                if let Ok(pcm) = read_pcm_window(&wav, a, a + cap, cap) {
                    if pcm.len() >= 8000 {
                        if let Ok(v) = enc.encode_pcm(&pcm) {
                            emb.push(v);
                        }
                    }
                }
            }
        }
    }
    emb
}

// ======================= count-estimator bake-off =======================
// Each estimator takes the embeddings and returns a predicted speaker count.
// truth (known_k) is used only for scoring the report, never fed to an
// estimator.
fn bakeoff(emb: &[Vec<f32>], local: &[Vec<f32>], truth: usize) {
    println!("\n################ COUNT-ESTIMATOR BAKE-OFF ################");
    println!("(truth = {})", if truth > 0 { truth.to_string() } else { "?".into() });
    if emb.len() < 8 {
        println!("too few embeddings");
        return;
    }
    // Subsample to a common cap.
    let sub = subsample(emb, 360);
    println!("using {} of {} embeddings", sub.len(), emb.len());

    // ---- k=1 null test ----
    // Fit a 2-component GMM to the pairwise-cosine distribution and compare
    // its BIC to a 1-component fit; a 1-component win reports single-speaker.
    let xs = pairwise(&sub);
    let gmm = fit_2gmm(&xs);
    let (bic1, bic2) = bic_1v2(&xs, &gmm);
    let multi = bic2 < bic1;
    let thr = valley(&gmm);
    println!(
        "k=1 null test: BIC1={:.0} BIC2={:.0} -> {} | valley thr={:.3} (humps {:.2}/{:.2}, w {:.2}/{:.2})",
        bic1,
        bic2,
        if multi { "MULTI-speaker" } else { "SINGLE speaker (k=1)" },
        thr,
        gmm.0,
        gmm.3,
        gmm.2,
        gmm.5
    );

    // ---- 1-vs-2 discriminator panel: measures candidate signals at k=2.
    {
        let labels2 = spherical_kmeans(&sub, 2);
        let c0 = centroid_refs(&sub.iter().zip(&labels2).filter(|(_, &l)| l == 0).map(|(e, _)| e).collect::<Vec<_>>());
        let c1 = centroid_refs(&sub.iter().zip(&labels2).filter(|(_, &l)| l == 1).map(|(e, _)| e).collect::<Vec<_>>());
        let n0 = labels2.iter().filter(|&&l| l == 0).count();
        let n1 = sub.len() - n0;
        let sil2 = silhouette(&sub, &labels2, 2);
        let bc = cosine(&c0, &c1);
        let frac = n0.min(n1) as f32 / sub.len() as f32;
        println!(
            "1-vs-2 panel @k=2: silhouette={:.3}  between-centroid-cos={:.3}  smaller-frac={:.3}  hump-sep={:.3}",
            sil2,
            bc,
            frac,
            gmm.3 - gmm.0
        );
    }

    // ---- prediction strength: PS(k) for k=2..6, and the PS-chosen count
    // (largest k with PS >= 0.8; defaults to 1 since PS(1)=1).
    print!("prediction-strength: ");
    for k in 2..=6usize {
        print!("PS({k})={:.2} ", prediction_strength(&sub, k));
    }
    println!("-> k={}", est_prediction_strength(&sub));

    let null = |k: usize| if multi { k } else { 1 };
    let preds = [
        ("silhouette-tol(0.05)", null(est_silhouette_tol(&sub))),
        ("prediction-strength", est_prediction_strength(&sub)),
        ("eigengap-spectral", est_eigengap(&sub)),
        ("NME auto-tuning", est_nme(&sub)),
        ("affinity-valley AHC", est_ahc_valley(&sub, thr)),
        ("DP-means(valley λ)", est_dpmeans(&sub, 1.0 - thr)),
        ("GMM-BIC", est_gmm_bic(&sub)),
    ];
    println!("\n{:<24} {:>6} {:>6}", "estimator", "k", "hit");
    for (name, k) in preds {
        let hit = if truth > 0 {
            if k == truth { "  ✓" } else { "  ✗" }
        } else {
            ""
        };
        println!("{name:<24} {k:>6} {hit}");
    }

    // ---- AEC-bleed check: split system at the silhouette-chosen k and
    // measure each cluster's similarity to the local centroid.
    if !local.is_empty() {
        let lc = centroid(local);
        let k = est_silhouette_tol(&sub).max(2);
        let labels = spherical_kmeans(&sub, k);
        println!("\nAEC-bleed check (system split at k={k}, local '{}' centroid):", "you");
        println!("{:>7} {:>6} {:>14}", "cluster", "size", "cos→local");
        for c in 0..k {
            let members: Vec<&Vec<f32>> =
                sub.iter().zip(&labels).filter(|(_, &l)| l as usize == c).map(|(e, _)| e).collect();
            if members.is_empty() {
                continue;
            }
            let cc = centroid_refs(&members);
            let s = cosine(&cc, &lc);
            let flag = if s > 0.5 { "  <== BLEED (you)" } else { "" };
            println!("{c:>7} {:>6} {s:>14.3}{flag}", members.len());
        }
        // Reference: local-centroid similarity to the system centroid.
        let sys_c = centroid(&sub);
        println!(
            "reference: cos(local_centroid, system_centroid) = {:.3}",
            cosine(&lc, &sys_c)
        );
    }
}

// Tibshirani prediction strength for cluster count k. Split the data, cluster
// the train half, classify the test half by train centroids, and measure how
// well the test's own k-means clusters are preserved (min co-membership over
// test clusters). Stable structure scores ~1; an arbitrary split scores low.
fn prediction_strength(emb: &[Vec<f32>], k: usize) -> f32 {
    if k <= 1 {
        return 1.0;
    }
    let mut scores = Vec::new();
    for s in 0..6u64 {
        let (mut a, mut b): (Vec<Vec<f32>>, Vec<Vec<f32>>) = (Vec::new(), Vec::new());
        for (i, e) in emb.iter().enumerate() {
            let h = (i as u64).wrapping_mul(2654435761).wrapping_add(s.wrapping_mul(40503));
            if (h >> 16) & 1 == 0 {
                a.push(e.clone());
            } else {
                b.push(e.clone());
            }
        }
        if a.len() < k || b.len() < k {
            continue;
        }
        let la = spherical_kmeans(&a, k);
        let ca = centroids_of(&a, &la, k);
        let lb = spherical_kmeans(&b, k);
        // classify each test point by nearest train centroid
        let cross: Vec<usize> = b
            .iter()
            .map(|e| {
                (0..k)
                    .max_by(|&x, &y| cosine(e, &ca[x]).partial_cmp(&cosine(e, &ca[y])).unwrap())
                    .unwrap()
            })
            .collect();
        // for each test cluster, fraction of within-cluster pairs that stay
        // co-assigned under the train classifier; PS = min over clusters.
        let mut min_frac = 1.0f32;
        for c in 0..k {
            let idx: Vec<usize> = (0..b.len()).filter(|&i| lb[i] as usize == c).collect();
            if idx.len() < 2 {
                continue;
            }
            let (mut same, mut tot) = (0u64, 0u64);
            for x in 0..idx.len() {
                for y in (x + 1)..idx.len() {
                    tot += 1;
                    if cross[idx[x]] == cross[idx[y]] {
                        same += 1;
                    }
                }
            }
            if tot > 0 {
                min_frac = min_frac.min(same as f32 / tot as f32);
            }
        }
        scores.push(min_frac);
    }
    if scores.is_empty() {
        0.0
    } else {
        scores.iter().sum::<f32>() / scores.len() as f32
    }
}

fn est_prediction_strength(emb: &[Vec<f32>]) -> usize {
    let mut best = 1;
    for k in 2..=8usize {
        if prediction_strength(emb, k) >= 0.8 {
            best = k;
        }
    }
    best
}

fn centroids_of(emb: &[Vec<f32>], labels: &[u32], k: usize) -> Vec<Vec<f32>> {
    let dim = emb[0].len();
    let mut acc = vec![vec![0f32; dim]; k];
    let mut cnt = vec![0usize; k];
    for (i, e) in emb.iter().enumerate() {
        let c = labels[i] as usize;
        for (x, v) in acc[c].iter_mut().zip(e) {
            *x += v;
        }
        cnt[c] += 1;
    }
    for c in 0..k {
        let norm = acc[c].iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
        for x in acc[c].iter_mut() {
            *x /= norm;
        }
    }
    acc
}

fn centroid(emb: &[Vec<f32>]) -> Vec<f32> {
    let refs: Vec<&Vec<f32>> = emb.iter().collect();
    centroid_refs(&refs)
}
fn centroid_refs(emb: &[&Vec<f32>]) -> Vec<f32> {
    let dim = emb[0].len();
    let mut acc = vec![0f32; dim];
    for e in emb {
        for (a, v) in acc.iter_mut().zip(e.iter()) {
            *a += v;
        }
    }
    let norm = acc.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    for a in acc.iter_mut() {
        *a /= norm;
    }
    acc
}

fn pairwise(emb: &[Vec<f32>]) -> Vec<f32> {
    let mut xs = Vec::new();
    for i in 0..emb.len() {
        for j in (i + 1)..emb.len() {
            xs.push(cosine(&emb[i], &emb[j]));
        }
    }
    xs
}

fn subsample(emb: &[Vec<f32>], max: usize) -> Vec<Vec<f32>> {
    if emb.len() <= max {
        return emb.to_vec();
    }
    let stride = emb.len() as f64 / max as f64;
    (0..max).map(|i| emb[(i as f64 * stride) as usize].clone()).collect()
}

// k that maximizes silhouette, but prefer the smallest k within 0.05 of the peak.
fn est_silhouette_tol(emb: &[Vec<f32>]) -> usize {
    let mut sils = vec![];
    for k in 2..=8usize {
        let labels = spherical_kmeans(emb, k);
        sils.push((k, silhouette(emb, &labels, k)));
    }
    let peak = sils.iter().cloned().fold((1, 0.0f32), |a, b| if b.1 > a.1 { b } else { a });
    sils.iter()
        .filter(|(_, s)| *s >= peak.1 - 0.05)
        .map(|(k, _)| *k)
        .min()
        .unwrap_or(1)
}

// (m0, v0, w0, m1, v1, w1) — two 1-D Gaussians over the pairwise-cosine values.
type Gmm = (f32, f32, f32, f32, f32, f32);

fn gauss(x: f32, m: f32, v: f32) -> f32 {
    (-(x - m) * (x - m) / (2.0 * v)).exp() / (v * 6.2832).sqrt()
}

// Fit a 2-component 1-D GMM to pairwise cosines via EM. The two humps are
// "different speaker" (low) and "same speaker" (high).
fn fit_2gmm(xs: &[f32]) -> Gmm {
    let (mut m0, mut m1) = (0.1f32, 0.6f32);
    let (mut v0, mut v1) = (0.05f32, 0.05f32);
    let (mut w0, mut w1) = (0.5f32, 0.5f32);
    for _ in 0..50 {
        let (mut s0, mut s1) = (0.0f64, 0.0f64);
        let (mut sm0, mut sm1) = (0.0f64, 0.0f64);
        for &x in xs {
            let p0 = w0 * gauss(x, m0, v0);
            let p1 = w1 * gauss(x, m1, v1);
            let r = p1 / (p0 + p1 + 1e-12);
            s0 += (1.0 - r) as f64;
            s1 += r as f64;
            sm0 += ((1.0 - r) * x) as f64;
            sm1 += (r * x) as f64;
        }
        m0 = (sm0 / s0.max(1e-9)) as f32;
        m1 = (sm1 / s1.max(1e-9)) as f32;
        let (mut sv0, mut sv1) = (0.0f64, 0.0f64);
        for &x in xs {
            let p0 = w0 * gauss(x, m0, v0);
            let p1 = w1 * gauss(x, m1, v1);
            let r = p1 / (p0 + p1 + 1e-12);
            sv0 += ((1.0 - r) * (x - m0) * (x - m0)) as f64;
            sv1 += (r * (x - m1) * (x - m1)) as f64;
        }
        v0 = (sv0 / s0.max(1e-9)).max(1e-4) as f32;
        v1 = (sv1 / s1.max(1e-9)).max(1e-4) as f32;
        w0 = (s0 / xs.len() as f64) as f32;
        w1 = (s1 / xs.len() as f64) as f32;
    }
    (m0, v0, w0, m1, v1, w1)
}

// Valley = where the two components' weighted densities cross, between the means.
fn valley(g: &Gmm) -> f32 {
    let (m0, v0, w0, m1, v1, w1) = *g;
    let (lo, hi) = (m0.min(m1), m0.max(m1));
    let steps = 200;
    let dx = (hi - lo) / steps as f32;
    let mut t = (lo + hi) / 2.0;
    let mut best = f32::INFINITY;
    let mut x = lo;
    for _ in 0..steps {
        let d = (w0 * gauss(x, m0, v0) - w1 * gauss(x, m1, v1)).abs();
        if d < best {
            best = d;
            t = x;
        }
        x += dx;
    }
    t
}

// BIC of a single Gaussian (2 params) vs the 2-component GMM (5 params) on xs.
// Lower BIC wins; if the 1-Gaussian wins, the distribution is unimodal => 1 speaker.
fn bic_1v2(xs: &[f32], g: &Gmm) -> (f32, f32) {
    let n = xs.len() as f32;
    let mean = xs.iter().sum::<f32>() / n;
    let var = (xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n).max(1e-4);
    let ll1: f64 = xs.iter().map(|&x| (gauss(x, mean, var).max(1e-12)).ln() as f64).sum();
    let (m0, v0, w0, m1, v1, w1) = *g;
    let ll2: f64 = xs
        .iter()
        .map(|&x| ((w0 * gauss(x, m0, v0) + w1 * gauss(x, m1, v1)).max(1e-12)).ln() as f64)
        .sum();
    let bic1 = (-2.0 * ll1 + 2.0 * (n as f64).ln()) as f32;
    let bic2 = (-2.0 * ll2 + 5.0 * (n as f64).ln()) as f32;
    (bic1, bic2)
}

// ---- average-linkage AHC, stop when best inter-cluster cosine < thr ----
fn est_ahc_valley(emb: &[Vec<f32>], thr: f32) -> usize {
    let n = emb.len();
    let mut clusters: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    // precompute pairwise sims
    let sim = |a: &[usize], b: &[usize]| -> f32 {
        let mut s = 0.0;
        for &i in a {
            for &j in b {
                s += cosine(&emb[i], &emb[j]);
            }
        }
        s / (a.len() * b.len()) as f32
    };
    loop {
        if clusters.len() <= 1 {
            break;
        }
        let mut best = (0usize, 1usize, f32::NEG_INFINITY);
        for i in 0..clusters.len() {
            for j in (i + 1)..clusters.len() {
                let s = sim(&clusters[i], &clusters[j]);
                if s > best.2 {
                    best = (i, j, s);
                }
            }
        }
        if best.2 < thr {
            break;
        }
        let (i, j, _) = best;
        let merged: Vec<usize> = clusters[i].iter().chain(clusters[j].iter()).copied().collect();
        clusters.remove(j);
        clusters[i] = merged;
    }
    clusters.len()
}

// ---- DP-means: new cluster when nearest centroid distance > lambda ----
fn est_dpmeans(emb: &[Vec<f32>], lambda: f32) -> usize {
    let dim = emb[0].len();
    let mut centers: Vec<Vec<f32>> = vec![emb[0].clone()];
    let mut labels = vec![0u32; emb.len()];
    for _ in 0..20 {
        // assignment (may open new clusters)
        for (i, e) in emb.iter().enumerate() {
            let mut bs = f32::NEG_INFINITY;
            let mut bl = 0u32;
            for (c, ctr) in centers.iter().enumerate() {
                let s = cosine(e, ctr);
                if s > bs {
                    bs = s;
                    bl = c as u32;
                }
            }
            if 1.0 - bs > lambda {
                centers.push(e.clone());
                labels[i] = (centers.len() - 1) as u32;
            } else {
                labels[i] = bl;
            }
        }
        // recompute centroids; drop empties
        let k = centers.len();
        let mut acc = vec![vec![0f32; dim]; k];
        let mut cnt = vec![0usize; k];
        for (i, e) in emb.iter().enumerate() {
            let c = labels[i] as usize;
            for (a, v) in acc[c].iter_mut().zip(e) {
                *a += v;
            }
            cnt[c] += 1;
        }
        let mut newc = vec![];
        for c in 0..k {
            if cnt[c] > 0 {
                let norm = acc[c].iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
                newc.push(acc[c].iter().map(|x| x / norm).collect());
            }
        }
        centers = newc;
    }
    centers.len()
}

// ---- GMM-BIC proxy: k-means inertia + k*ln(n) penalty, pick min ----
fn est_gmm_bic(emb: &[Vec<f32>]) -> usize {
    let n = emb.len() as f32;
    let mut best = (1usize, f32::INFINITY);
    for k in 1..=8usize {
        let labels = spherical_kmeans(emb, k);
        let rss = inertia(emb, &labels, k).max(1e-6);
        let bic = n * (rss / n).ln() + (k as f32) * n.ln();
        if bic < best.1 {
            best = (k, bic);
        }
    }
    best.0
}

// ---- eigengap on the normalized-Laplacian of the cosine affinity ----
fn est_eigengap(emb: &[Vec<f32>]) -> usize {
    let a = affinity(emb, 0); // 0 = no kNN binarization (dense)
    let ev = laplacian_eigenvalues(&a);
    eigengap_k(&ev)
}

// ---- NME auto-tuning: sweep kNN binarization p, minimize p / max-eigengap ----
fn est_nme(emb: &[Vec<f32>]) -> usize {
    let mut best = (2usize, f32::INFINITY, 1usize); // (p, ratio, k)
    for p in (2..=30).step_by(2) {
        let a = affinity(emb, p);
        let ev = laplacian_eigenvalues(&a);
        let (k, gap) = max_eigengap(&ev);
        let ratio = p as f32 / gap.max(1e-6);
        if ratio < best.1 {
            best = (p, ratio, k);
        }
    }
    best.2
}

// cosine affinity; if knn>0 keep only top-knn neighbors per row then symmetrize (max).
fn affinity(emb: &[Vec<f32>], knn: usize) -> Vec<Vec<f32>> {
    let n = emb.len();
    let mut a = vec![vec![0f32; n]; n];
    for i in 0..n {
        for j in 0..n {
            if i != j {
                a[i][j] = cosine(&emb[i], &emb[j]).max(0.0);
            }
        }
    }
    if knn > 0 && knn < n {
        let mut b = vec![vec![0f32; n]; n];
        for i in 0..n {
            let mut idx: Vec<usize> = (0..n).filter(|&j| j != i).collect();
            idx.sort_by(|&x, &y| a[i][y].partial_cmp(&a[i][x]).unwrap());
            for &j in idx.iter().take(knn) {
                b[i][j] = a[i][j];
            }
        }
        // symmetrize via max
        for i in 0..n {
            for j in (i + 1)..n {
                let m = b[i][j].max(b[j][i]);
                b[i][j] = m;
                b[j][i] = m;
            }
        }
        b
    } else {
        a
    }
}

// eigenvalues (ascending) of the symmetric normalized Laplacian L = I - D^-1/2 A D^-1/2.
fn laplacian_eigenvalues(a: &[Vec<f32>]) -> Vec<f32> {
    let n = a.len();
    let deg: Vec<f32> = a.iter().map(|r| r.iter().sum::<f32>().max(1e-9)).collect();
    let mut l = vec![vec![0f64; n]; n];
    for i in 0..n {
        for j in 0..n {
            let norm = (a[i][j] as f64) / (deg[i] as f64).sqrt() / (deg[j] as f64).sqrt();
            l[i][j] = if i == j { 1.0 - norm } else { -norm };
        }
    }
    jacobi_eigenvalues(l)
}

fn eigengap_k(ev: &[f32]) -> usize {
    let lim = ev.len().min(12);
    let mut best = (1usize, f32::NEG_INFINITY);
    for i in 1..lim {
        let gap = ev[i] - ev[i - 1];
        if gap > best.1 {
            best = (i, gap);
        }
    }
    best.0
}

fn max_eigengap(ev: &[f32]) -> (usize, f32) {
    let lim = ev.len().min(12);
    let mut best = (1usize, f32::NEG_INFINITY);
    for i in 1..lim {
        let gap = ev[i] - ev[i - 1];
        if gap > best.1 {
            best = (i, gap);
        }
    }
    best
}

// Jacobi eigenvalue algorithm for a symmetric matrix; returns eigenvalues ascending.
fn jacobi_eigenvalues(mut a: Vec<Vec<f64>>) -> Vec<f32> {
    let n = a.len();
    for _ in 0..100 {
        // find largest off-diagonal
        let (mut p, mut q, mut off) = (0, 1, 0.0);
        for i in 0..n {
            for j in (i + 1)..n {
                if a[i][j].abs() > off {
                    off = a[i][j].abs();
                    p = i;
                    q = j;
                }
            }
        }
        if off < 1e-7 {
            break;
        }
        let app = a[p][p];
        let aqq = a[q][q];
        let apq = a[p][q];
        let phi = 0.5 * (2.0 * apq).atan2(aqq - app);
        let (c, s) = (phi.cos(), phi.sin());
        for k in 0..n {
            let akp = a[k][p];
            let akq = a[k][q];
            a[k][p] = c * akp - s * akq;
            a[k][q] = s * akp + c * akq;
        }
        for k in 0..n {
            let apk = a[p][k];
            let aqk = a[q][k];
            a[p][k] = c * apk - s * aqk;
            a[q][k] = s * apk + c * aqk;
        }
        let _ = (app, aqq);
    }
    let mut ev: Vec<f32> = (0..n).map(|i| a[i][i] as f32).collect();
    ev.sort_by(|x, y| x.partial_cmp(y).unwrap());
    ev
}

fn report(label: &str, emb: &[Vec<f32>], known_k: usize) {
    println!("\n================ {label} ================");
    println!("n embeddings = {}", emb.len());
    if emb.len() < 4 {
        println!("(too few to analyze)");
        return;
    }

    // Pairwise cosine distribution (sample up to ~20k pairs).
    let mut pcs: Vec<f32> = Vec::new();
    let step = ((emb.len() * emb.len()) / 20_000).max(1);
    let mut k = 0usize;
    for i in 0..emb.len() {
        for j in (i + 1)..emb.len() {
            if k % step == 0 {
                pcs.push(cosine(&emb[i], &emb[j]));
            }
            k += 1;
        }
    }
    pcs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f32| pcs[((p * (pcs.len() - 1) as f32) as usize).min(pcs.len() - 1)];
    println!(
        "pairwise cosine  p05={:.3} p25={:.3} p50={:.3} p75={:.3} p95={:.3}  (low tail => some pairs are different speakers)",
        pct(0.05), pct(0.25), pct(0.50), pct(0.75), pct(0.95)
    );
    // crude bimodality: histogram in 0.1 bins over [-0.2,1.0]
    print!("hist ");
    for lo in 0..12 {
        let l = -0.2 + lo as f32 * 0.1;
        let h = l + 0.1;
        let c = pcs.iter().filter(|&&x| x >= l && x < h).count();
        print!("[{:.1}:{}] ", l, c);
    }
    println!();

    // Spherical k-means k=1..6, mean silhouette.
    println!("k   silhouette   cluster_sizes");
    for kk in 1..=6usize {
        let labels = spherical_kmeans(emb, kk);
        let sil = silhouette(emb, &labels, kk);
        let sizes = sizes(&labels, kk);
        let mark = if kk == known_k { "  <- known_k" } else { "" };
        println!("{kk}   {sil:>9.3}   {sizes:?}{mark}");
    }

    // What cluster_speakers() returns.
    let def = cluster_speakers(emb, None);
    let ndef = uniq(&def);
    println!(
        "shipping cluster_speakers(auto): {} clusters sizes {:?}",
        ndef,
        sizes(&def, ndef)
    );
    if known_k > 0 {
        let forced = cluster_speakers(emb, Some(known_k));
        let nf = uniq(&forced);
        println!(
            "shipping cluster_speakers(known={known_k}): {} clusters sizes {:?}",
            nf,
            sizes(&forced, nf)
        );
    }
}

// k-means++ init (deterministic LCG) + spherical k-means, best of several restarts.
fn spherical_kmeans(emb: &[Vec<f32>], k: usize) -> Vec<u32> {
    if k <= 1 {
        return vec![0; emb.len()];
    }
    let mut best_labels = vec![0u32; emb.len()];
    let mut best_inertia = f32::INFINITY;
    for restart in 0..6u64 {
        let labels = kmeans_once(emb, k, 0x9E3779B9u64.wrapping_add(restart.wrapping_mul(0x1234567)));
        let inertia = inertia(emb, &labels, k);
        if inertia < best_inertia {
            best_inertia = inertia;
            best_labels = labels;
        }
    }
    best_labels
}

fn inertia(emb: &[Vec<f32>], labels: &[u32], k: usize) -> f32 {
    let dim = emb[0].len();
    let mut centers = vec![vec![0f32; dim]; k];
    let mut cnt = vec![0usize; k];
    for (i, e) in emb.iter().enumerate() {
        let c = labels[i] as usize;
        for (a, v) in centers[c].iter_mut().zip(e) {
            *a += v;
        }
        cnt[c] += 1;
    }
    for c in 0..k {
        let norm = centers[c].iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
        for a in centers[c].iter_mut() {
            *a /= norm;
        }
    }
    let mut tot = 0f32;
    for (i, e) in emb.iter().enumerate() {
        tot += 1.0 - cosine(e, &centers[labels[i] as usize]);
    }
    tot
}

fn kmeans_once(emb: &[Vec<f32>], k: usize, mut seed: u64) -> Vec<u32> {
    let n = emb.len();
    let dim = emb[0].len();
    let mut rng = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 33) as f32 / (1u64 << 31) as f32
    };
    // k-means++ : first center random, rest by D^2 (cosine-distance) weighting.
    let mut centers: Vec<Vec<f32>> = vec![emb[(rng() * n as f32) as usize % n].clone()];
    while centers.len() < k {
        let d2: Vec<f32> = emb
            .iter()
            .map(|e| {
                let nearest = centers
                    .iter()
                    .map(|c| 1.0 - cosine(e, c))
                    .fold(f32::INFINITY, f32::min);
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
            let mut cnt = 0usize;
            for (i, e) in emb.iter().enumerate() {
                if labels[i] as usize == c {
                    for (a, v) in acc.iter_mut().zip(e) {
                        *a += v;
                    }
                    cnt += 1;
                }
            }
            if cnt > 0 {
                let norm = acc.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
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

fn silhouette(emb: &[Vec<f32>], labels: &[u32], k: usize) -> f32 {
    if k <= 1 {
        return 0.0;
    }
    let n = emb.len();
    // cosine distance = 1 - cos
    let mut total = 0f32;
    let mut count = 0usize;
    // subsample for O(n^2) safety on big n
    let stride = (n / 600).max(1);
    for i in (0..n).step_by(stride) {
        let li = labels[i];
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
        let a = if cnts[li as usize] > 0 {
            (sums[li as usize] / cnts[li as usize] as f64) as f32
        } else {
            0.0
        };
        let b = (0..k)
            .filter(|&c| c as u32 != li && cnts[c] > 0)
            .map(|c| (sums[c] / cnts[c] as f64) as f32)
            .fold(f32::INFINITY, f32::min);
        if b.is_finite() {
            let s = (b - a) / a.max(b).max(1e-9);
            total += s;
            count += 1;
        }
    }
    if count > 0 {
        total / count as f32
    } else {
        0.0
    }
}

fn sizes(labels: &[u32], k: usize) -> Vec<usize> {
    let mut v = vec![0usize; k.max(uniq(labels))];
    for &l in labels {
        if (l as usize) < v.len() {
            v[l as usize] += 1;
        }
    }
    v
}

fn uniq(labels: &[u32]) -> usize {
    let mut m = 0u32;
    for &l in labels {
        m = m.max(l);
    }
    (m as usize) + 1
}
