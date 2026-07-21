//! The Summarize button's single backend trigger: drain pending live transcription,
//! run final-pass transcription, run dedup, then generate the AI summary. Emits
//! ProgressEvents via a callback (the Tauri wrapper turns those into emitted events).

use crate::error::{AppError, Result};
use crate::state::{AppState, ProviderId, VaultState};
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize)]
pub struct FinalizeRequest {
    pub session_id: String,
    pub summary_provider: Option<ProviderId>,
    pub model: Option<String>,
    /// When false (default), the cascade pauses before the summary if any
    /// diarized speaker cluster is still unlabeled, returning
    /// `FinalizeOutcome::NeedsLabels`. The resume command sets this true to
    /// skip the gate and run the summary tail.
    #[serde(default)]
    pub skip_label_gate: bool,
}

/// Result of the finalize cascade. Either the summary completed, or the
/// cascade paused at the speaker-label gate and is waiting for the user to
/// label the listed clusters and call `recording_resume_finalize`.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FinalizeOutcome {
    /// Transcript + audio shipped. `summary` is `None` when summary
    /// generation failed; the transcript is saved regardless.
    Completed { summary: Option<summarize::SessionSummary> },
    NeedsLabels { clusters: Vec<u32> },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProgressEvent {
    pub stage: String,
    pub progress: f32,
    pub message: Option<String>,
}

/// Live finalize status, written to `<session>/finalize.status.json` at each
/// stage boundary. Read by the SessionStatus widget and the startup
/// crash-recovery audit. Single-writer, atomic temp+rename.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FinalizeStatus {
    pub stage: String,
    pub progress: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub updated_at_unix: i64,
}

pub const FINALIZE_STATUS_FILE: &str = "finalize.status.json";

/// Atomically write the finalize status sidecar. Best-effort: logs on failure,
/// never aborts the cascade.
pub fn write_finalize_status(session_dir: &std::path::Path, stage: &str, progress: f32, message: Option<&str>) {
    let status = FinalizeStatus {
        stage: stage.to_string(),
        progress,
        message: message.map(|s| s.to_string()),
        updated_at_unix: crate::now_unix(),
    };
    let path = session_dir.join(FINALIZE_STATUS_FILE);
    let tmp = path.with_extension("json.tmp");
    match serde_json::to_vec(&status) {
        Ok(bytes) => {
            if syncsafe::write(&tmp, &bytes).and_then(|_| syncsafe::rename(&tmp, &path)).is_err() {
                log::warn!("finalize status sidecar write failed for {}", session_dir.display());
            }
        }
        Err(e) => log::warn!("finalize status encode failed: {e}"),
    }
}

/// Read the finalize status sidecar, or None if absent/unparseable.
pub fn read_finalize_status(session_dir: &std::path::Path) -> Option<FinalizeStatus> {
    let bytes = syncsafe::read(session_dir.join(FINALIZE_STATUS_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Read the finalize status, reconciled against the manifest.
///
/// The manifest is authoritative: when `finalized_at_unix_seconds` is set and
/// the session is not crash-`interrupted`, a non-terminal sidecar is reported
/// as `done`. Pure read — the stale file is left untouched. Sessions with no
/// `finalized_at`, or with `interrupted: true`, pass through unchanged.
pub fn read_finalize_status_reconciled(session_dir: &std::path::Path) -> Option<FinalizeStatus> {
    let status = read_finalize_status(session_dir)?;
    let terminal = matches!(status.stage.as_str(), "done" | "error" | "awaiting-labels");
    if !terminal && manifest_is_finalized(session_dir) {
        return Some(FinalizeStatus {
            stage: "done".into(),
            progress: 1.0,
            message: None,
            updated_at_unix: status.updated_at_unix,
        });
    }
    Some(status)
}

/// True if the session's manifest has `finalized_at` set and is not flagged
/// crash-`interrupted`.
fn manifest_is_finalized(session_dir: &std::path::Path) -> bool {
    #[derive(serde::Deserialize)]
    struct Probe {
        finalized_at_unix_seconds: Option<i64>,
        #[serde(default)]
        interrupted: bool,
    }
    syncsafe::read(session_dir.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Probe>(&b).ok())
        .map(|m| m.finalized_at_unix_seconds.is_some() && !m.interrupted)
        .unwrap_or(false)
}

/// Orphan-recovery bookkeeping, written to `<session>/finalize.recovery.json`.
/// Orphan recovery counts attempts here and sets `failed` once
/// `FINALIZE_MAX_ATTEMPTS` is hit. Cleared on success or a user Retry.
pub const FINALIZE_RECOVERY_FILE: &str = "finalize.recovery.json";

/// Give up after this many crashing finalize attempts.
pub const FINALIZE_MAX_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FinalizeRecovery {
    #[serde(default)]
    pub attempts: u32,
    /// Given up after exceeding the attempt cap.
    #[serde(default)]
    pub failed: bool,
    /// Human-readable reason shown to the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub updated_at_unix: i64,
}

pub fn read_finalize_recovery(session_dir: &std::path::Path) -> FinalizeRecovery {
    syncsafe::read(session_dir.join(FINALIZE_RECOVERY_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn write_finalize_recovery(session_dir: &std::path::Path, rec: &FinalizeRecovery) {
    let path = session_dir.join(FINALIZE_RECOVERY_FILE);
    let tmp = path.with_extension("json.tmp");
    if let Ok(bytes) = serde_json::to_vec(rec) {
        if syncsafe::write(&tmp, &bytes).and_then(|_| syncsafe::rename(&tmp, &path)).is_err() {
            log::warn!("finalize recovery sidecar write failed for {}", session_dir.display());
        }
    }
}

/// Clear the recovery sidecar — on a successful finalize or a user "Retry".
pub fn clear_finalize_recovery(session_dir: &std::path::Path) {
    let _ = syncsafe::remove_file(session_dir.join(FINALIZE_RECOVERY_FILE));
}

/// Emit a stage transition: logs the wall-clock duration of the previous
/// stage, writes the on-disk status sidecar (when a session dir is given),
/// and invokes `emit`. Returns the new "stage start" instant for the next
/// call.
fn next_stage(
    emit: &mut dyn FnMut(ProgressEvent),
    session_dir: Option<&std::path::Path>,
    last: Instant,
    prev_label: Option<&str>,
    stage: &str,
    progress: f32,
    message: Option<&str>,
) -> Instant {
    let elapsed = last.elapsed().as_secs_f32();
    if let Some(prev) = prev_label {
        log::info!("finalize stage {prev} took {elapsed:.1}s");
    }
    log::info!("finalize stage {stage} starting (progress {progress:.2})");
    if let Some(dir) = session_dir {
        write_finalize_status(dir, stage, progress, message);
    }
    emit(ProgressEvent {
        stage: stage.into(),
        progress,
        message: message.map(|s| s.into()),
    });
    Instant::now()
}

/// Run the finalize cascade: final-pass transcription -> dedup -> AI summary,
/// then mark the manifest finalized. `emit` is invoked at each stage boundary;
/// the Tauri wrapper turns those calls into emitted frontend events.
pub fn finalize_and_summarize_impl(
    app: &AppState,
    vs: &VaultState,
    req: FinalizeRequest,
    mut emit: impl FnMut(ProgressEvent),
) -> Result<FinalizeOutcome> {
    let cascade_start = Instant::now();
    // Lift the BLAS thread cap for the cascade; restored to 1 on drop.
    let _blas_guard = OpenBlasUncapGuard::full_parallelism_for_finalize();
    let session_root = app.profile.session_path(&req.session_id);
    let mut last = next_stage(
        &mut emit,
        Some(&session_root),
        cascade_start,
        None,
        "finalizing",
        0.05,
        Some("closing session"),
    );

    // Run AEC before transcription; the orchestrator picks up mic_aec.wav.
    // Idempotent: chunks whose mic_aec.wav already exists are skipped.
    last = next_stage(
        &mut emit,
        Some(&session_root),
        last,
        Some("finalizing"),
        "echo-cancelling",
        0.10,
        Some("removing speaker bleed"),
    );
    // AEC + DFN3 denoise write one output file per chunk (mic_aec.wav,
    // mic_dn.wav). A background sampler counts those on disk against the
    // total and re-stamps the status sidecar with the current % while the
    // passes run.
    let denoise_enabled =
        crate::settings::Settings::load_or_default(&app.profile.settings_path()).denoise_enabled;
    let chunk_dirs: Vec<std::path::PathBuf> = std::fs::read_dir(session_root.join("chunks"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    // Work units: one per chunk for AEC, plus one per chunk for denoise when on.
    let total_units = (chunk_dirs.len() * if denoise_enabled { 2 } else { 1 }).max(1);
    let aec_hb_stop = Arc::new(AtomicBool::new(false));
    let aec_hb = {
        let dir = session_root.clone();
        let stop = aec_hb_stop.clone();
        let chunk_dirs = chunk_dirs.clone();
        std::thread::spawn(move || {
            let mut elapsed = 0u32;
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(1));
                elapsed += 1;
                if elapsed % 3 != 0 || stop.load(Ordering::Relaxed) {
                    continue; // ~3 s cadence
                }
                let done: usize = chunk_dirs
                    .iter()
                    .map(|d| {
                        d.join("mic_aec.wav").is_file() as usize
                            + (denoise_enabled && d.join("mic_dn.wav").is_file()) as usize
                    })
                    .sum();
                let frac = (done as f32 / total_units as f32).min(1.0);
                // echo-cancelling owns the 0.10..0.15 progress band.
                let progress = 0.10 + 0.05 * frac;
                let msg = format!("removing speaker bleed ({}%)", (frac * 100.0) as u32);
                write_finalize_status(&dir, "echo-cancelling", progress, Some(&msg));
            }
        })
    };

    if let Err(e) = recording::apply_aec(&session_root) {
        log::warn!("AEC for {}: {e}", req.session_id);
    }

    // DFN3 denoise (mic_dn.wav sidecar for opus + diarization). Runs before
    // the background mixdown below; meeting.opus picks up mic_dn.wav. Any
    // failure falls back to un-denoised audio downstream. Whisper never reads
    // mic_dn.
    if denoise_enabled {
        if let Err(e) = recording::apply_denoise(&session_root) {
            log::warn!("denoise for {} (continuing without): {e}", req.session_id);
        }
    } else {
        log::debug!("denoise disabled in settings; skipping");
    }

    // Stop the sampler before the next stage stamps its own status.
    aec_hb_stop.store(true, Ordering::Relaxed);
    let _ = aec_hb.join();

    // Background mixdown: the meeting.opus build starts after AEC completes
    // and overlaps the transcribe + dedup + LLM stages; the compressing stage
    // checks for its output.
    let _mixdown_handle = {
        let session_id = req.session_id.clone();
        let dir = session_root.clone();
        let manifest_now: Option<recording::manifest::SessionManifest> =
            syncsafe::read(dir.join("manifest.json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
        manifest_now.map(|m| {
            std::thread::spawn(move || {
                match recording::mixdown::build_meeting_audio(
                    &dir,
                    &m,
                    &recording::compress::CompressParams::default(),
                ) {
                    Ok(n) => log::info!(
                        "built meeting.opus for {} ({} bytes) — background overlap",
                        session_id, n
                    ),
                    Err(e) => log::warn!("meeting.opus for {}: {e}", session_id),
                }
            })
        })
    };

    last = next_stage(&mut emit, Some(&session_root), last, Some("echo-cancelling"), "transcribing", 0.15, None);
    // Three ways to get transcript.json, in order:
    //   1. Already on disk: skip.
    //   2. Promote a complete live transcript: synthesize transcript.json
    //      from live_transcript.jsonl.
    //   3. Fall back to the local whisper full-pass over the WAVs when the
    //      live transcript is missing or incomplete.
    if session_root.join("transcript.json").is_file() {
        log::info!(
            "finalize: transcript.json already on disk for {} — skipping transcribe",
            req.session_id
        );
    } else {
        let manifest_for_promote: Option<recording::manifest::SessionManifest> =
            syncsafe::read(session_root.join("manifest.json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
        let promotion = manifest_for_promote
            .as_ref()
            .and_then(|m| try_promote_live_transcript(&session_root, m, &req.session_id));
        match promotion {
            // Promoted: patch coverage gaps from the WAVs when present, then
            // re-decode the mic track from the AEC-cleaned audio. Both steps
            // are best-effort: on failure the promoted transcript stays.
            Some(promo) => {
                let settings =
                    crate::settings::Settings::load_or_default(&app.profile.settings_path());
                let model_path = settings
                    .whisper_model_path
                    .clone()
                    .map(std::path::PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("DAISY_WHISPER_MODEL_DIR")
                            .map(|d| std::path::PathBuf::from(d).join("ggml-base.en.bin"))
                    });
                match model_path {
                    Some(mp) => {
                        if !promo.gaps.is_empty() {
                            match crate::commands::gap_patch::patch_gaps_on_disk(
                                &session_root,
                                &mp,
                                &promo.gaps,
                                &promo.spans,
                            ) {
                                Ok(n) => log::info!(
                                    "finalize: patched {} gap(s) for {}, recovered {} seg(s) — skipped full pass",
                                    promo.gaps.len(),
                                    req.session_id,
                                    n
                                ),
                                Err(e) => log::warn!(
                                    "finalize: gap-patch failed for {}: {e} — keeping promoted live transcript",
                                    req.session_id
                                ),
                            }
                        }
                        write_finalize_status(
                            &session_root,
                            "transcribing",
                            0.35,
                            Some("re-decoding your mic track"),
                        );
                        match crate::commands::gap_patch::redecode_mic_on_disk(
                            &session_root,
                            &mp,
                            &promo.spans,
                        ) {
                            Ok((old, new)) => log::info!(
                                "finalize: mic re-decode for {}: {} live seg(s) replaced by {} whisper seg(s)",
                                req.session_id,
                                old,
                                new
                            ),
                            Err(e) => log::warn!(
                                "finalize: mic re-decode failed for {}: {e} — keeping promoted mic segments",
                                req.session_id
                            ),
                        }
                    }
                    None => log::warn!(
                        "finalize: no whisper model for {} — keeping promoted live transcript as-is",
                        req.session_id
                    ),
                }
            }
            // Live transcript missing/too incomplete → full whisper pass.
            None => {
                // Per-chunk progress across the transcribing band (0.15 → 0.6).
                let progress = |done: usize, total: usize| {
                    let p = if total > 0 {
                        0.15 + (done as f32 / total as f32) * 0.45
                    } else {
                        0.15
                    };
                    write_finalize_status(
                        &session_root,
                        "transcribing",
                        p,
                        Some(&format!("transcribed {done}/{total} chunks")),
                    );
                };
                // The progress callback fires only at chunk boundaries. A
                // timer re-stamps the sidecar's current value between
                // callbacks; the callback stays the sole % author.
                let tx_hb_stop = Arc::new(AtomicBool::new(false));
                let tx_hb = {
                    let dir = session_root.clone();
                    let stop = tx_hb_stop.clone();
                    std::thread::spawn(move || {
                        let mut elapsed = 0u32;
                        while !stop.load(Ordering::Relaxed) {
                            std::thread::sleep(Duration::from_secs(1));
                            elapsed += 1;
                            if elapsed % 30 != 0 || stop.load(Ordering::Relaxed) {
                                continue;
                            }
                            if let Some(s) = read_finalize_status(&dir) {
                                if s.stage == "transcribing" {
                                    write_finalize_status(&dir, &s.stage, s.progress, s.message.as_deref());
                                }
                            }
                        }
                    })
                };
                let res = crate::commands::pipeline::transcribe_impl(
                    app,
                    crate::commands::pipeline::TranscribeRequest {
                        session_id: req.session_id.clone(),
                        model: None,
                    },
                    Some(&progress),
                );
                tx_hb_stop.store(true, Ordering::Relaxed);
                let _ = tx_hb.join();
                res.map_err(|e| {
                    let err = AppError::Config(format!("transcribe: {e}"));
                    write_finalize_status(&session_root, "error", 0.15, Some(&err.to_string()));
                    err
                })?;
            }
        }
    }

    last = next_stage(&mut emit, Some(&session_root), last, Some("transcribing"), "deduping", 0.6, None);

    // Per-step timings inside the deduping stage (dedup, diarize, voiceprint
    // match, transcript.md re-render), logged at INFO.
    let dedup_t = Instant::now();
    if !session_root.join("transcript.dedup.json").is_file() {
        crate::commands::pipeline::dedup_impl(
            app,
            crate::commands::pipeline::DedupRequest {
                session_id: req.session_id.clone(),
            },
        )
        .map_err(|e| {
            let err = AppError::Config(format!("dedup: {e}"));
            write_finalize_status(&session_root, "error", 0.6, Some(&err.to_string()));
            err
        })?;
    } else {
        log::info!(
            "finalize: transcript.dedup.json already on disk for {} — skipping dedup",
            req.session_id
        );
    }
    log::info!(
        "  deduping.dedup_impl took {:.1}s",
        dedup_t.elapsed().as_secs_f32()
    );

    // Local diarization: when the transcriber didn't label speakers, cluster
    // the system segments by voice embedding. Runs before voiceprint
    // matching, which names those clusters.
    let diarize_t = Instant::now();
    if crate::commands::voiceprints::transcript_undiarized(app, &req.session_id) {
        // Manifest-pinned speaker count (set at import); None = auto-estimate.
        let expected = crate::commands::summary::load_manifest(app, &req.session_id)
            .ok()
            .and_then(|m| m.expected_speakers);
        match crate::commands::voiceprints::diarize_session_impl(app, &req.session_id, expected, None) {
            Ok(r) => log::info!(
                "local diarization for {}: {} speaker(s), {} segment(s)",
                req.session_id, r.speakers, r.segments_labeled
            ),
            Err(ref e) if e.to_string().contains("model missing") => {
                // Voiceprint model not installed: flag the manifest.
                log::warn!("local diarization for {}: voiceprint model missing — flagging manifest", req.session_id);
                let mp = app.profile.session_path(&req.session_id).join("manifest.json");
                if let Ok(bytes) = syncsafe::read(&mp) {
                    if let Ok(mut m) = serde_json::from_slice::<recording::manifest::SessionManifest>(&bytes) {
                        m.diarization_unavailable = true;
                        let tmp = mp.with_extension("json.tmp");
                        let _ = syncsafe::write(&tmp, serde_json::to_vec_pretty(&m).unwrap_or_default());
                        let _ = syncsafe::rename(&tmp, &mp);
                    }
                }
            }
            Err(e) => log::warn!("local diarization for {}: {e}", req.session_id),
        }
    }
    log::info!(
        "  deduping.diarize took {:.1}s",
        diarize_t.elapsed().as_secs_f32()
    );

    // Voiceprint auto-match: for any diarized cluster without a manual label,
    // embed a sample of its audio and look it up in the vault. Best-effort.
    let vp_t = Instant::now();
    if let Ok(n) =
        crate::commands::voiceprints::rematch_session_speakers_impl(app, vs, &req.session_id)
    {
        if n > 0 {
            log::info!("voiceprint auto-match: {n} cluster(s) labelled for {}", req.session_id);
            // Re-render transcript.md with the new labels.
            let rerender_t = Instant::now();
            let _ = crate::commands::session::rerender_session_transcript_impl(
                app,
                &req.session_id,
            );
            log::info!(
                "  deduping.rerender_transcript took {:.1}s",
                rerender_t.elapsed().as_secs_f32()
            );
        }
    }
    log::info!(
        "  deduping.voiceprint_match took {:.1}s",
        vp_t.elapsed().as_secs_f32()
    );

    // Speaker-label gate: if any diarized cluster is still unlabeled, pause
    // and return NeedsLabels. The resume command runs the tail with
    // skip_label_gate = true.
    if !req.skip_label_gate {
        let unlabeled = crate::commands::voiceprints::unlabeled_clusters_impl(app, &req.session_id)
            .unwrap_or_default();
        if !unlabeled.is_empty() {
            // The sidecar message carries the unlabeled cluster ids as a
            // JSON array.
            let clusters_json = serde_json::to_string(&unlabeled).unwrap_or_else(|_| "[]".to_string());
            let _ = next_stage(
                &mut emit,
                Some(&session_root),
                last,
                Some("deduping"),
                "awaiting-labels",
                0.75,
                Some(&clusters_json),
            );
            log::info!(
                "finalize paused at label gate for {}: {} unlabeled cluster(s)",
                req.session_id,
                unlabeled.len()
            );
            return Ok(FinalizeOutcome::NeedsLabels { clusters: unlabeled });
        }
    }

    let summary = run_finalize_tail(app, vs, &req, &mut emit, last).map_err(|e| {
        write_finalize_status(&session_root, "error", 1.0, Some(&e.to_string()));
        e
    })?;
    log::info!(
        "finalize cascade for {} took {:.1}s total",
        req.session_id,
        cascade_start.elapsed().as_secs_f32()
    );
    Ok(FinalizeOutcome::Completed { summary })
}

/// Run a finalize cascade that was paused at the speaker-label gate: skips
/// straight to the summary tail.
pub fn resume_finalize_impl(
    app: &AppState,
    vs: &VaultState,
    req: FinalizeRequest,
    mut emit: impl FnMut(ProgressEvent),
) -> Result<FinalizeOutcome> {
    let start = Instant::now();
    let _blas_guard = OpenBlasUncapGuard::full_parallelism_for_finalize();
    let session_root = app.profile.session_path(&req.session_id);
    let last = next_stage(&mut emit, Some(&session_root), start, None, "finalizing", 0.70, Some("resuming"));
    let summary = run_finalize_tail(app, vs, &req, &mut emit, last).map_err(|e| {
        write_finalize_status(&session_root, "error", 1.0, Some(&e.to_string()));
        e
    })?;
    Ok(FinalizeOutcome::Completed { summary })
}

/// The tail of the cascade: summary -> chapters -> compress -> finalize-stamp
/// -> done. Shared by the normal path and the resume path.
fn run_finalize_tail(
    app: &AppState,
    vs: &VaultState,
    req: &FinalizeRequest,
    emit: &mut dyn FnMut(ProgressEvent),
    last: Instant,
) -> Result<Option<summarize::SessionSummary>> {
    let session_root = app.profile.session_path(&req.session_id);
    // prev_label None: the stage before summarizing differs between the
    // normal and resume paths.
    let mut last = next_stage(emit, Some(&session_root), last, None, "summarizing", 0.8, None);
    // Summary is best-effort: on failure the transcript, chapters, compressed
    // audio, and finalize stamp still ship.
    let summary: Option<summarize::SessionSummary> =
        match crate::commands::summary::summary_generate_impl(
            app,
            vs,
            crate::commands::summary::SummaryGenerateRequest {
                session_id: req.session_id.clone(),
                provider: req.summary_provider,
                model: req.model.clone(),
                force: Some(false),
                prompt_id: None,
            },
        ) {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!(
                    "summary for {} skipped (transcript saved anyway): {e}",
                    req.session_id
                );
                None
            }
        };

    // Topic chapters: best-effort; failure logs and continues.
    last = next_stage(
        emit,
        Some(&session_root),
        last,
        Some("summarizing"),
        "chaptering",
        0.88,
        Some("identifying topics"),
    );
    match crate::commands::chapters::extract_chapters_impl(
        app,
        vs,
        crate::commands::chapters::ChaptersRequest {
            session_id: req.session_id.clone(),
            provider: req.summary_provider,
            model: req.model.clone(),
        },
    ) {
        Ok(r) if r.skipped => log::info!("chapters skipped for {}: {}", req.session_id, r.reason.unwrap_or_default()),
        Ok(r) => log::info!("chapters: {} extracted for {}", r.chapters.len(), req.session_id),
        Err(e) => log::warn!("chapters for {}: {e}", req.session_id),
    }

    // Read the manifest once — used for both the mixdown and the finalized-stamp.
    let mp = app
        .profile
        .session_path(&req.session_id)
        .join("manifest.json");
    let manifest: Option<recording::manifest::SessionManifest> = syncsafe::read(&mp)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());

    last = next_stage(
        emit,
        Some(&session_root),
        last,
        Some("chaptering"),
        "compressing",
        0.92,
        Some("compressing audio"),
    );
    if let Some(m) = manifest.as_ref() {
        let session_dir = app.profile.session_path(&req.session_id);
        let opus_path = session_dir.join(recording::mixdown::MEETING_AUDIO_NAME);
        if opus_path.is_file() {
            // The background mixdown already produced meeting.opus.
            log::info!(
                "meeting.opus for {} already on disk ({} bytes) — background mixdown beat the cascade",
                req.session_id,
                std::fs::metadata(&opus_path).map(|md| md.len()).unwrap_or(0),
            );
        } else {
            // No meeting.opus yet: build it on the cascade thread.
            match recording::mixdown::build_meeting_audio(
                &session_dir,
                m,
                &recording::compress::CompressParams::default(),
            ) {
                Ok(n) => log::info!("built meeting.opus for {} ({} bytes)", req.session_id, n),
                Err(e) => log::warn!("meeting.opus for {}: {e}", req.session_id),
            }
        }
    }

    // Mark finalized (idempotent — only stamps if not already finalized).
    stamp_session_finalized(&mp);

    // GC live_transcript.jsonl: drop `final` lines superseded by `polished`
    // ones. Best-effort; failure logs only.
    let session_dir = app.profile.session_path(&req.session_id);
    if let Err(e) = gc_live_transcript_jsonl(&session_dir) {
        log::warn!("live_transcript gc for {}: {e}", req.session_id);
    }

    let _ = next_stage(emit, Some(&session_root), last, Some("compressing"), "done", 1.0, None);
    Ok(summary)
}

/// Idempotently stamp `finalized_at_unix_seconds` on the manifest at `mp`,
/// AND close out any open chunk / recording_segment still missing an
/// `ended_at_unix_seconds` (stamped with now when no duration is known).
/// Also called by non-cascade recovery paths.
pub(crate) fn stamp_session_finalized(mp: &std::path::Path) {
    let Ok(bytes) = syncsafe::read(mp) else { return };
    let Ok(mut m): std::result::Result<recording::manifest::SessionManifest, _> =
        serde_json::from_slice(&bytes)
    else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut changed = false;
    if m.finalized_at_unix_seconds.is_none() {
        m.finalized_at_unix_seconds = Some(now);
        changed = true;
    }
    // Clear the interrupted flag set by crash recovery.
    if m.interrupted {
        m.interrupted = false;
        changed = true;
    }
    // Close any still-open chunk. Uses the chunk's start + duration if a
    // duration is known, and now if not.
    for c in m.chunks.iter_mut() {
        if c.ended_at_unix_seconds.is_none() {
            let ended = match c.duration_seconds {
                Some(d) if d > 0 => c.started_at_unix_seconds + d as i64,
                _ => now,
            };
            c.ended_at_unix_seconds = Some(ended);
            changed = true;
        }
    }
    for seg in m.recording_segments.iter_mut() {
        if seg.stopped_at_unix_seconds.is_none() {
            // Uses the last chunk's stamped end time, or now.
            let stop = m
                .chunks
                .last()
                .and_then(|c| c.ended_at_unix_seconds)
                .unwrap_or(now);
            seg.stopped_at_unix_seconds = Some(stop);
            seg.last_chunk_index = m.chunks.last().map(|c| c.index);
            changed = true;
        }
    }
    if !changed {
        return;
    }
    let tmp = mp.with_extension("json.tmp");
    let _ = syncsafe::write(&tmp, serde_json::to_vec_pretty(&m).unwrap_or_default());
    let _ = syncsafe::rename(&tmp, mp);
}

/// Mark a session complete from any path that produced artifacts without
/// running the full cascade.
pub fn mark_session_complete_impl(app: &AppState, session_id: &str) -> Result<()> {
    let mp = app.profile.session_path(session_id).join("manifest.json");
    if !mp.is_file() {
        return Ok(()); // session gone — nothing to stamp
    }
    stamp_session_finalized(&mp);
    Ok(())
}

/// Result of a successful live-transcript promotion: `transcript.json` is on
/// disk. `gaps` lists any uncovered spans still needing a targeted whisper
/// patch (empty = fully covered). `spans` are the chunk spans the patch I/O
/// slices from.
struct Promotion {
    gaps: Vec<(u32, u32)>,
    spans: Vec<transcript::promote::ChunkSpan>,
}

/// Promote the live transcript to `transcript.json` when it covers the
/// session (modulo small patchable gaps). Returns `None` when the live
/// transcript is missing or too incomplete; the caller then runs the full
/// whisper pass.
fn try_promote_live_transcript(
    session_root: &std::path::Path,
    manifest: &recording::manifest::SessionManifest,
    session_id: &str,
) -> Option<Promotion> {
    use transcript::model::Track;
    use transcript::promote::{promote_live_to_transcript, ChunkSpan, LiveSeg, GAP_TOLERANCE_MS};

    // Sessions shorter than MIN_PROMOTE_MS are not promoted; they take the
    // full whisper pass.
    const MIN_PROMOTE_MS: u64 = 180_000; // 3 min
    let recorded_ms: u64 = manifest
        .chunks
        .iter()
        .map(|c| c.duration_seconds.unwrap_or(0).saturating_mul(1000))
        .sum();
    if recorded_ms > 0 && recorded_ms < MIN_PROMOTE_MS {
        log::info!(
            "finalize: {} is short ({}s) — skipping live-transcript promotion, full whisper pass for quality",
            session_id,
            recorded_ms / 1000
        );
        return None;
    }

    let live_path = session_root.join("live_transcript.jsonl");
    let lines = match recording::live_transcript::read_all(&live_path) {
        Ok(l) => l,
        Err(_) => return None, // no live transcript on disk
    };
    let finals: Vec<&recording::live_transcript::LiveTranscriptLine> =
        lines.iter().filter(|l| l.is_final).collect();
    if finals.is_empty() {
        return None;
    }

    // Chunk spans: session-relative start = cumulative duration; the mic
    // track kind follows whether the chunk has an AEC wav.
    let mut spans: Vec<ChunkSpan> = Vec::with_capacity(manifest.chunks.len());
    let mut cursor_ms: u32 = 0;
    for c in &manifest.chunks {
        let mic_track = if c.mic_aec_wav_relative.is_some() {
            Track::MicAec
        } else {
            Track::Mic
        };
        let mic_wav = c
            .mic_aec_wav_relative
            .clone()
            .unwrap_or_else(|| c.mic_wav_relative.clone());
        spans.push(ChunkSpan {
            index: c.index,
            start_ms: cursor_ms,
            mic_track,
            mic_wav,
            system_wav: c.system_wav_relative.clone(),
        });
        let dur_ms = c
            .duration_seconds
            .map(|s| (s as u32).saturating_mul(1000))
            .or_else(|| match c.ended_at_unix_seconds {
                Some(e) if e > c.started_at_unix_seconds => {
                    Some(((e - c.started_at_unix_seconds) as u32).saturating_mul(1000))
                }
                _ => None,
            })
            .unwrap_or(0);
        cursor_ms = cursor_ms.saturating_add(dur_ms);
    }
    if spans.is_empty() {
        return None;
    }

    let segs: Vec<LiveSeg> = finals
        .iter()
        .map(|l| LiveSeg {
            is_system: matches!(l.track, recording::live_transcript::LiveTrack::System),
            start_ms: l.start_ms,
            end_ms: l.end_ms,
            text: l.text.clone(),
        })
        .collect();

    // Recording length: cumulative chunk duration, or the last spoken word if the
    // final chunk lacked a stamped duration.
    let max_end = segs.iter().map(|s| s.end_ms).max().unwrap_or(0);
    let total_ms = cursor_ms.max(max_end);

    let union_spans: Vec<(u32, u32)> = segs.iter().map(|s| (s.start_ms, s.end_ms)).collect();
    let gaps = transcript::gap::uncovered_spans(&union_spans, total_ms, GAP_TOLERANCE_MS);
    if !gaps.is_empty() && !crate::commands::gap_patch::should_patch(&gaps, total_ms) {
        log::info!(
            "finalize: live transcript too incomplete for {} ({} final segs, {} gap(s) totalling {}ms of {}ms) — local whisper full pass",
            session_id,
            segs.len(),
            gaps.len(),
            gaps.iter().map(|(s, e)| e - s).sum::<u32>(),
            total_ms
        );
        return None;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let bleed_rate = transcript::promote::promotion_bleed_rate(&segs);
    log::info!(
        "finalize: promote quality for {}: bleed_rate={:.2} bleed_coverage={:.2} pooled_dup_rate={:.2}",
        session_id,
        bleed_rate,
        transcript::echo_direction::bleed_coverage(&spans, 12),
        transcript::promote::pooled_duplication_rate(&segs),
    );
    if bleed_rate >= transcript::promote::PROMOTE_BLEED_MAX_RATE {
        log::info!(
            "finalize: live transcript bled for {} ({:.0}% of mic finals duplicate nearby system text) — local whisper full pass",
            session_id,
            bleed_rate * 100.0
        );
        return None;
    }
    let oracle = transcript::echo_direction::WavOracle::new(&spans);
    let st = promote_live_to_transcript(&segs, &spans, session_id, "live", now, &|m, s| {
        oracle.direction(m, s)
    });
    let json = match serde_json::to_vec_pretty(&st) {
        Ok(j) => j,
        Err(e) => {
            log::warn!("finalize: promote serialize failed for {session_id}: {e}");
            return None;
        }
    };
    if let Err(e) = syncsafe::write(session_root.join("transcript.json"), json) {
        log::warn!("finalize: promote write failed for {session_id}: {e}");
        return None;
    }
    log::info!(
        "finalize: promoted live transcript for {} ({} final segs, {} gap(s) to patch) — skipped whisper full pass",
        session_id,
        segs.len(),
        gaps.len()
    );
    Some(Promotion { gaps, spans })
}

/// Garbage-collect `<session>/live_transcript.jsonl` once the canonical
/// transcript is on disk: when `polished` lines are present, they are kept
/// and `final` lines are dropped. When there are no `polished` lines, the
/// file is left untouched.
fn gc_live_transcript_jsonl(session_dir: &std::path::Path) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let path = session_dir.join("live_transcript.jsonl");
    if !path.is_file() {
        return Ok(());
    }
    let file = syncsafe::open(&path)?;
    let reader = BufReader::new(file);
    let mut kept: Vec<String> = Vec::new();
    let mut dropped: usize = 0;
    for line in reader.lines().map_while(std::io::Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let kind = serde_json::from_str::<serde_json::Value>(&line)
            .ok()
            .and_then(|v| v.get("kind").and_then(|k| k.as_str().map(String::from)))
            .unwrap_or_else(|| "final".to_string());
        if kind == "polished" {
            kept.push(line);
        } else {
            dropped += 1;
        }
    }
    // No polished lines: leave the file untouched.
    if kept.is_empty() {
        return Ok(());
    }
    if dropped == 0 {
        return Ok(());
    }
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut w = syncsafe::create(&tmp)?;
        for line in &kept {
            w.write_all(line.as_bytes())?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
    }
    syncsafe::rename(&tmp, &path)?;
    log::info!(
        "live_transcript gc: dropped {} final line(s), kept {} polished line(s) in {}",
        dropped,
        kept.len(),
        path.display()
    );
    Ok(())
}


/// RAII guard that bumps libopenblas's thread cap to full parallelism for the
/// duration of a finalize cascade, then restores the recording-time cap of 1
/// on drop. Linux-only; a no-op elsewhere.
struct OpenBlasUncapGuard;

impl OpenBlasUncapGuard {
    #[cfg(target_os = "linux")]
    fn full_parallelism_for_finalize() -> Self {
        let n = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4);
        crate::openblas::cap_openblas_threads(n);
        Self
    }
    #[cfg(not(target_os = "linux"))]
    fn full_parallelism_for_finalize() -> Self { Self }
}

impl Drop for OpenBlasUncapGuard {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        crate::openblas::cap_openblas_threads(1);
    }
}

#[cfg(test)]
mod sidecar_tests {
    use super::*;

    #[test]
    fn writes_and_reads_finalize_status_sidecar() {
        let dir = std::env::temp_dir().join(format!("daisy-fin-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        syncsafe::create_dir_all(&dir).unwrap();
        write_finalize_status(&dir, "transcribing", 0.3, Some("working"));
        let s = read_finalize_status(&dir).expect("sidecar present");
        assert_eq!(s.stage, "transcribing");
        assert!((s.progress - 0.3).abs() < 1e-6);
        assert_eq!(s.message.as_deref(), Some("working"));
        write_finalize_status(&dir, "done", 1.0, None);
        assert_eq!(read_finalize_status(&dir).unwrap().stage, "done");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_sidecar_reconciles_to_done_when_manifest_finalized() {
        let dir = std::env::temp_dir().join(format!("daisy-fin-recon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        syncsafe::create_dir_all(&dir).unwrap();
        let mp = dir.join("manifest.json");
        // Stale, non-terminal sidecar (interrupted re-process leftover).
        write_finalize_status(&dir, "summarizing", 0.8, None);

        // Not finalized → genuine interrupted; stays as-is.
        syncsafe::write(&mp, br#"{"finalized_at_unix_seconds":null,"interrupted":false}"#).unwrap();
        assert_eq!(read_finalize_status_reconciled(&dir).unwrap().stage, "summarizing");

        // Finalized + not interrupted → manifest wins, reconcile to done.
        syncsafe::write(&mp, br#"{"finalized_at_unix_seconds":1781653890,"interrupted":false}"#).unwrap();
        assert_eq!(read_finalize_status_reconciled(&dir).unwrap().stage, "done");

        // Finalized but crash-interrupted → real recovery case, not done.
        syncsafe::write(&mp, br#"{"finalized_at_unix_seconds":1781653890,"interrupted":true}"#).unwrap();
        assert_eq!(read_finalize_status_reconciled(&dir).unwrap().stage, "summarizing");

        // Terminal stages pass through untouched even when finalized.
        write_finalize_status(&dir, "awaiting-labels", 0.6, None);
        syncsafe::write(&mp, br#"{"finalized_at_unix_seconds":1781653890,"interrupted":false}"#).unwrap();
        assert_eq!(read_finalize_status_reconciled(&dir).unwrap().stage, "awaiting-labels");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
