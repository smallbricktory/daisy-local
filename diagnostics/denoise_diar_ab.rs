//! Does DFN3 denoise actually change diarization? For one session, embed the
//! SAME mic-track ASR segments twice — once from the denoised `mic_dn.wav`,
//! once from the echo-cancelled `mic_aec.wav` (the fallback when denoise is
//! off) — with the same WeSpeaker encoder, then compare:
//!   * per-segment cosine(dn, aec): how far denoise moves each voiceprint
//!     (≈1.0 ⇒ denoise barely changes the embedding ⇒ no diarization value)
//!   * cluster_speakers() count on dn-embeddings vs aec-embeddings (auto + a
//!     forced known_k): does the denoise change the actual speaker split?
//!
//! Usage:
//!   DAISY_VOICEPRINT_DIR=$PWD/models/voiceprints \
//!   cargo run --release --example denoise_diar_ab -p voiceprints -- <session_dir> [known_k]

use serde_json::Value;
use std::path::PathBuf;
use voiceprints::{cluster_speakers, cosine, read_pcm_window, Encoder};

const MIN_MS: u32 = 1_200;
const EMBED_CAP_MS: u32 = 8_000;

fn rel_for<'a>(manifest: &'a Value, idx: i64, key: &str) -> Option<&'a str> {
    manifest["chunks"].as_array()?.iter().find_map(|c| {
        (c["index"].as_i64() == Some(idx)).then(|| c[key].as_str()).flatten()
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: denoise_diar_ab <session_dir> [known_k=0]");
        std::process::exit(2);
    }
    let dir = PathBuf::from(&args[1]);
    let known_k: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

    let manifest: Value =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).expect("manifest.json"))
            .expect("parse manifest");
    let tpath = {
        let d = dir.join("transcript.dedup.json");
        if d.is_file() { d } else { dir.join("transcript.json") }
    };
    let transcript: Value =
        serde_json::from_slice(&std::fs::read(&tpath).expect("transcript")).expect("parse transcript");

    let mut enc = Encoder::load().expect("load encoder (set DAISY_VOICEPRINT_DIR)");

    // Pair up: for each qualifying mic-track segment, embed from mic_dn and mic_aec.
    let mut emb_dn: Vec<Vec<f32>> = Vec::new();
    let mut emb_aec: Vec<Vec<f32>> = Vec::new();
    let mut pair_cos: Vec<f32> = Vec::new();
    let (mut seg_total, mut seg_missing) = (0usize, 0usize);

    for ch in transcript["chunks"].as_array().unwrap_or(&vec![]).iter() {
        let cidx = ch["chunk_index"].as_i64().unwrap_or(-1);
        // Normalize Windows backslashes — manifests written on Windows record
        // mic_dn as `chunks\NNNN\mic_dn.wav`, which doesn't resolve on Linux.
        let dn_path = rel_for(&manifest, cidx, "mic_dn_wav_relative").map(|r| dir.join(r.replace('\\', "/")));
        let aec_path = rel_for(&manifest, cidx, "mic_aec_wav_relative").map(|r| dir.join(r.replace('\\', "/")));
        let (Some(dn), Some(aec)) = (dn_path, aec_path) else { continue };
        if !dn.is_file() || !aec.is_file() {
            continue;
        }
        for tr in ch["tracks"].as_array().unwrap_or(&vec![]).iter() {
            let t = tr["track"].as_str().unwrap_or("");
            if t != "mic" && t != "mic_aec" {
                continue;
            }
            for s in tr["segments"].as_array().unwrap_or(&vec![]).iter() {
                let a = s["start_ms"].as_u64().unwrap_or(0) as u32;
                let b = s["end_ms"].as_u64().unwrap_or(0) as u32;
                if b <= a || b - a < MIN_MS {
                    continue;
                }
                seg_total += 1;
                let cap = (b - a).min(EMBED_CAP_MS);
                let dn_pcm = read_pcm_window(&dn, a, a + cap, cap);
                let aec_pcm = read_pcm_window(&aec, a, a + cap, cap);
                let (Ok(dp), Ok(ap)) = (dn_pcm, aec_pcm) else { seg_missing += 1; continue };
                if dp.len() < 8000 || ap.len() < 8000 {
                    seg_missing += 1;
                    continue;
                }
                let (Ok(vd), Ok(va)) = (enc.encode_pcm(&dp), enc.encode_pcm(&ap)) else {
                    seg_missing += 1;
                    continue;
                };
                pair_cos.push(cosine(&vd, &va));
                emb_dn.push(vd);
                emb_aec.push(va);
            }
        }
    }

    if pair_cos.is_empty() {
        eprintln!("no paired mic segments embedded (segments seen: {seg_total}, skipped: {seg_missing}). Wrong track? in-person mic?");
        std::process::exit(1);
    }

    let n = pair_cos.len();
    let mut sorted = pair_cos.clone();
    sorted.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let mean = pair_cos.iter().sum::<f32>() / n as f32;
    let median = sorted[n / 2];
    let min = sorted[0];
    let p10 = sorted[n / 10];

    println!("session: {}", dir.display());
    println!("paired mic segments embedded: {n} (skipped {seg_missing} of {seg_total})");
    println!("per-segment cosine(mic_dn, mic_aec) — how much denoise moves the voiceprint:");
    println!("  mean={mean:.4}  median={median:.4}  p10={p10:.4}  min={min:.4}");
    println!("  (≈1.000 ⇒ denoise barely changes the embedding ⇒ little diarization value)");

    let nclusters = |labels: &[u32]| labels.iter().collect::<std::collections::BTreeSet<_>>().len();
    let dn_auto = cluster_speakers(&emb_dn, None);
    let aec_auto = cluster_speakers(&emb_aec, None);
    println!(
        "cluster_speakers(auto):   mic_dn={} (sil={:.3})  mic_aec={} (sil={:.3})",
        nclusters(&dn_auto), mean_silhouette(&emb_dn, &dn_auto),
        nclusters(&aec_auto), mean_silhouette(&emb_aec, &aec_auto)
    );
    let k = if known_k > 0 { Some(known_k) } else { None };
    let dn = cluster_speakers(&emb_dn, k);
    let aec = cluster_speakers(&emb_aec, k);
    println!(
        "cluster_speakers(known={known_k}): mic_dn={} (sil={:.3})  mic_aec={} (sil={:.3})",
        nclusters(&dn), mean_silhouette(&emb_dn, &dn),
        nclusters(&aec), mean_silhouette(&emb_aec, &aec)
    );
    // Assignment agreement: do dn and aec group the same segment PAIRS together?
    // (label ids aren't comparable, so compare same-cluster relationships.)
    let (mut same, mut tot) = (0usize, 0usize);
    for i in 0..dn.len() {
        for j in (i + 1)..dn.len() {
            tot += 1;
            if (dn[i] == dn[j]) == (aec[i] == aec[j]) {
                same += 1;
            }
        }
    }
    println!(
        "assignment agreement (known={known_k}): {:.1}% of segment-pairs grouped the same way",
        100.0 * same as f32 / tot.max(1) as f32
    );
    println!("higher silhouette = cleaner speaker separation. dn>aec ⇒ denoise helps; dn≈aec ⇒ no value; dn<aec ⇒ denoise hurts.");
}

/// Mean silhouette over all points using cosine distance (1 - cosine). For each
/// point: a = mean distance to its own cluster, b = min mean distance to any
/// other cluster; silhouette = (b - a) / max(a, b). Singletons score 0.
fn mean_silhouette(emb: &[Vec<f32>], labels: &[u32]) -> f32 {
    use std::collections::BTreeSet;
    let clusters: Vec<u32> = labels.iter().copied().collect::<BTreeSet<_>>().into_iter().collect();
    if clusters.len() < 2 {
        return 0.0;
    }
    let dist = |a: &[f32], b: &[f32]| 1.0 - cosine(a, b);
    let mut total = 0.0f32;
    for i in 0..emb.len() {
        let mut sums: std::collections::BTreeMap<u32, (f32, usize)> = std::collections::BTreeMap::new();
        for j in 0..emb.len() {
            if i == j {
                continue;
            }
            let e = sums.entry(labels[j]).or_insert((0.0, 0));
            e.0 += dist(&emb[i], &emb[j]);
            e.1 += 1;
        }
        let a = sums.get(&labels[i]).map(|&(s, c)| if c > 0 { s / c as f32 } else { 0.0 }).unwrap_or(0.0);
        let b = sums
            .iter()
            .filter(|(&cl, _)| cl != labels[i])
            .map(|(_, &(s, c))| if c > 0 { s / c as f32 } else { f32::INFINITY })
            .fold(f32::INFINITY, f32::min);
        if b.is_finite() {
            total += (b - a) / a.max(b).max(1e-6);
        }
    }
    total / emb.len() as f32
}
