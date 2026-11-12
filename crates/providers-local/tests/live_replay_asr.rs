// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Live-ASR replay harness: feeds a real session WAV into the streaming
//! transcriber (whisper for a ggml file, Nemotron for a model directory) at real-time pace and prints every interim/final.
//! Field verification of the live path without a live meeting.
//!
//!   DAISY_REPLAY_WAV=…/chunks/0004/mic.wav \
//!   DAISY_REPLAY_MODEL=…/models/whisper/ggml-base.en.bin \
//!   cargo test -p providers-local --test live_replay_asr -- --ignored --nocapture
//!
//! Optional: DAISY_REPLAY_EXPECT="phrase one|phrase two" — the run fails
//! unless every phrase appears (case-insensitive) in the committed finals.

use providers_local::streaming::transcriber::LocalWhisperRealtime;
use providers_realtime::{RealtimeEvent, RealtimeTranscriber};
use std::path::PathBuf;
use tokio::sync::mpsc;

fn read_wav_16k(path: &PathBuf) -> Vec<i16> {
    let mut r = hound::WavReader::open(path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    assert_eq!(r.spec().sample_rate, 16_000);
    assert_eq!(r.spec().channels, 1);
    r.samples::<i16>().map(|s| s.unwrap()).collect()
}

#[tokio::test]
#[ignore]
async fn live_replay_asr() {
    let wav = PathBuf::from(std::env::var("DAISY_REPLAY_WAV").expect("DAISY_REPLAY_WAV"));
    let model = PathBuf::from(std::env::var("DAISY_REPLAY_MODEL").expect("DAISY_REPLAY_MODEL"));
    let expect: Vec<String> = std::env::var("DAISY_REPLAY_EXPECT")
        .map(|s| s.split('|').map(|p| p.trim().to_lowercase()).collect())
        .unwrap_or_default();

    let samples = read_wav_16k(&wav);
    println!("replay: {:?} — {:.1}s", wav, samples.len() as f64 / 16_000.0);

    let t: Box<dyn RealtimeTranscriber> = if model.is_dir() {
        Box::new(providers_local::nemotron::realtime::NemotronRealtime::new(model, "replay"))
    } else {
        Box::new(LocalWhisperRealtime::new(model, 4, "replay"))
    };
    let (audio_tx, audio_rx) = mpsc::unbounded_channel::<Vec<i16>>();
    let (events_tx, mut events_rx) = mpsc::channel::<RealtimeEvent>(64);

    let feeder = tokio::spawn(async move {
        // 100 ms chunks at real-time pace: the hop controller and window
        // growth behave as they would live.
        const CHUNK: usize = 1_600;
        for c in samples.chunks(CHUNK) {
            let _ = audio_tx.send(c.to_vec());
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // Trailing silence lets the last window finalize before EOF.
        for _ in 0..30 {
            let _ = audio_tx.send(vec![0i16; CHUNK]);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });

    let printer = tokio::spawn(async move {
        let mut finals: Vec<String> = Vec::new();
        while let Some(ev) = events_rx.recv().await {
            match ev {
                RealtimeEvent::Final { segment, .. } => {
                    println!(
                        "FINAL   {:>6}-{:<6} {}",
                        segment.start_ms, segment.end_ms, segment.text
                    );
                    finals.push(segment.text);
                }
                RealtimeEvent::Interim { segment } => {
                    println!("interim {:>6}-{:<6} {}", segment.start_ms, segment.end_ms, segment.text);
                }
                RealtimeEvent::FallingBehind => println!("BEHIND  decode below real time"),
                RealtimeEvent::Stat(_) => {}
                RealtimeEvent::Error(e) => println!("ERROR   {e}"),
            }
        }
        finals
    });

    let accepted = t.run(16_000, audio_rx, events_tx).await.expect("run");
    feeder.await.unwrap();
    let finals = printer.await.unwrap();

    println!("accepted segments: {}", accepted.len());
    let joined = finals.join(" ").to_lowercase();
    println!("committed text: {joined}");
    for phrase in &expect {
        assert!(
            joined.contains(phrase),
            "expected phrase missing from committed finals: {phrase:?}"
        );
    }
}
