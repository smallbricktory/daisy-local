use providers_http::{transcribe_session, ProviderError, Transcriber};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use tempfile::TempDir;
use transcript::{Segment, Track};

struct FakeTranscriber {
    calls: AtomicU32,
}

impl Transcriber for FakeTranscriber {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn model(&self) -> &str {
        "fake-1"
    }
    fn transcribe(&self, _wav: &Path, _lang: Option<&str>) -> Result<Vec<Segment>, ProviderError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Segment {
            start_ms: 0,
            end_ms: 1000,
            text: format!("call {n}"),
            confidence: None,
            speaker_id: None,
        }])
    }
}

#[test]
fn orchestrator_visits_every_chunk_per_track_pair() {
    let td = TempDir::new().unwrap();
    let root = td.path();
    // Fake out files referenced by the manifest. The orchestrator passes the
    // path to the provider; our fake doesn't open it.
    for i in 1..=2 {
        let dir = root.join(format!("chunks/{:04}", i));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mic_aec.wav"), b"fake").unwrap();
        std::fs::write(dir.join("system.wav"), b"fake").unwrap();
    }

    let manifest = br#"{
      "session_id": "x",
      "created_at_unix_seconds": 1000,
      "chunks": [
        {"index":1, "started_at_unix_seconds":1000, "duration_seconds":10,
         "mic_wav_relative":"chunks/0001/mic.wav",
         "system_wav_relative":"chunks/0001/system.wav",
         "mic_aec_wav_relative":"chunks/0001/mic_aec.wav"},
        {"index":2, "started_at_unix_seconds":1010, "duration_seconds":10,
         "mic_wav_relative":"chunks/0002/mic.wav",
         "system_wav_relative":"chunks/0002/system.wav",
         "mic_aec_wav_relative":"chunks/0002/mic_aec.wav"}
      ]
    }"#;

    let provider = FakeTranscriber {
        calls: AtomicU32::new(0),
    };
    let st = transcribe_session(&provider, root, manifest, Some("en"), &|_, _| {}).unwrap();
    assert_eq!(st.session_id, "x");
    assert_eq!(st.provider, "fake");
    assert_eq!(st.model, "fake-1");
    assert_eq!(st.chunks.len(), 2);
    assert_eq!(st.chunks[0].chunk_index, 1);
    assert_eq!(st.chunks[0].tracks.len(), 2);
    // First track is mic_aec (preferred when present); second is system.
    assert_eq!(st.chunks[0].tracks[0].track, Track::MicAec);
    assert_eq!(st.chunks[0].tracks[1].track, Track::System);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4); // 2 chunks × 2 tracks
}

#[test]
fn orchestrator_falls_back_to_raw_mic_when_no_aec() {
    let td = TempDir::new().unwrap();
    let root = td.path();
    let dir = root.join("chunks/0001");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mic.wav"), b"fake").unwrap();
    std::fs::write(dir.join("system.wav"), b"fake").unwrap();

    let manifest = br#"{
      "session_id": "x",
      "created_at_unix_seconds": 2000,
      "chunks": [
        {"index":1, "started_at_unix_seconds":2000, "duration_seconds":10,
         "mic_wav_relative":"chunks/0001/mic.wav",
         "system_wav_relative":"chunks/0001/system.wav",
         "mic_aec_wav_relative":null}
      ]
    }"#;
    let provider = FakeTranscriber {
        calls: AtomicU32::new(0),
    };
    let st = transcribe_session(&provider, root, manifest, None, &|_, _| {}).unwrap();
    assert_eq!(st.chunks[0].tracks[0].track, Track::Mic);
    assert_eq!(
        st.chunks[0].tracks[0].source_wav_relative,
        std::path::PathBuf::from("chunks/0001/mic.wav")
    );
}

#[test]
fn orchestrator_always_runs_provider_ignoring_live_transcript() {
    let td = TempDir::new().unwrap();
    let root = td.path();
    let dir = root.join("chunks/0001");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mic_aec.wav"), b"fake").unwrap();
    std::fs::write(dir.join("system.wav"), b"fake").unwrap();
    // Only Zipformer Finals — no polished. Orchestrator must still run Whisper.
    let jsonl = "\
{\"track\":\"mic\",\"start_ms\":0,\"end_ms\":500,\"text\":\"INTERIM ZIPFORMER\",\"final\":true,\"received_at_unix\":0,\"kind\":\"final\"}\n\
";
    std::fs::write(root.join("live_transcript.jsonl"), jsonl).unwrap();

    let manifest = br#"{
      "session_id": "x",
      "created_at_unix_seconds": 1000,
      "chunks": [
        {"index":1, "started_at_unix_seconds":1000, "duration_seconds":10,
         "mic_wav_relative":"chunks/0001/mic.wav",
         "system_wav_relative":"chunks/0001/system.wav",
         "mic_aec_wav_relative":"chunks/0001/mic_aec.wav"}
      ]
    }"#;
    let provider = FakeTranscriber {
        calls: AtomicU32::new(0),
    };
    let _ = transcribe_session(&provider, root, manifest, None, &|_, _| {}).unwrap();
    // 1 chunk × 2 tracks = 2 provider calls — no polished entries available.
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}
