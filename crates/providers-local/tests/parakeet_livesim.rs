//! Parakeet streaming replay: feed a WAV through `ParakeetRealtime::run()` in
//! 1 s frames and compare the committed transcript against a direct windowed
//! decode of the same audio. Model-gated:
//!
//!   DAISY_PARAKEET_TEST_MODEL_DIR=…/parakeet-tdt-0.6b-v2 \
//!   DAISY_PARAKEET_WAV=…/speech.wav \
//!   cargo test -p providers-local --test parakeet_livesim -- --nocapture

use providers_local::parakeet::realtime::{ParakeetRealtime, ParakeetTune};
use providers_local::parakeet::{ParakeetASR, ParakeetOpts};
use providers_realtime::{RealtimeEvent, RealtimeTranscriber};
use std::path::PathBuf;

fn read_wav_16k_i16(path: &str) -> Vec<i16> {
    let mut reader = hound::WavReader::open(path).expect("open wav");
    assert_eq!(reader.spec().sample_rate, 16_000, "expect 16k wav");
    reader.samples::<i16>().map(|s| s.unwrap()).collect()
}

#[test]
fn committed_stream_matches_windowed_batch() {
    let Ok(model_dir) = std::env::var("DAISY_PARAKEET_TEST_MODEL_DIR") else {
        eprintln!("skip: DAISY_PARAKEET_TEST_MODEL_DIR not set");
        return;
    };
    let Ok(wav) = std::env::var("DAISY_PARAKEET_WAV") else {
        eprintln!("skip: DAISY_PARAKEET_WAV not set");
        return;
    };
    let samples = read_wav_16k_i16(&wav);

    // Streamed run: 1 s frames, no pacing (decode is serialized; pacing only
    // affects latency, not commit correctness).
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let accepted = rt.block_on(async {
        let backend = ParakeetRealtime::new(
            PathBuf::from(&model_dir),
            ParakeetTune::default(),
            "parakeet test",
        );
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

    let streamed_words: usize =
        accepted.iter().map(|s| s.text.split_whitespace().count()).sum();
    let streamed_text: String =
        accepted.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");
    println!("streamed ({streamed_words} words): {streamed_text}");

    // Ordered, non-overlapping (>300 ms) committed spans.
    for pair in accepted.windows(2) {
        assert!(
            pair[1].start_ms as i64 >= pair[0].end_ms as i64 - 300,
            "committed spans must not overlap by more than the drift guard: {:?} then {:?}",
            pair[0],
            pair[1]
        );
    }

    // Windowed batch reference over the same audio (60 s windows).
    let mut asr =
        ParakeetASR::new(&PathBuf::from(&model_dir), &ParakeetOpts::default()).expect("load");
    let f32s: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();
    let mut batch_words = 0usize;
    for win in f32s.chunks(60 * 16_000) {
        batch_words += asr.decode(win).expect("batch decode").len();
    }
    println!("batch words: {batch_words}");
    assert!(streamed_words > 0, "streamed run must commit words");
    let diff = (streamed_words as f64 - batch_words as f64).abs() / batch_words as f64;
    assert!(
        diff <= 0.15,
        "streamed word count within 15% of batch: streamed={streamed_words} batch={batch_words}"
    );
}
