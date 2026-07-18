//! Tauri commands that drive the transcription pipeline against a session
//! already on disk.

use crate::error::{AppError, Result};
use crate::state::{AppState, ProviderConfig, ProviderId};
use providers_http::{transcribe_session, Transcriber};
use serde::Deserialize;
use std::collections::HashMap;
use transcript::dedup::{dedup_session, DedupParams};
use transcript::render::render_markdown_with_speakers;
use transcript::SessionTranscript;

#[derive(Debug, Deserialize)]
pub struct TranscribeRequest {
    pub session_id: String,
    /// Path to a ggml-*.bin whisper model. `None` falls back to
    /// `settings.whisper_model_path`, then `DAISY_WHISPER_MODEL_DIR`.
    pub model: Option<String>,
}

pub fn transcribe_impl(
    app: &AppState,
    req: TranscribeRequest,
    on_progress: Option<&dyn Fn(usize, usize)>,
) -> Result<usize> {
    // Lift the BLAS=1 startup cap for the duration of the transcribe; the
    // guard restores cap=1 on drop.
    #[cfg(target_os = "linux")]
    let _blas_guard = {
        struct G;
        impl Drop for G {
            fn drop(&mut self) { crate::openblas::cap_openblas_threads(1); }
        }
        // Cap at cores-2 (minimum 1).
        let n = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4)
            .saturating_sub(2)
            .max(1);
        crate::openblas::cap_openblas_threads(n);
        G
    };
    let mut req = req;
    let settings = crate::settings::Settings::load_or_default(&app.profile.settings_path());
    // The "model" is a path to a ggml file. If the caller didn't pass one,
    // fall back to settings.whisper_model_path or the bundled path via
    // DAISY_WHISPER_MODEL_DIR.
    if req.model.is_none() {
        req.model = settings.whisper_model_path.clone().or_else(|| {
            std::env::var_os("DAISY_WHISPER_MODEL_DIR")
                .map(|d| std::path::PathBuf::from(d).join("ggml-base.en.bin").to_string_lossy().into_owned())
        });
    }

    let session = app.profile.session_path(&req.session_id);
    if !session.is_dir() {
        return Err(AppError::SessionNotFound(req.session_id.clone()));
    }

    // Run echo-cancellation before transcribing; the orchestrator picks up
    // mic_aec.wav (mic minus speaker bleed) over raw mic.wav. Idempotent:
    // chunks whose mic_aec.wav already exists are skipped. Best-effort: a
    // missing model logs a warning and leaves raw mic.wav in place.
    if let Err(e) = recording::apply_aec(&session) {
        log::warn!("AEC (pre-transcribe) for {}: {e}", req.session_id);
    }

    // If the raw chunk WAVs are gone but the stereo meeting.opus archive
    // remains, rebuild per-track WAVs from it.
    if let Err(e) = rehydrate_from_opus(&session) {
        log::warn!("rehydrate from opus for {}: {e}", req.session_id);
    }

    let manifest_bytes = syncsafe::read(session.join("manifest.json"))?;
    // ASR priming terms (attendee names + tag vocabulary): local Whisper via
    // initial_prompt. Cloud LLM transcribers (Groq/OpenAI) ignore them.
    let terms = serde_json::from_slice::<recording::manifest::SessionManifest>(&manifest_bytes)
        .ok()
        .map(|m| {
            let tags = crate::commands::tags::load_tags_file(app)
                .map(|f| f.tags)
                .unwrap_or_default();
            let vocab_terms =
                crate::commands::transcribe_priming::collect_tag_vocab_terms(&tags, &m.tag_ids);
            let attendees: Vec<String> =
                m.attendees.iter().map(|a| a.display_name.clone()).collect();
            crate::commands::transcribe_priming::meeting_terms(None, &attendees, &vocab_terms)
        })
        .unwrap_or_default();
    let provider: Box<dyn Transcriber> = build_whisper_provider(req.model, terms)?;
    let noop = |_: usize, _: usize| {};
    let prog: &dyn Fn(usize, usize) = on_progress.unwrap_or(&noop);
    let st = transcribe_session(&*provider, &session, &manifest_bytes, None, prog)?;
    let n = st
        .chunks
        .iter()
        .flat_map(|c| c.tracks.iter())
        .map(|t| t.segments.len())
        .sum::<usize>();
    let json = serde_json::to_vec_pretty(&st)?;
    syncsafe::write(session.join("transcript.json"), json)?;
    // Drop the per-chunk resume checkpoints written during the pass.
    providers_http::clear_chunk_transcript_checkpoints(&session);
    Ok(n)
}

/// If a session's raw chunk WAVs are gone but `meeting.opus` (the stereo
/// archive, L=mic / R=system) is present, decode it and rebuild a single
/// chunk's mic.wav + system.wav, rewriting the manifest to that one chunk.
/// No-op when chunk audio still exists or there's no archive. Best-effort.
fn rehydrate_from_opus(session_dir: &std::path::Path) -> Result<()> {
    use recording::manifest::{ChunkManifest, SessionManifest};

    let mp = session_dir.join("manifest.json");
    let Ok(bytes) = syncsafe::read(&mp) else { return Ok(()) };
    let Ok(manifest) = serde_json::from_slice::<SessionManifest>(&bytes) else { return Ok(()) };

    // Any chunk audio still on disk? Then nothing to rehydrate.
    let has_audio = manifest.chunks.iter().any(|c| {
        session_dir.join(&c.system_wav_relative).is_file()
            || session_dir.join(&c.mic_wav_relative).is_file()
    });
    if has_audio {
        return Ok(());
    }
    let opus = session_dir.join("meeting.opus");
    if !opus.is_file() {
        return Ok(());
    }

    let (channels, inter) = recording::compress::decode_opus(&opus)
        .map_err(|e| AppError::Config(format!("decode archive: {e}")))?;
    let (mic, sys) = recording::compress::deinterleave_stereo(channels, &inter);
    // Mono archive: the single track is treated as the system track.
    let (mic_pcm, sys_pcm): (Vec<i16>, Vec<i16>) = if channels >= 2 {
        (mic, sys)
    } else {
        (vec![0i16; crate::commands::SILENT_MIC_PLACEHOLDER_SAMPLES], sys)
    };

    let chunk_dir = session_dir.join("chunks/0001");
    syncsafe::create_dir_all(&chunk_dir)?;
    write_wav_mono_16k(&chunk_dir.join("mic.wav"), &mic_pcm)?;
    write_wav_mono_16k(&chunk_dir.join("system.wav"), &sys_pcm)?;

    let dur = (sys_pcm.len().max(mic_pcm.len()) as u64) / 16_000;
    let mut m = manifest;
    m.chunks = vec![ChunkManifest {
        index: 1,
        started_at_unix_seconds: m.created_at_unix_seconds,
        ended_at_unix_seconds: Some(m.created_at_unix_seconds + dur as i64),
        duration_seconds: Some(dur),
        mic_wav_relative: std::path::PathBuf::from("chunks/0001/mic.wav"),
        system_wav_relative: std::path::PathBuf::from("chunks/0001/system.wav"),
        mic_aec_wav_relative: None,
        mic_dn_wav_relative: None,
    }];
    let tmp = mp.with_extension("json.tmp");
    syncsafe::write(&tmp, serde_json::to_vec_pretty(&m)?)?;
    syncsafe::rename(&tmp, &mp)?;
    log::info!("rehydrated {} from meeting.opus archive", session_dir.display());
    Ok(())
}

use crate::commands::write_wav_mono_16k;

#[derive(Debug, Deserialize)]
pub struct DedupRequest {
    pub session_id: String,
}

pub fn dedup_impl(app: &AppState, req: DedupRequest) -> Result<DedupSummary> {
    let session = app.profile.session_path(&req.session_id);
    if !session.is_dir() {
        return Err(AppError::SessionNotFound(req.session_id.clone()));
    }
    let bytes = syncsafe::read(session.join("transcript.json"))?;
    let mut st: SessionTranscript = serde_json::from_slice(&bytes)?;
    // Mask profanity on the finalized transcript; every finalize/
    // re-transcribe path flows through dedup. The live view is masked
    // separately at the streaming segment constructor.
    for chunk in &mut st.chunks {
        for track in &mut chunk.tracks {
            for seg in &mut track.segments {
                seg.text = transcript::text::mask_profanity(&seg.text);
            }
        }
    }
    let metrics_enabled = crate::settings::Settings::load_or_default(&app.profile.settings_path())
        .debug_level
        .verbose();
    apply_energy_gate(&mut st, &session, app.profile.root(), &req.session_id, metrics_enabled);
    let result = dedup_session(&st, &session, &DedupParams::default())?;
    syncsafe::write(
        session.join("transcript.dedup.json"),
        serde_json::to_vec_pretty(&result.deduped)?,
    )?;
    // Render timestamps as time since the meeting start: each chunk's start
    // offset is added to its segment times. The manifest's speaker_map
    // supplies labels for diarized turns; unlabelled clusters render as
    // "Person A/B/C".
    let offsets = chunk_offsets_ms(&session);
    let speakers = speaker_map_for(&session);
    let md = render_markdown_with_speakers(&result.deduped, &offsets, &speakers);
    syncsafe::write(session.join("transcript.md"), md.as_bytes())?;
    Ok(DedupSummary {
        dropped: result.report.dropped,
        kept: result.report.kept_count,
    })
}

/// The threshold-derivation rule in effect; bump when the placement
/// algorithm changes so recorded verdicts stay interpretable.
const GATE_VERSION: u32 = 2;

/// Gate the transcript's mic segments on the session's energy anchors,
/// record valid anchors into the per-device store, and write the verdicts to
/// the session's flight-recorder sidecar. Multi-local-speaker sessions are
/// measured but never gated (a quiet second speaker reads as residue). Drops
/// nothing and records nothing when anchors are missing or weakly separated.
pub(crate) fn apply_energy_gate(
    st: &mut SessionTranscript,
    session_dir: &std::path::Path,
    profile_root: &std::path::Path,
    session_id: &str,
    metrics_enabled: bool,
) -> usize {
    let probe = crate::commands::manifest_gate_probe(session_dir);
    let out =
        transcript::energy_gate::gate_session(st, session_dir, probe.single_local_speaker);
    log::info!(
        "energy gate for {session_id}: speech={:?} residue={:?} threshold={:?} ({} speech / {} residue windows), apply={}, dropped {}",
        out.anchors.speech_dbfs, out.anchors.residue_dbfs, out.anchors.threshold_dbfs,
        out.anchors.speech_windows, out.anchors.residue_windows,
        probe.single_local_speaker, out.dropped
    );

    // Only threshold-valid anchors are trustworthy enough to teach the
    // device store — a listen-only meeting's "speech" windows are breathing
    // and keyboard noise.
    if let (Some(dev), Some(speech), true) = (
        probe.mic_description.as_deref(),
        out.anchors.speech_dbfs,
        out.anchors.threshold_dbfs.is_some(),
    ) {
        let mut store = recording::speech_levels::SpeechLevels::load(profile_root);
        store.record(dev, recording::speech_levels::LevelSample {
            at_unix: crate::now_unix(),
            session_id: Some(session_id.to_string()),
            source: recording::speech_levels::LevelSource::Meeting,
            speech_dbfs: speech,
            residue_dbfs: out.anchors.residue_dbfs,
        });
        if let Err(e) = store.save(profile_root) {
            log::warn!("speech_levels save failed: {e}");
        }
    }

    // Flight recorder (debug logging only): the gate summary plus one
    // verdict per mic segment, keyed by session-absolute stream time.
    let fr = recording::flight_recorder::FlightRecorder::open_if(metrics_enabled, session_dir);
    fr.energy_gate(
        GATE_VERSION,
        out.anchors.speech_dbfs,
        out.anchors.residue_dbfs,
        out.anchors.threshold_dbfs,
        out.anchors.speech_windows,
        out.anchors.residue_windows,
        probe.single_local_speaker,
        out.dropped,
    );
    let offsets = chunk_offsets_ms(session_dir);
    for p in &out.segment_peaks {
        let base = i64::from(offsets.get(&p.chunk_index).copied().unwrap_or(0));
        fr.segment_gate(base + i64::from(p.start_ms), base + i64::from(p.end_ms), p.peak_dbfs, p.kept);
    }
    out.dropped
}

/// Diarized cluster_id → display name lookup from the session's manifest.
/// Empty when the manifest is missing or no speakers are labelled.
fn speaker_map_for(session_dir: &std::path::Path) -> HashMap<u32, String> {
    let Ok(bytes) = syncsafe::read(session_dir.join("manifest.json")) else {
        return HashMap::new();
    };
    let Ok(m) = serde_json::from_slice::<recording::manifest::SessionManifest>(&bytes) else {
        return HashMap::new();
    };
    m.speaker_map
        .into_iter()
        .map(|s| (s.cluster_id, s.display_name))
        .collect()
}

/// `chunk_index -> ms from the meeting start to that chunk's start`, read from
/// the session manifest. Empty (all offsets 0) if the manifest can't be read.
fn chunk_offsets_ms(session_dir: &std::path::Path) -> HashMap<u32, u32> {
    let mut out = HashMap::new();
    let Ok(bytes) = syncsafe::read(session_dir.join("manifest.json")) else {
        return out;
    };
    #[derive(serde::Deserialize)]
    struct M {
        created_at_unix_seconds: i64,
        #[serde(default)]
        chunks: Vec<C>,
    }
    #[derive(serde::Deserialize)]
    struct C {
        index: u32,
        #[serde(default)]
        started_at_unix_seconds: Option<i64>,
    }
    let Ok(m) = serde_json::from_slice::<M>(&bytes) else {
        return out;
    };
    const MAX_OFFSET_SECS: i64 = 7 * 86_400;
    for c in &m.chunks {
        if let Some(started) = c.started_at_unix_seconds {
            let secs = (started - m.created_at_unix_seconds).clamp(0, MAX_OFFSET_SECS) as u64;
            out.insert(c.index, (secs.saturating_mul(1000)).min(u32::MAX as u64) as u32);
        }
    }
    out
}

#[derive(Debug, serde::Serialize)]
pub struct DedupSummary {
    pub dropped: usize,
    pub kept: usize,
}

#[derive(Debug, Deserialize)]
pub struct PolishRequest {
    pub session_id: String,
    /// Which summary provider's LLM to use. Defaults to
    /// `settings.default_summary_provider` when None.
    pub provider: Option<ProviderId>,
    /// Model override; falls back to the provider's vault-stored model.
    pub model: Option<String>,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct PolishSummary {
    pub batches: u32,
    pub segments_polished: u32,
    pub segments_unchanged: u32,
    pub failed_batches: u32,
}

/// Polish the deduped transcript via the configured AI provider. Writes back
/// to `transcript.dedup.json` and re-renders `transcript.md`. Best-effort:
/// any batch failure logs and keeps the raw text. No-op when no AI provider
/// is configured.
pub fn polish_impl(
    app: &AppState,
    vs: &crate::state::VaultState,
    req: PolishRequest,
) -> Result<PolishSummary> {
    let session = app.profile.session_path(&req.session_id);
    if !session.is_dir() {
        return Err(AppError::SessionNotFound(req.session_id.clone()));
    }

    let settings = crate::settings::Settings::load_or_default(&app.profile.settings_path());
    let provider_id = match req.provider.or(settings.default_summary_provider) {
        Some(p) => p,
        None => {
            log::info!("polish: skipping (no AI provider configured)");
            return Ok(PolishSummary::default());
        }
    };
    let (provider_cfg, gateway): (
        Option<ProviderConfig>,
        Option<summarize::gateway::GatewayCreds>,
    ) = {
        let g = vs.keys.lock().unwrap();
        match g.as_ref() {
            Some(keys) => {
                let cfg = keys.providers.get(&provider_id).cloned();
                let gw = if provider_id == crate::state::ProviderId::DaisyGateway {
                    Some(crate::commands::summary::gateway_creds_from_keys(keys, "polish")?)
                } else {
                    None
                };
                (cfg, gw)
            }
            None => (None, None),
        }
    };
    // Provider-agnostic chat completer (Anthropic tool-use or OpenAI-compat
    // json_object), shared read-only across the rayon polish pool.
    let completer = crate::commands::summary::build_chat_completer_for(
        provider_id,
        provider_cfg.as_ref(),
        gateway,
    )?;

    let dedup_path = session.join("transcript.dedup.json");
    let bytes = syncsafe::read(&dedup_path)
        .map_err(|e| AppError::Config(format!("read transcript.dedup.json: {e}")))?;
    let mut st: SessionTranscript = serde_json::from_slice(&bytes)?;

    let mut summary = PolishSummary::default();

    // Each chunk is an independent LLM round-trip. Owned per-chunk work units
    // are built first, then the API calls run concurrently across a small
    // bounded pool.
    struct ChunkWork {
        chunk_pos: usize,
        positions: Vec<(usize, usize)>,
        segs: Vec<(&'static str, String)>,
    }
    let work: Vec<ChunkWork> = st
        .chunks
        .iter()
        .enumerate()
        .filter_map(|(ci, chunk)| {
            let mut positions = Vec::new();
            let mut segs = Vec::new();
            for (ti, tr) in chunk.tracks.iter().enumerate() {
                let label = match tr.track {
                    transcript::Track::MicAec | transcript::Track::Mic => "Me",
                    transcript::Track::System => "Them",
                };
                for (si, seg) in tr.segments.iter().enumerate() {
                    positions.push((ti, si));
                    segs.push((label, seg.text.clone()));
                }
            }
            if segs.is_empty() {
                None
            } else {
                Some(ChunkWork { chunk_pos: ci, positions, segs })
            }
        })
        .collect();
    summary.batches = work.len() as u32;

    const POLISH_CONCURRENCY: usize = 5;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(POLISH_CONCURRENCY)
        .build()
        .map_err(|e| AppError::Config(format!("polish pool: {e}")))?;
    // `par_iter().collect()` preserves input order: results[k] ↔ work[k].
    let results: Vec<std::result::Result<Vec<String>, ()>> = pool.install(|| {
        use rayon::prelude::*;
        work.par_iter()
            .map(|w| {
                let batch: Vec<summarize::polish::PolishSegment<'_>> = w
                    .segs
                    .iter()
                    .map(|(label, text)| summarize::polish::PolishSegment {
                        track: label,
                        text: text.as_str(),
                    })
                    .collect();
                match summarize::polish::polish_batch(completer.as_ref(), &batch) {
                    Ok(c) if c.len() == batch.len() => Ok(c),
                    Ok(_) => {
                        log::warn!("polish chunk-pos {}: length mismatch — keeping raw", w.chunk_pos);
                        Err(())
                    }
                    Err(e) => {
                        log::warn!("polish chunk-pos {}: {e}", w.chunk_pos);
                        Err(())
                    }
                }
            })
            .collect()
    });

    for (w, res) in work.iter().zip(results.into_iter()) {
        let cleaned = match res {
            Ok(c) => c,
            Err(()) => {
                summary.failed_batches += 1;
                continue;
            }
        };
        let chunk = &mut st.chunks[w.chunk_pos];
        for (&(ti, si), new_text) in w.positions.iter().zip(cleaned.into_iter()) {
            // Re-mask: persisted text is always profanity-masked.
            let new_text = transcript::text::mask_profanity(&new_text);
            if let Some(tr) = chunk.tracks.get_mut(ti) {
                if let Some(seg) = tr.segments.get_mut(si) {
                    if new_text == seg.text {
                        summary.segments_unchanged += 1;
                    } else {
                        seg.text = new_text;
                        summary.segments_polished += 1;
                    }
                }
            }
        }
    }

    syncsafe::write(&dedup_path, serde_json::to_vec_pretty(&st)?)?;
    let offsets = chunk_offsets_ms(&session);
    let speakers = speaker_map_for(&session);
    let md = render_markdown_with_speakers(&st, &offsets, &speakers);
    syncsafe::write(session.join("transcript.md"), md.as_bytes())?;

    Ok(summary)
}

fn infer_size_from_path(p: &std::path::Path) -> Option<String> {
    let stem = p.file_name()?.to_str()?;
    // e.g. "ggml-base.en.bin" → "base.en"
    let s = stem.strip_prefix("ggml-")?.strip_suffix(".bin")?;
    Some(s.to_string())
}

/// Build the on-device Whisper transcriber. The "model" is a GGML file path;
/// a missing file returns `AppError::ModelMissing`. Meeting proper-noun
/// terms (title/attendees/tags) become the initial_prompt sentence.
pub(crate) fn build_whisper_provider(
    model_path: Option<String>,
    terms: Vec<String>,
) -> Result<Box<dyn Transcriber>> {
    let path: std::path::PathBuf = model_path
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    if !path.is_file() {
        let size = infer_size_from_path(&path).unwrap_or_else(|| "base.en".into());
        return Err(AppError::ModelMissing { size });
    }
    Ok(Box::new(
        providers_local::WhisperLocalTranscriber::new(&path)
            .map_err(|e| AppError::Config(format!("load whisper model: {e}")))?
            .with_initial_prompt(crate::commands::transcribe_priming::vocab_sentence(&terms)),
    ))
}

#[cfg(test)]
mod whisper_provider_tests {
    use super::*;
    use crate::error::AppError;

    #[test]
    fn whisper_local_with_no_model_path_returns_model_missing() {
        let result = build_whisper_provider(None, Vec::new());
        let err = result.err().expect("expected Err, got Ok");
        match err {
            AppError::ModelMissing { size } => assert_eq!(size, "base.en"),
            other => panic!("expected ModelMissing, got {other:?}"),
        }
    }

    #[test]
    fn whisper_local_with_nonexistent_path_returns_model_missing() {
        let result = build_whisper_provider(
            Some("/definitely/not/here/ggml-base.en.bin".into()),
            Vec::new(),
        );
        let err = result.err().expect("expected Err, got Ok");
        assert!(matches!(err, AppError::ModelMissing { .. }));
    }
}

#[cfg(test)]
mod energy_gate_tests {
    use super::*;
    use crate::test_audio::{tone, write_wav};
    use transcript::model::{ChunkTranscript, Segment, Track, TrackTranscript};

    #[test]
    fn gates_residue_and_records_device_anchor() {
        let root = std::env::temp_dir().join(format!("daisy-egate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let session = root.join("sessions/daisy-1");
        syncsafe::create_dir_all(session.join("chunks/0001")).unwrap();
        syncsafe::write(
            session.join("manifest.json"),
            br#"{"mic_source_description":"Microphone (Test BRIO)"}"#,
        )
        .unwrap();
        // 30 s chunk: 0-15 s user speaking (-12 dB) with the system silent;
        // 15-30 s system active (-20 dB) with mic residue (-40 dB).
        let mut mic = tone(15, 0.25);
        mic.extend(tone(15, 0.01));
        let mut sys = vec![0i16; 16_000 * 15];
        sys.extend(tone(15, 0.1));
        write_wav(&session.join("chunks/0001/mic_aec.wav"), &mic);
        write_wav(&session.join("chunks/0001/system.wav"), &sys);

        let seg = |s: u32, e: u32, t: &str| Segment {
            start_ms: s,
            end_ms: e,
            text: t.into(),
            confidence: None,
            speaker_id: None,
        };
        let mut st = SessionTranscript {
            schema_version: SessionTranscript::SCHEMA,
            session_id: "daisy-1".into(),
            provider: "t".into(),
            model: "t".into(),
            transcribed_at_unix_seconds: 0,
            chunks: vec![ChunkTranscript {
                chunk_index: 1,
                tracks: vec![
                    TrackTranscript {
                        track: Track::MicAec,
                        source_wav_relative: "chunks/0001/mic_aec.wav".into(),
                        segments: vec![seg(1_000, 5_000, "real"), seg(20_000, 24_000, "ghost")],
                    },
                    TrackTranscript {
                        track: Track::System,
                        source_wav_relative: "chunks/0001/system.wav".into(),
                        segments: vec![seg(16_000, 29_000, "remote")],
                    },
                ],
            }],
        };
        let dropped = apply_energy_gate(&mut st, &session, &root, "daisy-1", true);
        assert_eq!(dropped, 1);
        let mic_t = st.chunks[0].tracks.iter().find(|t| t.track == Track::MicAec).unwrap();
        assert_eq!(mic_t.segments.len(), 1);
        assert_eq!(mic_t.segments[0].text, "real");
        let store = recording::speech_levels::SpeechLevels::load(&root);
        assert_eq!(store.devices["Microphone (Test BRIO)"].history.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
