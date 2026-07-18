//! Run speakrs on a Daisy session dir; on macOS this exercises the native
//! CoreML path.
//! Usage: SPEAKRS_MODELS_DIR=.../models/speakrs \
//!   cargo run --release --example time_speakrs -p voiceprints -- <session_dir>
use std::path::PathBuf;
use std::time::Instant;
fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("session dir"));
    let mut chunk_dirs: Vec<_> = std::fs::read_dir(dir.join("chunks"))
        .expect("chunks/").filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir()).collect();
    chunk_dirs.sort();
    let mut audio: Vec<f32> = Vec::new();
    for cd in &chunk_dirs {
        let w = cd.join("system.wav");
        if w.is_file() {
            if let Ok(mut s) = voiceprints::speakrs_diar::read_wav_f32(&w) { audio.append(&mut s); }
        }
    }
    let secs = audio.len() as f64 / 16000.0;
    eprintln!("loaded {:.0}s from {} chunks; running speakrs...", secs, chunk_dirs.len());
    let t = Instant::now();
    match voiceprints::speakrs_diar::diarize_audio(&audio) {
        Ok(turns) => {
            let spk: std::collections::HashSet<_> = turns.iter().map(|t| t.speaker.clone()).collect();
            let wall = t.elapsed().as_secs_f64();
            println!("OK audio={:.0}s wall={:.1}s realtime={:.1}x turns={} speakers={}",
                secs, wall, secs / wall, turns.len(), spk.len());
        }
        Err(e) => { println!("CRASH/ERR: {e}"); std::process::exit(1); }
    }
}
