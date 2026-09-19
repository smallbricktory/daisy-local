// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Naming, unnaming and forgetting the people in a meeting: the voiceprint
//! gallery in Settings and the speaker chips on a session.
//!
//! Voiceprints live in the vault, speaker labels live in the session's
//! manifest, and a name the user typed has to survive operations on the
//! other one. That crossing is what these drive.
use tauri_app_core::commands::lifecycle::{init_vault_impl, lock_vault_impl, unlock_vault_impl};
use tauri_app_core::commands::session::{
    add_session_speaker_impl, read_session_impl, remove_speaker_cluster_impl,
    set_session_speaker_label_impl,
};
use tauri_app_core::commands::voiceprints::{
    delete_voiceprint_impl, detach_speaker_voiceprint_impl, list_voiceprints_impl,
    rename_voiceprint_impl, VoiceprintRenameRequest,
};
use tauri_app_core::error::AppError;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, Voiceprint, VaultState};

const PASS: &str = "correct-horse-battery-staple-extended-22";

fn unlocked() -> (AppState, VaultState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let app = AppState::new(profile);
    let vs = VaultState::new();
    init_vault_impl(&app, &vs, PASS).unwrap();
    (app, vs, dir)
}

/// A voiceprint straight into the vault. Enrolling for real needs a model and
/// audio; every command under test here only reads the stored entry.
fn seed_voiceprint(vs: &VaultState, id: &str, name: &str, email: Option<&str>) {
    let mut g = vs.keys.lock().unwrap();
    let keys = g.as_mut().unwrap();
    keys.voiceprints.push(Voiceprint {
        id: id.into(),
        display_name: name.into(),
        email: email.map(str::to_string),
        vector: vec![],
        vectors: vec![vec![0.1; 8]],
        created_at_unix_seconds: 1_700_000_000,
        session_ids: vec![],
        contact_id: None,
    });
}

/// A finalized session carrying two diarized speakers, one of them matched to
/// a voiceprint.
fn synth_session(app: &AppState, id: &str, vp_id: Option<&str>) -> std::path::PathBuf {
    let dir = app.profile.session_path(id);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = serde_json::json!({
        "schema_version": 4,
        "session_id": id,
        "created_at_unix_seconds": 1_700_000_000i64,
        "sample_rate": 16000,
        "channels": 1,
        "mic_source_id": 1,
        "mic_source_node_name": "m",
        "mic_source_description": "M",
        "system_source_id": 2,
        "system_source_node_name": "s",
        "system_source_description": "S",
        "aec_mode": "disabled",
        "chunks": [],
        "finalized_at_unix_seconds": 1_700_000_130i64,
        "speaker_map": [
            {
                "cluster_id": 0,
                "display_name": "Rowan Hale",
                "email": "rowan@example.org",
                "voiceprint_id": vp_id,
                "match_confidence": vp_id.map(|_| 0.82),
            },
            { "cluster_id": 1, "display_name": "Speaker 2" },
        ],
    });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    dir
}

fn speaker_map(app: &AppState, sid: &str) -> Vec<serde_json::Value> {
    read_session_impl(app, sid).unwrap().manifest_json["speaker_map"]
        .as_array()
        .unwrap()
        .clone()
}

fn cluster(app: &AppState, sid: &str, cluster_id: u32) -> Option<serde_json::Value> {
    speaker_map(app, sid)
        .into_iter()
        .find(|s| s["cluster_id"] == serde_json::json!(cluster_id))
}

// ---- the gallery -----------------------------------------------------------

#[test]
fn the_gallery_is_empty_until_someone_is_enrolled() {
    let (app, vs, _d) = unlocked();
    assert!(list_voiceprints_impl(&app, &vs).unwrap().is_empty());
}

#[test]
fn a_locked_vault_shows_no_one_and_changes_no_one() {
    let (app, vs, _d) = unlocked();
    seed_voiceprint(&vs, "vp-1", "Rowan Hale", None);
    lock_vault_impl(&app, &vs).unwrap();

    assert!(matches!(
        list_voiceprints_impl(&app, &vs),
        Err(AppError::VaultLocked)
    ));
    assert!(matches!(
        delete_voiceprint_impl(&app, &vs, "vp-1"),
        Err(AppError::VaultLocked)
    ));
    assert!(matches!(
        rename_voiceprint_impl(
            &app,
            &vs,
            VoiceprintRenameRequest {
                id: "vp-1".into(),
                display_name: "Someone Else".into(),
                email: None,
            },
        ),
        Err(AppError::VaultLocked)
    ));
}

#[test]
fn renaming_someone_survives_locking_the_vault() {
    let (app, vs, _d) = unlocked();
    seed_voiceprint(&vs, "vp-1", "Rowan", Some("rowan@example.org"));

    rename_voiceprint_impl(
        &app,
        &vs,
        VoiceprintRenameRequest {
            id: "vp-1".into(),
            display_name: "  Rowan Hale  ".into(),
            email: Some("  ROWAN@example.org ".into()),
        },
    )
    .unwrap();

    let v = &list_voiceprints_impl(&app, &vs).unwrap()[0];
    assert_eq!(v.display_name, "Rowan Hale", "the typed spaces are dropped");
    assert_eq!(v.email.as_deref(), Some("ROWAN@example.org"));

    // The vault was re-encrypted, so a reopen still knows the new name.
    lock_vault_impl(&app, &vs).unwrap();
    unlock_vault_impl(&app, &vs, PASS).unwrap();
    assert_eq!(
        list_voiceprints_impl(&app, &vs).unwrap()[0].display_name,
        "Rowan Hale"
    );
}

#[test]
fn a_blank_name_is_refused_and_the_old_one_stands() {
    let (app, vs, _d) = unlocked();
    seed_voiceprint(&vs, "vp-1", "Rowan Hale", None);
    for blank in ["", "   ", "\t\n"] {
        let e = rename_voiceprint_impl(
            &app,
            &vs,
            VoiceprintRenameRequest {
                id: "vp-1".into(),
                display_name: blank.into(),
                email: None,
            },
        )
        .unwrap_err();
        assert!(matches!(e, AppError::Config(_)), "{blank:?} -> {e:?}");
    }
    assert_eq!(
        list_voiceprints_impl(&app, &vs).unwrap()[0].display_name,
        "Rowan Hale"
    );
}

#[test]
fn clearing_the_email_field_removes_it_rather_than_storing_blank() {
    let (app, vs, _d) = unlocked();
    seed_voiceprint(&vs, "vp-1", "Rowan Hale", Some("rowan@example.org"));
    rename_voiceprint_impl(
        &app,
        &vs,
        VoiceprintRenameRequest {
            id: "vp-1".into(),
            display_name: "Rowan Hale".into(),
            email: Some("   ".into()),
        },
    )
    .unwrap();
    assert_eq!(list_voiceprints_impl(&app, &vs).unwrap()[0].email, None);
}

#[test]
fn renaming_someone_who_is_not_in_the_gallery_is_an_error() {
    let (app, vs, _d) = unlocked();
    seed_voiceprint(&vs, "vp-1", "Rowan Hale", None);
    let e = rename_voiceprint_impl(
        &app,
        &vs,
        VoiceprintRenameRequest {
            id: "vp-nobody".into(),
            display_name: "Ghost".into(),
            email: None,
        },
    )
    .unwrap_err();
    assert!(matches!(e, AppError::Config(_)), "{e:?}");
    assert_eq!(
        list_voiceprints_impl(&app, &vs).unwrap().len(),
        1,
        "and no one is invented to hold the name"
    );
}

#[test]
fn deleting_someone_removes_only_them() {
    let (app, vs, _d) = unlocked();
    seed_voiceprint(&vs, "vp-1", "Rowan Hale", None);
    seed_voiceprint(&vs, "vp-2", "Abel Okafor", None);

    delete_voiceprint_impl(&app, &vs, "vp-1").unwrap();
    let left = list_voiceprints_impl(&app, &vs).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].display_name, "Abel Okafor");

    // Deleting the same person twice says so rather than silently passing.
    assert!(delete_voiceprint_impl(&app, &vs, "vp-1").is_err());
    assert!(delete_voiceprint_impl(&app, &vs, "vp-nobody").is_err());

    lock_vault_impl(&app, &vs).unwrap();
    unlock_vault_impl(&app, &vs, PASS).unwrap();
    assert_eq!(
        list_voiceprints_impl(&app, &vs).unwrap().len(),
        1,
        "the deletion is on disk, not only in memory"
    );
}

// ---- unlinking a speaker from the person -----------------------------------

#[test]
fn detaching_a_speaker_keeps_the_name_and_drops_the_match() {
    let (app, vs, _d) = unlocked();
    seed_voiceprint(&vs, "vp-1", "Rowan Hale", None);
    synth_session(&app, "daisy-1", Some("vp-1"));

    let before = cluster(&app, "daisy-1", 0).unwrap();
    assert_eq!(before["voiceprint_id"], serde_json::json!("vp-1"));

    detach_speaker_voiceprint_impl(&app, "daisy-1", 0).unwrap();

    let after = cluster(&app, "daisy-1", 0).unwrap();
    assert_eq!(after["voiceprint_id"], serde_json::Value::Null);
    assert_eq!(after["match_confidence"], serde_json::Value::Null);
    assert_eq!(
        after["display_name"],
        serde_json::json!("Rowan Hale"),
        "the person is still called what they were called"
    );
    assert_eq!(
        list_voiceprints_impl(&app, &vs).unwrap().len(),
        1,
        "and the gallery entry itself is untouched"
    );
}

#[test]
fn detaching_a_speaker_that_was_never_matched_changes_nothing() {
    let (app, _vs, _d) = unlocked();
    synth_session(&app, "daisy-1", None);
    let before = speaker_map(&app, "daisy-1");
    detach_speaker_voiceprint_impl(&app, "daisy-1", 1).unwrap();
    assert_eq!(speaker_map(&app, "daisy-1"), before);
}

#[test]
fn detaching_is_refused_for_a_session_that_is_not_there() {
    let (app, _vs, _d) = unlocked();
    assert!(matches!(
        detach_speaker_voiceprint_impl(&app, "daisy-missing", 0),
        Err(AppError::SessionNotFound(_))
    ));
}

#[test]
fn detaching_leaves_the_other_speakers_alone() {
    let (app, _vs, _d) = unlocked();
    synth_session(&app, "daisy-1", Some("vp-1"));
    detach_speaker_voiceprint_impl(&app, "daisy-1", 0).unwrap();
    assert_eq!(
        cluster(&app, "daisy-1", 1).unwrap()["display_name"],
        serde_json::json!("Speaker 2")
    );
}

// ---- removing a speaker from a session -------------------------------------

#[test]
fn removing_a_cluster_takes_it_out_of_the_session_only() {
    let (app, vs, _d) = unlocked();
    seed_voiceprint(&vs, "vp-1", "Rowan Hale", None);
    synth_session(&app, "daisy-1", Some("vp-1"));

    remove_speaker_cluster_impl(&app, "daisy-1", 0).unwrap();

    assert!(cluster(&app, "daisy-1", 0).is_none(), "the chip is gone");
    assert!(cluster(&app, "daisy-1", 1).is_some(), "the other speaker stays");
    assert_eq!(
        list_voiceprints_impl(&app, &vs).unwrap().len(),
        1,
        "removing a chip must not delete the person from the gallery"
    );
}

#[test]
fn removing_a_cluster_that_is_not_there_is_not_an_error() {
    let (app, _vs, _d) = unlocked();
    synth_session(&app, "daisy-1", None);
    remove_speaker_cluster_impl(&app, "daisy-1", 77).unwrap();
    assert_eq!(speaker_map(&app, "daisy-1").len(), 2);
}

#[test]
fn removing_from_a_session_that_is_not_there_is_refused() {
    let (app, _vs, _d) = unlocked();
    assert!(matches!(
        remove_speaker_cluster_impl(&app, "daisy-missing", 0),
        Err(AppError::SessionNotFound(_))
    ));
}

// ---- adding a speaker the diarizer never found -----------------------------

#[test]
fn an_added_speaker_gets_an_id_that_cannot_collide_with_a_diarized_one() {
    let (app, _vs, _d) = unlocked();
    synth_session(&app, "daisy-1", None);

    let first = add_session_speaker_impl(
        &app,
        "daisy-1",
        "Abel Okafor".into(),
        Some("abel@example.org".into()),
        None,
    )
    .unwrap();
    assert!(
        first >= 10_000,
        "hand-added speakers start above the diarizer's range, got {first}"
    );

    let second =
        add_session_speaker_impl(&app, "daisy-1", "Zoe Winters".into(), None, None).unwrap();
    assert_eq!(second, first + 1, "the next one takes the next free id");

    let added = cluster(&app, "daisy-1", first).unwrap();
    assert_eq!(added["display_name"], serde_json::json!("Abel Okafor"));
    assert_eq!(added["email"], serde_json::json!("abel@example.org"));
    assert_eq!(
        added["match_confidence"],
        serde_json::Value::Null,
        "a speaker the user typed has no match score"
    );
}

#[test]
fn an_added_speaker_can_be_linked_to_someone_in_the_gallery() {
    let (app, vs, _d) = unlocked();
    seed_voiceprint(&vs, "vp-1", "Rowan Hale", None);
    synth_session(&app, "daisy-1", None);
    let id = add_session_speaker_impl(
        &app,
        "daisy-1",
        "Rowan Hale".into(),
        None,
        Some("vp-1".into()),
    )
    .unwrap();
    assert_eq!(
        cluster(&app, "daisy-1", id).unwrap()["voiceprint_id"],
        serde_json::json!("vp-1")
    );
}

#[test]
fn a_speaker_needs_a_name() {
    let (app, _vs, _d) = unlocked();
    synth_session(&app, "daisy-1", None);
    for blank in ["", "  ", "\n"] {
        assert!(
            add_session_speaker_impl(&app, "daisy-1", blank.into(), None, None).is_err(),
            "{blank:?} is not a name"
        );
    }
    assert_eq!(speaker_map(&app, "daisy-1").len(), 2, "and none was added");
}

#[test]
fn a_typed_name_has_its_spaces_trimmed() {
    let (app, _vs, _d) = unlocked();
    synth_session(&app, "daisy-1", None);
    let id = add_session_speaker_impl(&app, "daisy-1", "  Abel Okafor \n".into(), None, None)
        .unwrap();
    assert_eq!(
        cluster(&app, "daisy-1", id).unwrap()["display_name"],
        serde_json::json!("Abel Okafor")
    );
}

#[test]
fn adding_to_a_session_that_is_not_there_is_refused() {
    let (app, _vs, _d) = unlocked();
    assert!(matches!(
        add_session_speaker_impl(&app, "daisy-missing", "Abel".into(), None, None),
        Err(AppError::SessionNotFound(_))
    ));
}

#[test]
fn a_speaker_added_then_removed_frees_nothing_and_the_next_id_still_climbs() {
    let (app, _vs, _d) = unlocked();
    synth_session(&app, "daisy-1", None);
    let first = add_session_speaker_impl(&app, "daisy-1", "Abel".into(), None, None).unwrap();
    remove_speaker_cluster_impl(&app, "daisy-1", first).unwrap();
    let second = add_session_speaker_impl(&app, "daisy-1", "Zoe".into(), None, None).unwrap();
    assert_eq!(
        second, first,
        "the freed id is reused once nothing references it"
    );
    assert_eq!(
        cluster(&app, "daisy-1", second).unwrap()["display_name"],
        serde_json::json!("Zoe")
    );
}

// ---- renaming a speaker on the session -------------------------------------

#[test]
fn renaming_a_speaker_changes_this_session_only() {
    let (app, vs, _d) = unlocked();
    seed_voiceprint(&vs, "vp-1", "Rowan Hale", None);
    synth_session(&app, "daisy-1", Some("vp-1"));
    synth_session(&app, "daisy-2", Some("vp-1"));

    set_session_speaker_label_impl(&app, "daisy-1", 0, "Ro".into(), None).unwrap();

    assert_eq!(
        cluster(&app, "daisy-1", 0).unwrap()["display_name"],
        serde_json::json!("Ro")
    );
    assert_eq!(
        cluster(&app, "daisy-2", 0).unwrap()["display_name"],
        serde_json::json!("Rowan Hale"),
        "the other meeting keeps the name it had"
    );
    assert_eq!(
        list_voiceprints_impl(&app, &vs).unwrap()[0].display_name,
        "Rowan Hale",
        "and so does the gallery"
    );
}
