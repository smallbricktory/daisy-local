// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Replay a recorded WAV through the REAL live-captions path (whisper for a
//! ggml file, Nemotron for a model directory) and print what it commits, the
//! offline harness for streaming transcripts.
//!
//!   cargo run --release -p providers-local --example live_replay -- MODEL WAV
//!
//! MODEL is a whisper ggml file or a Nemotron model directory. The wav is fed
//! in 20 ms frames paced to real time, matching the cadence a recording
//! produces.
//!
//! Output: every committed Final, the joined transcript,
//! `FINAL_LAG p50/p90`: each Final's receipt time on the feed clock minus its
//! segment end, and `FIRST_INTERIM_LAG p50/p90`: for each interim that adds
//! words to the open utterance, its receipt time minus the end of its newest
//! word (the caption latency a viewer sees).
//!
//!
//! Env:
//!   REPLAY_SPEED  playback rate (default 1.0 = real time).
//!   REPLAY_GREP   substring (case-insensitive); reports whether it survived
//!                 into the committed transcript. Exit 1 when it did not.

use providers_local::streaming::transcriber::LocalWhisperRealtime;
use providers_realtime::{RealtimeEvent, RealtimeTranscriber};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(model), Some(wav)) = (args.next(), args.next()) else {
        eprintln!("usage: live_replay <model.bin | nemotron-model-dir> <wav-16k-mono>");
        std::process::exit(2);
    };
    let needle = std::env::var("REPLAY_GREP").ok();
    let speed: f64 = std::env::var("REPLAY_SPEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &f64| *v > 0.0)
        .unwrap_or(1.0);

    let samples: Vec<i16> = providers_local::wav::decode_wav_16k_mono_f32(
        std::path::Path::new(&wav),
    )
    .expect("decode wav")
    .iter()
    .map(|&s| (s * 32768.0).clamp(-32768.0, 32767.0) as i16)
    .collect();
    println!(
        "replaying {:.1}s of audio through the live path at {speed}x",
        samples.len() as f32 / 16_000.0
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");

    let finals = rt.block_on(async move {
        let t: Box<dyn RealtimeTranscriber> = if std::path::Path::new(&model).is_dir() {
            Box::new(providers_local::nemotron::realtime::NemotronRealtime::new(
                model.into(),
                "nemotron (replay)",
            ))
        } else {
            Box::new(LocalWhisperRealtime::new(model.into(), 4, "whisper (replay)"))
        };
        let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        let (events_tx, mut events_rx) = tokio::sync::mpsc::channel::<RealtimeEvent>(256);

        // Feed 20 ms frames on a real-time schedule (scaled by `speed`),
        // matching the engine's tee cadence.
        let started = tokio::time::Instant::now();
        let feeder = tokio::spawn(async move {
            let frame_ms = 20.0 / speed;
            let start = started;
            for (i, frame) in samples.chunks(320).enumerate() {
                let due = start
                    + std::time::Duration::from_micros((i as f64 * frame_ms * 1000.0) as u64);
                tokio::time::sleep_until(due).await;
                if audio_tx.send(frame.to_vec()).is_err() {
                    return;
                }
            }
            // Dropping the sender closes the stream: the transcriber flushes
            // its remaining interim tail as Final and returns.
        });

        // Drain events.
        let collector = tokio::spawn(async move {
            let mut committed: Vec<(u32, u32, String)> = Vec::new();
            let mut lags: Vec<i64> = Vec::new();
            let mut interim_lags: Vec<i64> = Vec::new();
            let mut open: Option<(u32, usize)> = None;
            while let Some(ev) = events_rx.recv().await {
                match ev {
                    RealtimeEvent::Final { segment, .. } => {
                        let fed_ms = (started.elapsed().as_secs_f64() * 1000.0 * speed) as i64;
                        lags.push(fed_ms - segment.end_ms as i64);
                        committed.push((segment.start_ms, segment.end_ms, segment.text));
                    }
                    RealtimeEvent::Error(e) => eprintln!("  !! {e}"),
                    RealtimeEvent::FallingBehind => eprintln!("  ~~ falling behind"),
                    RealtimeEvent::Stat(_) => {}
                    RealtimeEvent::Interim { segment } => {
                        let fed_ms = (started.elapsed().as_secs_f64() * 1000.0 * speed) as i64;
                        let words = segment.text.split_whitespace().count();
                        let shown = match open {
                            Some((start, n)) if start == segment.start_ms => n,
                            _ => 0,
                        };
                        if words > shown {
                            interim_lags.push(fed_ms - segment.end_ms as i64);
                        }
                        open = Some((segment.start_ms, words));
                    }
                }
            }
            (committed, lags, interim_lags)
        });

        let accepted = t.run(16_000, audio_rx, events_tx).await;
        let _ = feeder.await;
        let (committed, lags, interim_lags) = collector.await.unwrap_or_default();
        (accepted.map(|v| v.len()).unwrap_or(0), committed, lags, interim_lags)
    });

    let (n_accepted, committed, mut lags, mut interim_lags) = finals;
    println!("--- committed finals ({n_accepted} accepted by the transcriber) ---");
    for (start, end, text) in &committed {
        println!("  [{start:>6}-{end:>6}ms] {text}");
    }
    let joined = committed
        .iter()
        .map(|(_, _, t)| t.trim())
        .collect::<Vec<_>>()
        .join(" ");
    println!("--- joined ---\n{joined}");
    for (name, v) in [("FINAL_LAG", &mut lags), ("FIRST_INTERIM_LAG", &mut interim_lags)] {
        v.sort_unstable();
        if !v.is_empty() {
            let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
            println!("{name} p50 {} ms  p90 {} ms  (n={})", q(0.5), q(0.9), v.len());
        }
    }

    if let Some(needle) = needle {
        let hit = joined.to_lowercase().contains(&needle.to_lowercase());
        println!("\nREPLAY_GREP {needle:?}: {}", if hit { "PRESENT" } else { "MISSING" });
        if !hit {
            std::process::exit(1);
        }
    }
}
