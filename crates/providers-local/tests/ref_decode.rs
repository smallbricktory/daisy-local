// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Offline reference decoder: full-context whisper decode of a session WAV,
//! one segment per line, for building eval-corpus reference transcripts.
//!
//!   DAISY_REF_WAV=…/chunks/0000/mic.wav \
//!   DAISY_REF_WHISPER=…/ggml-large-v3-turbo.bin \
//!   [DAISY_REF_OUT=…/ref.tsv] \
//!   cargo test -p providers-local --test ref_decode --release -- --ignored --nocapture

use providers_local::WhisperLocalTranscriber;
use std::io::Write;
use std::path::PathBuf;

/// Voiced regions by frame energy: 100 ms frame RMS, threshold above the
/// noise floor, regions closer than 2 s merged, 300 ms pad. Sparse tracks
/// (system audio) decoded whole-file make whisper hallucinate loops on the
/// silence; decoding only voiced regions avoids that.
fn voiced_regions(samples: &[f32]) -> Vec<(usize, usize)> {
    const FRAME: usize = 1_600;
    let rms: Vec<f32> = samples
        .chunks(FRAME)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let mut sorted = rms.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = sorted[sorted.len() / 5];
    let thresh = (floor * 4.0).max(0.004);

    let mut regions: Vec<(usize, usize)> = Vec::new();
    for (i, &e) in rms.iter().enumerate() {
        if e < thresh {
            continue;
        }
        let (s, e_) = (i * FRAME, ((i + 1) * FRAME).min(samples.len()));
        match regions.last_mut() {
            Some(last) if s <= last.1 + 2 * 16_000 => last.1 = e_,
            _ => regions.push((s, e_)),
        }
    }
    let pad = 4_800;
    for r in &mut regions {
        r.0 = r.0.saturating_sub(pad);
        r.1 = (r.1 + pad).min(samples.len());
    }
    regions
}

#[test]
#[ignore]
fn ref_decode() {
    let wav = PathBuf::from(std::env::var("DAISY_REF_WAV").expect("DAISY_REF_WAV"));
    let model = PathBuf::from(std::env::var("DAISY_REF_WHISPER").expect("DAISY_REF_WHISPER"));
    let gate = std::env::var("DAISY_REF_GATE").is_ok();

    let mut r = hound::WavReader::open(&wav).unwrap_or_else(|e| panic!("open {wav:?}: {e}"));
    assert_eq!(r.spec().sample_rate, 16_000);
    assert_eq!(r.spec().channels, 1);
    let samples: Vec<f32> =
        r.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect();
    let dur_s = samples.len() as f64 / 16_000.0;
    println!("ref_decode: {wav:?} — {dur_s:.1}s (gate={gate})");

    let threads = std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(4);
    let t = WhisperLocalTranscriber::with_threads(&model, threads).expect("load model");
    let t0 = std::time::Instant::now();
    let mut tokens = Vec::new();
    if gate {
        let regions = voiced_regions(&samples);
        let voiced: f64 =
            regions.iter().map(|(s, e)| (e - s) as f64).sum::<f64>() / 16_000.0;
        println!("gated: {} region(s), {voiced:.1}s voiced of {dur_s:.1}s", regions.len());
        for (s, e) in regions {
            let off_ms = (s / 16) as i64;
            for mut tk in t.transcribe_samples(&samples[s..e]).expect("decode") {
                tk.start_ms += off_ms;
                tk.end_ms += off_ms;
                tokens.push(tk);
            }
        }
    } else {
        tokens = t.transcribe_samples(&samples).expect("decode");
    }
    let took = t0.elapsed().as_secs_f64();
    println!("decoded in {took:.1}s ({:.2}x realtime)", dur_s / took);

    let mut out = String::new();
    for tk in &tokens {
        out.push_str(&format!("{}\t{}\t{}\n", tk.start_ms, tk.end_ms, tk.text));
    }
    print!("{out}");
    if let Ok(path) = std::env::var("DAISY_REF_OUT") {
        std::fs::File::create(&path).unwrap().write_all(out.as_bytes()).unwrap();
        println!("wrote {path}");
    }
}
