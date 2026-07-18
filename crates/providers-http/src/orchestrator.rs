//! Drive a Transcriber across a session manifest, producing a
//! SessionTranscript value. The only writes are per-chunk resume checkpoints
//! (`chunks/NNNN/transcript.json`); the caller owns the session-level
//! `transcript.json` and clears the checkpoints once it is durable
//! (`clear_chunk_transcript_checkpoints`).
//!
//! Every chunk/track is transcribed with the supplied provider.

use crate::error::Result;
use crate::Transcriber;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use transcript::{ChunkTranscript, SessionTranscript, Track, TrackTranscript};

/// Run `provider` over every chunk of the session at `session_root`.
///
/// `manifest_json_bytes` is the serialized `SessionManifest`.
///
/// Each chunk yields two tracks: the mic (preferring `mic_aec.wav` over
/// `mic.wav` when available) and the system audio.
pub fn transcribe_session(
    provider: &dyn Transcriber,
    session_root: &Path,
    manifest_json_bytes: &[u8],
    language_hint: Option<&str>,
    on_progress: &dyn Fn(usize, usize),
) -> Result<SessionTranscript> {
    #[derive(serde::Deserialize)]
    struct ChunkRef {
        index: u32,
        mic_wav_relative: std::path::PathBuf,
        system_wav_relative: std::path::PathBuf,
        mic_aec_wav_relative: Option<std::path::PathBuf>,
    }
    #[derive(serde::Deserialize)]
    struct ManifestRef {
        session_id: String,
        chunks: Vec<ChunkRef>,
    }

    let manifest: ManifestRef =
        serde_json::from_slice(manifest_json_bytes).map_err(crate::ProviderError::Decode)?;

    let total_chunks = manifest.chunks.len();
    let mut out_chunks: Vec<ChunkTranscript> = Vec::with_capacity(total_chunks);

    for (chunk_pos, c) in manifest.chunks.iter().enumerate() {
        // Resume checkpoint for this chunk (chunks/NNNN/transcript.json).
        // When present and parseable, the chunk is loaded from it instead of
        // being re-transcribed.
        let ckpt_path = c
            .system_wav_relative
            .parent()
            .map(|p| session_root.join(p))
            .unwrap_or_else(|| session_root.join(format!("chunks/{:04}", c.index)))
            .join("transcript.json");
        if let Ok(bytes) = syncsafe::read(&ckpt_path) {
            if let Ok(done) = serde_json::from_slice::<ChunkTranscript>(&bytes) {
                log::info!("chunk {}: resumed from checkpoint", c.index);
                out_chunks.push(done);
                on_progress(chunk_pos + 1, total_chunks);
                continue;
            }
        }

        // Prefer mic_aec.wav; fall back to raw mic.wav when the manifest
        // references an AEC file that is missing on disk.
        let (mic_track, mic_rel) = match c.mic_aec_wav_relative.as_ref() {
            Some(p) if session_root.join(p).is_file() => (Track::MicAec, p.clone()),
            Some(p) => {
                log::warn!(
                    "chunk {}: mic_aec missing ({}); falling back to raw mic.wav",
                    c.index,
                    session_root.join(p).display()
                );
                (Track::Mic, c.mic_wav_relative.clone())
            }
            None => (Track::Mic, c.mic_wav_relative.clone()),
        };

        // Tracks are transcribed sequentially: one decode in flight at a time.
        // A track whose wav is missing or silent is skipped.
        let mic_abs = session_root.join(&mic_rel);
        let sys_abs = session_root.join(&c.system_wav_relative);
        let chunk_idx = c.index;
        let transcribe_track = |label: &str, abs: &std::path::Path| -> Result<Vec<_>> {
            if !abs.is_file() {
                log::info!("chunk {chunk_idx}: skipping {label} — {} not on disk", abs.display());
                return Ok(Vec::new());
            }
            log::info!("transcribing chunk {chunk_idx} {label} ({})", abs.display());
            if transcript::rms::is_silent_wav(abs).unwrap_or(false) {
                log::info!(
                    "skipping silent {} (RMS < {} dBFS)",
                    abs.display(),
                    transcript::rms::SILENCE_THRESHOLD_DBFS
                );
                return Ok(Vec::new());
            }
            provider.transcribe(abs, language_hint)
        };
        let mic_segs = transcribe_track("mic", &mic_abs)?;
        let sys_segs = transcribe_track("system", &sys_abs)?;

        let chunk_t = ChunkTranscript {
            chunk_index: c.index,
            tracks: vec![
                TrackTranscript {
                    track: mic_track,
                    source_wav_relative: mic_rel,
                    segments: mic_segs,
                },
                TrackTranscript {
                    track: Track::System,
                    source_wav_relative: c.system_wav_relative.clone(),
                    segments: sys_segs,
                },
            ],
        };
        // Checkpoint this chunk. Best-effort: after a failed write the chunk
        // re-transcribes on resume.
        if let Err(e) = write_chunk_checkpoint(&ckpt_path, &chunk_t) {
            log::warn!("chunk {}: checkpoint write failed: {e}", c.index);
        }
        out_chunks.push(chunk_t);
        on_progress(chunk_pos + 1, total_chunks);
    }

    Ok(SessionTranscript {
        schema_version: SessionTranscript::SCHEMA,
        session_id: manifest.session_id,
        provider: provider.name().to_string(),
        model: provider.model().to_string(),
        transcribed_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        chunks: out_chunks,
    })
}

/// Atomically write a chunk's transcript to its per-chunk resume checkpoint.
fn write_chunk_checkpoint(path: &Path, chunk: &ChunkTranscript) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        syncsafe::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(chunk)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    syncsafe::write(&tmp, &bytes)?;
    syncsafe::rename(&tmp, path)
}

/// Remove the per-chunk resume checkpoints (`chunks/NNNN/transcript.json`).
/// Call once the session-level `transcript.json` is durably written.
/// Best-effort: missing files / unreadable dir are ignored.
pub fn clear_chunk_transcript_checkpoints(session_root: &Path) {
    let chunks_dir = session_root.join("chunks");
    let Ok(entries) = std::fs::read_dir(&chunks_dir) else {
        return;
    };
    for ent in entries.flatten() {
        let ckpt = ent.path().join("transcript.json");
        if ckpt.is_file() {
            let _ = syncsafe::remove_file(&ckpt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use transcript::Segment;

    struct CountingTranscriber {
        calls: AtomicUsize,
    }
    impl Transcriber for CountingTranscriber {
        fn name(&self) -> &'static str {
            "counting"
        }
        fn model(&self) -> &str {
            "counting"
        }
        fn transcribe(
            &self,
            _wav: &Path,
            _language_hint: Option<&str>,
        ) -> Result<Vec<Segment>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
    }

    #[test]
    fn transcribes_every_chunk_even_when_polished_present() {
        let dir = std::env::temp_dir().join(format!("daisy-orch-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        syncsafe::create_dir_all(&dir).unwrap();

        let manifest = serde_json::json!({
            "session_id": "s1",
            "created_at_unix_seconds": 1000,
            "chunks": [{
                "index": 1,
                "started_at_unix_seconds": 1000,
                "duration_seconds": 300,
                "mic_wav_relative": "chunks/0001/mic.wav",
                "system_wav_relative": "chunks/0001/system.wav",
                "mic_aec_wav_relative": "chunks/0001/mic_aec.wav"
            }]
        });
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();

        // The orchestrator skips tracks whose wav is missing or silent;
        // both tracks get real non-silent wavs.
        syncsafe::create_dir_all(dir.join("chunks/0001")).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        for name in ["mic.wav", "system.wav"] {
            let mut w = hound::WavWriter::create(dir.join("chunks/0001").join(name), spec).unwrap();
            for i in 0..16_000 {
                // Audible tone; is_silent_wav() returns false for it.
                w.write_sample(((i as f32 * 0.2).sin() * 8000.0) as i16).unwrap();
            }
            w.finalize().unwrap();
        }

        let jsonl = "\
{\"track\":\"mic\",\"start_ms\":0,\"end_ms\":1000,\"text\":\"hi\",\"kind\":\"polished\"}\n\
{\"track\":\"system\",\"start_ms\":0,\"end_ms\":1000,\"text\":\"yo\",\"kind\":\"polished\"}\n";
        syncsafe::write(dir.join("live_transcript.jsonl"), jsonl).unwrap();

        let provider = CountingTranscriber { calls: AtomicUsize::new(0) };
        let out = transcribe_session(&provider, &dir, &manifest_bytes, None, &|_, _| {}).unwrap();

        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert_eq!(out.chunks.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tone_wav(path: &Path) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        syncsafe::create_dir_all(path.parent().unwrap()).unwrap();
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..16_000 {
            w.write_sample(((i as f32 * 0.2).sin() * 8000.0) as i16).unwrap();
        }
        w.finalize().unwrap();
    }

    #[test]
    fn writes_per_chunk_checkpoint() {
        let dir = std::env::temp_dir().join(format!("daisy-orch-ckw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        tone_wav(&dir.join("chunks/0001/mic.wav"));
        tone_wav(&dir.join("chunks/0001/system.wav"));
        let manifest = serde_json::json!({
            "session_id": "s1", "created_at_unix_seconds": 1000,
            "chunks": [{
                "index": 1, "started_at_unix_seconds": 1000, "duration_seconds": 300,
                "mic_wav_relative": "chunks/0001/mic.wav",
                "system_wav_relative": "chunks/0001/system.wav",
                "mic_aec_wav_relative": null
            }]
        });
        let mb = serde_json::to_vec(&manifest).unwrap();
        let provider = CountingTranscriber { calls: AtomicUsize::new(0) };
        transcribe_session(&provider, &dir, &mb, None, &|_, _| {}).unwrap();

        // The chunk's result is checkpointed to chunks/0001/transcript.json.
        let ckpt = dir.join("chunks/0001/transcript.json");
        assert!(ckpt.is_file(), "per-chunk checkpoint should be written");
        let parsed: ChunkTranscript =
            serde_json::from_slice(&syncsafe::read(&ckpt).unwrap()).unwrap();
        assert_eq!(parsed.chunk_index, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_skips_checkpointed_chunks() {
        let dir = std::env::temp_dir().join(format!("daisy-orch-rsm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for idx in [1u32, 2] {
            tone_wav(&dir.join(format!("chunks/{idx:04}/mic.wav")));
            tone_wav(&dir.join(format!("chunks/{idx:04}/system.wav")));
        }
        // Pre-write a checkpoint for chunk 1.
        let cached = ChunkTranscript {
            chunk_index: 1,
            tracks: vec![TrackTranscript {
                track: Track::Mic,
                source_wav_relative: "chunks/0001/mic.wav".into(),
                segments: vec![Segment {
                    start_ms: 0,
                    end_ms: 500,
                    text: "cached".into(),
                    confidence: None,
                    speaker_id: None,
                }],
            }],
        };
        syncsafe::write(
            dir.join("chunks/0001/transcript.json"),
            serde_json::to_vec(&cached).unwrap(),
        )
        .unwrap();

        let manifest = serde_json::json!({
            "session_id": "s1", "created_at_unix_seconds": 1000,
            "chunks": [
                {"index": 1, "started_at_unix_seconds": 1000, "duration_seconds": 300,
                 "mic_wav_relative": "chunks/0001/mic.wav", "system_wav_relative": "chunks/0001/system.wav", "mic_aec_wav_relative": null},
                {"index": 2, "started_at_unix_seconds": 1300, "duration_seconds": 300,
                 "mic_wav_relative": "chunks/0002/mic.wav", "system_wav_relative": "chunks/0002/system.wav", "mic_aec_wav_relative": null}
            ]
        });
        let mb = serde_json::to_vec(&manifest).unwrap();
        let provider = CountingTranscriber { calls: AtomicUsize::new(0) };
        let out = transcribe_session(&provider, &dir, &mb, None, &|_, _| {}).unwrap();

        // Chunk 1 loaded from checkpoint (0 calls); only chunk 2's 2 tracks ran.
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert_eq!(out.chunks.len(), 2);
        let c1 = out.chunks.iter().find(|c| c.chunk_index == 1).unwrap();
        assert_eq!(c1.tracks[0].segments[0].text, "cached");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_channel_import_transcribes_system_only() {
        // An import is a single-(system-)track session: no mic wav on disk.
        // The orchestrator skips the missing mic and transcribes only system.
        let dir = std::env::temp_dir().join(format!("daisy-orch-sc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        syncsafe::create_dir_all(dir.join("chunks/0001")).unwrap();

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        // Only system.wav exists (mic.wav intentionally absent).
        let mut w = hound::WavWriter::create(dir.join("chunks/0001/system.wav"), spec).unwrap();
        for i in 0..16_000 {
            w.write_sample(((i as f32 * 0.2).sin() * 8000.0) as i16).unwrap();
        }
        w.finalize().unwrap();

        let manifest = serde_json::json!({
            "session_id": "imp1",
            "created_at_unix_seconds": 1000,
            "chunks": [{
                "index": 1,
                "started_at_unix_seconds": 1000,
                "duration_seconds": 1,
                "mic_wav_relative": "chunks/0001/mic.wav",
                "system_wav_relative": "chunks/0001/system.wav",
                "mic_aec_wav_relative": null
            }]
        });
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();

        let provider = CountingTranscriber { calls: AtomicUsize::new(0) };
        let out = transcribe_session(&provider, &dir, &manifest_bytes, None, &|_, _| {}).unwrap();

        // Only the system track was transcribed (mic missing → skipped).
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(out.chunks.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
