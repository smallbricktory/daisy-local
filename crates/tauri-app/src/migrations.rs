// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! One-shot, version-specific data migrations, run once at vault unlock and
//! recorded in `<profile>/migrations.json`.
//!
//!  - Steps are idempotent and best-effort per item (items that fail to
//!    parse are skipped).
//!  - Steps run in declaration order; each is marked applied only on success.
use crate::error::{AppError, Result};
use crate::state::{AppState, VaultState};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
struct MigrationsFile {
    #[serde(default)]
    applied: Vec<String>,
}

fn migrations_path(app: &AppState) -> std::path::PathBuf {
    app.profile.root().join("migrations.json")
}

fn load_applied(app: &AppState) -> Vec<String> {
    syncsafe::read(migrations_path(app))
        .ok()
        .and_then(|b| serde_json::from_slice::<MigrationsFile>(&b).ok())
        .map(|f| f.applied)
        .unwrap_or_default()
}

fn mark_applied(app: &AppState, id: &str) -> Result<()> {
    mark_applied_at(&migrations_path(app), id)
}

/// Serializes the read-modify-write below.
static MARKER_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn mark_applied_at(path: &std::path::Path, id: &str) -> Result<()> {
    let _held = MARKER_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let mut applied = syncsafe::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<MigrationsFile>(&b).ok())
        .map(|f| f.applied)
        .unwrap_or_default();
    if !applied.iter().any(|a| a == id) {
        applied.push(id.to_string());
    }
    syncsafe::write_atomic(path, serde_json::to_vec_pretty(&MigrationsFile { applied })?)?;
    Ok(())
}

/// Runs all not-yet-applied migrations. Called from vault unlock. Failures
/// are logged per step and do not abort later steps or the unlock.
pub fn run_on_unlock(app: &AppState, vs: &VaultState) {
    let applied = load_applied(app);
    let steps: &[(&str, fn(&AppState, &VaultState) -> Result<()>)] = &[
        ("2026-05-vault-v3-tags-out", vault_v3_tags_out),
        ("2026-07-contacts-from-voiceprints", contacts_from_voiceprints),
        ("2026-07-purge-removed-providers", purge_removed_providers),
        ("2026-08-integration-history-jsonl", integration_history_to_jsonl),
        ("2026-09-tag-use-counts-zero", tag_use_counts_zero),
        ("2026-09-voice-samples", voice_samples),
        ("2026-09-diarized-sessions-get-a-run", diarized_sessions_get_a_run),
        ("2026-09-meta-speaker-maps", meta_speaker_maps),
        ("2026-09-table-names-to-meta", table_names_to_meta),
        ("2026-10-parakeet-models-out", parakeet_models_out),
    ];
    for (id, step) in steps {
        if applied.iter().any(|a| a == id) {
            continue;
        }
        match step(app, vs) {
            Ok(()) => {
                if let Err(e) = mark_applied(app, id) {
                    log::warn!("migration {id}: applied but marker write failed: {e}");
                } else {
                    log::info!("migration {id}: applied");
                }
            }
            Err(e) => log::warn!("migration {id}: failed (will retry next unlock): {e}"),
        }
    }

    // Per-machine marker: settings.<MACHINE>.json blocks migrate on their own machine.
    let mic_key_id = format!("2026-08-mic-id-to-key-{}", crate::hardware::machine_name());
    if !applied.iter().any(|a| a == &mic_key_id) {
        match mic_id_to_key_step(app) {
            Ok(()) => {
                if let Err(e) = mark_applied(app, &mic_key_id) {
                    log::warn!("migration {mic_key_id}: applied but marker write failed: {e}");
                } else {
                    log::info!("migration {mic_key_id}: applied");
                }
            }
            Err(e) => log::warn!("migration {mic_key_id}: failed (will retry next unlock): {e}"),
        }
    }

    let asr_id = format!("2026-10-asr-parakeet-out-{}", crate::hardware::machine_name());
    if !applied.iter().any(|a| a == &asr_id) {
        match app.update_machine_settings(drop_parakeet_asr) {
            Ok(_) => {
                if let Err(e) = mark_applied(app, &asr_id) {
                    log::warn!("migration {asr_id}: applied but marker write failed: {e}");
                } else {
                    log::info!("migration {asr_id}: applied");
                }
            }
            Err(e) => log::warn!("migration {asr_id}: failed (will retry next unlock): {e}"),
        }
    }

    let diarizer_id = format!("2026-10-diarizer-out-{}", crate::hardware::machine_name());
    if !applied.iter().any(|a| a == &diarizer_id) {
        match app.update_machine_settings(drop_diarizer_choice) {
            Ok(_) => {
                if let Err(e) = mark_applied(app, &diarizer_id) {
                    log::warn!("migration {diarizer_id}: applied but marker write failed: {e}");
                } else {
                    log::info!("migration {diarizer_id}: applied");
                }
            }
            Err(e) => log::warn!("migration {diarizer_id}: failed (will retry next unlock): {e}"),
        }
    }

    let faint_marker_id = format!("2026-10-faint-marker-out-{}", crate::hardware::machine_name());
    if !applied.iter().any(|a| a == &faint_marker_id) {
        match app.update_machine_settings(drop_faint_audio_marker) {
            Ok(_) => {
                if let Err(e) = mark_applied(app, &faint_marker_id) {
                    log::warn!("migration {faint_marker_id}: applied but marker write failed: {e}");
                } else {
                    log::info!("migration {faint_marker_id}: applied");
                }
            }
            Err(e) => log::warn!("migration {faint_marker_id}: failed (will retry next unlock): {e}"),
        }
    }

    // Profile-wide: the per-machine diarizer map in the synced settings file.
    let by_machine_id = "2026-10-diarizer-by-machine-out";
    if !applied.iter().any(|a| a == by_machine_id) {
        match app.update_settings(drop_diarizer_by_machine) {
            Ok(_) => {
                if let Err(e) = mark_applied(app, by_machine_id) {
                    log::warn!("migration {by_machine_id}: applied but marker write failed: {e}");
                } else {
                    log::info!("migration {by_machine_id}: applied");
                }
            }
            Err(e) => log::warn!("migration {by_machine_id}: failed (will retry next unlock): {e}"),
        }
    }

    // Also per-machine: the inbox is `sync.<MACHINE>.inbox`, so each machine
    // moves its own staging area under the device each record belongs to.
    let inbox_id = format!("2026-09-inbox-by-device-{}", crate::hardware::machine_name());
    if !applied.iter().any(|a| a == &inbox_id) {
        match crate::sync::server::inbox_of(app).adopt_device_paths(&app.profile.sessions_dir()) {
            Ok(n) => {
                log::info!("migration {inbox_id}: applied ({n} record(s))");
                if let Err(e) = mark_applied(app, &inbox_id) {
                    log::warn!("migration {inbox_id}: applied but marker write failed: {e}");
                }
            }
            Err(e) => log::warn!("migration {inbox_id}: failed (will retry next unlock): {e}"),
        }
    }
    let records_id = format!("2026-09-inbox-records-{}", crate::hardware::machine_name());
    if !applied.iter().any(|a| a == &records_id) {
        match crate::sync::server::inbox_of(app).drop_split_record_areas() {
            Ok(()) => {
                if let Err(e) = mark_applied(app, &records_id) {
                    log::warn!("migration {records_id}: applied but marker write failed: {e}");
                } else {
                    log::info!("migration {records_id}: applied");
                }
            }
            Err(e) => log::warn!("migration {records_id}: failed (will retry next unlock): {e}"),
        }
    }

    // Steps that read audio run on their own threads and write their marker on completion. A failed or interrupted run retries next unlock.
    const BACKFILL_ID: &str = "2026-07-speech-levels-backfill";
    if !applied.iter().any(|a| a == BACKFILL_ID) {
        let profile_root = app.profile.root().to_path_buf();
        let sessions_dir = app.profile.sessions_dir();
        run_in_background(BACKFILL_ID, migrations_path(app), move || {
            speech_levels_backfill_at(&profile_root, &sessions_dir)
        });
    }

    const PHONE_WAV_ID: &str = "2026-09-phone-chunks-to-wav";
    // After the phone-chunk conversion, which rewrites the same manifests on its own thread; until that has run, this waits for the next unlock.
    // Per machine: each desktop moves the sessions written there.
    let timeline_id = format!("2026-10-sample-true-timeline-{}", crate::hardware::machine_name());
    if applied.iter().any(|a| a == PHONE_WAV_ID) && !applied.iter().any(|a| a == &timeline_id) {
        let sessions_dir = app.profile.sessions_dir();
        run_in_background(timeline_id, migrations_path(app), move || {
            sample_true_timeline_at(&sessions_dir)
        });
    }

    if !applied.iter().any(|a| a == PHONE_WAV_ID) {
        let sessions_dir = app.profile.sessions_dir();
        run_in_background(PHONE_WAV_ID, migrations_path(app), move || {
            phone_chunks_to_wav_at(&sessions_dir)
        });
    }

    const TURN_CAP_ID: &str = "2026-10-transcripts-regroup-by-sentence";
    if !applied.iter().any(|a| a == TURN_CAP_ID) {
        let sessions_dir = app.profile.sessions_dir();
        run_in_background(TURN_CAP_ID, migrations_path(app), move || {
            regroup_transcripts_at(&sessions_dir)
        });
    }
}

/// Re-renders every session's `transcript.md` from its segments. A session
/// with no transcript is skipped. One whose manifest has not arrived or does
/// not parse, or that is still recording or finalizing, waits; so does one
/// that fails to render. Fails while any session waits, so the next unlock
/// tries again. Returns the sessions rendered.
fn regroup_transcripts_at(sessions_dir: &std::path::Path) -> Result<usize> {
    let mut rendered = 0usize;
    let mut waiting = 0usize;
    for dir in crate::commands::session_store::session_dirs(sessions_dir)? {
        if crate::commands::session_store::preferred_transcript_path(&dir).is_none() {
            continue;
        }
        let Some(m) = crate::commands::session_store::read_manifest_opt(&dir) else {
            log::info!("regroup: {} waits: its manifest does not parse", dir.display());
            waiting += 1;
            continue;
        };
        // A session still recording or finalizing renders its own transcript
        // when it finishes.
        let settled = m.finalized_at_unix_seconds.is_some()
            || crate::commands::finalize::read_finalize_status(&dir)
                .is_some_and(|s| matches!(s.stage.as_str(), "awaiting-labels" | "error"));
        if m.chunks.iter().any(|c| c.ended_at_unix_seconds.is_none()) || !settled {
            log::info!("regroup: {} waits: still recording or finalizing", dir.display());
            waiting += 1;
            continue;
        }
        match crate::commands::session::rerender_transcript_md_at(&dir) {
            Ok(()) => rendered += 1,
            Err(e) => {
                log::warn!("regroup: {} waits: {e}", dir.display());
                waiting += 1;
            }
        }
    }
    if waiting > 0 {
        return Err(AppError::Config(format!("{waiting} session(s) wait to regroup ({rendered} rendered)")));
    }
    Ok(rendered)
}

/// Moves the tags a v2 vault payload carried into `<profile>/tags.json` (when
/// that file is not there yet) and rewrites the vault without them, at
/// schema 3 or later. The tags are read from the payload on disk: the
/// in-memory keys never parse the field.
fn vault_v3_tags_out(app: &AppState, vs: &VaultState) -> Result<()> {
    use crate::commands::tags::{tags_path, TagsFile};
    let tags: Vec<crate::state::Tag> = match syncsafe::read(crate::commands::lifecycle::vault_path(app)) {
        Ok(bytes) if !bytes.is_empty() => {
            let env: vault::Envelope = serde_json::from_slice(&bytes)?;
            let plain = {
                let pass = vs.passphrase.lock().unwrap();
                let pass = pass.as_ref().ok_or(AppError::VaultLocked)?;
                vault::decrypt(&env, pass.as_str())
                    .map_err(|e| AppError::Config(format!("v3 tags decrypt: {e}")))?
            };
            let v: serde_json::Value = serde_json::from_slice(&plain)?;
            serde_json::from_value(v.get("tags").cloned().unwrap_or(serde_json::json!([])))
                .unwrap_or_default()
        }
        _ => Vec::new(),
    };
    {
        let mut g = vs.keys.lock().unwrap();
        let keys = g.as_mut().ok_or(AppError::VaultLocked)?;
        keys.schema_version = keys.schema_version.max(3);
    }
    let tp = tags_path(app);
    if !tp.is_file() && !tags.is_empty() {
        crate::commands::tags::save_tags_file(app, &TagsFile { tags, ..Default::default() })?;
    }
    crate::commands::lifecycle::re_encrypt_keys(app, vs)?;
    let _ = syncsafe::remove_file(app.profile.root().join(".migrated_v3"));
    Ok(())
}

/// Steps in flight, keyed by the marker file they will write and the step id.
static IN_FLIGHT: std::sync::Mutex<
    Option<std::collections::HashSet<(std::path::PathBuf, String)>>,
> = std::sync::Mutex::new(None);

/// Runs `step` on its own thread and marks `id` applied when it succeeds.
/// A second call for the same marker while the first is running does nothing.
/// The step returns how many items it touched.
fn run_in_background(
    id: impl Into<String>,
    marker: std::path::PathBuf,
    step: impl FnOnce() -> Result<usize> + Send + 'static,
) {
    let id: String = id.into();
    {
        let mut guard = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if !guard.get_or_insert_with(Default::default).insert((marker.clone(), id.clone())) {
            return;
        }
    }
    std::thread::spawn(move || {
        match step() {
            Ok(n) => {
                log::info!("migration {id}: applied ({n} session(s))");
                if let Err(e) = mark_applied_at(&marker, &id) {
                    log::warn!("migration {id}: marker write failed: {e}");
                }
            }
            Err(e) => log::warn!("migration {id}: failed (will retry next unlock): {e}"),
        }
        let mut guard = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(set) = guard.as_mut() {
            set.remove(&(marker, id));
        }
    });
}

/// Moves every session it can measure onto the sample-true timeline: stamps
/// its chunk lengths from its WAVs, then moves its speaker sample windows,
/// its chapters and its rendered transcript. Returns the sessions changed;
/// fails while any session waits to be measured.
fn sample_true_timeline_at(sessions_dir: &std::path::Path) -> Result<usize> {
    let mut changed = 0usize;
    let mut waiting = 0usize;
    for dir in crate::commands::session_store::session_dirs(sessions_dir).unwrap_or_default() {
        match retime_session(&dir) {
            Ok(Retimed::Changed) => changed += 1,
            Ok(Retimed::Unchanged) => {}
            Ok(Retimed::Waiting(why)) => {
                log::info!("timeline: {} waits: {why}", dir.display());
                waiting += 1;
            }
            Err(e) => log::warn!("timeline: {} skipped: {e}", dir.display()),
        }
    }
    if waiting > 0 {
        return Err(AppError::Config(format!(
            "{waiting} session(s) wait for their audio or their finalize ({changed} moved)"
        )));
    }
    Ok(changed)
}

/// What [`retime_session`] did with one session.
#[derive(Debug, PartialEq, Eq)]
enum Retimed {
    Changed,
    Unchanged,
    /// Not measurable yet, for the reason given.
    Waiting(&'static str),
}

/// Shift at or past which the rendered transcript is rendered again: its
/// times are whole seconds.
const RETIME_RENDER_MS: u64 = 1_000;

/// One session of [`sample_true_timeline_at`].
fn retime_session(dir: &std::path::Path) -> Result<Retimed> {
    use recording::manifest::SessionManifest;
    let Ok(bytes) = syncsafe::read(dir.join("manifest.json")) else { return Ok(Retimed::Unchanged) };
    let Ok(mut m) = serde_json::from_slice::<SessionManifest>(&bytes) else { return Ok(Retimed::Unchanged) };
    if !m.schema_is_supported() {
        return Ok(Retimed::Unchanged);
    }
    // A chunk still open is being recorded here or on another desktop, or
    // waits for crash recovery; a session between stop and its finalize
    // stamp is being finalized. Their manifests are still being written.
    if m.chunks.iter().any(|c| c.ended_at_unix_seconds.is_none()) {
        return Ok(Retimed::Waiting("a chunk is still open"));
    }
    if m.finalized_at_unix_seconds.is_none() {
        let settled = crate::commands::finalize::read_finalize_status(dir)
            .is_some_and(|s| matches!(s.stage.as_str(), "awaiting-labels" | "error"));
        if !settled {
            return Ok(Retimed::Waiting("its finalize has not finished"));
        }
    }
    let unmeasured = m.chunks.iter().any(|c| c.audio_samples.is_none());
    if unmeasured && !recording::manifest_ops::chunk_audio_on_disk(dir, &m) {
        return Ok(Retimed::Waiting("a chunk WAV is not on disk"));
    }

    let old = wall_chunk_starts_ms(&m);
    let before = m.chunk_offsets_ms();
    let stamped = unmeasured && recording::manifest_ops::stamp_chunk_audio_at(dir, &mut m);
    let new: Vec<(u32, u64, u64)> = m
        .chunk_spans_ms()
        .into_iter()
        .map(|(i, start, len)| (i, u64::from(start), len))
        .collect();

    let windows_move = !m.sample_spans_on_audio_timeline && !m.cluster_sample_spans.is_empty();
    if windows_move {
        for c in m.cluster_sample_spans.iter_mut() {
            for w in c.windows.iter_mut() {
                let start = retime_ms(u64::from(w.0), &old, &new);
                let end = start + u64::from(w.1.saturating_sub(w.0));
                *w = (clamp_u32(start), clamp_u32(end));
            }
        }
        m.sample_spans_on_audio_timeline = true;
    }

    let chapters_moved = retime_chapters(dir, &old, &new)?;
    let summary_folded = fold_chapters_if_moved(dir)?;
    let offsets = m.chunk_offsets_ms();
    let shift = offsets
        .iter()
        .map(|(i, at)| u64::from(*at).abs_diff(u64::from(before.get(i).copied().unwrap_or(0))))
        .chain(
            old.iter()
                .filter_map(|(i, o)| offsets.get(i).map(|n| o.abs_diff(u64::from(*n)))),
        )
        .max()
        .unwrap_or(0);
    // transcript.md an older build rendered sits on the wall clock; once the
    // chunks are measured it renders on the audio.
    if stamped
        && shift >= RETIME_RENDER_MS
        && crate::commands::session_store::preferred_transcript_path(dir).is_some()
    {
        crate::commands::session::render_transcript_md_on(dir, &offsets)?;
    }
    if stamped || windows_move {
        syncsafe::write_atomic(dir.join("manifest.json"), &serde_json::to_vec_pretty(&m)?)?;
    }
    Ok(if stamped || windows_move || chapters_moved || summary_folded {
        Retimed::Changed
    } else {
        Retimed::Unchanged
    })
}

/// Furthest a session's creation stamp sits before its first chunk on a
/// recording nobody re-dated.
const WALL_ZERO_LEAD_SECS: i64 = 300;

/// `(chunk_index, start_ms)` of each chunk on the wall-clock timeline: its wall-clock start minus the session's creation. A re-dated creation stamp is replaced by the first chunk's start.
fn wall_chunk_starts_ms(m: &recording::manifest::SessionManifest) -> Vec<(u32, u64)> {
    let first = m.chunks.iter().map(|c| c.started_at_unix_seconds).min().unwrap_or(m.created_at_unix_seconds);
    let lead = first.saturating_sub(m.created_at_unix_seconds);
    let zero = if (0..=WALL_ZERO_LEAD_SECS).contains(&lead) { m.created_at_unix_seconds } else { first };
    m.chunks
        .iter()
        .map(|c| {
            let secs = c.started_at_unix_seconds.saturating_sub(zero).clamp(0, 7 * 86_400);
            (c.index, secs as u64 * 1000)
        })
        .collect()
}

/// `t_ms` on the wall-clock timeline `old`, moved onto `new` (`(index,
/// start, len)`): the same distance into the same chunk, no further than its
/// end. A time before every chunk stays where it is.
fn retime_ms(t_ms: u64, old: &[(u32, u64)], new: &[(u32, u64, u64)]) -> u64 {
    let Some((index, old_start)) = old
        .iter()
        .filter(|(_, start)| *start <= t_ms)
        .max_by_key(|(i, start)| (*start, *i))
        .copied()
    else {
        return t_ms;
    };
    let Some(&(_, new_start, len)) = new.iter().find(|(i, _, _)| *i == index) else {
        return t_ms;
    };
    let into = t_ms - old_start;
    new_start.saturating_add(if len > 0 { into.min(len) } else { into })
}

fn clamp_u32(v: u64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Moves a schema-1 `chapters.json` from the wall-clock timeline `old` onto
/// `new` and writes it at schema 2. True when it moved.
fn retime_chapters(
    dir: &std::path::Path,
    old: &[(u32, u64)],
    new: &[(u32, u64, u64)],
) -> Result<bool> {
    use summarize::chapters::{hms_to_secs, SessionChapters};
    let path = dir.join("chapters.json");
    let Ok(bytes) = syncsafe::read(&path) else { return Ok(false) };
    let Ok(mut ch) = serde_json::from_slice::<SessionChapters>(&bytes) else { return Ok(false) };
    if ch.schema_version >= SessionChapters::SCHEMA {
        return Ok(false);
    }
    for c in ch.chapters.iter_mut() {
        let ms = u64::from(hms_to_secs(&c.start_hms)) * 1000;
        c.start_hms = hms(retime_ms(ms, old, new));
    }
    ch.schema_version = SessionChapters::SCHEMA;
    syncsafe::write_atomic(&path, &serde_json::to_vec_pretty(&ch)?)?;
    Ok(true)
}

/// Folds a schema-2 `chapters.json` into `summary.json` when the summary carries the same chapters at other times. True when it wrote.
fn fold_chapters_if_moved(dir: &std::path::Path) -> Result<bool> {
    use summarize::chapters::{hms_to_secs, SessionChapters};
    let Ok(bytes) = syncsafe::read(dir.join("chapters.json")) else { return Ok(false) };
    let Ok(ch) = serde_json::from_slice::<SessionChapters>(&bytes) else { return Ok(false) };
    if ch.schema_version < SessionChapters::SCHEMA {
        return Ok(false);
    }
    let Ok(bytes) = syncsafe::read(dir.join("summary.json")) else { return Ok(false) };
    let Ok(summary) = summarize::SessionSummary::decode(&bytes) else { return Ok(false) };
    let same_titles = summary.chapters.len() == ch.chapters.len()
        && summary.chapters.iter().zip(&ch.chapters).all(|(s, c)| s.title == c.title);
    let moved = summary
        .chapters
        .iter()
        .zip(&ch.chapters)
        .any(|(s, c)| s.start_ms != Some(u64::from(hms_to_secs(&c.start_hms)) * 1000));
    if !same_titles || !moved {
        return Ok(false);
    }
    crate::commands::chapters::fold_chapters(dir, &ch.chapters)?;
    Ok(true)
}

/// `HH:MM:SS` of whole seconds in `ms`.
fn hms(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Converts `mic.m4a` chunks of phone-synced sessions to `mic.wav`, names them in the manifest and marks the mic as hearing the room. A session made from an audio file gains its `recording_kind`. Returns the sessions changed; a chunk that does not decode fails the step, so it is retried next unlock.
fn phone_chunks_to_wav_at(sessions_dir: &std::path::Path) -> Result<usize> {
    let mut changed = 0;
    let mut failed = 0;
    for dir in crate::commands::session_store::session_dirs(sessions_dir).unwrap_or_default() {
        let mp = dir.join("manifest.json");
        let Ok(bytes) = syncsafe::read(&mp) else { continue };
        let Ok(mut m) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        let mut touched = false;
        let id = m["session_id"].as_str().unwrap_or_default().to_string();
        if id.starts_with("daisy-import-") && m.get("recording_kind").is_none_or(|k| k.is_null()) {
            m["recording_kind"] = serde_json::json!("imported_file");
            touched = true;
        }
        if m["recording_kind"].as_str() == Some("phone_room") {
            if m["single_local_speaker"] != serde_json::json!(false) {
                m["single_local_speaker"] = serde_json::json!(false);
                touched = true;
            }
            if let Some(chunks) = m["chunks"].as_array_mut() {
                for c in chunks {
                    let Some(rel) = c["mic_wav_relative"].as_str().map(str::to_string) else { continue };
                    if !rel.ends_with(".m4a") || !crate::sync::inbox::is_relative_file_path(&rel) {
                        continue;
                    }
                    let src = syncsafe::join_rel(&dir, &rel);
                    let wav_rel = crate::sync::import::wav_path_of(&rel);
                    let wav = syncsafe::join_rel(&dir, &wav_rel);
                    if !wav.is_file() {
                        let decoded = match recording::audio_import::decode_to_mono_16k(&src) {
                            Ok(d) => d,
                            Err(e) => {
                                log::warn!("migration: {id} {rel} does not decode: {e}");
                                failed += 1;
                                continue;
                            }
                        };
                        recording::write_wav_mono_i16(&wav, &decoded.pcm, recording::audio_import::TARGET_RATE)
                            .map_err(|e| AppError::Io(format!("write {}: {e}", wav.display())))?;
                    }
                    c["mic_wav_relative"] = serde_json::json!(wav_rel);
                    let _ = syncsafe::remove_file(&src);
                    touched = true;
                }
            }
        }
        if touched {
            syncsafe::write_atomic(&mp, serde_json::to_vec_pretty(&m)?)?;
            changed += 1;
        }
    }
    if failed > 0 {
        return Err(AppError::Config(format!("{failed} phone chunk(s) did not decode")));
    }
    Ok(changed)
}

/// Re-encrypts the vault, purging entries for removed providers (dropped at
/// parse — see `state::providers_drop_unknown`) from the file.
fn purge_removed_providers(app: &AppState, vs: &VaultState) -> Result<()> {
    crate::commands::lifecycle::re_encrypt_keys(app, vs)
}

/// Converts the legacy `integration_history.json` whole-array file into the rotated-JSONL store at `<profile>/integration_history/`, then deletes the old file. An unparseable legacy file is dropped.
fn tag_use_counts_zero(app: &AppState, _vs: &VaultState) -> Result<()> {
    let file = crate::commands::tags::load_tags_file(app)?;
    if file.tags.iter().any(|t| t.use_count != 0) {
        crate::commands::tags::save_tags_file(app, &file)?;
    }
    Ok(())
}

fn diarized_sessions_get_a_run(app: &AppState, _vs: &VaultState) -> Result<()> {
    diarized_sessions_get_a_run_at(&app.profile.sessions_dir())
}

fn diarized_sessions_get_a_run_at(sessions_dir: &std::path::Path) -> Result<()> {
    use recording::session_meta as meta;
    let mut failed = 0usize;
    for dir in crate::commands::session_store::session_dirs(sessions_dir).unwrap_or_default() {
        if meta::current_run(&dir).is_some() {
            continue;
        }
        let top_names = meta::path(&dir).is_file()
            && meta::load(&dir).extra.get("speaker_map").and_then(|v| v.as_object()).is_some_and(|m| !m.is_empty());
        if !top_names && !crate::commands::session_store::is_diarized(&dir) {
            continue;
        }
        if let Err(e) = meta::mint_run(&dir) {
            log::warn!("migration: {} got no run: {e}", dir.display());
            failed += 1;
        }
    }
    if failed > 0 {
        return Err(AppError::Recording(format!("{failed} session(s) got no run")));
    }
    Ok(())
}

fn meta_speaker_maps(app: &AppState, _vs: &VaultState) -> Result<()> {
    meta_speaker_maps_at(&app.profile.sessions_dir())
}

fn meta_speaker_maps_at(sessions_dir: &std::path::Path) -> Result<()> {
    use recording::session_meta as meta;
    let mut failed = 0usize;
    for dir in crate::commands::session_store::session_dirs(sessions_dir).unwrap_or_default() {
        if !meta::path(&dir).is_file() {
            continue;
        }
        let held = meta::load(&dir);
        if !held.extra.contains_key("speaker_map") && !held.extra.contains_key("diarization_id") {
            continue;
        }
        let names: meta::SpeakerMap =
            held.extra.get("speaker_map").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
        let run = match held.extra.get("diarization_id").and_then(|v| v.as_str()) {
            Some(run) => {
                if meta::current_run(&dir).is_none_or(|r| r == meta::LEGACY_RUN) {
                    meta::set_current_run(&dir, run).map_err(|e| AppError::Recording(e.to_string()))?;
                }
                Some(run.to_string())
            }
            // The step before gives every file with names here a run; a
            // file it could not give one stays as it is until the next unlock.
            None if !names.is_empty() => match meta::current_run(&dir) {
                Some(run) => Some(run),
                None => {
                    failed += 1;
                    continue;
                }
            },
            None => None,
        };
        meta::update(&dir, |m| {
            m.extra.remove("speaker_map");
            m.extra.remove("diarization_id");
            if let Some(run) = &run {
                let map = m.speaker_maps.entry(run.clone()).or_default();
                for (cluster, speaker) in &names {
                    map.entry(cluster.clone()).or_insert_with(|| speaker.clone());
                }
            }
        })
        .map_err(|e| AppError::Recording(e.to_string()))?;
    }
    if failed > 0 {
        return Err(AppError::Recording(format!("{failed} file(s) with names have no run")));
    }
    Ok(())
}

fn table_names_to_meta(app: &AppState, _vs: &VaultState) -> Result<()> {
    table_names_to_meta_at(&app.profile.sessions_dir())
}

fn table_names_to_meta_at(sessions_dir: &std::path::Path) -> Result<()> {
    use recording::session_meta as meta;
    for dir in crate::commands::session_store::session_dirs(sessions_dir).unwrap_or_default() {
        let Some(file) = recording::speakers::read(&dir).unwrap_or(None) else {
            continue;
        };
        let named: Vec<(u32, String)> = file
            .speakers
            .iter()
            .filter_map(|s| {
                let name = s.name.as_deref().map(str::trim).filter(|n| !n.is_empty())?;
                Some((s.id, name.to_string()))
            })
            .collect();
        if named.is_empty() {
            continue;
        }
        let Some(run) = meta::current_run(&dir) else { continue };
        let stamp = manifest_stamp(&dir);
        let carried = !meta::path(&dir).is_file();
        meta::update(&dir, |m| {
            for (cluster_id, name) in &named {
                if carried {
                    let cell = m.speaker_maps.get_mut(&run).and_then(|map| map.get_mut(&cluster_id.to_string()));
                    if let Some(cell) = cell {
                        if cell.display_name != *name {
                            cell.display_name = name.clone();
                            cell.contact_id = None;
                        }
                        continue;
                    }
                }
                m.set_speaker_if_absent(&run, *cluster_id, name, stamp);
            }
        })
        .map_err(|e| AppError::Recording(e.to_string()))?;
    }
    Ok(())
}

/// The second `SessionMeta::carried_over` would stamp a cell for this
/// session at: its manifest's `finalized_at_unix_seconds`, falling back to
/// `created_at_unix_seconds`; 0 with no manifest or an unparseable one.
fn manifest_stamp(dir: &std::path::Path) -> i64 {
    syncsafe::read(dir.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<recording::manifest::SessionManifest>(&b).ok())
        .map(|m| m.finalized_at_unix_seconds.unwrap_or(m.created_at_unix_seconds))
        .unwrap_or(0)
}

fn voice_samples(app: &AppState, vs: &VaultState) -> Result<()> {
    crate::commands::voiceprints::update_vault(app, vs, |keys| {
        for v in keys.voiceprints.iter_mut() {
            crate::sync::voices::mint_legacy(v);
        }
        Ok(())
    })
}

fn integration_history_to_jsonl(app: &AppState, _vs: &VaultState) -> Result<()> {
    use crate::commands::history::{append_history, HistoryEntry};
    let old = app.profile.root().join("integration_history.json");
    if !old.is_file() {
        return Ok(());
    }
    // A legacy file next to an existing JSONL dir means a partial store: it is dropped and re-converted.
    let dir = crate::commands::history::history_dir(app);
    if dir.is_dir() {
        syncsafe::remove_dir_all(&dir)?;
    }
    let mut entries: Vec<HistoryEntry> = syncsafe::read(&old)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    // Oldest first.
    entries.sort_by_key(|e| e.at_unix_seconds);
    for e in entries {
        append_history(app, e)?;
    }
    std::fs::remove_file(&old)?;
    Ok(())
}

/// Creates a Contact per voiceprint, links `voiceprint.contact_id`, and
/// stamps `speaker_map[].contact_id` on every historical manifest (matched
/// by exact email, else exact normalized name).
fn contacts_from_voiceprints(app: &AppState, vs: &VaultState) -> Result<()> {
    use crate::commands::contacts::{load_contacts, save_contacts, upsert_contact, Contact};

    let mut contacts = load_contacts(app)?;

    // 1. A Contact per voiceprint; vp-id → contact-id is kept for the vault link.
    let need: Vec<(String, String, Option<String>)> = {
        let g = vs.keys.lock().unwrap();
        let keys = g.as_ref().ok_or(AppError::VaultLocked)?;
        keys.voiceprints
            .iter()
            .filter(|v| v.contact_id.is_none())
            .map(|v| (v.id.clone(), v.display_name.clone(), v.email.clone()))
            .collect()
    };
    if !need.is_empty() {
        let mut map = std::collections::HashMap::new();
        for (vp_id, name, email) in &need {
            let cid = upsert_contact(&mut contacts, name, email.as_deref(), crate::now_unix());
            map.insert(vp_id.clone(), cid);
        }
        crate::commands::voiceprints::update_vault(app, vs, move |keys| {
            for v in keys.voiceprints.iter_mut() {
                if v.contact_id.is_none() {
                    if let Some(cid) = map.get(&v.id) {
                        v.contact_id = Some(cid.clone());
                    }
                }
            }
            Ok(())
        })?;
    }

    // 2. Stamps contact_id onto historical speaker labels, manifest by
    //    manifest. Match = exact email (case-insensitive) first, else exact
    //    normalized name. Unmatched labels stay unlinked.
    fn norm(s: &str) -> String {
        s.trim().to_lowercase()
    }
    let find = |contacts: &[Contact], email: Option<&str>, name: &str| -> Option<String> {
        let email_n = email.map(norm).filter(|e| !e.is_empty());
        if let Some(e) = &email_n {
            if let Some(c) = contacts.iter().find(|c| c.emails.iter().any(|x| norm(x) == *e)) {
                return Some(c.id.clone());
            }
        }
        let n = norm(name);
        contacts.iter().find(|c| norm(&c.display_name) == n).map(|c| c.id.clone())
    };
    let sessions = app.profile.sessions_dir();
    for dir in crate::commands::session_store::session_dirs(&sessions).unwrap_or_default() {
        let mpath = dir.join("manifest.json");
        let Some(mut m) = crate::commands::session_store::read_manifest_opt(&dir) else {
            continue;
        };
        let mut changed = false;
        for label in m.speaker_map.iter_mut() {
            if label.contact_id.is_none() {
                if let Some(cid) = find(&contacts, label.email.as_deref(), &label.display_name) {
                    label.contact_id = Some(cid);
                    changed = true;
                }
            }
        }
        if changed {
            if let Ok(bytes) = serde_json::to_vec_pretty(&m) {
                let _ = syncsafe::write_atomic(&mpath, &bytes);
            }
        }
    }

    save_contacts(app, &contacts)?;
    Ok(())
}

/// Seeds the per-device speech-level store from recordings already on disk: newest-first, up to 10 sessions per device, at most 4 chunks per session (stopping early once both anchor classes have ample windows). Imports (single-track) and sessions whose anchors fail the gate's validity rule are skipped. Returns the sessions recorded.
fn speech_levels_backfill_at(
    profile_root: &std::path::Path,
    sessions_dir: &std::path::Path,
) -> Result<usize> {
    use recording::speech_levels::{LevelSample, LevelSource, SpeechLevels};

    // Windows per anchor class at which a chunk stops contributing.
    const AMPLE_WINDOWS: usize = 60;

    let mut ids: Vec<(i64, std::path::PathBuf)> =
        crate::commands::session_store::session_dirs(sessions_dir)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|p| {
                let name = p.file_name()?.to_str()?;
                if name.starts_with("daisy-import-") {
                    return None;
                }
                let ts: i64 = name.strip_prefix("daisy-")?.parse().ok()?;
                Some((ts, p))
            })
            .collect();
    ids.sort_by_key(|(ts, _)| std::cmp::Reverse(*ts));

    let mut store = SpeechLevels::load(profile_root);
    let mut per_device: std::collections::HashMap<String, usize> = Default::default();
    let mut recorded = 0usize;
    for (ts, dir) in ids {
        let probe = ::recording::manifest::manifest_gate_probe(&dir);
        let (Some(dev), Some(voice_processed)) = (probe.mic_description, probe.mic_voice_processed)
        else {
            // Skip a session with no device or with chunks that mix capture domains.
            continue;
        };
        let n = per_device.entry(dev.clone()).or_insert(0);
        if *n >= 10 {
            continue;
        }
        let mut speech = Vec::new();
        let mut residue = Vec::new();
        let mut chunk_dirs: Vec<std::path::PathBuf> = std::fs::read_dir(dir.join("chunks"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        chunk_dirs.sort();
        for cd in chunk_dirs.iter().take(4) {
            if speech.len() >= AMPLE_WINDOWS && residue.len() >= AMPLE_WINDOWS {
                break;
            }
            let mic_path = if cd.join("mic_aec.wav").is_file() {
                cd.join("mic_aec.wav")
            } else {
                cd.join("mic.wav")
            };
            let Some((sp, rs)) =
                transcript::energy_gate::scan_chunk_files(&mic_path, &cd.join("system.wav"))
            else {
                continue;
            };
            speech.extend(sp);
            residue.extend(rs);
        }
        let anchors = transcript::energy_gate::anchors_from_windows(&speech, &residue);
        // Same validity rule as the finalize path: only threshold-grade
        // anchors teach the store.
        let (Some(speech_dbfs), true) = (anchors.speech_dbfs, anchors.threshold_dbfs.is_some())
        else {
            continue;
        };
        let sid = dir.file_name().and_then(|s| s.to_str()).map(String::from);
        store.record(&dev, LevelSample {
            at_unix: ts,
            session_id: sid,
            source: LevelSource::Meeting,
            speech_dbfs,
            residue_dbfs: anchors.residue_dbfs,
            voice_processed,
        });
        *n += 1;
        recorded += 1;
    }
    if recorded > 0 {
        store
            .save(profile_root)
            .map_err(|e| AppError::Config(format!("speech_levels save: {e}")))?;
    }
    Ok(recorded)
}

/// Converts this machine's persisted numeric mic id to the stable device key. Device absent: the preference is dropped. The numeric field is cleared either way.
fn mic_id_to_key_step(app: &AppState) -> Result<()> {
    // An enumeration failure fails the step (marker unwritten, retried next unlock).
    let sources = audio_engine::source::list_sources()
        .map_err(|e| AppError::Config(format!("mic-id-to-key: list sources: {e}")))?;
    app.update_machine_settings(|m| migrate_mic_entry(m, &sources))
        .map(|_| ())
        .map_err(|e| AppError::Config(format!("mic-id-to-key: {e}")))
}

fn migrate_mic_entry(
    m: &mut crate::settings::MachineSettings,
    sources: &[audio_engine::source::Source],
) {
    if m.default_mic_key.is_none() {
        m.default_mic_key =
            crate::commands::settings::mic_id_to_key(m.default_mic_source_id, sources);
    }
    m.default_mic_source_id = None;
}

/// Drops the `asr.parakeet` tune block and maps an `asr.active` naming the parakeet row to the nemotron row.
fn drop_parakeet_asr(m: &mut crate::settings::MachineSettings) {
    m.asr.unknown.remove("parakeet");
    if m.asr.active.as_deref() == Some("parakeet-tdt-0.6b-v2-int8") {
        m.asr.active = Some(providers_local::registry::NEMOTRON_ID.to_string());
    }
}

/// Removes the per-machine `diarizer` choice from a machine settings file.
fn drop_diarizer_choice(m: &mut crate::settings::MachineSettings) {
    m.unknown.remove("diarizer");
}

/// Removes the `asr.faint_audio_marker` toggle from a machine settings file.
fn drop_faint_audio_marker(m: &mut crate::settings::MachineSettings) {
    m.asr.unknown.remove("faint_audio_marker");
}

/// Removes the profile-wide `diarizer_by_machine` map.
fn drop_diarizer_by_machine(s: &mut crate::settings::Settings) {
    s.unknown.remove("diarizer_by_machine");
}

/// Deletes the profile's downloaded parakeet model dir and its partial
/// download files.
fn parakeet_models_out(app: &AppState, _vs: &VaultState) -> Result<()> {
    parakeet_models_out_at(&app.profile.models_dir())
}

fn parakeet_models_out_at(models_dir: &std::path::Path) -> Result<()> {
    let dir = models_dir.join("parakeet");
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| AppError::Io(format!("remove {}: {e}", dir.display())))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn parakeet_asr_block_drops_and_its_active_id_moves_to_nemotron() {
        let mut m = crate::settings::MachineSettings::default();
        m.asr.unknown.insert("parakeet".into(), serde_json::json!({"hop_s": 7}));
        m.asr.active = Some("parakeet-tdt-0.6b-v2-int8".into());
        super::drop_parakeet_asr(&mut m);
        assert!(!m.asr.unknown.contains_key("parakeet"));
        assert_eq!(m.asr.active.as_deref(), Some(providers_local::registry::NEMOTRON_ID));
    }

    fn unlock_with_machine_file(file: serde_json::Value) -> (AppState, tempfile::TempDir) {
        let (app, t) = app();
        std::fs::write(
            crate::settings::machine_settings_path(&app.profile.settings_path()),
            serde_json::to_vec(&file).unwrap(),
        )
        .unwrap();
        let vs = crate::state::VaultState::default();
        crate::commands::lifecycle::init_vault_impl(&app, &vs, "correct horse battery staple").unwrap();
        let vs2 = crate::state::VaultState::default();
        crate::commands::lifecycle::unlock_vault_impl(&app, &vs2, "correct horse battery staple").unwrap();
        run_on_unlock(&app, &vs2);
        (app, t)
    }

    #[test]
    fn live_speaker_names_are_on_after_unlock_for_a_file_that_predates_the_setting() {
        let (app, _t) = unlock_with_machine_file(serde_json::json!({ "ui_zoom": 1.5, "diarizer": "speakrs" }));
        let m = app.machine_settings();
        assert!(m.live_speaker_names);
        assert_eq!(m.ui_zoom, 1.5, "other values stay");
    }

    #[test]
    fn live_speaker_names_stay_off_after_unlock_when_the_file_says_off() {
        let (app, _t) =
            unlock_with_machine_file(serde_json::json!({ "live_speaker_names": false, "diarizer": "speakrs" }));
        assert!(!app.machine_settings().live_speaker_names);
    }

    #[test]
    fn diarizer_choice_drops_and_other_unknown_keys_stay() {
        let mut m = crate::settings::MachineSettings::default();
        m.unknown.insert("diarizer".into(), serde_json::json!("speakrs"));
        m.unknown.insert("some_later_option".into(), serde_json::json!(7));
        super::drop_diarizer_choice(&mut m);
        assert!(!m.unknown.contains_key("diarizer"));
        assert_eq!(m.unknown["some_later_option"], serde_json::json!(7));
        let once = m.clone();
        super::drop_diarizer_choice(&mut m);
        assert_eq!(m, once, "a second run changes nothing");
    }

    #[test]
    fn faint_audio_marker_toggle_drops_and_other_asr_keys_stay() {
        let json = serde_json::json!({
            "asr": { "active": "ggml-base.en.bin", "faint_audio_marker": false, "some_later_option": 7 },
        });
        let mut m: crate::settings::MachineSettings = serde_json::from_value(json).unwrap();
        assert_eq!(m.asr.unknown["faint_audio_marker"], serde_json::json!(false), "an old file still loads");
        super::drop_faint_audio_marker(&mut m);
        assert!(!m.asr.unknown.contains_key("faint_audio_marker"));
        assert_eq!(m.asr.unknown["some_later_option"], serde_json::json!(7));
        assert_eq!(m.asr.active.as_deref(), Some("ggml-base.en.bin"));
        let once = m.clone();
        super::drop_faint_audio_marker(&mut m);
        assert_eq!(m, once, "a second run changes nothing");
    }

    #[test]
    fn diarizer_by_machine_drops_and_other_unknown_keys_stay() {
        let mut s = crate::settings::Settings::load_or_default(std::path::Path::new("/nonexistent/settings.json"));
        s.unknown.insert("diarizer_by_machine".into(), serde_json::json!({"host-a": "kmeans"}));
        s.unknown.insert("some_later_option".into(), serde_json::json!(7));
        super::drop_diarizer_by_machine(&mut s);
        assert!(!s.unknown.contains_key("diarizer_by_machine"));
        assert_eq!(s.unknown["some_later_option"], serde_json::json!(7));
        let once = s.clone();
        super::drop_diarizer_by_machine(&mut s);
        assert_eq!(s, once, "a second run changes nothing");
    }

    #[test]
    fn old_settings_payloads_with_the_retired_keys_deserialize() {
        let m: crate::settings::MachineSettings =
            serde_json::from_value(serde_json::json!({ "diarizer": "speakrs", "ui_zoom": 1.5 })).unwrap();
        assert_eq!(m.unknown["diarizer"], serde_json::json!("speakrs"));
        assert_eq!(m.ui_zoom, 1.5);
        let s: crate::settings::Settings = serde_json::from_value(
            serde_json::json!({ "diarizer_by_machine": {"host-a": "kmeans"}, "diarizer": "kmeans" }),
        )
        .unwrap();
        assert_eq!(s.unknown["diarizer_by_machine"], serde_json::json!({"host-a": "kmeans"}));
    }

    #[test]
    fn another_active_model_is_kept() {
        let mut m = crate::settings::MachineSettings::default();
        m.asr.active = Some("ggml-base.en.bin".into());
        super::drop_parakeet_asr(&mut m);
        assert_eq!(m.asr.active.as_deref(), Some("ggml-base.en.bin"));
    }

    #[test]
    fn parakeet_model_dir_is_removed_and_its_absence_is_fine() {
        let td = tempfile::tempdir().unwrap();
        let d = td.path().join("parakeet/parakeet-tdt-0.6b-v2-int8");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("encoder.int8.onnx"), b"x").unwrap();
        std::fs::write(td.path().join("ggml-base.en.bin"), b"x").unwrap();
        super::parakeet_models_out_at(td.path()).unwrap();
        assert!(!td.path().join("parakeet").exists());
        assert!(td.path().join("ggml-base.en.bin").exists(), "other models stay");
        super::parakeet_models_out_at(td.path()).unwrap();
    }

    #[test]
    fn one_runs_names_move_under_that_run_and_a_file_with_no_run_gets_one() {
        let td = tempfile::tempdir().unwrap();
        let manifest = serde_json::json!({
            "schema_version": 4, "session_id": "daisy-1", "created_at_unix_seconds": 1, "sample_rate": 16000,
            "channels": 1, "chunks": [], "finalized_at_unix_seconds": 2,
        });
        let names = serde_json::json!({ "0": { "display_name": "Priya", "updated_at_unix_seconds": 9 } });
        let old = [
            ("daisy-1", serde_json::json!({ "schema_version": 1, "diarization_id": "run-a", "speaker_map": names, "zz": 1 })),
            ("daisy-2", serde_json::json!({ "schema_version": 1, "speaker_map": names })),
            ("daisy-3", serde_json::json!({ "schema_version": 1, "speaker_map": {} })),
        ];
        for (id, meta) in &old {
            let dir = td.path().join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
            std::fs::write(dir.join("meta.json"), serde_json::to_vec(meta).unwrap()).unwrap();
        }
        // A phone's session whose table names no run, and one whose table
        // the old file's run is recorded on.
        std::fs::create_dir_all(td.path().join("daisy-4")).unwrap();
        std::fs::write(td.path().join("daisy-4/manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::write(td.path().join("daisy-4/speakers.json"), br#"{"schema_version":2,"speakers":[{"id":0}]}"#).unwrap();
        std::fs::write(
            td.path().join("daisy-4/meta.json"),
            serde_json::to_vec(&serde_json::json!({ "schema_version": 1, "speaker_map": names })).unwrap(),
        )
        .unwrap();
        std::fs::write(td.path().join("daisy-1/speakers.json"), br#"{"schema_version":2,"speakers":[{"id":0}]}"#).unwrap();
        super::diarized_sessions_get_a_run_at(td.path()).unwrap();
        super::meta_speaker_maps_at(td.path()).unwrap();

        use recording::session_meta as meta;
        let four = td.path().join("daisy-4");
        assert_eq!(meta::current_run(&four).as_deref(), Some(meta::LEGACY_RUN), "no run is minted for a phone's table");
        let m = meta::load(&four);
        assert_eq!(m.speakers(meta::LEGACY_RUN).unwrap()["0"].display_name, "Priya", "the names go under the legacy run");
        assert!(!m.extra.contains_key("speaker_map"));
        assert!(recording::speakers::read(&four).unwrap().unwrap().diarization_id.is_none());
        assert_eq!(
            recording::speakers::read(&td.path().join("daisy-1")).unwrap().unwrap().diarization_id.as_deref(),
            Some("run-a"),
            "the run goes on the table the session reads"
        );
        let one = td.path().join("daisy-1");
        assert_eq!(meta::current_run(&one).as_deref(), Some("run-a"), "the run the names were written against");
        let m = meta::load(&one);
        assert_eq!(m.speakers("run-a").unwrap()["0"].display_name, "Priya");
        assert!(!m.extra.contains_key("speaker_map") && !m.extra.contains_key("diarization_id"));
        assert_eq!(m.extra["zz"], 1, "a key another Daisy wrote stays");

        let two = td.path().join("daisy-2");
        let run = meta::current_run(&two).expect("names with no run have the one the step before gave them");
        assert_eq!(meta::load(&two).speakers(&run).unwrap()["0"].display_name, "Priya");

        let three = td.path().join("daisy-3");
        assert!(meta::current_run(&three).is_none(), "no names, no run");
        assert!(meta::load(&three).speaker_maps.is_empty());

        let before = std::fs::read(one.join("meta.json")).unwrap();
        super::meta_speaker_maps_at(td.path()).unwrap();
        assert_eq!(std::fs::read(one.join("meta.json")).unwrap(), before, "running it twice changes nothing");
    }

    #[test]
    fn names_at_the_top_with_no_run_wait_for_one() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("daisy-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&thin_manifest()).unwrap()).unwrap();
        let old = serde_json::json!({ "schema_version": 1, "speaker_map": { "0": { "display_name": "Priya", "updated_at_unix_seconds": 9 } } });
        std::fs::write(dir.join("meta.json"), serde_json::to_vec(&old).unwrap()).unwrap();
        let before = std::fs::read(dir.join("meta.json")).unwrap();
        assert!(super::meta_speaker_maps_at(td.path()).is_err(), "retried at the next unlock");
        assert_eq!(std::fs::read(dir.join("meta.json")).unwrap(), before, "the names stay where they are");
    }

    fn session(td: &tempfile::TempDir, id: &str, manifest: serde_json::Value) -> std::path::PathBuf {
        let dir = td.path().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        dir
    }

    #[test]
    fn a_diarized_session_with_no_run_gets_one_once_with_its_manifests_names() {
        let td = tempfile::tempdir().unwrap();
        let mut named = thin_manifest();
        named["speaker_map"] = serde_json::json!([{ "cluster_id": 3, "display_name": "Priya", "contact_id": "c-3" }]);
        let one = session(&td, "daisy-1", named);
        recording::session_meta::update(&one, |m| m.set_title("Board review", 900)).unwrap();
        // Clusters on the transcript alone, no name given yet.
        let two = session(&td, "daisy-2", thin_manifest());
        let transcript = serde_json::json!({
            "schema_version": 1, "session_id": "daisy-2", "provider": "local", "model": "m",
            "transcribed_at_unix_seconds": 2,
            "chunks": [{ "chunk_index": 0, "tracks": [{ "track": "mic", "source_wav_relative": "chunks/0000/mic.wav",
                "segments": [{ "start_ms": 0, "end_ms": 900, "text": "Morning.", "confidence": null, "speaker_id": 1 }] }] }],
        });
        std::fs::write(two.join("transcript.json"), serde_json::to_vec(&transcript).unwrap()).unwrap();

        super::diarized_sessions_get_a_run_at(td.path()).unwrap();

        use recording::session_meta as meta;
        let run = meta::current_run(&one).expect("a named session gets a run");
        let cell = meta::load(&one).speakers(&run).cloned().expect("the manifest's names are seeded")["3"].clone();
        assert_eq!((cell.display_name.as_str(), cell.contact_id.as_deref(), cell.updated_at_unix_seconds), ("Priya", Some("c-3"), 2));
        assert!(meta::current_run(&two).is_some(), "clusters on the transcript get a run");

        let files = |d: &std::path::Path| (std::fs::read(d.join("manifest.json")).unwrap(), std::fs::read(d.join("meta.json")).unwrap());
        let before = files(&one);
        let two_before = std::fs::read(two.join("manifest.json")).unwrap();
        super::diarized_sessions_get_a_run_at(td.path()).unwrap();
        assert_eq!(files(&one), before, "running it twice changes nothing");
        assert_eq!(std::fs::read(two.join("manifest.json")).unwrap(), two_before);
    }

    #[test]
    fn a_speaker_table_or_no_clusters_gets_no_run() {
        let td = tempfile::tempdir().unwrap();
        let mut named = thin_manifest();
        named["speaker_map"] = serde_json::json!([{ "cluster_id": 0, "display_name": "Priya" }]);
        let phone = session(&td, "daisy-1", named);
        std::fs::write(phone.join("speakers.json"), br#"{"schema_version":2,"speakers":[{"id":0}]}"#).unwrap();
        let plain = session(&td, "daisy-2", thin_manifest());

        super::diarized_sessions_get_a_run_at(td.path()).unwrap();

        use recording::session_meta as meta;
        assert_eq!(meta::current_run(&phone).as_deref(), Some(meta::LEGACY_RUN), "the table's run stands");
        assert!(crate::commands::session_store::read_manifest_opt(&phone).unwrap().diarization_id.is_none(), "nothing minted");
        assert!(meta::current_run(&plain).is_none(), "no clusters, no run");
        assert!(!meta::path(&plain).exists());
    }

    /// A finalized session on the old wall-clock timeline: chunk 1 holds
    /// 6 s, chunk 2 spans 11 s of wall clock around a 5 s sleep and holds
    /// 6 s, then a 10 s pause, then chunk 3 holds 3 s. Old offsets 0, 6, 27 s;
    /// sample-true 0, 6, 12 s.
    fn slept_session(td: &tempfile::TempDir) -> std::path::PathBuf {
        let chunk = |i: u32, start: i64, end: i64| serde_json::json!({
            "index": i, "started_at_unix_seconds": start, "ended_at_unix_seconds": end,
            "duration_seconds": end - start,
            "mic_wav_relative": format!("chunks/{i:04}/mic.wav"),
            "system_wav_relative": format!("chunks/{i:04}/system.wav"),
            "mic_aec_wav_relative": null,
        });
        let mut m = thin_manifest();
        m["created_at_unix_seconds"] = 1000.into();
        m["chunks"] = serde_json::json!([chunk(1, 1000, 1006), chunk(2, 1006, 1017), chunk(3, 1027, 1030)]);
        m["cluster_sample_spans"] = serde_json::json!([{ "cluster_id": 0, "windows": [[2000, 3000], [28000, 29500]] }]);
        let dir = session(td, "daisy-1000", m);
        for (i, secs) in [(1u32, 6usize), (2, 6), (3, 3)] {
            let d = dir.join(format!("chunks/{i:04}"));
            std::fs::create_dir_all(&d).unwrap();
            let pcm = vec![1i16; secs * 16_000];
            recording::write_wav_mono_i16(&d.join("mic.wav"), &pcm, 16_000).unwrap();
            recording::write_wav_mono_i16(&d.join("system.wav"), &pcm, 16_000).unwrap();
        }
        let ch = summarize::chapters::SessionChapters {
            schema_version: 1, session_id: "daisy-1000".into(), model: "m".into(),
            generated_at_unix_seconds: 0,
            chapters: vec![summarize::chapters::Chapter {
                title: "After the pause".into(), start_hms: "00:00:28".into(), summary: None,
            }],
        };
        std::fs::write(dir.join("chapters.json"), serde_json::to_vec(&ch).unwrap()).unwrap();
        let summary = summarize::SessionSummary::hand_written("daisy-1000", "test", "# md", 1);
        std::fs::write(dir.join("summary.json"), summary.encode()).unwrap();
        crate::commands::chapters::fold_chapters(&dir, &ch.chapters).unwrap();
        let seg = |text: &str| transcript::Segment {
            start_ms: 1_000, end_ms: 2_000, text: text.into(), confidence: None, speaker_id: None,
            words: Vec::new(), split_group: None,
        };
        let st = transcript::SessionTranscript {
            schema_version: 1, session_id: "daisy-1000".into(), provider: "t".into(), model: "t".into(),
            transcribed_at_unix_seconds: 0,
            provisional: false,
            edited_at_unix_seconds: None,
            chunks: [1u32, 3].into_iter().map(|i| transcript::ChunkTranscript {
                chunk_index: i,
                tracks: vec![transcript::TrackTranscript {
                    track: transcript::Track::Mic,
                    source_wav_relative: format!("chunks/{i:04}/mic.wav").into(),
                    segments: vec![seg(&format!("chunk {i}."))],
                }],
            }).collect(),
        };
        std::fs::write(dir.join("transcript.json"), serde_json::to_vec(&st).unwrap()).unwrap();
        dir
    }

    fn read_json<T: serde::de::DeserializeOwned>(path: std::path::PathBuf) -> T {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn manifest_at(dir: &std::path::Path) -> recording::manifest::SessionManifest {
        read_json(dir.join("manifest.json"))
    }

    fn chapter_start(dir: &std::path::Path) -> (u32, String) {
        let ch: summarize::chapters::SessionChapters = read_json(dir.join("chapters.json"));
        (ch.schema_version, ch.chapters[0].start_hms.clone())
    }

    fn summary_chapter_ms(dir: &std::path::Path) -> Option<u64> {
        summarize::SessionSummary::decode(&std::fs::read(dir.join("summary.json")).unwrap()).unwrap().chapters[0]
            .start_ms
    }

    /// Asserts `dir` sits on the sample-true timeline, everything moved once.
    fn assert_moved_once(dir: &std::path::Path) {
        let m = manifest_at(dir);
        assert_eq!(m.cluster_sample_spans[0].windows, vec![(2000, 3000), (13000, 14500)]);
        assert!(m.sample_spans_on_audio_timeline);
        assert_eq!(chapter_start(dir), (2, "00:00:13".to_string()));
        assert_eq!(summary_chapter_ms(dir), Some(13_000));
    }

    #[test]
    fn a_session_with_a_sleep_and_a_pause_moves_onto_the_audio_timeline() {
        let td = tempfile::tempdir().unwrap();
        let dir = slept_session(&td);
        assert_eq!(sample_true_timeline_at(td.path()).unwrap(), 1);

        let m = manifest_at(&dir);
        assert_eq!(m.chunks[1].duration_seconds, Some(6), "the sleep is out of the duration");
        assert_eq!(m.chunks[1].audio_samples, Some(6 * 16_000));
        assert_eq!(m.chunks[1].ended_at_unix_seconds, Some(1017), "wall stamps stay");
        assert_moved_once(&dir);
        let md = std::fs::read_to_string(dir.join("transcript.md")).unwrap();
        assert!(md.contains("[00:00:13]"), "{md}");

        // Once moved, a second run finds nothing to do.
        assert_eq!(sample_true_timeline_at(td.path()).unwrap(), 0);
        assert_moved_once(&dir);
    }

    #[test]
    fn a_phone_recording_moves_too() {
        // This desktop diarized the phone's session and chaptered it from a
        // transcript.md it rendered on the wall clock.
        let td = tempfile::tempdir().unwrap();
        let dir = slept_session(&td);
        let mut m: serde_json::Value = read_json(dir.join("manifest.json"));
        m["recording_kind"] = "phone_room".into();
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();

        assert_eq!(sample_true_timeline_at(td.path()).unwrap(), 1);
        assert_moved_once(&dir);
    }

    #[test]
    fn a_run_cut_short_before_the_manifest_moves_nothing_twice() {
        // Chapters, summary and transcript.md written; the manifest not.
        let td = tempfile::tempdir().unwrap();
        let dir = slept_session(&td);
        let before = std::fs::read(dir.join("manifest.json")).unwrap();
        sample_true_timeline_at(td.path()).unwrap();
        std::fs::write(dir.join("manifest.json"), &before).unwrap();

        assert_eq!(sample_true_timeline_at(td.path()).unwrap(), 1);
        assert_moved_once(&dir);
    }

    #[test]
    fn files_another_desktop_moved_arrive_in_any_order_and_move_once() {
        // The moved manifest arrived; chapters.json and summary.json did not.
        let td = tempfile::tempdir().unwrap();
        let dir = slept_session(&td);
        let chapters = std::fs::read(dir.join("chapters.json")).unwrap();
        let summary = std::fs::read(dir.join("summary.json")).unwrap();
        sample_true_timeline_at(td.path()).unwrap();
        std::fs::write(dir.join("chapters.json"), &chapters).unwrap();
        std::fs::write(dir.join("summary.json"), &summary).unwrap();
        assert_eq!(sample_true_timeline_at(td.path()).unwrap(), 1);
        assert_moved_once(&dir);

        // The moved chapters arrived; the manifest did not.
        let td = tempfile::tempdir().unwrap();
        let dir = slept_session(&td);
        let manifest = std::fs::read(dir.join("manifest.json")).unwrap();
        sample_true_timeline_at(td.path()).unwrap();
        std::fs::write(dir.join("manifest.json"), &manifest).unwrap();
        assert_eq!(sample_true_timeline_at(td.path()).unwrap(), 1);
        assert_moved_once(&dir);

        // The moved chapters arrived; the summary did not.
        let td = tempfile::tempdir().unwrap();
        let dir = slept_session(&td);
        let summary = std::fs::read(dir.join("summary.json")).unwrap();
        sample_true_timeline_at(td.path()).unwrap();
        std::fs::write(dir.join("summary.json"), &summary).unwrap();
        assert_eq!(sample_true_timeline_at(td.path()).unwrap(), 1);
        assert_moved_once(&dir);
    }

    #[test]
    fn a_session_finalize_measured_before_its_windows_moved_still_moves_them() {
        // A finalize an older build left at awaiting-labels resumed here: its
        // stamp measured the chunks; the windows and chapters stayed put.
        let td = tempfile::tempdir().unwrap();
        let dir = slept_session(&td);
        let mut m = manifest_at(&dir);
        recording::manifest_ops::stamp_chunk_audio_at(&dir, &mut m);
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();

        assert_eq!(sample_true_timeline_at(td.path()).unwrap(), 1);
        assert_moved_once(&dir);
    }

    #[test]
    fn windows_this_build_diarized_stay_where_they_are() {
        let td = tempfile::tempdir().unwrap();
        let dir = slept_session(&td);
        let mut m: serde_json::Value = read_json(dir.join("manifest.json"));
        m["sample_spans_on_audio_timeline"] = true.into();
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        sample_true_timeline_at(td.path()).unwrap();
        assert_eq!(manifest_at(&dir).cluster_sample_spans[0].windows, vec![(2000, 3000), (28000, 29500)]);
    }

    /// Runs the step on a session that must wait, and checks nothing moved.
    fn assert_waits(td: &tempfile::TempDir, dir: &std::path::Path) {
        let before = std::fs::read(dir.join("manifest.json")).unwrap();
        assert!(sample_true_timeline_at(td.path()).is_err(), "the step stays unmarked");
        assert_eq!(std::fs::read(dir.join("manifest.json")).unwrap(), before);
        assert_eq!(chapter_start(dir), (1, "00:00:28".to_string()));
        assert!(!dir.join("transcript.md").exists());
    }

    #[test]
    fn a_session_still_recording_waits() {
        let td = tempfile::tempdir().unwrap();
        let dir = slept_session(&td);
        let mut m: serde_json::Value = read_json(dir.join("manifest.json"));
        m["chunks"][2]["ended_at_unix_seconds"] = serde_json::Value::Null;
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        assert_waits(&td, &dir);
    }

    #[test]
    fn a_session_being_finalized_waits_and_one_awaiting_labels_moves() {
        let td = tempfile::tempdir().unwrap();
        let dir = slept_session(&td);
        let mut m: serde_json::Value = read_json(dir.join("manifest.json"));
        m["finalized_at_unix_seconds"] = serde_json::Value::Null;
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        crate::commands::finalize::write_finalize_status(&dir, "transcribing", 0.3, None);
        assert_waits(&td, &dir);

        crate::commands::finalize::write_finalize_status(&dir, "awaiting-labels", 0.9, None);
        assert_eq!(sample_true_timeline_at(td.path()).unwrap(), 1);
        assert_moved_once(&dir);
    }

    #[test]
    fn a_session_whose_audio_is_not_here_waits_for_it() {
        // Still syncing, or pruned to meeting.opus: nothing is stamped from
        // the manifest's own numbers, and nothing moves.
        let td = tempfile::tempdir().unwrap();
        let dir = slept_session(&td);
        let wav = dir.join("chunks/0002/mic.wav");
        let sys = dir.join("chunks/0002/system.wav");
        std::fs::rename(&wav, td.path().join("mic.wav")).unwrap();
        std::fs::rename(&sys, td.path().join("system.wav")).unwrap();
        assert_waits(&td, &dir);
        assert!(manifest_at(&dir).chunks.iter().all(|c| c.audio_samples.is_none()));

        std::fs::rename(td.path().join("mic.wav"), &wav).unwrap();
        std::fs::rename(td.path().join("system.wav"), &sys).unwrap();
        assert_eq!(sample_true_timeline_at(td.path()).unwrap(), 1);
        assert_moved_once(&dir);
    }

    #[test]
    fn a_re_dated_session_moves_from_where_its_chunks_were() {
        // The user set the recording date a day later after it was chaptered.
        let td = tempfile::tempdir().unwrap();
        let dir = slept_session(&td);
        let mut m: serde_json::Value = read_json(dir.join("manifest.json"));
        m["created_at_unix_seconds"] = (1000 + 86_400).into();
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(sample_true_timeline_at(td.path()).unwrap(), 1);
        assert_moved_once(&dir);
    }

    #[test]
    fn a_malformed_manifest_from_another_device_does_not_panic() {
        let td = tempfile::tempdir().unwrap();
        let dir = slept_session(&td);
        let mut m: serde_json::Value = read_json(dir.join("manifest.json"));
        m["created_at_unix_seconds"] = i64::MIN.into();
        m["chunks"][0]["audio_samples"] = u64::MAX.into();
        m["chunks"][1]["started_at_unix_seconds"] = i64::MAX.into();
        m["chunks"][1]["mic_samples"] = u64::MAX.into();
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        let m = manifest_at(&dir);
        let _ = (m.chunk_offsets_ms(), m.capture_offsets_ms(), m.audio_ms(), m.capture_ms());
        let _ = sample_true_timeline_at(td.path());
        let m = manifest_at(&dir);
        let _ = (m.chunk_offsets_ms(), m.capture_offsets_ms(), m.audio_ms(), m.capture_ms());
    }

    fn thin_manifest() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 4, "session_id": "daisy-1", "created_at_unix_seconds": 1, "sample_rate": 16000,
            "channels": 1, "chunks": [], "finalized_at_unix_seconds": 2,
        })
    }

    #[test]
    fn a_table_only_name_lands_in_meta_and_on_the_manifest() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("daisy-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&thin_manifest()).unwrap()).unwrap();
        std::fs::write(
            dir.join("speakers.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 2, "diarization_id": "run-a",
                "speakers": [{ "id": 0, "name": "Priya" }, { "id": 1 }],
            }))
            .unwrap(),
        )
        .unwrap();

        super::table_names_to_meta_at(td.path()).unwrap();

        use recording::session_meta as meta;
        let m = meta::load(&dir);
        assert_eq!(m.speakers("run-a").unwrap()["0"].display_name, "Priya");
        assert_eq!(m.speakers("run-a").unwrap()["0"].updated_at_unix_seconds, 2, "stamped at finalized_at_unix_seconds");
        assert!(!m.speakers("run-a").unwrap().contains_key("1"), "an unnamed cluster gets no cell");
        let manifest_after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let labels = manifest_after["speaker_map"].as_array().unwrap();
        assert!(
            labels.iter().any(|l| l["cluster_id"] == 0 && l["display_name"] == "Priya"),
            "the projection reaches the manifest too"
        );
    }

    #[test]
    fn with_no_meta_file_the_table_name_wins_over_the_manifest_name_it_was_shown_over() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("daisy-1");
        std::fs::create_dir_all(&dir).unwrap();
        let mut manifest = thin_manifest();
        manifest["speaker_map"] = serde_json::json!([
            { "cluster_id": 0, "display_name": "Pri", "contact_id": "c-old" },
            { "cluster_id": 1, "display_name": "Sam", "contact_id": "c-sam" },
        ]);
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::write(
            dir.join("speakers.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 2, "diarization_id": "run-a",
                "speakers": [{ "id": 0, "name": "Priya" }, { "id": 1, "name": "Sam" }],
            }))
            .unwrap(),
        )
        .unwrap();

        super::table_names_to_meta_at(td.path()).unwrap();

        let m = recording::session_meta::load(&dir);
        let map = m.speakers("run-a").unwrap();
        assert_eq!((map["0"].display_name.as_str(), map["0"].contact_id.as_deref()), ("Priya", None), "the renamed row loses the old name's contact");
        assert_eq!((map["1"].display_name.as_str(), map["1"].contact_id.as_deref()), ("Sam", Some("c-sam")), "a name that agrees keeps its contact");
        let after: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert!(after["speaker_map"].as_array().unwrap().iter().any(|l| l["cluster_id"] == 0 && l["display_name"] == "Priya"));
    }

    #[test]
    fn an_existing_meta_cell_even_a_cleared_one_is_not_overwritten() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("daisy-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&thin_manifest()).unwrap()).unwrap();
        std::fs::write(
            dir.join("speakers.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 2, "diarization_id": "run-a",
                "speakers": [{ "id": 0, "name": "Priya" }, { "id": 1, "name": "Sam" }],
            }))
            .unwrap(),
        )
        .unwrap();

        use recording::session_meta as meta;
        meta::update(&dir, |m| {
            m.set_speaker("run-a", 0, "Rowan", None, 500);
            m.clear_speaker("run-a", 1, 500);
        })
        .unwrap();

        super::table_names_to_meta_at(td.path()).unwrap();

        let m = meta::load(&dir);
        assert_eq!(m.speakers("run-a").unwrap()["0"].display_name, "Rowan", "the existing name wins");
        assert_eq!(m.speakers("run-a").unwrap()["1"].display_name, "", "a cleared cell is not resurrected by the table");
    }

    #[test]
    fn a_legacy_table_with_no_run_lands_under_legacy() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("daisy-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&thin_manifest()).unwrap()).unwrap();
        std::fs::write(
            dir.join("speakers.json"),
            serde_json::to_vec(&serde_json::json!({ "schema_version": 2, "speakers": [{ "id": 0, "name": "Priya" }] })).unwrap(),
        )
        .unwrap();

        super::table_names_to_meta_at(td.path()).unwrap();

        use recording::session_meta as meta;
        let m = meta::load(&dir);
        assert_eq!(m.speakers(meta::LEGACY_RUN).unwrap()["0"].display_name, "Priya");
    }

    #[test]
    fn table_names_to_meta_is_idempotent() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("daisy-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&thin_manifest()).unwrap()).unwrap();
        std::fs::write(
            dir.join("speakers.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 2, "diarization_id": "run-a", "speakers": [{ "id": 0, "name": "Priya" }],
            }))
            .unwrap(),
        )
        .unwrap();

        super::table_names_to_meta_at(td.path()).unwrap();
        let before = std::fs::read(dir.join("meta.json")).unwrap();
        super::table_names_to_meta_at(td.path()).unwrap();
        assert_eq!(std::fs::read(dir.join("meta.json")).unwrap(), before, "running it twice changes nothing");
    }

    #[test]
    fn phone_chunks_become_wavs_and_file_imports_gain_their_kind() {
        let td = tempfile::tempdir().unwrap();
        let sessions = td.path().join("sessions");
        let phone = sessions.join("daisy-1700000000");
        std::fs::create_dir_all(phone.join("chunks/0001")).unwrap();
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../recording/tests/fixtures/burst-16k.m4a"),
            phone.join("chunks/0001/mic.m4a"),
        )
        .unwrap();
        std::fs::write(phone.join("manifest.json"), serde_json::to_vec(&serde_json::json!({
            "schema_version": 4, "session_id": "daisy-1700000000", "created_at_unix_seconds": 1_700_000_000i64,
            "sample_rate": 16000, "channels": 1, "recording_kind": "phone_room", "single_local_speaker": true,
            "chunks": [{"index": 1, "mic_wav_relative": "chunks/0001/mic.m4a", "duration_seconds": 2}],
        })).unwrap()).unwrap();
        let file = sessions.join("daisy-import-1700000042");
        std::fs::create_dir_all(&file).unwrap();
        std::fs::write(file.join("manifest.json"), serde_json::to_vec(&serde_json::json!({
            "schema_version": 4, "session_id": "daisy-import-1700000042", "created_at_unix_seconds": 1_700_000_042i64,
            "sample_rate": 16000, "channels": 1, "chunks": [],
        })).unwrap()).unwrap();
        let desktop = sessions.join("daisy-1700000141");
        std::fs::create_dir_all(&desktop).unwrap();
        let untouched = serde_json::to_vec(&serde_json::json!({
            "schema_version": 4, "session_id": "daisy-1700000141", "created_at_unix_seconds": 1_700_000_141i64,
            "sample_rate": 16000, "channels": 1, "chunks": [{"index": 1, "mic_wav_relative": "chunks/0001/mic.wav"}],
        })).unwrap();
        std::fs::write(desktop.join("manifest.json"), &untouched).unwrap();

        assert_eq!(super::phone_chunks_to_wav_at(&sessions).unwrap(), 2);

        let m: serde_json::Value = serde_json::from_slice(&std::fs::read(phone.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(m["chunks"][0]["mic_wav_relative"], "chunks/0001/mic.wav");
        assert_eq!(m["single_local_speaker"], false);
        assert!(phone.join("chunks/0001/mic.wav").is_file());
        assert!(!phone.join("chunks/0001/mic.m4a").exists());
        let f: serde_json::Value = serde_json::from_slice(&std::fs::read(file.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(f["recording_kind"], "imported_file");
        assert_eq!(std::fs::read(desktop.join("manifest.json")).unwrap(), untouched, "a desktop recording is not touched");

        assert_eq!(super::phone_chunks_to_wav_at(&sessions).unwrap(), 0, "idempotent");
    }

    use super::*;
    use crate::commands::contacts::{load_contacts, save_contacts, Contact};

    fn mic_src(id: u32, key: &str) -> audio_engine::source::Source {
        audio_engine::source::Source {
            id,
            key: audio_engine::topology::DeviceKey(key.to_string()),
            node_name: key.to_string(),
            description: format!("desc {key}"),
            kind: audio_engine::source::SourceKind::Mic,
            default_sample_rate: 48_000,
            default_channels: 1,
            is_virtual: false,
        }
    }

    #[test]
    fn mic_id_migrates_to_key_when_device_present() {
        let mut m = crate::settings::MachineSettings::default();
        m.default_mic_source_id = Some(42);
        migrate_mic_entry(&mut m, &[mic_src(42, "alsa_input.usb-Foo")]);
        assert_eq!(m.default_mic_key.as_deref(), Some("alsa_input.usb-Foo"));
        assert_eq!(m.default_mic_source_id, None);
    }

    #[test]
    fn mic_id_dropped_when_device_absent() {
        let mut m = crate::settings::MachineSettings::default();
        m.default_mic_source_id = Some(42);
        migrate_mic_entry(&mut m, &[mic_src(7, "other")]);
        assert_eq!(m.default_mic_key, None);
        assert_eq!(m.default_mic_source_id, None);
    }

    #[test]
    fn mic_key_already_set_is_never_overwritten() {
        let mut m = crate::settings::MachineSettings::default();
        m.default_mic_key = Some("keep-me".into());
        m.default_mic_source_id = Some(42);
        migrate_mic_entry(&mut m, &[mic_src(42, "alsa_input.usb-Foo")]);
        assert_eq!(m.default_mic_key.as_deref(), Some("keep-me"));
        assert_eq!(m.default_mic_source_id, None);
    }
    use recording::manifest::{AecMode, Attendee, AttendeeRole, SessionManifest, SpeakerLabel};

    fn app() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (AppState::new(profile), tmp)
    }

    #[test]
    fn a_catalog_written_with_counts_is_rewritten_with_zeros() {
        let (app, _t) = app();
        std::fs::write(app.profile.root().join("tags.json"), br##"{"schema_version":1,"tags":[
            {"id":"t1","name":"Ops","color_hex":"#123456","prompt_md":null,"created_at_unix_seconds":1,"use_count":44}]}"##).unwrap();
        tag_use_counts_zero(&app, &crate::state::VaultState::default()).unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(app.profile.root().join("tags.json")).unwrap()).unwrap();
        assert_eq!(v["tags"][0]["use_count"], 0);
        assert_eq!(v["tags"][0]["name"], "Ops");
    }

    fn manifest_with(labels: Vec<SpeakerLabel>) -> SessionManifest {
        SessionManifest {
            recording_kind: None,
            origin_device: None,
            extra: Default::default(),
            schema_version: 2, session_id: "s".into(), created_at_unix_seconds: 0,
            sample_rate: 16000, channels: 1, mic_source_id: 1,
            mic_source_node_name: "m".into(), mic_source_description: "m".into(),
            system_source_id: 2, system_source_node_name: "s".into(),
            system_source_description: "s".into(), aec_mode: AecMode::Disabled,
            chunks: vec![], finalized_at_unix_seconds: None, title: None,
            meeting_id: "mid".into(), tag_ids: vec![], notes_md_relative: None,
            attendees: vec![Attendee { display_name: "x".into(), role: AttendeeRole::Other }],
            calendar: None, recording_segments: vec![],
            speaker_map: labels, language: None, diarization_unavailable: false,
            single_local_speaker: true,
            expected_speakers: None,
            sent_integration_ids: vec![], cluster_sides: vec![], mixed_clusters: vec![], cluster_sample_spans: vec![], sample_spans_on_audio_timeline: false, speaker_separation: None, diarize_count: None, diarization_id: None, interrupted: false,
            denoise_applied: None,
            dismissed_suggested_tag_ids: vec![],
        }
    }

    #[test]
    fn links_voiceprints_into_contacts_and_vault() {
        use crate::state::Voiceprint;
        let (app, _t) = app();
        // One contact pre-exists under the same email.
        save_contacts(&app, &[Contact {
            extra: Default::default(), updated_at_unix_seconds: None,
            id: "pre".into(), display_name: "Alice Smith".into(),
            emails: vec!["alice@x.com".into()], created_at_unix_seconds: 1,
        }]).unwrap();
        let vs = crate::state::VaultState::default();
        let mut keys = crate::state::DecryptedKeys::default();
        keys.voiceprints = vec![
            Voiceprint { id: "vp1".into(), display_name: "Alice".into(), email: Some("ALICE@X.COM".into()),
                vector: vec![], vectors: vec![], created_at_unix_seconds: 0, session_ids: vec![], contact_id: None,
                samples: vec![], updated_at_unix_seconds: None, extra: Default::default() },
            Voiceprint { id: "vp2".into(), display_name: "Zed".into(), email: None,
                vector: vec![], vectors: vec![], created_at_unix_seconds: 0, session_ids: vec![], contact_id: None,
                samples: vec![], updated_at_unix_seconds: None, extra: Default::default() },
        ];
        *vs.keys.lock().unwrap() = Some(keys);
        *vs.passphrase.lock().unwrap() = Some(zeroize::Zeroizing::new("correct horse battery staple".into()));

        contacts_from_voiceprints(&app, &vs).unwrap();

        // vp1 reused the email-matched contact; vp2 got a fresh one.
        let g = vs.keys.lock().unwrap();
        let vps = &g.as_ref().unwrap().voiceprints;
        assert_eq!(vps[0].contact_id.as_deref(), Some("pre"));
        let zed_cid = vps[1].contact_id.clone().expect("vp2 linked");
        drop(g);
        let contacts = load_contacts(&app).unwrap();
        assert_eq!(contacts.len(), 2, "no duplicate for the email match");
        assert!(contacts.iter().any(|c| c.id == zed_cid && c.display_name == "Zed"));
        // The vault was re-encrypted to disk with the links.
        assert!(crate::commands::lifecycle::vault_path(&app).is_file());
    }

    #[test]
    fn integration_history_json_converts_to_jsonl_and_old_file_is_deleted() {
        use crate::commands::history::{read_history, HistoryEntry};
        let (app, _t) = app();
        let entry = |at: i64, id: &str| HistoryEntry {
            at_unix_seconds: at,
            session_id: "s1".into(),
            meeting_id: "m1".into(),
            meeting_title: Some("Acme kickoff".into()),
            integration_id: id.into(),
            integration_name: "Ops hook".into(),
            kind: "webhook".into(),
            payloads_sent: vec!["summary".into()],
            status: "ok".into(),
        };
        // Legacy file stored a whole array, not necessarily time-ordered.
        let old = app.profile.root().join("integration_history.json");
        syncsafe::write(&old, serde_json::to_vec(&[entry(200, "i2"), entry(100, "i1")]).unwrap())
            .unwrap();

        let vs = crate::state::VaultState::default();
        integration_history_to_jsonl(&app, &vs).unwrap();

        assert!(!old.exists(), "legacy file deleted");
        let back = read_history(&app, 10, 0).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].integration_id, "i2"); // newest first
        assert_eq!(back[1].integration_id, "i1");
        // Re-run with the file gone is a no-op.
        integration_history_to_jsonl(&app, &vs).unwrap();
        assert_eq!(read_history(&app, 10, 0).unwrap().len(), 2);

        // Partial previous run: the legacy file reappears next to the
        // already-populated JSONL dir (converted, then died before the
        // delete). Re-run replaces the partial store — no duplicates.
        syncsafe::write(&old, serde_json::to_vec(&[entry(200, "i2"), entry(100, "i1")]).unwrap())
            .unwrap();
        integration_history_to_jsonl(&app, &vs).unwrap();
        assert!(!old.exists(), "legacy file deleted on re-run");
        let back = read_history(&app, 10, 0).unwrap();
        assert_eq!(back.len(), 2, "re-run must not duplicate entries");
        assert_eq!(back[0].integration_id, "i2");
    }

    #[test]
    fn a_v2_vault_moves_its_tags_out() {
        let (app, _t) = app();
        let pass = "correct horse battery staple";
        let payload = serde_json::json!({
            "schema_version": 2,
            "providers": {},
            "tags": [{"id": "t-1", "name": "Acme", "color_hex": "#123456", "prompt_md": null,
                      "created_at_unix_seconds": 1, "use_count": 0}],
        });
        let env = vault::encrypt(&serde_json::to_vec(&payload).unwrap(), pass).unwrap();
        let path = crate::commands::lifecycle::vault_path(&app);
        syncsafe::write_atomic(&path, serde_json::to_vec(&env).unwrap()).unwrap();
        let vs = crate::state::VaultState::default();
        crate::commands::lifecycle::unlock_vault_impl(&app, &vs, pass).unwrap();

        vault_v3_tags_out(&app, &vs).unwrap();

        let tags: crate::commands::tags::TagsFile =
            serde_json::from_slice(&syncsafe::read(crate::commands::tags::tags_path(&app)).unwrap()).unwrap();
        assert_eq!(tags.tags.len(), 1);
        assert_eq!(tags.tags[0].name, "Acme");
        let env: vault::Envelope = serde_json::from_slice(&syncsafe::read(&path).unwrap()).unwrap();
        let back: serde_json::Value = serde_json::from_slice(&vault::decrypt(&env, pass).unwrap()).unwrap();
        assert!(back.get("tags").is_none(), "the vault is rewritten without them");
        assert_eq!(back["schema_version"], 3);
    }

    #[test]
    fn an_opened_vault_runs_every_step_and_marks_it() {
        let (app, _t) = app();
        let vs = crate::state::VaultState::default();
        crate::commands::lifecycle::init_vault_impl(&app, &vs, "correct horse battery staple").unwrap();
        let vs2 = crate::state::VaultState::default();
        crate::commands::lifecycle::unlock_vault_impl(&app, &vs2, "correct horse battery staple").unwrap();
        run_on_unlock(&app, &vs2);
        assert!(load_applied(&app).contains(&"2026-05-vault-v3-tags-out".to_string()));
        assert!(load_applied(&app).contains(&"2026-07-contacts-from-voiceprints".to_string()));
        // The backfill marks itself from its background thread.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !load_applied(&app).contains(&"2026-07-speech-levels-backfill".to_string()) {
            assert!(std::time::Instant::now() < deadline, "backfill marker never appeared");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn backfills_speech_levels_from_existing_sessions_idempotently() {
        let (app, _t) = app();
        let session = app.profile.sessions_dir().join("daisy-1784000000");
        syncsafe::create_dir_all(session.join("chunks/0001")).unwrap();
        syncsafe::write(
            session.join("manifest.json"),
            br#"{"mic_source_description":"Microphone (Test BRIO)"}"#,
        )
        .unwrap();
        // 30 s chunk: 15 s speech with system silent, then 15 s system active with mic residue.
        use crate::test_audio::{tone, write_wav};
        let mut mic = tone(15, 0.25);
        mic.extend(tone(15, 0.01));
        let mut sys = vec![0i16; 16_000 * 15];
        sys.extend(tone(15, 0.1));
        write_wav(&session.join("chunks/0001/mic_aec.wav"), &mic);
        write_wav(&session.join("chunks/0001/system.wav"), &sys);
        // An import session must be ignored; a listen-only session (mic all
        // but silent) must not teach an anchor.
        syncsafe::create_dir_all(app.profile.sessions_dir().join("daisy-import-1")).unwrap();
        let listen = app.profile.sessions_dir().join("daisy-1784000100");
        syncsafe::create_dir_all(listen.join("chunks/0001")).unwrap();
        syncsafe::write(
            listen.join("manifest.json"),
            br#"{"mic_source_description":"Quiet Mic"}"#,
        )
        .unwrap();
        write_wav(&listen.join("chunks/0001/mic.wav"), &tone(30, 0.0008));
        write_wav(&listen.join("chunks/0001/system.wav"), &tone(30, 0.1));

        let n = speech_levels_backfill_at(app.profile.root(), &app.profile.sessions_dir())
            .unwrap();
        assert_eq!(n, 1);
        let store = recording::speech_levels::SpeechLevels::load(app.profile.root());
        assert_eq!(store.devices["Microphone (Test BRIO)"].history.len(), 1);
        assert!(store.devices["Microphone (Test BRIO)"].history[0].speech_dbfs > -14.0);
        assert!(!store.devices.contains_key("Quiet Mic"));

        // Re-running replaces by session id — no duplicate history.
        speech_levels_backfill_at(app.profile.root(), &app.profile.sessions_dir()).unwrap();
        let store = recording::speech_levels::SpeechLevels::load(app.profile.root());
        assert_eq!(store.devices["Microphone (Test BRIO)"].history.len(), 1);
    }

    #[test]
    fn unparseable_manifest_is_skipped_and_good_ones_still_stamp() {
        let (app, _t) = app();
        save_contacts(&app, &[Contact {
            extra: Default::default(), updated_at_unix_seconds: None,
            id: "b".into(), display_name: "Bob".into(), emails: vec![], created_at_unix_seconds: 1,
        }]).unwrap();
        // One corrupt manifest, one good.
        let bad = app.profile.sessions_dir().join("corrupt");
        syncsafe::create_dir_all(&bad).unwrap();
        syncsafe::write(bad.join("manifest.json"), b"{not json").unwrap();
        let good = app.profile.sessions_dir().join("ok");
        syncsafe::create_dir_all(&good).unwrap();
        let m = manifest_with(vec![SpeakerLabel {
            extra: Default::default(),
            cluster_id: 0, display_name: "Bob".into(), email: None,
            voiceprint_id: None, match_confidence: None, contact_id: None,
        }]);
        syncsafe::write(good.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();

        let vs = crate::state::VaultState::default();
        *vs.keys.lock().unwrap() = Some(Default::default());
        contacts_from_voiceprints(&app, &vs).unwrap();

        let back: SessionManifest =
            serde_json::from_slice(&syncsafe::read(good.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(back.speaker_map[0].contact_id.as_deref(), Some("b"));
        // Corrupt file untouched, not deleted, no panic.
        assert_eq!(syncsafe::read(bad.join("manifest.json")).unwrap(), b"{not json");
    }

    #[test]
    fn stamps_contact_ids_onto_historical_labels_and_is_marker_gated() {
        let (app, _t) = app();
        // Existing contacts.
        save_contacts(&app, &[
            Contact { id: "a".into(), display_name: "Alice".into(), emails: vec!["alice@x.com".into()], created_at_unix_seconds: 1, extra: Default::default(), updated_at_unix_seconds: None },
            Contact { id: "b".into(), display_name: "Bob".into(), emails: vec![], created_at_unix_seconds: 1, extra: Default::default(), updated_at_unix_seconds: None },
        ]).unwrap();
        let dir = app.profile.sessions_dir().join("s1");
        syncsafe::create_dir_all(&dir).unwrap();
        let m = manifest_with(vec![
            SpeakerLabel { cluster_id: 0, display_name: "someone".into(), email: Some("ALICE@X.COM".into()), voiceprint_id: None, match_confidence: None, contact_id: None, extra: Default::default() },
            SpeakerLabel { cluster_id: 1, display_name: "bob".into(), email: None, voiceprint_id: None, match_confidence: None, contact_id: None, extra: Default::default() },
            SpeakerLabel { cluster_id: 2, display_name: "stranger".into(), email: None, voiceprint_id: None, match_confidence: None, contact_id: None, extra: Default::default() },
        ]);
        syncsafe::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();

        // The step runs with an empty unlocked vault.
        let vs = crate::state::VaultState::default();
        *vs.keys.lock().unwrap() = Some(Default::default());
        contacts_from_voiceprints(&app, &vs).unwrap();

        let back: SessionManifest =
            serde_json::from_slice(&syncsafe::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(back.speaker_map[0].contact_id.as_deref(), Some("a")); // email, case-insensitive
        assert_eq!(back.speaker_map[1].contact_id.as_deref(), Some("b")); // name, case-insensitive
        assert_eq!(back.speaker_map[2].contact_id, None); // no match → untouched
        assert_eq!(load_contacts(&app).unwrap().len(), 2); // no phantom contacts

        // Marker gating: run_on_unlock applies once, then skips.
        run_on_unlock(&app, &vs);
        assert!(load_applied(&app).contains(&"2026-07-contacts-from-voiceprints".to_string()));
        // The audio-reading steps write their markers from background
        // threads; wait for them so the marker count below is stable.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        for id in ["2026-07-speech-levels-backfill", "2026-09-phone-chunks-to-wav", "2026-10-transcripts-regroup-by-sentence"] {
            while !load_applied(&app).contains(&id.to_string()) {
                assert!(std::time::Instant::now() < deadline, "{id} marker never appeared");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        let count = load_applied(&app).len();
        run_on_unlock(&app, &vs); // no panic, no duplicate marker
        assert_eq!(load_applied(&app).len(), count);
    }

    #[test]
    fn regroup_rerenders_old_paragraphs_as_capped_turns_and_skips_sessions_without_a_transcript() {
        use transcript::{ChunkTranscript, Segment, SessionTranscript, Track, TrackTranscript};
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("daisy-1");
        syncsafe::create_dir_all(&dir).unwrap();
        let seg = |start_ms: u32, text: &str| Segment {
            start_ms, end_ms: start_ms + 1_000, text: text.into(), confidence: None, speaker_id: None,
            words: Vec::new(), split_group: None,
        };
        let st = SessionTranscript {
            schema_version: SessionTranscript::SCHEMA,
            session_id: "daisy-1".into(),
            provider: "local".into(),
            model: "whisper".into(),
            transcribed_at_unix_seconds: 0,
            provisional: false,
            edited_at_unix_seconds: None,
            chunks: vec![ChunkTranscript {
                chunk_index: 1,
                tracks: vec![TrackTranscript {
                    track: Track::MicAec,
                    source_wav_relative: std::path::PathBuf::from("chunks/0001/mic_aec.wav"),
                    segments: vec![seg(0, "One is here."), seg(1_500, "Two is here."), seg(3_000, "Three is here."), seg(4_500, "Four is here.")],
                }],
            }],
        };
        let mut done = manifest_with(vec![]);
        done.finalized_at_unix_seconds = Some(1);
        syncsafe::write(dir.join("manifest.json"), serde_json::to_vec(&done).unwrap()).unwrap();
        syncsafe::write(dir.join("transcript.json"), serde_json::to_vec(&st).unwrap()).unwrap();
        syncsafe::write(dir.join("transcript.md"), b"[00:00:00] **Me**: One is here. Two is here. Three is here. Four is here.\n").unwrap();
        let bare = tmp.path().join("daisy-2");
        syncsafe::create_dir_all(&bare).unwrap();
        syncsafe::write(bare.join("manifest.json"), serde_json::to_vec(&manifest_with(vec![])).unwrap()).unwrap();

        // A session still finalizing waits, and the step retries.
        let busy = tmp.path().join("daisy-3");
        syncsafe::create_dir_all(&busy).unwrap();
        syncsafe::write(busy.join("manifest.json"), serde_json::to_vec(&manifest_with(vec![])).unwrap()).unwrap();
        syncsafe::write(busy.join("transcript.json"), serde_json::to_vec(&st).unwrap()).unwrap();
        syncsafe::write(busy.join("transcript.md"), b"old").unwrap();
        assert!(regroup_transcripts_at(tmp.path()).is_err());
        assert_eq!(syncsafe::read(busy.join("transcript.md")).unwrap(), b"old");
        syncsafe::write(busy.join("manifest.json"), serde_json::to_vec(&done).unwrap()).unwrap();

        assert_eq!(regroup_transcripts_at(tmp.path()).unwrap(), 2);
        let md = String::from_utf8(syncsafe::read(dir.join("transcript.md")).unwrap()).unwrap();
        assert!(md.contains("[00:00:00] **Me**: One is here. Two is here. Three is here.\n"), "{md}");
        assert!(md.contains("[00:00:04] **Me**: Four is here.\n"), "{md}");
        assert!(!bare.join("transcript.md").exists());
    }
}
