//! Benchmark local Whisper transcription on this machine.
//!
//! Usage:
//!   cargo run --release --example bench -p providers-local -- \
//!     <wav-path> [<ggml-base.en.bin-path>]
//!
//! The WAV must be 16 kHz mono. If no model path is given, the program
//! downloads ggml-base.en.bin into ./bench-cache/.
//!
//! Reports wall-clock transcription time vs audio duration → realtime factor.

use std::path::PathBuf;
use std::time::Instant;

use providers_http::Transcriber;
use providers_local::{download_ggml_model, WhisperLocalTranscriber};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let wav: PathBuf = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: bench <wav> [<model>]"))?
        .into();
    let model: PathBuf = match args.next() {
        Some(p) => p.into(),
        None => {
            let cache = std::path::PathBuf::from("bench-cache");
            std::fs::create_dir_all(&cache)?;
            eprintln!("downloading ggml-base.en.bin into {} (one-time)…", cache.display());
            download_ggml_model("base.en", &cache)?
        }
    };

    // Read the WAV header to report audio duration.
    let reader = hound::WavReader::open(&wav)?;
    let spec = reader.spec();
    let samples = reader.duration() as u64;
    let audio_secs = samples as f64 / spec.sample_rate as f64;

    eprintln!("audio: {}, {} Hz, {} ch, {:.2}s", wav.display(), spec.sample_rate, spec.channels, audio_secs);
    eprintln!("model: {}", model.display());
    eprintln!("transcribing…");

    let trans = WhisperLocalTranscriber::new(&model)?;
    let started = Instant::now();
    let segs = trans.transcribe(&wav, Some("en"))?;
    let elapsed = started.elapsed().as_secs_f64();

    let rtf = audio_secs / elapsed;
    eprintln!();
    eprintln!("=== bench ===");
    eprintln!("audio    : {:>8.2} s", audio_secs);
    eprintln!("wall     : {:>8.2} s", elapsed);
    eprintln!("realtime : {:>8.2}x", rtf);
    eprintln!("segments : {}", segs.len());
    eprintln!();
    eprintln!("Live captions need >= 1.0x realtime to keep up. base.en typically");
    eprintln!("hits 5-15x on a modern laptop, so a 5-second live chunk takes ~0.3-1.0s.");
    Ok(())
}
