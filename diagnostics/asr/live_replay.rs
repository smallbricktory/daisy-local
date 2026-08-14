//! Replay a recorded WAV through the REAL live-captions path and print what
//! it commits — the offline harness for streaming-transcript bugs.
//!
//! The live path is a sliding window plus LocalAgreement-2: a word is emitted
//! as Final only once two consecutive decodes agree on it as a prefix, and the
//! window's audio is trimmed at the committed boundary. Bugs there (a word
//! clipped by the trim, a word that never agrees and is dropped) are invisible
//! in a batch transcription and only reproduce through this exact machinery.
//!
//!   cargo run --release -p providers-local --example live_replay -- MODEL WAV
//!
//! Feeds the wav in 20 ms frames PACED TO REAL TIME. Pacing is not optional:
//! the decode trigger is "hop ms of new audio since the last decode", so
//! dumping the file instantly grows one enormous window and commits a few
//! giant blocks — nothing like the ~1 s hops a recording produces, and the
//! agreement/trim behaviour under test disappears.
//!
//! Env:
//!   REPLAY_SPEED  playback rate (default 1.0 = real time). Above ~2 the
//!                 window dynamics start to drift from live; use 1.0 to
//!                 reproduce a field bug.
//!   REPLAY_GREP   substring (case-insensitive); reports whether it survived
//!                 into the committed transcript. Exit 1 when it did not —
//!                 so a dropped-word regression fails a script.

use providers_local::streaming::transcriber::LocalWhisperRealtime;
use providers_realtime::{RealtimeEvent, RealtimeTranscriber};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(model), Some(wav)) = (args.next(), args.next()) else {
        eprintln!("usage: live_replay <model.bin> <wav-16k-mono>");
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
        let t = LocalWhisperRealtime::new(model.into(), 4, "whisper (replay)");
        let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        let (events_tx, mut events_rx) = tokio::sync::mpsc::channel::<RealtimeEvent>(256);

        // Feed 20 ms frames on a real-time schedule (scaled by `speed`),
        // matching the engine's tee cadence. Absolute deadlines rather than
        // per-frame sleeps so decode stalls cannot make the feed drift.
        let feeder = tokio::spawn(async move {
            let frame_ms = 20.0 / speed;
            let start = tokio::time::Instant::now();
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

        // Drain events so the bounded channel never blocks the decoder.
        let collector = tokio::spawn(async move {
            let mut committed: Vec<(u32, u32, String)> = Vec::new();
            while let Some(ev) = events_rx.recv().await {
                match ev {
                    RealtimeEvent::Final { segment } => {
                        committed.push((segment.start_ms, segment.end_ms, segment.text));
                    }
                    RealtimeEvent::Error(e) => eprintln!("  !! {e}"),
                    RealtimeEvent::PossibleMissedSpeech { start_ms, end_ms } => {
                        eprintln!("  ?? possible missed speech {start_ms}-{end_ms}");
                    }
                    RealtimeEvent::FallingBehind => eprintln!("  ~~ falling behind"),
                    RealtimeEvent::Interim { .. } => {}
                }
            }
            committed
        });

        let accepted = t.run(16_000, audio_rx, events_tx).await;
        let _ = feeder.await;
        let committed = collector.await.unwrap_or_default();
        (accepted.map(|v| v.len()).unwrap_or(0), committed)
    });

    let (n_accepted, committed) = finals;
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

    if let Some(needle) = needle {
        let hit = joined.to_lowercase().contains(&needle.to_lowercase());
        println!("\nREPLAY_GREP {needle:?}: {}", if hit { "PRESENT" } else { "MISSING" });
        if !hit {
            std::process::exit(1);
        }
    }
}
