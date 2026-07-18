use providers_realtime::{event::RealtimeEvent, RealtimeError, RealtimeTranscriber};
use transcript::Segment;

struct EchoTranscriber;

#[async_trait::async_trait]
impl RealtimeTranscriber for EchoTranscriber {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn model(&self) -> &str {
        "echo-1"
    }

    async fn run(
        &self,
        sample_rate: u32,
        mut audio_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<i16>>,
        events_tx: tokio::sync::mpsc::Sender<RealtimeEvent>,
    ) -> Result<Vec<Segment>, RealtimeError> {
        let mut accepted = Vec::new();
        let mut total_samples: u32 = 0;
        while let Some(frame) = audio_rx.recv().await {
            let frame_samples = frame.len() as u32;
            total_samples += frame_samples;
            let start_ms =
                ((total_samples - frame_samples) as u64 * 1000 / sample_rate as u64) as u32;
            let end_ms = (total_samples as u64 * 1000 / sample_rate as u64) as u32;
            let seg = Segment {
                start_ms,
                end_ms,
                text: format!("{} samples", frame_samples),
                confidence: Some(1.0),
                speaker_id: None,
            };
            let _ = events_tx
                .send(RealtimeEvent::Final {
                    segment: seg.clone(),
                })
                .await;
            accepted.push(seg);
        }
        Ok(accepted)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn echo_run_emits_final_per_frame() {
    let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel::<RealtimeEvent>(8);

    let transcriber = EchoTranscriber;
    let handle = tokio::spawn(async move {
        transcriber.run(16_000, audio_rx, events_tx).await
    });

    audio_tx.send(vec![0_i16; 1600]).unwrap(); // 100ms at 16kHz
    audio_tx.send(vec![1_i16; 800]).unwrap(); // 50ms
    drop(audio_tx);

    // Collect events.
    let e1 = events_rx.recv().await.unwrap();
    let e2 = events_rx.recv().await.unwrap();
    assert!(matches!(e1, RealtimeEvent::Final { .. }));
    assert!(matches!(e2, RealtimeEvent::Final { .. }));

    let segments = handle.await.unwrap().unwrap();
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].start_ms, 0);
    assert_eq!(segments[0].end_ms, 100);
    assert_eq!(segments[0].text, "1600 samples");
    assert_eq!(segments[1].start_ms, 100);
    assert_eq!(segments[1].end_ms, 150);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_audio_yields_no_segments() {
    let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
    let (events_tx, _events_rx) = tokio::sync::mpsc::channel::<RealtimeEvent>(8);

    let transcriber = EchoTranscriber;
    let handle = tokio::spawn(async move {
        transcriber.run(16_000, audio_rx, events_tx).await
    });
    drop(audio_tx);

    let segments = handle.await.unwrap().unwrap();
    assert!(segments.is_empty());
}

#[test]
fn name_and_model_are_stable() {
    let t = EchoTranscriber;
    assert_eq!(t.name(), "echo");
    assert_eq!(t.model(), "echo-1");
}
