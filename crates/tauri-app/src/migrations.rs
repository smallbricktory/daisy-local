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

fn mark_applied_at(path: &std::path::Path, id: &str) -> Result<()> {
    let mut applied = syncsafe::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<MigrationsFile>(&b).ok())
        .map(|f| f.applied)
        .unwrap_or_default();
    if !applied.iter().any(|a| a == id) {
        applied.push(id.to_string());
    }
    let tmp = path.with_extension("json.tmp");
    syncsafe::write(&tmp, serde_json::to_vec_pretty(&MigrationsFile { applied })?)?;
    syncsafe::rename(&tmp, path)?;
    Ok(())
}

/// Runs all not-yet-applied migrations. Called from vault unlock. Failures
/// are logged per step and do not abort later steps or the unlock.
pub fn run_on_unlock(app: &AppState, vs: &VaultState) {
    let applied = load_applied(app);
    let steps: &[(&str, fn(&AppState, &VaultState) -> Result<()>)] = &[
        ("2026-07-contacts-from-voiceprints", contacts_from_voiceprints),
        ("2026-07-purge-removed-providers", purge_removed_providers),
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

    // The speech-levels backfill reads WAVs (potentially GBs) and must not
    // block the unlock; it runs on its own thread and writes its marker on
    // completion. A failed/interrupted run retries next unlock.
    const BACKFILL_ID: &str = "2026-07-speech-levels-backfill";
    static BACKFILL_RUNNING: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    if !applied.iter().any(|a| a == BACKFILL_ID)
        && !BACKFILL_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        let profile_root = app.profile.root().to_path_buf();
        let sessions_dir = app.profile.sessions_dir();
        let marker = migrations_path(app);
        std::thread::spawn(move || {
            match speech_levels_backfill_at(&profile_root, &sessions_dir) {
                Ok(n) => {
                    log::info!("migration {BACKFILL_ID}: applied ({n} session(s) recorded)");
                    if let Err(e) = mark_applied_at(&marker, BACKFILL_ID) {
                        log::warn!("migration {BACKFILL_ID}: marker write failed: {e}");
                    }
                }
                Err(e) => {
                    log::warn!("migration {BACKFILL_ID}: failed (will retry next unlock): {e}")
                }
            }
            BACKFILL_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }
}

/// Re-encrypts the vault, purging entries for removed providers (dropped at
/// parse — see `state::providers_drop_unknown`) from the file.
fn purge_removed_providers(app: &AppState, vs: &VaultState) -> Result<()> {
    crate::commands::lifecycle::re_encrypt_keys(app, vs)
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
    if let Ok(rd) = std::fs::read_dir(&sessions) {
        for e in rd.flatten() {
            let mpath = e.path().join("manifest.json");
            let Ok(bytes) = syncsafe::read(&mpath) else { continue };
            let Ok(mut m) = serde_json::from_slice::<recording::manifest::SessionManifest>(&bytes)
            else {
                continue;
            };
            let mut changed = false;
            for label in m.speaker_map.iter_mut() {
                if label.contact_id.is_none() {
                    if let Some(cid) = find(&contacts, label.email.as_deref(), &label.display_name)
                    {
                        label.contact_id = Some(cid);
                        changed = true;
                    }
                }
            }
            if changed {
                let tmp = mpath.with_extension("json.tmp");
                if let Ok(bytes) = serde_json::to_vec_pretty(&m) {
                    let _ = syncsafe::write(&tmp, &bytes).and_then(|_| syncsafe::rename(&tmp, &mpath));
                }
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

    let mut ids: Vec<(i64, std::path::PathBuf)> = std::fs::read_dir(sessions_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_str()?.to_string();
            if !p.is_dir() || name.starts_with("daisy-import-") {
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
        let Some(dev) = crate::commands::manifest_gate_probe(&dir).mic_description else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::contacts::{load_contacts, save_contacts, Contact};
    use recording::manifest::{AecMode, Attendee, AttendeeRole, SessionManifest, SpeakerLabel};

    fn app() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (AppState::new(profile), tmp)
    }

    fn manifest_with(labels: Vec<SpeakerLabel>) -> SessionManifest {
        SessionManifest {
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
            sent_integration_ids: vec![], cluster_sides: vec![], interrupted: false,
            denoise_applied: None,
        }
    }

    #[test]
    fn links_voiceprints_into_contacts_and_vault() {
        use crate::state::Voiceprint;
        let (app, _t) = app();
        // One contact pre-exists under the same email.
        save_contacts(&app, &[Contact {
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
            id: "b".into(), display_name: "Bob".into(), emails: vec![], created_at_unix_seconds: 1,
        }]).unwrap();
        // One corrupt manifest, one good.
        let bad = app.profile.sessions_dir().join("corrupt");
        syncsafe::create_dir_all(&bad).unwrap();
        syncsafe::write(bad.join("manifest.json"), b"{not json").unwrap();
        let good = app.profile.sessions_dir().join("ok");
        syncsafe::create_dir_all(&good).unwrap();
        let m = manifest_with(vec![SpeakerLabel {
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
            Contact { id: "a".into(), display_name: "Alice".into(), emails: vec!["alice@x.com".into()], created_at_unix_seconds: 1 },
            Contact { id: "b".into(), display_name: "Bob".into(), emails: vec![], created_at_unix_seconds: 1 },
        ]).unwrap();
        let dir = app.profile.sessions_dir().join("s1");
        syncsafe::create_dir_all(&dir).unwrap();
        let m = manifest_with(vec![
            SpeakerLabel { cluster_id: 0, display_name: "someone".into(), email: Some("ALICE@X.COM".into()), voiceprint_id: None, match_confidence: None, contact_id: None },
            SpeakerLabel { cluster_id: 1, display_name: "bob".into(), email: None, voiceprint_id: None, match_confidence: None, contact_id: None },
            SpeakerLabel { cluster_id: 2, display_name: "stranger".into(), email: None, voiceprint_id: None, match_confidence: None, contact_id: None },
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
        let count = load_applied(&app).len();
        run_on_unlock(&app, &vs); // no panic, no duplicate marker
        assert_eq!(load_applied(&app).len(), count);
    }
}
