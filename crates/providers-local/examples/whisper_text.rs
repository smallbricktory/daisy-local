//! Dump the plain transcript of a 16 kHz mono WAV using the production
//! WhisperLocalTranscriber. Lets us diff one model's output against another
//! (e.g. base vs large-v3-turbo) to estimate the accuracy delta.
//!
//!   whisper_text <model.bin> <wav-16k-mono>

use providers_http::Transcriber;
use providers_local::WhisperLocalTranscriber;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: whisper_text <model.bin> <wav-16k-mono>");
        std::process::exit(2);
    }
    let t = WhisperLocalTranscriber::new(&args[1]).expect("load model");
    let segs = t.transcribe(Path::new(&args[2]), Some("en")).expect("transcribe");
    let text = segs.iter().map(|s| s.text.trim()).collect::<Vec<_>>().join(" ");
    println!("{text}");
}
