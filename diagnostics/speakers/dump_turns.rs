// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Dump raw speakrs turns for a session's system track as JSONL
//! (`{"start":..,"end":..,"speaker":".."}`), one per line.
//! Usage: SPEAKRS_MODELS_DIR=.../models/speakrs \
//!   cargo run --release --example dump_turns -p voiceprints -- <session_dir> [known_count]
use std::path::PathBuf;
fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("session dir"));
    let known: Option<usize> =
        std::env::args().nth(2).and_then(|s| s.parse().ok()).filter(|n| *n > 0);
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
        eprintln!("after {:?}: {:.1}s", cd.file_name().unwrap(), audio.len() as f64 / 16000.0);
    }
    let turns = voiceprints::speakrs_diar::diarize_audio(&audio, known).expect("diarize");
    for t in &turns {
        println!("{{\"start\":{:.3},\"end\":{:.3},\"speaker\":\"{}\"}}", t.start, t.end, t.speaker);
    }
    eprintln!("turns={}", turns.len());
}
