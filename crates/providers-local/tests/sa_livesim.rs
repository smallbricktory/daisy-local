// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! SpeechAnalyzer streaming replay: feed a WAV through `SaRealtime::run()`
//! in 1 s frames and sanity-check the committed transcript. macOS-only and
//! env-gated:
//!
//!   DAISY_SA_WAV=…/speech.wav \
//!   cargo test -p providers-local --test sa_livesim -- --nocapture
//!
//! Skips when SpeechAnalyzer is unavailable or the env var is unset. SA live
//! output is verbatim its batch decode, so the check here is word volume +
//! span ordering.

#[cfg(target_os = "macos")]
#[test]
fn batch_transcriber_produces_ordered_segments() {
    use providers_http::Transcriber;
    use providers_local::macos::{batch::SaTranscriber, sa_available};

    let Ok(wav) = std::env::var("DAISY_SA_WAV") else {
        eprintln!("skip: DAISY_SA_WAV not set");
        return;
    };
    if !sa_available() {
        eprintln!("skip: SpeechAnalyzer unavailable on this machine");
        return;
    }
    let t = SaTranscriber::new("en-US");
    let segs = t.transcribe(std::path::Path::new(&wav), None).expect("batch transcribe");
    let words: usize = segs.iter().map(|s| s.text.split_whitespace().count()).sum();
    println!("sa batch: {} segment(s), {words} words", segs.len());
    assert!(words >= 10, "expected sentence-scale output, got {words} words");
    for pair in segs.windows(2) {
        assert!(pair[1].start_ms >= pair[0].start_ms, "segments ordered");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn committed_stream_produces_ordered_transcript() {
    use providers_local::macos::{realtime::SaRealtime, sa_available};
    use providers_realtime::{RealtimeEvent, RealtimeTranscriber};

    let Ok(wav) = std::env::var("DAISY_SA_WAV") else {
        eprintln!("skip: DAISY_SA_WAV not set");
        return;
    };
    if !sa_available() {
        eprintln!("skip: SpeechAnalyzer unavailable on this machine");
        return;
    }
    let mut reader = hound::WavReader::open(&wav).expect("open wav");
    assert_eq!(reader.spec().sample_rate, 16_000, "expect 16k wav");
    let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let accepted = rt.block_on(async {
        let backend = SaRealtime::new("en-US");
        let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel();
        let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(256);
        let drain = tokio::spawn(async move {
            let mut finals = 0usize;
            while let Some(ev) = events_rx.recv().await {
                if matches!(ev, RealtimeEvent::Final { .. }) {
                    finals += 1;
                }
            }
            finals
        });
        for chunk in samples.chunks(16_000) {
            audio_tx.send(chunk.to_vec()).unwrap();
        }
        drop(audio_tx);
        let accepted = backend.run(16_000, audio_rx, events_tx).await.expect("run");
        let finals = drain.await.unwrap();
        assert_eq!(finals, accepted.len(), "every accepted segment was emitted as Final");
        accepted
    });

    let words: usize = accepted.iter().map(|s| s.text.split_whitespace().count()).sum();
    let text: String = accepted.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");
    println!("streamed ({words} words): {text}");
    assert!(words > 0, "speech wav must commit words");
    for pair in accepted.windows(2) {
        assert!(
            pair[1].start_ms as i64 >= pair[0].end_ms as i64 - 300,
            "committed spans must not overlap by more than the drift guard: {:?} then {:?}",
            pair[0],
            pair[1]
        );
    }
}
