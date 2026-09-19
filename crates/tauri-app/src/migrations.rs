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

/// Serializes the read-modify-write below: background steps finish on their
/// own threads, and two of them writing this file at once lose an id.
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
        ("2026-07-contacts-from-voiceprints", contacts_from_voiceprints),
        ("2026-07-purge-removed-providers", purge_removed_providers),
        ("2026-08-integration-history-jsonl", integration_history_to_jsonl),
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

    // Per-machine marker: settings.<MACHINE>.json blocks migrate on their
    // own machine (the migrations marker file is profile-synced, so one
    // global id would strand the other machines' blocks).
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

    // Steps that read audio (potentially GBs) must not block the unlock;
    // each runs on its own thread and writes its marker on completion. A
    // failed or interrupted run retries next unlock.
    const BACKFILL_ID: &str = "2026-07-speech-levels-backfill";
    if !applied.iter().any(|a| a == BACKFILL_ID) {
        let profile_root = app.profile.root().to_path_buf();
        let sessions_dir = app.profile.sessions_dir();
        run_in_background(BACKFILL_ID, migrations_path(app), move || {
            speech_levels_backfill_at(&profile_root, &sessions_dir)
        });
    }

    const PHONE_WAV_ID: &str = "2026-09-phone-chunks-to-wav";
    if !applied.iter().any(|a| a == PHONE_WAV_ID) {
        let sessions_dir = app.profile.sessions_dir();
        run_in_background(PHONE_WAV_ID, migrations_path(app), move || {
            phone_chunks_to_wav_at(&sessions_dir)
        });
    }
}

/// Steps in flight, keyed by the marker file they will write and the step id.
/// One profile's run never blocks another's, so two profiles open at once
/// (and every test in this binary) each migrate.
static IN_FLIGHT: std::sync::Mutex<
    Option<std::collections::HashSet<(std::path::PathBuf, &'static str)>>,
> = std::sync::Mutex::new(None);

/// Runs `step` on its own thread and marks `id` applied when it succeeds.
/// A second call for the same marker while the first is running does nothing.
/// The step returns how many items it touched.
fn run_in_background(
    id: &'static str,
    marker: std::path::PathBuf,
    step: impl FnOnce() -> Result<usize> + Send + 'static,
) {
    {
        let mut guard = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if !guard.get_or_insert_with(Default::default).insert((marker.clone(), id)) {
            return;
        }
    }
    std::thread::spawn(move || {
        match step() {
            Ok(n) => {
                log::info!("migration {id}: applied ({n} session(s))");
                if let Err(e) = mark_applied_at(&marker, id) {
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

/// Sessions a phone synced before the import decoded audio hold `mic.m4a`
/// chunks nothing here reads and say the mic heard one person. Each such
/// chunk becomes the `mic.wav` the import writes today, the manifest names
/// it and says the mic heard the room. A session made from an audio file
/// gains the `recording_kind` the import stamps today. Returns the sessions
/// changed; a chunk that does not decode fails the step, so it is tried
/// again next unlock.
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
                    let src = dir.join(&rel);
                    let wav_rel = crate::sync::import::wav_path_of(&rel);
                    let wav = dir.join(&wav_rel);
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

/// Converts the legacy `integration_history.json` whole-array file into the
/// rotated-JSONL store at `<profile>/integration_history/`, then deletes the
/// old file. An unparseable legacy file is dropped (it was unreadable to the
/// old loader too).
fn integration_history_to_jsonl(app: &AppState, _vs: &VaultState) -> Result<()> {
    use crate::commands::history::{append_history, HistoryEntry};
    let old = app.profile.root().join("integration_history.json");
    if !old.is_file() {
        return Ok(());
    }
    // Legacy file still present next to an existing JSONL dir = a previous
    // run appended entries but died before deleting the file; the partial
    // store is dropped so this re-conversion can't duplicate entries.
    let dir = crate::commands::history::history_dir(app);
    if dir.is_dir() {
        syncsafe::remove_dir_all(&dir)?;
    }
    let mut entries: Vec<HistoryEntry> = syncsafe::read(&old)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    // Oldest first, so the JSONL append order matches event order.
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

/// Seeds the per-device speech-level store from recordings already on disk:
/// newest-first, up to 10 sessions per device, at most 4 chunks per session
/// (stopping early once both anchor classes have ample windows). Imports
/// (single-track) are skipped, as are sessions whose anchors don't pass the
/// gate's validity rule — a listen-only meeting must not teach noise-floor
/// peaks as the device's speech level. Returns the sessions recorded.
fn speech_levels_backfill_at(
    profile_root: &std::path::Path,
    sessions_dir: &std::path::Path,
) -> Result<usize> {
    use recording::speech_levels::{LevelSample, LevelSource, SpeechLevels};

    // A chunk yields ~300 windows; this many in each class is plenty.
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
            // No device, or a session whose chunks mix capture domains: its
            // levels belong to neither and teach nothing.
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

/// Converts this machine's persisted numeric mic id to the stable device
/// key. Device absent → the preference is dropped (system default takes
/// over at record time). The numeric field is cleared either way — it is a
/// resolved view from here on, never persisted truth.
fn mic_id_to_key_step(app: &AppState) -> Result<()> {
    // Enumeration failure must fail the step (marker unwritten, retried
    // next unlock) — an empty device world would silently drop the pref.
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

#[cfg(test)]
mod tests {
    #[test]
    fn phone_chunks_become_wavs_and_file_imports_gain_their_kind() {
        let td = tempfile::tempdir().unwrap();
        let sessions = td.path().join("sessions");
        let phone = sessions.join("daisy-1787249858");
        std::fs::create_dir_all(phone.join("chunks/0001")).unwrap();
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../recording/tests/fixtures/burst-16k.m4a"),
            phone.join("chunks/0001/mic.m4a"),
        )
        .unwrap();
        std::fs::write(phone.join("manifest.json"), serde_json::to_vec(&serde_json::json!({
            "schema_version": 4, "session_id": "daisy-1787249858", "created_at_unix_seconds": 1_787_249_858i64,
            "sample_rate": 16000, "channels": 1, "recording_kind": "phone_room", "single_local_speaker": true,
            "chunks": [{"index": 1, "mic_wav_relative": "chunks/0001/mic.m4a", "duration_seconds": 2}],
        })).unwrap()).unwrap();
        let file = sessions.join("daisy-import-1787249900");
        std::fs::create_dir_all(&file).unwrap();
        std::fs::write(file.join("manifest.json"), serde_json::to_vec(&serde_json::json!({
            "schema_version": 4, "session_id": "daisy-import-1787249900", "created_at_unix_seconds": 1_787_249_900i64,
            "sample_rate": 16000, "channels": 1, "chunks": [],
        })).unwrap()).unwrap();
        let desktop = sessions.join("daisy-1787249999");
        std::fs::create_dir_all(&desktop).unwrap();
        let untouched = serde_json::to_vec(&serde_json::json!({
            "schema_version": 4, "session_id": "daisy-1787249999", "created_at_unix_seconds": 1_787_249_999i64,
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
            sent_integration_ids: vec![], cluster_sides: vec![], mixed_clusters: vec![], cluster_sample_spans: vec![], speaker_separation: None, interrupted: false,
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
                vector: vec![], vectors: vec![], created_at_unix_seconds: 0, session_ids: vec![], contact_id: None },
            Voiceprint { id: "vp2".into(), display_name: "Zed".into(), email: None,
                vector: vec![], vectors: vec![], created_at_unix_seconds: 0, session_ids: vec![], contact_id: None },
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
    fn unlock_runs_migrations_once() {
        let (app, _t) = app();
        let vs = crate::state::VaultState::default();
        crate::commands::lifecycle::init_vault_impl(&app, &vs, "correct horse battery staple").unwrap();
        let vs2 = crate::state::VaultState::default();
        crate::commands::lifecycle::unlock_vault_impl(&app, &vs2, "correct horse battery staple").unwrap();
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
        // 30 s chunk: 15 s speech (-12 dB) w/ system silent, then 15 s system
        // active w/ mic residue (-40 dB).
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
        for id in ["2026-07-speech-levels-backfill", "2026-09-phone-chunks-to-wav"] {
            while !load_applied(&app).contains(&id.to_string()) {
                assert!(std::time::Instant::now() < deadline, "{id} marker never appeared");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        let count = load_applied(&app).len();
        run_on_unlock(&app, &vs); // no panic, no duplicate marker
        assert_eq!(load_applied(&app).len(), count);
    }
}
