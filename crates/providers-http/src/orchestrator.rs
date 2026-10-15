// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
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
///
/// `resume` loads chunks that already have a checkpoint instead of
/// transcribing them again. Without it every chunk is transcribed and its
/// checkpoint overwritten.
pub fn transcribe_session(
    provider: &dyn Transcriber,
    session_root: &Path,
    manifest_json_bytes: &[u8],
    language_hint: Option<&str>,
    resume: bool,
    on_progress: &dyn Fn(usize, usize),
) -> Result<SessionTranscript> {
    #[derive(serde::Deserialize)]
    struct ChunkRef {
        index: u32,
        mic_wav_relative: std::path::PathBuf,
        #[serde(default)]
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

    /// One chunk still to transcribe.
    struct Pending {
        pos: usize,
        index: u32,
        ckpt_path: std::path::PathBuf,
        mic_track: Track,
        mic_rel: std::path::PathBuf,
        system_rel: std::path::PathBuf,
        mic_abs: std::path::PathBuf,
        sys_abs: std::path::PathBuf,
    }

    let mut out_chunks: Vec<Option<ChunkTranscript>> = (0..total_chunks).map(|_| None).collect();
    let mut pending: Vec<Pending> = Vec::new();
    let mut finished = 0usize;

    for (chunk_pos, c) in manifest.chunks.iter().enumerate() {
        // Resume checkpoint for this chunk (chunks/NNNN/transcript.json).
        // When `resume` is set and it is present and parseable, the chunk is
        // loaded from it instead of being re-transcribed.
        let ckpt_path = c
            .system_wav_relative
            .parent()
            .map(|p| session_root.join(p))
            .unwrap_or_else(|| session_root.join(format!("chunks/{:04}", c.index)))
            .join("transcript.json");
        if let Some(bytes) = resume.then(|| syncsafe::read(&ckpt_path).ok()).flatten() {
            if let Ok(done) = serde_json::from_slice::<ChunkTranscript>(&bytes) {
                log::info!("chunk {}: resumed from checkpoint", c.index);
                out_chunks[chunk_pos] = Some(done);
                finished += 1;
                on_progress(finished, total_chunks);
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
        pending.push(Pending {
            pos: chunk_pos,
            index: c.index,
            ckpt_path,
            mic_track,
            mic_abs: syncsafe::join_rel(session_root, &mic_rel),
            sys_abs: syncsafe::join_rel(session_root, &c.system_wav_relative),
            mic_rel,
            system_rel: c.system_wav_relative.clone(),
        });
    }

    // One job per pending chunk and track, mic first, run on as many threads
    // as the provider's `parallelism` allows. A track whose wav is missing or
    // silent is skipped.
    let transcribe_track = |chunk_idx: u32, label: &str, abs: &Path| -> Result<Vec<_>> {
        if !abs.is_file() {
            log::info!("chunk {chunk_idx}: skipping {label} — {} not on disk", abs.display());
            return Ok(Vec::new());
        }
        // A wav whose header can't be read is skipped like a missing track.
        if let Err(e) = transcript::wav_io::wav_spec(abs) {
            log::warn!("chunk {chunk_idx}: skipping unreadable {label} track: {e}");
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
    let decode = |job: usize| -> Result<Vec<_>> {
        let p = &pending[job / 2];
        if job % 2 == 0 {
            transcribe_track(p.index, "mic", &p.mic_abs)
        } else {
            transcribe_track(p.index, "system", &p.sys_abs)
        }
    };

    let mut results: Vec<Option<Vec<_>>> = (0..pending.len() * 2).map(|_| None).collect();
    // Takes one finished job; a chunk with both tracks done is checkpointed
    // and reported.
    let mut handle = |job: usize, segs: Vec<_>| {
        results[job] = Some(segs);
        let ci = job / 2;
        if results[ci * 2].is_none() || results[ci * 2 + 1].is_none() {
            return;
        }
        let p = &pending[ci];
        let chunk_t = ChunkTranscript {
            chunk_index: p.index,
            tracks: vec![
                TrackTranscript {
                    track: p.mic_track,
                    source_wav_relative: p.mic_rel.clone(),
                    segments: results[ci * 2].take().unwrap_or_default(),
                },
                TrackTranscript {
                    track: Track::System,
                    source_wav_relative: p.system_rel.clone(),
                    segments: results[ci * 2 + 1].take().unwrap_or_default(),
                },
            ],
        };
        // Checkpoint this chunk. Best-effort.
        if let Err(e) = write_chunk_checkpoint(&p.ckpt_path, &chunk_t) {
            log::warn!("chunk {}: checkpoint write failed: {e}", p.index);
        }
        out_chunks[p.pos] = Some(chunk_t);
        finished += 1;
        on_progress(finished, total_chunks);
    };

    let jobs = pending.len() * 2;
    let workers = provider.parallelism().clamp(1, jobs.max(1));
    if workers == 1 {
        for job in 0..jobs {
            handle(job, decode(job)?);
        }
    } else {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let next = AtomicUsize::new(0);
        let failed = AtomicBool::new(false);
        let mut first_err = None;
        std::thread::scope(|s| {
            let (tx, rx) = std::sync::mpsc::channel();
            for _ in 0..workers {
                let tx = tx.clone();
                let (next, failed, decode) = (&next, &failed, &decode);
                s.spawn(move || {
                    transcript::cpu::lower_to_background_priority();
                    loop {
                        let job = next.fetch_add(1, Ordering::Relaxed);
                        if job >= jobs || failed.load(Ordering::Relaxed) {
                            break;
                        }
                        if tx.send((job, decode(job))).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(tx);
            for (job, r) in rx {
                match r {
                    Ok(segs) => handle(job, segs),
                    Err(e) => {
                        failed.store(true, Ordering::Relaxed);
                        first_err.get_or_insert(e);
                    }
                }
            }
        });
        if let Some(e) = first_err {
            return Err(e);
        }
    }
    let out_chunks: Vec<ChunkTranscript> = out_chunks.into_iter().flatten().collect();

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
        provisional: false,
        edited_at_unix_seconds: None,
    })
}

/// Atomically write a chunk's transcript to its per-chunk resume checkpoint.
fn write_chunk_checkpoint(path: &Path, chunk: &ChunkTranscript) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        syncsafe::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(chunk)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    syncsafe::write_atomic(path, &bytes)
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
        for name in ["mic.wav", "system.wav"] {
            tone_wav(&dir.join("chunks/0001").join(name));
        }

        let jsonl = "\
{\"track\":\"mic\",\"start_ms\":0,\"end_ms\":1000,\"text\":\"hi\",\"kind\":\"polished\"}\n\
{\"track\":\"system\",\"start_ms\":0,\"end_ms\":1000,\"text\":\"yo\",\"kind\":\"polished\"}\n";
        syncsafe::write(dir.join("live_transcript.jsonl"), jsonl).unwrap();

        let provider = CountingTranscriber { calls: AtomicUsize::new(0) };
        let out = transcribe_session(&provider, &dir, &manifest_bytes, None, false, &|_, _| {}).unwrap();

        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert_eq!(out.chunks.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One second of audible tone; is_silent_wav() returns false for it.
    fn tone_wav(path: &Path) {
        syncsafe::create_dir_all(path.parent().unwrap()).unwrap();
        let samples: Vec<i16> = (0..16_000).map(|i| ((i as f32 * 0.2).sin() * 8000.0) as i16).collect();
        transcript::wav_io::write_wav_mono_i16(path, &samples, 16_000).unwrap();
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
        transcribe_session(&provider, &dir, &mb, None, false, &|_, _| {}).unwrap();

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
                    words: Vec::new(), split_group: None,
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
        let out = transcribe_session(&provider, &dir, &mb, None, true, &|_, _| {}).unwrap();

        // Chunk 1 loaded from checkpoint (0 calls); only chunk 2's 2 tracks ran.
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert_eq!(out.chunks.len(), 2);
        let c1 = out.chunks.iter().find(|c| c.chunk_index == 1).unwrap();
        assert_eq!(c1.tracks[0].segments[0].text, "cached");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_pass_ignores_leftover_checkpoints_and_replaces_them() {
        let dir = std::env::temp_dir().join(format!("daisy-orch-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        tone_wav(&dir.join("chunks/0001/mic.wav"));
        tone_wav(&dir.join("chunks/0001/system.wav"));
        // A checkpoint without word timings.
        let stale = ChunkTranscript {
            chunk_index: 1,
            tracks: vec![TrackTranscript {
                track: Track::Mic,
                source_wav_relative: "chunks/0001/mic.wav".into(),
                segments: vec![Segment {
                    start_ms: 0,
                    end_ms: 500,
                    text: "stale".into(),
                    confidence: None,
                    speaker_id: None,
                    words: Vec::new(), split_group: None,
                }],
            }],
        };
        let ckpt = dir.join("chunks/0001/transcript.json");
        syncsafe::write(&ckpt, serde_json::to_vec(&stale).unwrap()).unwrap();
        let manifest = serde_json::json!({
            "session_id": "s1", "created_at_unix_seconds": 1000,
            "chunks": [{"index": 1, "started_at_unix_seconds": 1000, "duration_seconds": 300,
                "mic_wav_relative": "chunks/0001/mic.wav", "system_wav_relative": "chunks/0001/system.wav", "mic_aec_wav_relative": null}]
        });
        let mb = serde_json::to_vec(&manifest).unwrap();
        let provider = CountingTranscriber { calls: AtomicUsize::new(0) };
        let out = transcribe_session(&provider, &dir, &mb, None, false, &|_, _| {}).unwrap();

        assert_eq!(provider.calls.load(Ordering::SeqCst), 2, "both tracks were transcribed again");
        assert!(out.chunks[0].tracks.iter().all(|t| t.segments.is_empty()), "nothing came from the stale checkpoint");
        let rewritten: ChunkTranscript = serde_json::from_slice(&syncsafe::read(&ckpt).unwrap()).unwrap();
        assert!(rewritten.tracks.iter().all(|t| t.segments.is_empty()), "the checkpoint holds the fresh result");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_channel_import_transcribes_system_only() {
        // An import is a single-(system-)track session: no mic wav on disk.
        // The orchestrator skips the missing mic and transcribes only system.
        let dir = std::env::temp_dir().join(format!("daisy-orch-sc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        syncsafe::create_dir_all(dir.join("chunks/0001")).unwrap();

        // Only system.wav exists (mic.wav intentionally absent).
        tone_wav(&dir.join("chunks/0001/system.wav"));

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
        let out = transcribe_session(&provider, &dir, &manifest_bytes, None, false, &|_, _| {}).unwrap();

        // Only the system track was transcribed (mic missing → skipped).
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(out.chunks.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Names each decode by chunk and track; reports how many ran at once.
    struct PathNamer {
        width: usize,
        running: AtomicUsize,
        peak: AtomicUsize,
        fail_on: Option<&'static str>,
    }
    impl Transcriber for PathNamer {
        fn name(&self) -> &'static str {
            "namer"
        }
        fn model(&self) -> &str {
            "namer"
        }
        fn parallelism(&self) -> usize {
            self.width
        }
        fn transcribe(&self, wav: &Path, _: Option<&str>) -> Result<Vec<Segment>> {
            let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            self.running.fetch_sub(1, Ordering::SeqCst);
            let name = format!(
                "{}/{}",
                wav.parent().and_then(|p| p.file_name()).unwrap().to_string_lossy(),
                wav.file_name().unwrap().to_string_lossy()
            );
            if self.fail_on == Some(name.as_str()) {
                return Err(crate::ProviderError::Other("boom".into()));
            }
            Ok(vec![Segment { start_ms: 0, end_ms: 500, text: name, ..Default::default() }])
        }
    }

    fn four_chunk_session(tag: &str) -> (std::path::PathBuf, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!("daisy-orch-par-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut chunks = Vec::new();
        for idx in 1..=4u32 {
            tone_wav(&dir.join(format!("chunks/{idx:04}/mic.wav")));
            tone_wav(&dir.join(format!("chunks/{idx:04}/system.wav")));
            chunks.push(serde_json::json!({
                "index": idx, "started_at_unix_seconds": 1000, "duration_seconds": 300,
                "mic_wav_relative": format!("chunks/{idx:04}/mic.wav"),
                "system_wav_relative": format!("chunks/{idx:04}/system.wav"),
                "mic_aec_wav_relative": null
            }));
        }
        let m = serde_json::json!({ "session_id": "s1", "created_at_unix_seconds": 1000, "chunks": chunks });
        (dir, serde_json::to_vec(&m).unwrap())
    }

    #[test]
    fn parallel_pass_returns_the_serial_transcript() {
        let namer = |width| PathNamer {
            width,
            running: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            fail_on: None,
        };
        let (dir, mb) = four_chunk_session("eq");
        let serial = namer(1);
        let a = transcribe_session(&serial, &dir, &mb, None, false, &|_, _| {}).unwrap();
        assert_eq!(serial.peak.load(Ordering::SeqCst), 1);

        let wide = namer(3);
        let seen = std::sync::Mutex::new(Vec::new());
        let b = transcribe_session(&wide, &dir, &mb, None, false, &|d, t| seen.lock().unwrap().push((d, t)))
            .unwrap();
        assert!(wide.peak.load(Ordering::SeqCst) > 1, "decodes ran concurrently");
        assert_eq!(a.chunks, b.chunks);
        assert_eq!(b.chunks.iter().map(|c| c.chunk_index).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
        assert_eq!(b.chunks[2].tracks[0].segments[0].text, "0003/mic.wav");
        assert_eq!(b.chunks[2].tracks[1].segments[0].text, "0003/system.wav");
        assert_eq!(*seen.lock().unwrap(), vec![(1, 4), (2, 4), (3, 4), (4, 4)]);
        assert!(dir.join("chunks/0004/transcript.json").is_file(), "every chunk is checkpointed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_parallel_decode_failure_fails_the_pass() {
        let (dir, mb) = four_chunk_session("err");
        let p = PathNamer {
            width: 3,
            running: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            fail_on: Some("0002/system.wav"),
        };
        assert!(transcribe_session(&p, &dir, &mb, None, false, &|_, _| {}).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
