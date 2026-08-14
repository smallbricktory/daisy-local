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
    match serde_json::to_vec(&status) {
        Ok(bytes) => {
            if syncsafe::write_atomic(&path, &bytes).is_err() {
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

/// True when the file exists AND parses as a `SessionTranscript`. A present
/// but corrupt file (torn write, sync conflict) re-runs its producer stage
/// instead of blocking every stage after it.
fn parses_as_transcript(path: &std::path::Path) -> bool {
    let Ok(bytes) = syncsafe::read(path) else { return false };
    match serde_json::from_slice::<transcript::SessionTranscript>(&bytes) {
        Ok(_) => true,
        Err(e) => {
            log::warn!("finalize: {} is corrupt ({e}); regenerating", path.display());
            false
        }
    }
}

pub fn read_finalize_recovery(session_dir: &std::path::Path) -> FinalizeRecovery {
    syncsafe::read(session_dir.join(FINALIZE_RECOVERY_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn write_finalize_recovery(session_dir: &std::path::Path, rec: &FinalizeRecovery) {
    let path = session_dir.join(FINALIZE_RECOVERY_FILE);
    if let Ok(bytes) = serde_json::to_vec(rec) {
        if syncsafe::write_atomic(&path, &bytes).is_err() {
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
    // One settings read for the cascade. The manifest is NOT hoisted: the
    // AEC/denoise stage writes mic_aec/mic_dn sidecar paths into it, and
    // both the mixdown and the promotion MUST see those (a pre-stage copy
    // would silently build meeting.opus and patch gaps from the raw
    // echo-laden mic).
    let machine = app.machine_settings();
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
    let denoise_enabled = machine.denoise_enabled;
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
    // joins it.
    let mixdown_handle = {
        let session_id = req.session_id.clone();
        let dir = session_root.clone();
        // Post-AEC read: build_meeting_audio picks mic_dn > mic_aec > raw
        // from the manifest fields the stage above just wrote.
        crate::commands::session_store::read_manifest_opt(&dir).map(|m| {
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
    if parses_as_transcript(&session_root.join("transcript.json")) {
        log::info!(
            "finalize: transcript.json already on disk for {} — skipping transcribe",
            req.session_id
        );
    } else {
        // Post-AEC read: gap patching must target the AEC-cleaned track, not
        // the raw mic.
        let manifest_for_promote = crate::commands::session_store::read_manifest_opt(&session_root);
        let promotion = manifest_for_promote
            .as_ref()
            .and_then(|m| try_promote_live_transcript(&session_root, m, &req.session_id));
        match promotion {
            // Promoted with coverage holes: patch just the gaps from the
            // WAVs, best-effort — on failure the promoted transcript stays.
            Some(promo) if !promo.gaps.is_empty() => {
                // Gap slices decode with the same engine batch dispatch uses,
                // so patching works on installs that bundle no whisper model.
                match crate::commands::pipeline::build_batch_provider(
                    &machine,
                    &app.profile.models_dir(),
                    None,
                    Vec::new(),
                ) {
                    Ok(provider) => match crate::commands::gap_patch::patch_gaps_on_disk(
                        &session_root,
                        &*provider,
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
                    },
                    Err(e) => log::warn!(
                        "finalize: no engine for gap patch on {} ({e}) — keeping promoted live transcript as-is",
                        req.session_id
                    ),
                }
            }
            // Promoted, fully covered: the live transcript is the transcript.
            Some(_) => {}
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
    if !parses_as_transcript(&session_root.join("transcript.dedup.json")) {
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
                        let _ = syncsafe::write_atomic(&mp, serde_json::to_vec_pretty(&m).unwrap_or_default());
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

    let summary = run_finalize_tail(app, vs, &req, &mut emit, last, mixdown_handle);
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
    let summary = run_finalize_tail(app, vs, &req, &mut emit, last, None);
    Ok(FinalizeOutcome::Completed { summary })
}

/// The tail of the cascade: summary -> chapters -> compress -> finalize-stamp
/// -> done. Shared by the normal path and the resume path. Every stage is
/// best-effort; the tail cannot fail.
fn run_finalize_tail(
    app: &AppState,
    vs: &VaultState,
    req: &FinalizeRequest,
    emit: &mut dyn FnMut(ProgressEvent),
    last: Instant,
    // Background meeting.opus build to wait for at the compressing stage;
    // None on the resume path (its cascade's thread is long detached).
    mixdown: Option<std::thread::JoinHandle<()>>,
) -> Option<summarize::SessionSummary> {
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
    let manifest = crate::commands::session_store::read_manifest_opt(&app.profile.session_path(&req.session_id));

    last = next_stage(
        emit,
        Some(&session_root),
        last,
        Some("chaptering"),
        "compressing",
        0.92,
        Some("compressing audio"),
    );
    // The background mixdown owns the build; wait for it rather than racing
    // it with a second encode.
    if let Some(h) = mixdown {
        let _ = h.join();
    }
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

    let _ = next_stage(emit, Some(&session_root), last, Some("compressing"), "done", 1.0, None);
    summary
}

/// Idempotently stamp `finalized_at_unix_seconds` on the manifest at `mp`,
/// clear the `interrupted` flag, and close out any still-open chunk /
/// recording_segment (see `recording::manifest_ops::stamp_finalized`).
/// Also called by non-cascade recovery paths. Best-effort: a missing or
/// unreadable manifest is left alone.
pub(crate) fn stamp_session_finalized(mp: &std::path::Path) {
    let Some(root) = mp.parent() else { return };
    let Ok(mut session) = recording::session::Session::load(root) else {
        return;
    };
    if let Err(e) = recording::manifest_ops::stamp_finalized(&mut session, false) {
        log::warn!("finalize stamp for {} failed: {e}", root.display());
    }
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

/// Promote the live transcript to `transcript.json`. This is the standard
/// finalize path (single-pass): the live decode is the transcript. Returns
/// `None` only when the live transcript is missing, too incomplete to patch,
/// or bled; the caller then runs the batch whisper pass.
fn try_promote_live_transcript(
    session_root: &std::path::Path,
    manifest: &recording::manifest::SessionManifest,
    session_id: &str,
) -> Option<Promotion> {
    use transcript::model::Track;
    use transcript::promote::{promote_live_to_transcript, ChunkSpan, LiveSeg, GAP_TOLERANCE_MS};

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
            mic_host_ns: c.mic_host_ns,
            system_host_ns: c.system_host_ns,
            mic_samples: c.mic_samples,
            system_samples: c.system_samples,
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
    // bleed_coverage deliberately streams each chunk transiently instead of
    // riding the WavOracle cache: caching every decoded chunk pair holds the
    // whole session's f32 audio in memory on long recordings.
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

// ── Orphan-session recovery ─────────────────────────────────────────────────
// Lives here, next to the attempt-cap sidecar format it enforces.

fn low_memory_reason() -> Option<String> {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let avail_mb = crate::perf::available_mb(&sys);
    (avail_mb < 1500).then(|| {
        format!("Your device was low on memory ({avail_mb} MB free) — close some apps and try again.")
    })
}

/// Session ids with an in-app orphan finalize currently running; a second
/// call for the same session returns immediately instead of running the
/// cascade twice.
fn orphan_in_flight() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static IN_FLIGHT: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    IN_FLIGHT.get_or_init(Default::default)
}

/// Removes its session id from the in-flight set on drop — including when
/// the cascade panics.
struct OrphanInFlightGuard(String);
impl Drop for OrphanInFlightGuard {
    fn drop(&mut self) {
        orphan_in_flight()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// Finalizes one orphaned session in-process. Runs the canonical cascade
/// (`finalize_and_summarize_impl`): AEC → transcribe → dedup → diarize →
/// mixdown, then the summary tail. The summary is best-effort and degrades
/// to `None` when the vault is locked or no provider is configured; runs at
/// startup before unlock. `skip_label_gate = true`: recovery never pauses
/// for speaker labels. Returns true if the manifest is now stamped
/// finalized. Panics are isolated to the caller's thread; the per-session
/// attempt cap (see `recover_orphan_sessions`) bounds retries.
pub fn finalize_orphan_in_app(app_handle: &tauri::AppHandle, session_id: &str) -> bool {
    use tauri::Manager;
    if !orphan_in_flight()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(session_id.to_string())
    {
        log::info!("orphan recovery: finalize already in flight for {session_id}; skipping");
        return false;
    }
    let _in_flight = OrphanInFlightGuard(session_id.to_string());
    let dir = {
        let state = app_handle.state::<AppState>();
        state.profile.session_path(session_id)
    };
    // Chunk durations lost to the crash are backfilled from WAV lengths
    // before the cascade, so promotion and mixdown see real durations.
    match recording::session::Session::load(&dir) {
        Ok(mut session) => {
            if let Err(e) = recording::manifest_ops::backfill_chunk_durations(&mut session) {
                log::warn!("orphan recovery: duration backfill for {session_id} failed: {e}");
            }
        }
        Err(e) => log::warn!("orphan recovery: session load for {session_id} failed: {e}"),
    }
    let req = FinalizeRequest {
        session_id: session_id.to_string(),
        summary_provider: None,
        model: None,
        skip_label_gate: true,
    };
    let outcome = {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        finalize_and_summarize_impl(&state, &vault, req, |_ev| {})
    };
    match &outcome {
        Ok(_) => log::info!("orphan recovery: finalized {session_id} in-app"),
        Err(e) => log::warn!("orphan recovery: in-app finalize failed for {session_id}: {e}"),
    }
    let finalized = crate::commands::session_store::read_manifest_opt(&dir)
        .map(|m| m.finalized_at_unix_seconds.is_some())
        .unwrap_or(false);
    if finalized {
        clear_finalize_recovery(&dir);
    }
    crate::library_events::emit(
        app_handle,
        crate::library_events::LibraryChangeKind::Finalized,
        session_id,
    );
    finalized
}

/// Runs in-app finalize recovery for any session whose manifest is missing
/// finalized_at_unix_seconds. Skips sessions younger than 60s or older than
/// 30 days.
fn recover_orphan_sessions(state: &AppState, app_handle: tauri::AppHandle) {
    let sessions_dir = state.profile.root().join("sessions");
    let dirs = crate::commands::session_store::session_dirs(&sessions_dir).unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Eligible orphans + attempt bookkeeping are collected up front, then one
    // background thread finalizes them sequentially.
    let mut eligible: Vec<String> = Vec::new();
    for p in dirs {
        let Some(m) = crate::commands::session_store::read_manifest_opt(&p) else {
            continue;
        };
        if m.finalized_at_unix_seconds.is_some() {
            continue;
        }
        let age = now - m.created_at_unix_seconds;
        if age < 60 || age > 60 * 60 * 24 * 30 {
            continue;
        }
        // Only sessions with at least one audio chunk on disk are processed.
        let has_audio = m.chunks.iter().any(|c| {
            p.join(&c.mic_wav_relative).is_file() || p.join(&c.system_wav_relative).is_file()
        });
        if !has_audio {
            continue;
        }
        // A fresh heartbeat means the session is being recorded right now —
        // possibly by ANOTHER machine sharing this profile over file sync,
        // where the PID in the file is meaningless. The 15-minute window
        // covers sync lag; a local crash orphan's heartbeat is long stale.
        if let Ok(hb) = recording::heartbeat::Heartbeat::read(&p.join("heartbeat")) {
            if hb.age_seconds() < 15 * 60 {
                log::info!(
                    "orphan recovery: {} heartbeat is {}s old — likely recording (possibly on another machine); skipping",
                    m.session_id,
                    hb.age_seconds()
                );
                continue;
            }
        }
        // Attempts are counted in a sidecar; recovery gives up once over the
        // cap.
        let mut rec = read_finalize_recovery(&p);
        if rec.failed {
            continue; // already given up — waits for a user "Retry"
        }
        if rec.attempts >= FINALIZE_MAX_ATTEMPTS {
            let reason = low_memory_reason()
                .unwrap_or_else(|| "We couldn't finish processing this recording.".to_string());
            log::warn!(
                "orphan recovery: giving up on {} after {} attempts ({reason})",
                m.session_id,
                rec.attempts
            );
            rec.failed = true;
            rec.reason = Some(reason);
            rec.updated_at_unix = now;
            write_finalize_recovery(&p, &rec);
            crate::library_events::emit(
                &app_handle,
                crate::library_events::LibraryChangeKind::Finalized,
                &m.session_id,
            );
            continue;
        }
        rec.attempts += 1;
        rec.updated_at_unix = now;
        write_finalize_recovery(&p, &rec);
        // Orphan detected: marked interrupted until a successful finalize
        // clears the flag (Library shows the badge in the meantime).
        if !m.interrupted {
            if let Ok(mut session) = recording::session::Session::load(&p) {
                let _ = session.update_manifest(|m| m.interrupted = true);
            }
        }
        eligible.push(m.session_id.clone());
    }
    if eligible.is_empty() {
        return;
    }
    log::info!("orphan recovery: {} session(s) to finalize in-app", eligible.len());
    std::thread::spawn(move || {
        for sid in eligible {
            finalize_orphan_in_app(&app_handle, &sid);
        }
        log::info!("orphan recovery: finished");
    });
}

/// Launch maintenance that needs vault keys: orphan finalize recovery and
/// the local integrity sweep. Both reach stages that decrypt provider
/// credentials, so they wait for the vault to open.
///
/// Runs once per launch; a lock/unlock cycle does not spend another attempt
/// from each orphan's `FINALIZE_MAX_ATTEMPTS` budget.
pub fn spawn_post_unlock_maintenance(app_handle: tauri::AppHandle) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tauri::Manager;
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        recover_orphan_sessions(&state, app_handle.clone());
        let (sessions, repairs) =
            crate::commands::integrity::audit_and_repair_local(&state, &vault, &app_handle);
        if repairs > 0 {
            log::info!("integrity: repaired {repairs} artifact(s) across {sessions} session(s)");
        }
    });
}

#[cfg(test)]
mod promote_tests {
    use super::*;

    fn write_silence_wav(path: &std::path::Path, seconds: u32) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for _ in 0..(seconds * 16_000) {
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
    }

    #[test]
    fn short_sessions_promote_the_live_transcript() {
        // Single-pass: even a 60s session must promote instead of taking a
        // full whisper pass — there is no minimum-duration floor.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_silence_wav(&root.join("chunks/0001/mic.wav"), 60);
        write_silence_wav(&root.join("chunks/0001/system.wav"), 60);
        let manifest = recording::manifest::SessionManifest {
            schema_version: 3,
            session_id: "test-session".into(),
            created_at_unix_seconds: 0,
            sample_rate: 16_000,
            channels: 1,
            mic_source_id: 1,
            mic_source_node_name: "m".into(),
            mic_source_description: "m".into(),
            system_source_id: 2,
            system_source_node_name: "s".into(),
            system_source_description: "s".into(),
            aec_mode: recording::manifest::AecMode::Always,
            chunks: vec![recording::manifest::ChunkManifest {
                index: 1,
                started_at_unix_seconds: 1000,
                ended_at_unix_seconds: Some(1060),
                duration_seconds: Some(60),
                mic_wav_relative: "chunks/0001/mic.wav".into(),
                system_wav_relative: "chunks/0001/system.wav".into(),
                mic_aec_wav_relative: None,
                mic_dn_wav_relative: None,
                mic_host_ns: None,
                system_host_ns: None,
                mic_samples: None,
                system_samples: None,
                mic_voice_processed: false,
                mic_zero_samples: None,
                mic_longest_zero_run: None,
            }],
            finalized_at_unix_seconds: None,
            title: None,
            meeting_id: "mid".into(),
            tag_ids: vec![],
            notes_md_relative: None,
            attendees: vec![],
            calendar: None,
            recording_segments: vec![],
            speaker_map: vec![],
            language: None,
            diarization_unavailable: false,
            expected_speakers: None,
            sent_integration_ids: vec![],
            single_local_speaker: true,
            cluster_sides: vec![],
            interrupted: false,
            denoise_applied: None,
        };
        // Fictional live finals covering the whole minute.
        let lines: String = (0..6)
            .map(|i| {
                format!(
                    r#"{{"track":"mic","start_ms":{},"end_ms":{},"text":"placeholder utterance {}","final":true,"received_at_unix":1000}}"#,
                    i * 10_000,
                    i * 10_000 + 9_800,
                    i
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        syncsafe::write(root.join("live_transcript.jsonl"), lines.as_bytes()).unwrap();

        let promo = try_promote_live_transcript(root, &manifest, "test-session");
        assert!(promo.is_some(), "short sessions must promote under single-pass");
        assert!(
            root.join("transcript.json").is_file(),
            "promotion writes transcript.json"
        );
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
