//! Dump the plain transcript of a 16 kHz mono WAV using the production
//! WhisperLocalTranscriber. Lets us diff one model's output against another
//! (e.g. base vs large-v3-turbo) to estimate the accuracy delta.
//!
//!   whisper_text <model.bin> <wav-16k-mono>
//!
//! Env:
//!   WHISPER_TEXT_SEGMENTS  "1" prints one `start_ms end_ms text` line per
//!                          segment instead of the joined text — lets a
//!                          caller score each segment against the audio's
//!                          own level (was this span even speech?).

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
    if std::env::var("WHISPER_TEXT_SEGMENTS").is_ok_and(|v| v == "1") {
        for s in &segs {
            println!("{}\t{}\t{}", s.start_ms, s.end_ms, s.text.trim());
        }
        return;
    }
    let text = segs.iter().map(|s| s.text.trim()).collect::<Vec<_>>().join(" ");
    println!("{text}");
}
