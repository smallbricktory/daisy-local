// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Live Nemotron path vs batch over the same WAV: the live run's Finals
//! carry the batch words. Model-gated:
//!
//!   DAISY_NEMOTRON_TEST_MODEL_DIR=…/nemotron-speech-streaming-en-0.6b-560ms-int8 \
//!   DAISY_NEMOTRON_WAV=…/track.wav \
//!   cargo test -p providers-local --release --test nemotron_livesim -- --nocapture

mod common;

use common::wer;
use providers_http::Transcriber;
use providers_local::nemotron::batch::NemotronLocalTranscriber;
use providers_local::nemotron::realtime::NemotronRealtime;
use providers_local::nemotron::NemotronOpts;
use providers_realtime::{RealtimeEvent, RealtimeTranscriber};
use std::path::PathBuf;

fn words(segs: &[transcript::Segment]) -> Vec<String> {
    segs.iter()
        .flat_map(|s| s.text.split_whitespace().map(str::to_lowercase).collect::<Vec<_>>())
        .collect()
}

#[test]
fn live_finals_carry_the_batch_words() {
    let Some(dir) = std::env::var_os("DAISY_NEMOTRON_TEST_MODEL_DIR").map(PathBuf::from) else { return eprintln!("skip") };
    let Some(wav) = std::env::var_os("DAISY_NEMOTRON_WAV").map(PathBuf::from) else { return eprintln!("skip") };
    let pcm: Vec<i16> = providers_local::wav::decode_wav_16k_mono_f32(&wav)
        .unwrap()
        .iter()
        .map(|&s| (s * 32768.0).clamp(-32768.0, 32767.0) as i16)
        .collect();

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let (accepted, finals, interims) = rt.block_on(async {
        let live = NemotronRealtime::new(dir.clone(), "livesim");
        let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel(1024);
        let collect = tokio::spawn(async move {
            let (mut finals, mut interims) = (0usize, 0usize);
            while let Some(ev) = ev_rx.recv().await {
                match ev {
                    RealtimeEvent::Final { .. } => finals += 1,
                    RealtimeEvent::Interim { segment } => {
                        assert!(!segment.text.trim().is_empty(), "empty interim");
                        interims += 1;
                    }
                    RealtimeEvent::Error(e) => panic!("live error: {e}"),
                    _ => {}
                }
            }
            (finals, interims)
        });
        for frame in pcm.chunks(320) {
            audio_tx.send(frame.to_vec()).unwrap();
        }
        drop(audio_tx);
        let accepted = live.run(16_000, audio_rx, ev_tx).await.expect("run");
        let (finals, interims) = collect.await.unwrap();
        (accepted, finals, interims)
    });
    assert_eq!(finals, accepted.len(), "every accepted segment was emitted as Final");
    assert!(interims > 0, "the open utterance is shown as interim");
    assert!(accepted.windows(2).all(|p| p[1].start_ms >= p[0].end_ms), "finals overlap");

    let batch = NemotronLocalTranscriber::with_opts(dir, "batch", NemotronOpts::live())
        .transcribe(&wav, None)
        .unwrap();
    let e = wer(&words(&batch), &words(&accepted));
    println!("live vs batch WER {e:.4} ({} finals, {} batch segments)", accepted.len(), batch.len());
    assert!(e <= 0.005, "live words drift from batch: {e:.4}");
}
