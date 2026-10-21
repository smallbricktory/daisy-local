// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Batch-decode 16 kHz mono wavs with the Nemotron transcriber and time them.
//!
//!   cargo run --release -p providers-local --example nemotron_decode -- \
//!       MODEL_DIR OUT_DIR [--threads N] WAV...
//!
//! Writes OUT_DIR/<wav stem>.tsv (start_ms, end_ms, text per segment) and
//! prints one JSON line per wav: audio_s, decode_s, rtf, segments.
//! The first wav's decode_s includes the model load; read rtf from the second
//! wav onward.

use providers_http::Transcriber;
use providers_local::nemotron::batch::NemotronLocalTranscriber;
use providers_local::nemotron::NemotronOpts;
use std::io::Write;
use std::path::{Path, PathBuf};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let threads = args.iter().position(|a| a == "--threads").map(|i| {
        let n: usize = args.get(i + 1).and_then(|v| v.parse().ok()).expect("--threads N");
        args.drain(i..i + 2);
        n
    });
    if args.len() < 3 {
        eprintln!("usage: nemotron_decode MODEL_DIR OUT_DIR [--threads N] WAV...");
        std::process::exit(2);
    }
    let opts = threads.map_or_else(NemotronOpts::background, |t| NemotronOpts { threads: t, background: false });
    let t = NemotronLocalTranscriber::with_opts(PathBuf::from(&args[0]), "nemotron (decode)", opts);
    let out = Path::new(&args[1]);
    std::fs::create_dir_all(out).expect("out dir");
    for wav in &args[2..] {
        let wav = Path::new(wav);
        let audio_s = providers_local::wav::decode_wav_16k_mono_f32(wav).expect("wav").len() as f64 / 16_000.0;
        let t0 = std::time::Instant::now();
        let segs = t.transcribe(wav, None).expect("transcribe");
        let decode_s = t0.elapsed().as_secs_f64();
        let stem = wav.file_stem().and_then(|s| s.to_str()).expect("wav stem");
        let mut f = std::fs::File::create(out.join(format!("{stem}.tsv"))).expect("tsv");
        for s in &segs {
            writeln!(f, "{}\t{}\t{}", s.start_ms, s.end_ms, s.text).unwrap();
        }
        println!(
            "{{\"wav\":\"{stem}\",\"audio_s\":{audio_s:.1},\"decode_s\":{decode_s:.2},\"rtf\":{:.4},\"segments\":{}}}",
            decode_s / audio_s,
            segs.len()
        );
    }
}
