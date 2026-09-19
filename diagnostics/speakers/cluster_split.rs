// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Internal structure of one speakrs cluster. Reads a turns JSONL dump
//! (`dump_turns`), embeds every turn window of the requested label from the
//! session's concatenated system track, and prints the pairwise-cosine matrix
//! plus a 2-means split of those turns.
//!
//! Usage:
//!   DAISY_VOICEPRINT_DIR=$PWD/models/voiceprints \
//!   cargo run --release --example cluster_split -p voiceprints -- <session_dir> <turns.jsonl> <LABEL> [min_secs]
use std::path::PathBuf;
use voiceprints::{cosine, Encoder};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(&a[1]);
    let jsonl = std::fs::read_to_string(&a[2]).expect("turns jsonl");
    let want = a[3].clone();
    let min_secs: f64 = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(1.5);

    let mut chunk_dirs: Vec<_> = std::fs::read_dir(dir.join("chunks"))
        .expect("chunks/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    chunk_dirs.sort();
    let mut audio: Vec<f32> = Vec::new();
    for cd in &chunk_dirs {
        let w = cd.join("system.wav");
        if w.is_file() {
            if let Ok((mut s, _)) = transcript::wav_io::read_wav_mono_f32(&w) {
                audio.append(&mut s);
            }
        }
    }
    let sr = 16_000usize;

    let mut turns: Vec<(f64, f64)> = Vec::new();
    // LABEL may be "@path": a file of "start end" seconds, one window per line,
    // to re-test an arbitrary subset (a side produced by an earlier split).
    if let Some(path) = want.strip_prefix('@') {
        for line in std::fs::read_to_string(path).expect("windows file").lines() {
            let mut it = line.split_whitespace();
            if let (Some(a), Some(b)) = (it.next(), it.next()) {
                turns.push((a.parse().unwrap(), b.parse().unwrap()));
            }
        }
    }
    for line in jsonl.lines().filter(|l| l.starts_with('{')) {
        if want.starts_with('@') {
            break;
        }
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        if v["speaker"].as_str() != Some(want.as_str()) {
            continue;
        }
        let (s, e) = (v["start"].as_f64().unwrap(), v["end"].as_f64().unwrap());
        if e - s >= min_secs {
            turns.push((s, e));
        }
    }
    turns.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
    // SUBWIN_S: chop each window into pieces of at most this many seconds, so a
    // single long window still carries a spread of its own.
    if let Ok(sub) = std::env::var("SUBWIN_S").unwrap_or_default().parse::<f64>() {
        if sub >= 1.0 {
            let mut chopped: Vec<(f64, f64)> = Vec::new();
            for (s, e) in &turns {
                let n = (((e - s) / sub).floor() as usize).max(1);
                let step = (e - s) / n as f64;
                for k in 0..n {
                    let (a, b) = (s + k as f64 * step, s + (k + 1) as f64 * step);
                    if b - a >= 1.0 {
                        chopped.push((a, b));
                    }
                }
            }
            turns = chopped;
        }
    }
    eprintln!("{want}: {} turn(s) >= {min_secs}s", turns.len());

    let mut enc = Encoder::load().expect("encoder (set DAISY_VOICEPRINT_DIR)");
    let mut embs: Vec<(f64, f64, Vec<f32>)> = Vec::new();
    for (s, e) in &turns {
        let a0 = (s * sr as f64) as usize;
        let a1 = ((e * sr as f64) as usize).min(audio.len());
        if a1 <= a0 {
            continue;
        }
        let pcm: Vec<i16> =
            audio[a0..a1].iter().map(|v| (v * 32768.0).clamp(-32768.0, 32767.0) as i16).collect();
        if let Ok(v) = enc.encode_pcm(&pcm) {
            embs.push((*s, *e, v));
        }
    }
    println!("embedded {} window(s)", embs.len());
    println!("\nidx  start    end");
    for (i, (s, e, _)) in embs.iter().enumerate() {
        println!("{i:3}  {s:7.1} {e:7.1}");
    }
    println!("\npairwise cosine");
    print!("     ");
    for i in 0..embs.len() {
        print!("{i:6}");
    }
    println!();
    for (i, (_, _, vi)) in embs.iter().enumerate() {
        print!("{i:4} ");
        for (_, _, vj) in embs.iter() {
            print!("{:6.2}", cosine(vi, vj));
        }
        println!();
    }
    // 2-means on the cluster's own turns: seed with the least-similar pair.
    let (mut a, mut b) = (0usize, 0usize);
    let mut worst = 2.0f32;
    for i in 0..embs.len() {
        for j in i + 1..embs.len() {
            let c = cosine(&embs[i].2, &embs[j].2);
            if c < worst {
                worst = c;
                a = i;
                b = j;
            }
        }
    }
    println!("\nleast-similar pair: {a} vs {b} = {worst:.3}");
    let (mut ca, mut cb) = (embs[a].2.clone(), embs[b].2.clone());
    for _ in 0..10 {
        let (mut sa, mut sb) = (vec![0.0f32; ca.len()], vec![0.0f32; cb.len()]);
        let (mut na, mut nb) = (0, 0);
        for (_, _, v) in &embs {
            if cosine(v, &ca) >= cosine(v, &cb) {
                for (d, s) in sa.iter_mut().zip(v) {
                    *d += s;
                }
                na += 1;
            } else {
                for (d, s) in sb.iter_mut().zip(v) {
                    *d += s;
                }
                nb += 1;
            }
        }
        let norm = |v: &mut Vec<f32>| {
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
            v.iter_mut().for_each(|x| *x /= n);
        };
        if na > 0 {
            ca = sa;
            norm(&mut ca);
        }
        if nb > 0 {
            cb = sb;
            norm(&mut cb);
        }
    }
    println!("\n2-means split (A / B), centroid cosine = {:.3}", cosine(&ca, &cb));
    let mut sides: Vec<char> = Vec::new();
    for (s, e, v) in &embs {
        let (da, db) = (cosine(v, &ca), cosine(v, &cb));
        let side = if da >= db { 'A' } else { 'B' };
        sides.push(side);
        println!("  {:7.1}-{:7.1}  {}  A={:.3} B={:.3}", s, e, side, da, db);
    }
    // Per-side size and internal coherence (mean pairwise cosine within the side).
    for want in ['A', 'B'] {
        let idx: Vec<usize> =
            (0..embs.len()).filter(|i| sides[*i] == want).collect();
        let secs: f64 = idx.iter().map(|i| embs[*i].1 - embs[*i].0).sum();
        let mut sum = 0.0f32;
        let mut n = 0u32;
        for (a, i) in idx.iter().enumerate() {
            for j in idx.iter().skip(a + 1) {
                sum += cosine(&embs[*i].2, &embs[*j].2);
                n += 1;
            }
        }
        let coh = if n > 0 { sum / n as f32 } else { 1.0 };
        println!(
            "SUMMARY side={want} turns={} secs={secs:.1} internal_cos={coh:.3}",
            idx.len()
        );
    }
    // Same predicate the shipped same-voice merge uses, applied to the two
    // sides of this cluster: across >= (within_a + within_b)/2 - 0.15 means
    // "one voice" (do not split).
    let mean_within = |want: char| {
        let idx: Vec<usize> = (0..embs.len()).filter(|i| sides[*i] == want).collect();
        let (mut sum, mut n) = (0.0f32, 0u32);
        for (a, i) in idx.iter().enumerate() {
            for j in idx.iter().skip(a + 1) {
                sum += cosine(&embs[*i].2, &embs[*j].2);
                n += 1;
            }
        }
        if n > 0 { Some(sum / n as f32) } else { None }
    };
    let (mut acc, mut nacc) = (0.0f32, 0u32);
    for i in 0..embs.len() {
        for j in 0..embs.len() {
            if sides[i] == 'A' && sides[j] == 'B' {
                acc += cosine(&embs[i].2, &embs[j].2);
                nacc += 1;
            }
        }
    }
    let across = if nacc > 0 { acc / nacc as f32 } else { 1.0 };
    let (wa, wb) = (mean_within('A'), mean_within('B'));
    let one_voice = match (wa, wb) {
        (Some(a), Some(b)) => across >= (a + b) / 2.0 - 0.15,
        _ => true,
    };
    println!(
        "SUMMARY centroid_cos={:.3} across={across:.3} within_a={:?} within_b={:?} one_voice={one_voice} label={want}",
        cosine(&ca, &cb),
        wa.map(|v| (v * 1000.0).round() / 1000.0),
        wb.map(|v| (v * 1000.0).round() / 1000.0)
    );
}
