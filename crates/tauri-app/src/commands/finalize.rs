// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
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

/// Serializes every write of a finalize status sidecar in this process.
fn sidecar_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Atomically write the finalize status sidecar. Best-effort: logs on failure,
/// never aborts the cascade.
pub fn write_finalize_status(session_dir: &std::path::Path, stage: &str, progress: f32, message: Option<&str>) {
    let _sidecar = sidecar_lock();
    write_finalize_status_locked(session_dir, stage, progress, message);
}

/// Re-stamps a non-terminal sidecar's time, keeping its stage, progress and
/// message; a terminal or absent one is left as it is.
fn refresh_finalize_status(session_dir: &std::path::Path) {
    let _sidecar = sidecar_lock();
    if let Some(s) = read_finalize_status(session_dir) {
        if !matches!(s.stage.as_str(), "done" | "error" | "awaiting-labels") {
            write_finalize_status_locked(session_dir, &s.stage, s.progress, s.message.as_deref());
        }
    }
}

fn write_finalize_status_locked(session_dir: &std::path::Path, stage: &str, progress: f32, message: Option<&str>) {
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

/// True when the file exists AND parses as a `SessionTranscript`.
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

/// Overall progress at the start of the "diarizing" stage, between dedup
/// (0.6) and the label gate (0.70 / 0.75).
const DIARIZING_PROGRESS: f32 = 0.65;

/// Emit a stage transition: logs the wall-clock duration of the previous
/// stage, writes the on-disk status sidecar and the telemetry stage line
/// (when a session dir is given), and invokes `emit`. Returns the new "stage
/// start" instant for the next call.
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
        recording::telemetry::append_stage(dir, stage);
    }
    emit(ProgressEvent {
        stage: stage.into(),
        progress,
        message: message.map(|s| s.into()),
    });
    Instant::now()
}

/// How long finalize waits for a recording's live-names workers to write
/// their final overlays before it cancels them.
const LIVE_NAMES_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Run the finalize cascade: final-pass transcription -> dedup -> AI summary,
/// then mark the manifest finalized. `emit` is invoked at each stage boundary;
/// the Tauri wrapper turns those calls into emitted frontend events. The
/// cascade runs at background priority within the finalize CPU budget.
pub fn finalize_and_summarize_impl(
    app: &AppState,
    vs: &VaultState,
    req: FinalizeRequest,
    emit: impl FnMut(ProgressEvent) + Send,
) -> Result<FinalizeOutcome> {
    // Claimed before the background queue: a cascade waiting for its turn
    // already counts as running for its session.
    let Some(running) = FinalizeClaim::take(&app.profile.session_path(&req.session_id), &req.session_id, SIDECAR_REFRESH)
    else {
        log::info!("finalize for {}: a finalize of this session is already running; not starting another", req.session_id);
        return Err(AppError::Config("This recording is still being processed.".into()));
    };
    finalize_claimed(app, vs, req, emit, running)
}

/// [`finalize_and_summarize_impl`] under a claim the caller holds; the
/// claim is released when the cascade returns or unwinds.
fn finalize_claimed(
    app: &AppState,
    vs: &VaultState,
    req: FinalizeRequest,
    emit: impl FnMut(ProgressEvent) + Send,
    _running: FinalizeClaim,
) -> Result<FinalizeOutcome> {
    let session_id = req.session_id.clone();
    let out = transcript::cpu::run_in_background(|| {
        let _blas_cap = crate::openblas::finalize_cap();
        finalize_cascade(app, vs, req, emit)
    });
    if out.is_err() {
        crate::live_diarize::cancel(&session_id);
    }
    out
}

fn finalize_cascade(
    app: &AppState,
    vs: &VaultState,
    req: FinalizeRequest,
    mut emit: impl FnMut(ProgressEvent),
) -> Result<FinalizeOutcome> {
    let cascade_start = Instant::now();
    let session_root = app.profile.session_path(&req.session_id);
    // One settings read for the cascade. The manifest is read after the AEC/denoise stage.
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

    // Chunk lengths come from the WAVs on disk.
    match recording::session::Session::load(&session_root) {
        Ok(mut session) => {
            if let Err(e) = recording::manifest_ops::stamp_chunk_audio(&mut session) {
                log::warn!("finalize: chunk audio stamp for {} failed: {e}", req.session_id);
            }
        }
        Err(e) => log::warn!("finalize: session load for {} failed: {e}", req.session_id),
    }

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
    // the background mixdown in the tail; meeting.opus picks up mic_dn.wav. Any
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
        // Post-AEC read: gap patching targets the AEC-cleaned track.
        let manifest_for_promote = crate::commands::session_store::read_manifest_opt(&session_root);
        let promotion = manifest_for_promote
            .as_ref()
            .and_then(|m| try_promote_live_transcript(&session_root, m, &req.session_id));
        match promotion {
            // Promoted with coverage holes: patch just the gaps from the
            // WAVs, best-effort — on failure the promoted transcript stays.
            Some(promo) if !promo.gaps.is_empty() => {
                // Gap slices decode with the same engine batch dispatch uses.
                let _asr = crate::commands::pipeline::asr_permit();
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
                // The progress callback fires only at chunk boundaries; the
                // finalize claim's keeper re-stamps the sidecar between them.
                let res = crate::commands::pipeline::transcribe_impl(
                    app,
                    crate::commands::pipeline::TranscribeRequest {
                        session_id: req.session_id.clone(),
                        model: None,
                        resume: true,
                    },
                    Some(&progress),
                );
                res.map_err(|e| {
                    let err = AppError::Config(format!("transcribe: {e}"));
                    write_finalize_status(&session_root, "error", 0.15, Some(&err.to_string()));
                    recording::telemetry::append_stage(&session_root, "error");
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
            recording::telemetry::append_stage(&session_root, "error");
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
    let mut prev_stage = "deduping";
    if crate::commands::voiceprints::transcript_undiarized(app, &req.session_id) {
        last = next_stage(&mut emit, Some(&session_root), last, Some(prev_stage), "diarizing", DIARIZING_PROGRESS, None);
        prev_stage = "diarizing";
        // The vault's voiceprints can set the speaker count; a locked vault
        // has none to offer.
        let gallery = vs.keys.lock().unwrap().as_ref().map(|k| k.voiceprints.clone()).unwrap_or_default();
        // The turns and embeddings the recording's live pass found, when it
        // covered every chunk as it is on disk now.
        let diarized = match crate::live_diarize::take(&req.session_id, &session_root) {
            Some(turns) => crate::commands::voiceprints::diarize_session_with_turns_impl(
                app, &req.session_id, None, None, &gallery, &turns,
            ),
            None => crate::commands::voiceprints::diarize_session_impl(app, &req.session_id, None, None, &gallery),
        };
        match diarized {
            Ok(r) => log::info!(
                "local diarization for {}: {} speaker(s), {} segment(s)",
                req.session_id, r.speakers, r.segments_labeled
            ),
            Err(ref e) if crate::commands::voiceprints::voice_model_unusable(e) => {
                // Voiceprint model missing or unreadable: flag the manifest.
                // The banner reads the model's state when it shows.
                log::warn!("local diarization for {}: {e} — flagging manifest", req.session_id);
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
    } else {
        crate::live_diarize::cancel(&req.session_id);
    }
    crate::live_names::wait(&req.session_id, LIVE_NAMES_WAIT);
    if !crate::commands::voiceprints::transcript_undiarized(app, &req.session_id) {
        crate::live_names::overlay::delete_all(&app.profile.session_path(&req.session_id));
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
                Some(prev_stage),
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

    let summary = run_finalize_tail(app, vs, &req, &mut emit, last);
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
    mut emit: impl FnMut(ProgressEvent) + Send,
) -> Result<FinalizeOutcome> {
    transcript::cpu::run_in_background(|| {
        let start = Instant::now();
        let _blas_cap = crate::openblas::finalize_cap();
        let session_root = app.profile.session_path(&req.session_id);
        let last = next_stage(&mut emit, Some(&session_root), start, None, "finalizing", 0.70, Some("resuming"));
        let summary = run_finalize_tail(app, vs, &req, &mut emit, last);
        Ok(FinalizeOutcome::Completed { summary })
    })
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
) -> Option<summarize::SessionSummary> {
    let session_root = app.profile.session_path(&req.session_id);
    // Background mixdown: the meeting.opus build overlaps the summary and
    // chapter stages at background priority; the compressing stage joins it.
    let mixdown = if session_root.join(recording::mixdown::MEETING_AUDIO_NAME).is_file() {
        None
    } else {
        let session_id = req.session_id.clone();
        let dir = session_root.clone();
        // build_meeting_audio picks mic_dn > mic_aec > raw from the manifest
        // fields the AEC stage wrote.
        crate::commands::session_store::read_manifest_opt(&dir).map(|m| {
            std::thread::spawn(move || {
                transcript::cpu::lower_to_background_priority();
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
    // The background mixdown owns the build; wait for it.
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
    if let Some(sid) = root.file_name().and_then(|n| n.to_str()) {
        crate::live_names::wait(sid, LIVE_NAMES_WAIT);
    }
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

    // Chunk spans on the live transcript's timeline: each chunk starts where the mic samples captured before it end. The mic track kind follows whether the chunk has an AEC wav.
    let offsets = manifest.capture_offsets_ms();
    let mut spans: Vec<ChunkSpan> = Vec::with_capacity(manifest.chunks.len());
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
            start_ms: offsets.get(&c.index).copied().unwrap_or(0),
            mic_track,
            mic_wav,
            system_wav: c.system_wav_relative.clone(),
            mic_host_ns: c.mic_host_ns,
            system_host_ns: c.system_host_ns,
            mic_samples: c.mic_samples,
            system_samples: c.system_samples,
        });
    }
    spans.sort_by_key(|s| s.start_ms);
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
            words: l.words.clone(),
        })
        .collect();

    // Recording length on the live timeline: the mic audio captured, or the
    // last spoken word if the final chunk lacked a count.
    let max_end = segs.iter().map(|s| s.end_ms).max().unwrap_or(0);
    let total_ms = u32::try_from(manifest.capture_ms()).unwrap_or(u32::MAX).max(max_end);

    let union_spans: Vec<(u32, u32)> = segs.iter().map(|s| (s.start_ms, s.end_ms)).collect();
    let gaps = transcript::gap::uncovered_spans(&union_spans, total_ms, GAP_TOLERANCE_MS);
    if !gaps.is_empty() && !crate::commands::gap_patch::should_patch(&gaps, total_ms) {
        log::info!(
            "finalize: live transcript too incomplete for {} ({} final segs, {} gap(s) totalling {}ms of {}ms) — local full-pass transcription",
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
    // bleed_coverage streams each chunk transiently.
    log::info!(
        "finalize: promote quality for {}: bleed_rate={:.2} bleed_coverage={:.2} pooled_dup_rate={:.2}",
        session_id,
        bleed_rate,
        transcript::echo_direction::bleed_coverage(&spans, 12),
        transcript::promote::pooled_duplication_rate(&segs),
    );
    if bleed_rate >= transcript::promote::PROMOTE_BLEED_MAX_RATE {
        log::info!(
            "finalize: live transcript bled for {} ({:.0}% of mic finals duplicate nearby system text) — local full-pass transcription",
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
        "finalize: promoted live transcript for {} ({} final segs, {} gap(s) to patch) — skipped full-pass transcription",
        session_id,
        segs.len(),
        gaps.len()
    );
    Some(Promotion { gaps, spans })
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

/// How often a running finalize re-stamps its progress sidecar.
const SIDECAR_REFRESH: Duration = Duration::from_secs(30);

/// Session ids with a finalize cascade running or queued in this process.
fn finalize_in_flight() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static IN_FLIGHT: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    IN_FLIGHT.get_or_init(Default::default)
}

/// True while a finalize of `session_id` runs or waits its turn in this
/// process.
pub fn finalize_running(session_id: &str) -> bool {
    finalize_in_flight().lock().unwrap_or_else(|e| e.into_inner()).contains(session_id)
}

/// One session's claim on the finalize cascade. While held, no second cascade of the session starts, and a keeper thread re-stamps the session's progress sidecar every `refresh`. Dropping it, panics included, releases both.
struct FinalizeClaim {
    session_id: String,
    stop: Arc<AtomicBool>,
    keeper: Option<std::thread::JoinHandle<()>>,
}

impl FinalizeClaim {
    fn take(session_dir: &std::path::Path, session_id: &str, refresh: Duration) -> Option<Self> {
        if !finalize_in_flight()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string())
        {
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let keeper = {
            let stop = Arc::clone(&stop);
            let dir = session_dir.to_path_buf();
            std::thread::spawn(move || {
                let tick = Duration::from_millis(50).min(refresh);
                let mut waited = Duration::ZERO;
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(tick);
                    waited += tick;
                    if waited < refresh || stop.load(Ordering::Relaxed) {
                        continue;
                    }
                    waited = Duration::ZERO;
                    refresh_finalize_status(&dir);
                }
            })
        };
        Some(Self { session_id: session_id.to_string(), stop, keeper: Some(keeper) })
    }
}

impl Drop for FinalizeClaim {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(k) = self.keeper.take() {
            let _ = k.join();
        }
        finalize_in_flight()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.session_id);
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
pub fn finalize_orphan_in_app<R: tauri::Runtime>(app_handle: &tauri::AppHandle<R>, session_id: &str) -> bool {
    use tauri::Manager;
    let dir = {
        let state = app_handle.state::<AppState>();
        state.profile.session_path(session_id)
    };
    // A finalize already running owns the session.
    let Some(running) = FinalizeClaim::take(&dir, session_id, SIDECAR_REFRESH) else {
        log::info!("orphan recovery: a finalize of {session_id} is already running; skipping");
        return false;
    };
    let req = FinalizeRequest {
        session_id: session_id.to_string(),
        summary_provider: None,
        model: None,
        skip_label_gate: true,
    };
    let outcome = {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        finalize_claimed(&state, &vault, req, |_ev| {}, running)
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

/// Heartbeats younger than this, written by another machine (or by a build
/// that did not record its host), may belong to a recording running there
/// right now; sync lag is covered too.
const FOREIGN_HEARTBEAT_LIVE_SECS: u64 = 15 * 60;

/// What [`reconcile_abandoned_sessions`] did to one session.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reconciled {
    /// A start that never opened a chunk: its directory was removed.
    Removed,
    /// Its open chunk and segment were closed (see
    /// `recording::manifest_ops::close_abandoned`).
    Closed,
}

/// True when this machine wrote the heartbeat from a process that is gone:
/// another pid than this one's, and no live process of this executable
/// holds it.
fn written_by_a_dead_process_here(hb: &recording::heartbeat::HeartbeatSnapshot) -> bool {
    hb.written_here() && hb.pid != std::process::id() && !crate::single_instance::pid_is_sibling(hb.pid)
}

/// True when the heartbeat at `dir` says a process may still be recording
/// the session: one this machine wrote from a live process, or one from
/// another machine (or of unknown origin) younger than
/// [`FOREIGN_HEARTBEAT_LIVE_SECS`].
fn heartbeat_says_live(hb: &recording::heartbeat::HeartbeatSnapshot) -> bool {
    if hb.written_here() {
        !written_by_a_dead_process_here(hb)
    } else {
        hb.age_seconds() < FOREIGN_HEARTBEAT_LIVE_SECS
    }
}

/// Closes every session a crash, kill or relaunch of Daisy on this machine left with an open recording segment. Only a session whose heartbeat this machine wrote from a process that is gone qualifies, and not one `live` names.
///
/// A qualifying session with no chunk in its manifest whose directory
/// holds no audio (`recording::manifest_ops::holds_no_recording`) is a
/// start that never opened its first chunk; its directory is removed.
/// Every other one is closed at the end of its audio and its heartbeat
/// removed. Runs once per launch, before orphan finalize.
pub(crate) fn reconcile_abandoned_sessions(
    sessions_dir: &std::path::Path,
    live: impl Fn(&str) -> bool,
) -> Vec<(String, Reconciled)> {
    let mut out = Vec::new();
    let dirs = crate::commands::session_store::session_dirs(sessions_dir).unwrap_or_default();
    for dir in dirs {
        let Some(m) = crate::commands::session_store::read_manifest_opt(&dir) else { continue };
        if !recording::manifest_ops::has_open_segment(&m) || live(&m.session_id) {
            continue;
        }
        let hb_path = dir.join("heartbeat");
        let Ok(hb) = recording::heartbeat::Heartbeat::read(&hb_path) else { continue };
        if !written_by_a_dead_process_here(&hb) {
            continue;
        }
        if m.chunks.is_empty()
            && m.finalized_at_unix_seconds.is_none()
            && recording::manifest_ops::holds_no_recording(&dir)
        {
            match syncsafe::remove_dir_all(&dir) {
                Ok(()) => {
                    log::info!("launch reconcile: removed {} (its start never opened a chunk)", m.session_id);
                    out.push((m.session_id, Reconciled::Removed));
                    continue;
                }
                Err(e) => log::warn!("launch reconcile: removing {}: {e}", m.session_id),
            }
        }
        let last_alive = Some(hb.last_update_unix as i64);
        match recording::session::Session::load(&dir)
            .and_then(|mut s| recording::manifest_ops::close_abandoned(&mut s, last_alive))
        {
            Ok(_) => {
                let _ = syncsafe::remove_file(&hb_path);
                log::info!("launch reconcile: closed the open segment of {}", m.session_id);
                out.push((m.session_id, Reconciled::Closed));
            }
            Err(e) => log::warn!("launch reconcile: closing {}: {e}", m.session_id),
        }
    }
    out
}

/// Runs in-app finalize recovery for any session whose manifest is missing
/// finalized_at_unix_seconds. Skips sessions younger than 60s or older than
/// 30 days.
fn recover_orphan_sessions<R: tauri::Runtime>(state: &AppState, app_handle: tauri::AppHandle<R>) {
    use tauri::Manager;
    let sessions_dir = state.profile.root().join("sessions");
    let reconciled = {
        let active = app_handle.state::<crate::commands::recording::ActiveRecordingHandle>();
        reconcile_abandoned_sessions(&sessions_dir, |sid| {
            crate::commands::recording::is_starting()
                || crate::commands::recording::recording_snapshot_impl(&active.0)
                    .is_some_and(|s| s.session_id == sid)
        })
    };
    for (sid, what) in &reconciled {
        let kind = match what {
            Reconciled::Removed => crate::library_events::LibraryChangeKind::Deleted,
            Reconciled::Closed => crate::library_events::LibraryChangeKind::Updated,
        };
        crate::library_events::emit(&app_handle, kind, sid);
    }
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
            syncsafe::join_rel(&p, &c.mic_wav_relative).is_file() || syncsafe::join_rel(&p, &c.system_wav_relative).is_file()
        });
        if !has_audio {
            continue;
        }
        // A heartbeat this process writes, or a fresh one from ANOTHER
        // machine sharing this profile over file sync, means the session is
        // being recorded right now (see `heartbeat_says_live`).
        if let Ok(hb) = recording::heartbeat::Heartbeat::read(&p.join("heartbeat")) {
            if heartbeat_says_live(&hb) {
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

/// Launch maintenance that needs vault keys: orphan finalize recovery and the local integrity sweep.
///
/// Runs once per launch; a lock/unlock cycle does not spend another attempt
/// from each orphan's `FINALIZE_MAX_ATTEMPTS` budget.
pub fn spawn_post_unlock_maintenance<R: tauri::Runtime>(app_handle: tauri::AppHandle<R>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tauri::Manager;
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn_blocking(move || {
        transcript::cpu::run_in_background(|| {
            let state = app_handle.state::<AppState>();
            let vault = app_handle.state::<VaultState>();
            recover_orphan_sessions(&state, app_handle.clone());
            let (sessions, repairs) =
                crate::commands::integrity::audit_and_repair_local(&state, &vault, &app_handle);
            if repairs > 0 {
                log::info!("integrity: repaired {repairs} artifact(s) across {sessions} session(s)");
            }
        })
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
        // Single-pass: a 60s session promotes.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_silence_wav(&root.join("chunks/0001/mic.wav"), 60);
        write_silence_wav(&root.join("chunks/0001/system.wav"), 60);
        let manifest = recording::manifest::SessionManifest {
            recording_kind: None,
            origin_device: None,
            extra: Default::default(),
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
                extra: Default::default(),
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
                audio_samples: None,
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
            mixed_clusters: vec![], cluster_sample_spans: vec![], sample_spans_on_audio_timeline: false, speaker_separation: None, diarize_count: None, diarization_id: None,
            interrupted: false,
            denoise_applied: None,
            dismissed_suggested_tag_ids: vec![],
        };
        // Fictional live finals covering the whole minute.
        let lines: String = (0..6)
            .map(|i| {
                format!(
                    r#"{{"track":"mic","start_ms":{},"end_ms":{},"text":"placeholder utterance {}","final":true,"received_at_unix":1000,"words":[{{"text":"placeholder","start_ms":{},"end_ms":{}}},{{"text":"utterance","start_ms":{},"end_ms":{}}},{{"text":"{}","start_ms":{},"end_ms":{}}}]}}"#,
                    i * 10_000,
                    i * 10_000 + 9_800,
                    i,
                    i * 10_000,
                    i * 10_000 + 400,
                    i * 10_000 + 500,
                    i * 10_000 + 900,
                    i,
                    i * 10_000 + 1_000,
                    i * 10_000 + 1_200,
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
        let st: transcript::SessionTranscript =
            serde_json::from_slice(&syncsafe::read(root.join("transcript.json")).unwrap()).unwrap();
        let segs = &st.chunks[0].tracks[0].segments;
        assert_eq!(segs.len(), 6);
        assert_eq!(segs[1].words.len(), 3, "live word times reach the stored transcript");
        assert_eq!(segs[1].words[1].start_ms, 10_500);
        assert!(segs.iter().all(|s| s.words_consistent()));
    }
}

#[cfg(test)]
mod gap_timeline_tests {
    use super::*;
    use std::sync::Mutex;

    /// Every sample holds `20000 + chunk * 100 + second into the chunk`, so a
    /// decoded slice names the audio it came from.
    fn write_marked_wav(path: &std::path::Path, chunk: i16, seconds: i16) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let pcm: Vec<i16> = (0..seconds)
            .flat_map(|s| std::iter::repeat_n(20_000 + chunk * 100 + s, 16_000))
            .collect();
        recording::write_wav_mono_i16(path, &pcm, 16_000).unwrap();
    }

    /// Decodes a slice to one segment naming where its audio came from.
    struct MarkReader(Mutex<Vec<String>>);
    impl providers_http::Transcriber for MarkReader {
        fn name(&self) -> &'static str {
            "mark"
        }
        fn model(&self) -> &str {
            "mark"
        }
        fn transcribe(
            &self,
            wav: &std::path::Path,
            _: Option<&str>,
        ) -> providers_http::Result<Vec<transcript::Segment>> {
            let (pcm, _) = recording::read_wav_mono_i16(wav).unwrap();
            let first = pcm[0] - 20_000;
            let text = format!("chunk {} second {}", first / 100, first % 100);
            self.0.lock().unwrap().push(text.clone());
            let end_ms = (pcm.len() as u32) * 1000 / 16_000;
            Ok(vec![transcript::Segment { start_ms: 0, end_ms, text, ..Default::default() }])
        }
    }

    /// Chunk 2's wall span runs
    /// 48 s around a sleep and holds 27 s of audio. On the live timeline the
    /// chunks start at 0, 30 and 57 s; on the wall clock at 0, 30 and 78 s.
    fn slept_session(root: &std::path::Path) -> recording::manifest::SessionManifest {
        let chunk = |i: i16, start: i64, end: i64, secs: i16| {
            write_marked_wav(&root.join(format!("chunks/{i:04}/mic.wav")), i, secs);
            write_marked_wav(&root.join(format!("chunks/{i:04}/system.wav")), i, secs);
            serde_json::json!({
                "index": i, "started_at_unix_seconds": start, "ended_at_unix_seconds": end,
                "duration_seconds": end - start,
                "mic_wav_relative": format!("chunks/{i:04}/mic.wav"),
                "system_wav_relative": format!("chunks/{i:04}/system.wav"),
                "mic_aec_wav_relative": null,
                "mic_samples": secs as u64 * 16_000, "system_samples": secs as u64 * 16_000,
            })
        };
        let m = serde_json::json!({
            "schema_version": 4, "session_id": "daisy-0", "created_at_unix_seconds": 0,
            "sample_rate": 16000, "channels": 1, "finalized_at_unix_seconds": null,
            "chunks": [chunk(1, 0, 30, 30), chunk(2, 30, 78, 27), chunk(3, 78, 108, 30)],
        });
        let manifest: recording::manifest::SessionManifest = serde_json::from_value(m).unwrap();
        syncsafe::write(root.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        manifest
    }

    /// Live finals every 3 s over the 87 s the live decode heard, with a 12 s
    /// hole from 63 to 75 s: 6 to 18 s into chunk 3.
    fn write_live_with_a_hole(root: &std::path::Path) {
        let lines: Vec<String> = (0..29u32)
            .map(|i| i * 3_000)
            .filter(|&t| !(63_000..75_000).contains(&t))
            .map(|t| {
                format!(
                    r#"{{"track":"mic","start_ms":{t},"end_ms":{},"text":"live at {t}","final":true,"received_at_unix":0}}"#,
                    t + 3_000
                )
            })
            .collect();
        syncsafe::write(root.join("live_transcript.jsonl"), lines.join("\n").as_bytes()).unwrap();
    }

    fn segments(root: &std::path::Path, chunk: u32) -> Vec<(u32, String)> {
        let st: transcript::SessionTranscript =
            serde_json::from_slice(&syncsafe::read(root.join("transcript.json")).unwrap()).unwrap();
        st.chunks
            .iter()
            .filter(|c| c.chunk_index == chunk)
            .flat_map(|c| c.tracks.iter().filter(|t| t.track != transcript::Track::System))
            .flat_map(|t| t.segments.iter().map(|s| (s.start_ms, s.text.clone())))
            .collect()
    }

    #[test]
    fn a_sleep_inside_a_chunk_leaves_live_lines_and_patches_on_the_audio() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let manifest = slept_session(root);
        write_live_with_a_hole(root);

        let promo = try_promote_live_transcript(root, &manifest, "daisy-0").expect("promotes");
        let starts: Vec<u32> = promo.spans.iter().map(|s| s.start_ms).collect();
        assert_eq!(starts, vec![0, 30_000, 57_000], "the sleep takes no time");
        assert_eq!(promo.gaps, vec![(63_000, 75_000)], "the hole is the only gap");

        // Live lines land in the chunk whose audio they heard.
        let c2 = segments(root, 2);
        assert_eq!(c2.last().map(|(t, _)| *t), Some(24_000), "54 s is 24 s into chunk 2: {c2:?}");
        assert_eq!(segments(root, 3).first().map(|(t, _)| *t), Some(0), "57 s opens chunk 3");

        let reader = MarkReader(Mutex::new(Vec::new()));
        let n = crate::commands::gap_patch::patch_gaps_on_disk(root, &reader, &promo.gaps, &promo.spans)
            .unwrap();
        assert_eq!(n, 2, "one slice per track");
        assert_eq!(
            *reader.0.lock().unwrap(),
            vec!["chunk 3 second 6".to_string(), "chunk 3 second 6".to_string()],
            "the slice decoded is the audio the hole covers"
        );
        let patched: Vec<(u32, String)> =
            segments(root, 3).into_iter().filter(|(_, t)| t.starts_with("chunk")).collect();
        assert_eq!(patched, vec![(6_000, "chunk 3 second 6".to_string())]);
    }
}

#[cfg(test)]
mod sidecar_tests {
    use super::*;

    #[test]
    fn diarizing_stage_sits_between_deduping_and_the_label_gate() {
        let dir = std::env::temp_dir().join(format!("daisy-fin-diarizing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        syncsafe::create_dir_all(&dir).unwrap();
        let mut events: Vec<(String, f32)> = Vec::new();
        let mut emit = |e: ProgressEvent| events.push((e.stage, e.progress));
        let mut last = Instant::now();
        last = next_stage(&mut emit, Some(&dir), last, None, "deduping", 0.6, None);
        last = next_stage(&mut emit, Some(&dir), last, Some("deduping"), "diarizing", DIARIZING_PROGRESS, None);
        assert_eq!(read_finalize_status(&dir).unwrap().stage, "diarizing");
        let _ = next_stage(&mut emit, Some(&dir), last, Some("diarizing"), "awaiting-labels", 0.75, Some("[1]"));
        let stages: Vec<&str> = events.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(stages, ["deduping", "diarizing", "awaiting-labels"]);
        assert!(events.windows(2).all(|w| w[0].1 < w[1].1), "progress rises across the stages");
        let _ = std::fs::remove_dir_all(&dir);
    }

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

#[cfg(test)]
mod reconcile_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    const CREATED: i64 = 1_700_000_000;

    /// A session the way a killed recorder leaves it: segment open, its
    /// chunks as given, a heartbeat `hb` (`pid`, `ts`, host line) or none.
    fn session(sessions: &Path, sid: &str, chunks: serde_json::Value, hb: Option<String>) -> PathBuf {
        let dir = sessions.join(sid);
        std::fs::create_dir_all(dir.join("chunks").join("0001")).unwrap();
        let manifest = serde_json::json!({
            "schema_version": 4, "session_id": sid, "created_at_unix_seconds": CREATED,
            "sample_rate": 16000, "channels": 1,
            "mic_source_id": 1, "mic_source_node_name": "m", "mic_source_description": "Desk mic",
            "system_source_id": 2, "system_source_node_name": "s", "system_source_description": "Speakers",
            "aec_mode": "always", "chunks": chunks, "finalized_at_unix_seconds": null,
            "meeting_id": "fictional-meeting", "tag_ids": [], "attendees": [],
            "recording_segments": [{
                "started_at_unix_seconds": CREATED, "stopped_at_unix_seconds": null,
                "first_chunk_index": 1, "last_chunk_index": null
            }],
            "interrupted": false, "speaker_map": [], "diarization_unavailable": false,
            "single_local_speaker": true, "cluster_sides": [], "mixed_clusters": [],
            "dismissed_suggested_tag_ids": []
        });
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        if let Some(hb) = hb {
            std::fs::write(dir.join("heartbeat"), hb).unwrap();
        }
        dir
    }

    fn one_open_chunk() -> serde_json::Value {
        serde_json::json!([{
            "index": 1, "started_at_unix_seconds": CREATED, "ended_at_unix_seconds": null,
            "duration_seconds": null,
            "mic_wav_relative": "chunks/0001/mic.wav", "system_wav_relative": "chunks/0001/system.wav",
            "mic_voice_processed": false
        }])
    }

    fn wav(path: &Path, seconds: usize) {
        transcript::wav_io::write_wav_mono_i16(path, &vec![0i16; seconds * 16_000], 16_000).unwrap();
    }

    fn manifest(dir: &Path) -> recording::manifest::SessionManifest {
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap()
    }

    fn hb_here(pid: u32) -> Option<String> {
        Some(format!("{pid}\n{}\n{}\n", CREATED + 5, recording::heartbeat::this_host()))
    }

    fn now() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    }

    #[test]
    fn a_session_this_machine_left_open_is_closed_at_the_end_of_its_audio() {
        let td = tempfile::tempdir().unwrap();
        // An earlier process on this machine; its heartbeat is seconds old,
        // the shape of a relaunch right after a kill.
        let dir = session(td.path(), "daisy-1", one_open_chunk(), hb_here(std::process::id() + 1));
        std::fs::write(
            dir.join("heartbeat"),
            format!("{}\n{}\n{}\n", std::process::id() + 1, now() - 5, recording::heartbeat::this_host()),
        )
        .unwrap();
        wav(&dir.join("chunks").join("0001").join("mic.wav"), 42);
        wav(&dir.join("chunks").join("0001").join("system.wav"), 42);

        let out = reconcile_abandoned_sessions(td.path(), |_| false);

        assert_eq!(out, vec![("daisy-1".to_string(), Reconciled::Closed)]);
        let m = manifest(&dir);
        assert_eq!(m.chunks[0].ended_at_unix_seconds, Some(CREATED + 42));
        assert_eq!(m.recording_segments[0].stopped_at_unix_seconds, Some(CREATED + 42));
        assert!(m.interrupted);
        assert!(!dir.join("heartbeat").exists(), "a closed session is no longer live");
    }

    #[test]
    fn a_start_that_never_opened_a_chunk_is_removed() {
        let td = tempfile::tempdir().unwrap();
        // The failed-start shape: the manifest still lists no chunk, the
        // mic file holds a bare header and the system file nothing.
        let dir = session(td.path(), "daisy-2", serde_json::json!([]), hb_here(std::process::id() + 1));
        wav(&dir.join("chunks").join("0001").join("mic.wav"), 0);
        std::fs::write(dir.join("chunks").join("0001").join("system.wav"), b"").unwrap();

        let out = reconcile_abandoned_sessions(td.path(), |_| false);

        assert_eq!(out, vec![("daisy-2".to_string(), Reconciled::Removed)]);
        assert!(!dir.exists());
    }

    #[test]
    fn live_sessions_are_left_open() {
        let td = tempfile::tempdir().unwrap();
        // This process is recording it.
        let mine = session(td.path(), "daisy-3", one_open_chunk(), hb_here(std::process::id()));
        // Another machine wrote a fresh heartbeat.
        let foreign = session(
            td.path(),
            "daisy-4",
            one_open_chunk(),
            Some(format!("77\n{}\nanother-machine\n", now() - 30)),
        );
        // The active recording, named by the caller.
        let active = session(td.path(), "daisy-5", one_open_chunk(), None);

        let out = reconcile_abandoned_sessions(td.path(), |sid| sid == "daisy-5");

        assert!(out.is_empty(), "{out:?}");
        for dir in [mine, foreign, active] {
            assert!(recording::manifest_ops::has_open_segment(&manifest(&dir)), "{}", dir.display());
        }
    }

    #[test]
    fn sessions_this_machine_cannot_prove_its_own_are_left_as_they_are() {
        let td = tempfile::tempdir().unwrap();
        let stale = now() - 2 * FOREIGN_HEARTBEAT_LIVE_SECS;
        // Another machine's recording, its heartbeat stale from sync lag or
        // clock skew; one with no chunk yet (a start still arriving).
        let foreign = session(td.path(), "daisy-6", one_open_chunk(), Some(format!("77\n{stale}\nanother-machine\n")));
        wav(&foreign.join("chunks").join("0001").join("mic.wav"), 3);
        let foreign_start = session(td.path(), "daisy-7", serde_json::json!([]), Some(format!("77\n{stale}\nanother-machine\n")));
        // No heartbeat at all: the manifest synced ahead of it.
        let no_hb = session(td.path(), "daisy-8", serde_json::json!([]), None);
        // A heartbeat from a build that wrote no host line.
        let old_build = session(td.path(), "daisy-9", serde_json::json!([]), Some(format!("77\n{stale}\n")));

        let out = reconcile_abandoned_sessions(td.path(), |_| false);

        assert!(out.is_empty(), "{out:?}");
        for dir in [foreign, foreign_start, no_hb, old_build] {
            assert!(dir.join("manifest.json").is_file(), "{} kept", dir.display());
            let m = manifest(&dir);
            assert!(recording::manifest_ops::has_open_segment(&m), "{} untouched", dir.display());
            assert!(!m.interrupted, "{} untouched", dir.display());
        }
    }

    #[test]
    fn a_chunkless_start_that_holds_audio_or_notes_is_closed_not_removed() {
        let td = tempfile::tempdir().unwrap();
        let audio = session(td.path(), "daisy-10", serde_json::json!([]), hb_here(std::process::id() + 1));
        wav(&audio.join("chunks").join("0001").join("mic.wav"), 2);
        let notes = session(td.path(), "daisy-11", serde_json::json!([]), hb_here(std::process::id() + 1));
        std::fs::write(notes.join("notes.json"), b"{\"md\":\"agenda\"}").unwrap();

        let mut out = reconcile_abandoned_sessions(td.path(), |_| false);
        out.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(
            out,
            vec![("daisy-10".to_string(), Reconciled::Closed), ("daisy-11".to_string(), Reconciled::Closed)]
        );
        for dir in [audio, notes] {
            assert!(!recording::manifest_ops::has_open_segment(&manifest(&dir)), "{}", dir.display());
        }
    }
}

#[cfg(test)]
mod claim_tests {
    use super::*;

    fn status_at(dir: &std::path::Path, stage: &str, updated_at_unix: i64) {
        let s = FinalizeStatus { stage: stage.into(), progress: 0.15, message: None, updated_at_unix };
        std::fs::write(dir.join(FINALIZE_STATUS_FILE), serde_json::to_vec(&s).unwrap()).unwrap();
    }

    #[test]
    fn a_session_has_one_finalize_at_a_time() {
        let td = tempfile::tempdir().unwrap();
        let first = FinalizeClaim::take(td.path(), "daisy-claim-1", SIDECAR_REFRESH).expect("first claim");
        assert!(finalize_running("daisy-claim-1"));
        assert!(FinalizeClaim::take(td.path(), "daisy-claim-1", SIDECAR_REFRESH).is_none());
        let other = FinalizeClaim::take(td.path(), "daisy-claim-2", SIDECAR_REFRESH);
        assert!(other.is_some(), "another session finalizes independently");
        drop(first);
        assert!(!finalize_running("daisy-claim-1"));
        assert!(FinalizeClaim::take(td.path(), "daisy-claim-1", SIDECAR_REFRESH).is_some());
    }

    #[test]
    fn a_second_finalize_of_a_running_session_does_not_start() {
        // Stop's cascade is still running when a Resume asks for another.
        let td = tempfile::tempdir().unwrap();
        let app = AppState::new(crate::profile::ProfileDir::at(td.path().join("daisy")).unwrap());
        let sid = "daisy-claim-3";
        let dir = app.profile.session_path(sid);
        std::fs::create_dir_all(&dir).unwrap();
        status_at(&dir, "transcribing", 1_000);
        let _running = FinalizeClaim::take(&dir, sid, SIDECAR_REFRESH).unwrap();

        let req = FinalizeRequest {
            session_id: sid.into(),
            summary_provider: None,
            model: None,
            skip_label_gate: true,
        };
        let r = finalize_and_summarize_impl(&app, &VaultState::default(), req, |_| {});

        assert!(matches!(r, Err(AppError::Config(_))), "{:?}", r.err());
        let s = read_finalize_status(&dir).unwrap();
        assert_eq!((s.stage.as_str(), s.updated_at_unix), ("transcribing", 1_000), "the running one's sidecar is untouched");
    }

    #[test]
    fn a_running_finalize_keeps_its_sidecar_fresh_through_a_long_stage() {
        let td = tempfile::tempdir().unwrap();
        status_at(td.path(), "transcribing", 1_000);
        let claim = FinalizeClaim::take(td.path(), "daisy-claim-4", Duration::from_millis(100)).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        drop(claim);
        let s = read_finalize_status(td.path()).unwrap();
        assert_eq!(s.stage, "transcribing");
        assert!(s.updated_at_unix > 1_000, "re-stamped while the stage ran");

        // A finished one is left as it is.
        status_at(td.path(), "done", 1_000);
        let claim = FinalizeClaim::take(td.path(), "daisy-claim-4", Duration::from_millis(100)).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        drop(claim);
        assert_eq!(read_finalize_status(td.path()).unwrap().updated_at_unix, 1_000);
    }
}
