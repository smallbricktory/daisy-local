// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Diarization D: turns from the segmentation model alone, then (optionally)
//! one WeSpeaker embedding per turn and spherical k-means over them.
//!
//!   cargo run --release -p voiceprints --example diag_d -- <wav> [k]
//!
//! `<wav>` is 16 kHz mono i16. Turns go to stdout as JSONL
//! `{"start","end","speaker"}` (`"T"` without `k`, `"D<n>"` with it; `k = 0`
//! estimates the count); overlap spans go to stderr as `OVERLAP` lines and
//! stage timings as `STAGE` lines.
//!
//! Env: DAISY_VOICEPRINT_DIR (models/voiceprints holding
//! `segmentation-3.0.onnx` and `model.onnx`).
use std::io::Write;
use std::time::Instant;

use voiceprints::segmentation::{SegmentationModel, SAMPLE_RATE};
use voiceprints::turns::{label_short_turns, TurnBuilder, MIN_EMBED_SECONDS};

const STEP_S: f32 = 1.0;
const EMBED_CAP_S: f64 = 8.0;
const SILENCE_FLOOR: f32 = 0.005;

fn rms(w: &[i16]) -> f32 {
    if w.is_empty() {
        return 0.0;
    }
    ((w.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / w.len() as f64).sqrt() / 32768.0) as f32
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let wav = a.get(1).expect("usage: diag_d <wav> [k]");
    let k: Option<usize> = a.get(2).map(|v| v.parse().expect("k"));

    let mut r = hound::WavReader::open(wav).expect("open wav");
    assert_eq!(r.spec().sample_rate as usize, SAMPLE_RATE, "sample rate");
    assert_eq!(r.spec().channels, 1, "mono");
    let pcm: Vec<i16> = r.samples::<i16>().map(|s| s.expect("sample")).collect();
    let audio: Vec<f32> = pcm.iter().map(|s| *s as f32 / 32768.0).collect();
    let audio_s = audio.len() as f64 / SAMPLE_RATE as f64;

    let t = Instant::now();
    let mut seg = SegmentationModel::load_background().expect("segmentation model");
    eprintln!("STAGE seg_load wall={:.2}s", t.elapsed().as_secs_f64());
    let t = Instant::now();
    let mut builder = TurnBuilder::new(STEP_S as f64);
    let windows = seg
        .run_windows(&audio, STEP_S, |i, p| builder.push_window(i, p))
        .expect("segmentation");
    let turns = builder.finish(audio_s);
    let wall = t.elapsed().as_secs_f64();
    eprintln!(
        "STAGE seg_turns wall={wall:.2}s audio={audio_s:.0}s rtf={:.1}x windows={windows} turns={} overlaps={}",
        audio_s / wall.max(1e-9),
        turns.spans.len(),
        turns.overlap_spans.len()
    );
    for (s, e) in &turns.overlap_spans {
        eprintln!("OVERLAP {s:.3} {e:.3}");
    }

    let out = std::io::stdout();
    let mut out = out.lock();
    let Some(k) = k else {
        for (s, e) in &turns.spans {
            writeln!(out, "{{\"start\":{s:.3},\"end\":{e:.3},\"speaker\":\"T\"}}").unwrap();
        }
        return;
    };

    let t = Instant::now();
    let mut enc = voiceprints::Encoder::load().expect("encoder");
    let mut embs: Vec<Vec<f32>> = Vec::new();
    let mut slot: Vec<Option<usize>> = Vec::with_capacity(turns.spans.len());
    for (s, e) in &turns.spans {
        let a = ((s * SAMPLE_RATE as f64).round() as usize).min(pcm.len());
        let b = ((e.min(s + EMBED_CAP_S) * SAMPLE_RATE as f64).round() as usize).min(pcm.len());
        let w = &pcm[a..b.max(a)];
        let embedded = if e - s < MIN_EMBED_SECONDS || w.len() < SAMPLE_RATE / 2 || rms(w) < SILENCE_FLOOR {
            None
        } else {
            enc.encode_pcm(w).ok()
        };
        slot.push(embedded.map(|v| {
            embs.push(v);
            embs.len() - 1
        }));
    }
    let cid = voiceprints::cluster_speakers(&embs, (k > 0).then_some(k));
    eprintln!(
        "STAGE embed_cluster wall={:.2}s embedded={} of {} k_out={}",
        t.elapsed().as_secs_f64(),
        embs.len(),
        turns.spans.len(),
        cid.iter().max().map_or(0, |m| m + 1)
    );
    let own: Vec<Option<u32>> = slot.iter().map(|s| s.map(|i| cid[i])).collect();
    for ((s, e), id) in turns.spans.iter().zip(label_short_turns(&turns.spans, &own)) {
        if let Some(id) = id {
            writeln!(out, "{{\"start\":{s:.3},\"end\":{e:.3},\"speaker\":\"D{id}\"}}").unwrap();
        }
    }
}
