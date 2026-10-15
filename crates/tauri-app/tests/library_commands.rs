// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The commands the library screen and a session's own pane call: notes,
//! tags on a session, the people dropdown, the storage figures, playback
//! audio, chapters, and the "save as" writer.
//!
//! Each test builds the on-disk state it needs and drives the command that
//! the frontend actually calls.
use tauri_app_core::commands::chapters::load_session_chapters_impl;
use tauri_app_core::commands::contacts::{list_contacts_impl, save_contacts, Contact};
use tauri_app_core::commands::files::save_text_file_impl;
use tauri_app_core::commands::finalize::mark_session_complete_impl;
use tauri_app_core::commands::library::list_sessions_impl;
use tauri_app_core::commands::lifecycle::{init_vault_impl, lock_vault_impl};
use tauri_app_core::commands::meeting::{
    create_note_session_impl, import_session_impl, session_assign_tags_impl, CreateNoteRequest,
    ImportSessionRequest,
};
use tauri_app_core::commands::playback::{
    file_url, session_has_playback_audio_impl, session_playback_audio_bytes_impl,
    session_playback_audio_url_impl,
};
use tauri_app_core::commands::recordings::recordings_stats_impl;
use tauri_app_core::commands::session::delete_session_impl;
use tauri_app_core::commands::session_store::is_safe_session_id;
use tauri_app_core::commands::tags::{create_tag_impl, list_tags_impl, search_tags_impl, CreateTagRequest};
use tauri_app_core::error::AppError;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, VaultState};

const PASS: &str = "correct-horse-battery-staple-extended-22";

fn temp_app() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    (AppState::new(profile), dir)
}

fn unlocked() -> (AppState, VaultState, tempfile::TempDir) {
    let (app, dir) = temp_app();
    let vs = VaultState::new();
    init_vault_impl(&app, &vs, PASS).unwrap();
    (app, vs, dir)
}

/// A finalized session on disk. Serde defaults fill everything not named.
fn synth_session(app: &AppState, id: &str, ts: i64) -> std::path::PathBuf {
    let dir = app.profile.session_path(id);
    std::fs::create_dir_all(dir.join("chunks/0001")).unwrap();
    let manifest = serde_json::json!({
        "schema_version": 4,
        "session_id": id,
        "created_at_unix_seconds": ts,
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
        "finalized_at_unix_seconds": ts + 130,
    });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    dir
}

fn read_manifest(app: &AppState, id: &str) -> serde_json::Value {
    let p = app.profile.session_path(id).join("manifest.json");
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn write_manifest(app: &AppState, id: &str, m: &serde_json::Value) {
    let p = app.profile.session_path(id).join("manifest.json");
    std::fs::write(p, serde_json::to_vec_pretty(m).unwrap()).unwrap();
}

fn tag(app: &AppState, name: &str) -> String {
    create_tag_impl(
        app,
        CreateTagRequest {
            name: name.into(),
            color_hex: "#3B4B9B".into(),
            prompt_md: None,
            vocab_md: None,
        },
    )
    .unwrap()
    .id
}

/// A tag's count as the app reads it after a change: every command that
/// moves a session's tags emits `library:changed`, which drops the cached
/// session list the count is taken from.
fn use_count(app: &AppState, id: &str) -> i64 {
    tauri_app_core::commands::library::invalidate_library_cache();
    list_tags_impl(app)
        .unwrap()
        .into_iter()
        .find(|t| t.id == id)
        .unwrap_or_else(|| panic!("tag {id} is gone"))
        .use_count as i64
}

// ---- typed notes -----------------------------------------------------------

#[test]
fn a_typed_note_becomes_a_finalized_session_the_library_lists() {
    let (app, _vs, _d) = unlocked();
    let sid = create_note_session_impl(
        &app,
        CreateNoteRequest {
            title: Some("Kickoff scratch".into()),
            notes_md: "# Kickoff\n\n- ship the thing\n".into(),
            tag_ids: vec![],
        },
    )
    .unwrap();

    let notes = app.profile.session_path(&sid).join("notes.md");
    assert_eq!(
        std::fs::read_to_string(&notes).unwrap(),
        "# Kickoff\n\n- ship the thing\n"
    );

    let m = read_manifest(&app, &sid);
    assert_eq!(m["title"], serde_json::json!("Kickoff scratch"));
    assert_eq!(m["chunks"], serde_json::json!([]), "a note has no audio");
    assert!(
        m["finalized_at_unix_seconds"].is_i64(),
        "it is finished the moment it is made — nothing will finalize it later"
    );
    assert_eq!(m["notes_md_relative"], serde_json::json!("notes.md"));

    let listed = list_sessions_impl(&app).unwrap();
    assert!(
        listed.iter().any(|s| s.session_id == sid),
        "a note the user typed shows up in the library"
    );
}

#[test]
fn a_note_with_a_blank_title_is_left_untitled() {
    let (app, _vs, _d) = unlocked();
    for blank in [Some("   ".to_string()), Some(String::new()), None] {
        let sid = create_note_session_impl(
            &app,
            CreateNoteRequest {
                title: blank.clone(),
                notes_md: "body".into(),
                tag_ids: vec![],
            },
        )
        .unwrap();
        assert_eq!(
            read_manifest(&app, &sid)["title"],
            serde_json::Value::Null,
            "{blank:?} is not a title the library should print"
        );
        std::fs::remove_dir_all(app.profile.session_path(&sid)).unwrap();
    }
}

#[test]
fn a_note_carries_the_tags_it_was_created_with() {
    let (app, _vs, _d) = unlocked();
    let a = tag(&app, "Northwind");
    let b = tag(&app, "Kickoff");
    let sid = create_note_session_impl(
        &app,
        CreateNoteRequest {
            title: None,
            notes_md: "body".into(),
            tag_ids: vec![a.clone(), b.clone()],
        },
    )
    .unwrap();
    let m = read_manifest(&app, &sid);
    assert_eq!(m["tag_ids"], serde_json::json!([a, b]));
}

#[test]
fn an_empty_note_body_is_still_written() {
    let (app, _vs, _d) = unlocked();
    let sid = create_note_session_impl(
        &app,
        CreateNoteRequest {
            title: Some("Empty".into()),
            notes_md: String::new(),
            tag_ids: vec![],
        },
    )
    .unwrap();
    let notes = app.profile.session_path(&sid).join("notes.md");
    assert!(notes.is_file(), "the file exists so the editor has something to open");
    assert_eq!(std::fs::read_to_string(notes).unwrap(), "");
}

// ---- tags on a session -----------------------------------------------------

#[test]
fn assigning_tags_moves_the_use_counts_both_ways() {
    let (app, vs, _d) = unlocked();
    let a = tag(&app, "Northwind");
    let b = tag(&app, "Kickoff");
    let c = tag(&app, "Retro");
    synth_session(&app, "daisy-1", 1_700_000_000);

    session_assign_tags_impl(&app, &vs, "daisy-1", vec![a.clone(), b.clone()]).unwrap();
    assert_eq!(use_count(&app, &a), 1);
    assert_eq!(use_count(&app, &b), 1);
    assert_eq!(use_count(&app, &c), 0);

    // Swapping one out moves exactly one count each way.
    session_assign_tags_impl(&app, &vs, "daisy-1", vec![a.clone(), c.clone()]).unwrap();
    assert_eq!(use_count(&app, &a), 1, "a tag that stayed is not double-counted");
    assert_eq!(use_count(&app, &b), 0);
    assert_eq!(use_count(&app, &c), 1);

    // Clearing them all takes every count back down.
    session_assign_tags_impl(&app, &vs, "daisy-1", vec![]).unwrap();
    assert_eq!(use_count(&app, &a), 0);
    assert_eq!(use_count(&app, &c), 0);
    assert_eq!(read_manifest(&app, "daisy-1")["tag_ids"], serde_json::json!([]));
}

#[test]
fn the_catalog_on_disk_carries_no_count() {
    let (app, vs, _d) = unlocked();
    let a = tag(&app, "Northwind");
    synth_session(&app, "daisy-1", 1_700_000_000);
    session_assign_tags_impl(&app, &vs, "daisy-1", vec![a.clone()]).unwrap();
    assert_eq!(use_count(&app, &a), 1);

    // A phone merges and hashes this file; a count is this library's own.
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(app.profile.root().join("tags.json")).unwrap()).unwrap();
    assert_eq!(file["tags"][0]["use_count"], 0, "the key stays, at 0");
}

#[test]
fn assigning_the_same_tags_twice_does_not_inflate_the_count() {
    let (app, vs, _d) = unlocked();
    let a = tag(&app, "Northwind");
    synth_session(&app, "daisy-1", 1_700_000_000);
    for _ in 0..3 {
        session_assign_tags_impl(&app, &vs, "daisy-1", vec![a.clone()]).unwrap();
    }
    assert_eq!(use_count(&app, &a), 1, "the call sets a set, it does not add to one");
}

#[test]
fn two_sessions_each_add_to_the_same_tag() {
    let (app, vs, _d) = unlocked();
    let a = tag(&app, "Northwind");
    synth_session(&app, "daisy-1", 1_700_000_000);
    synth_session(&app, "daisy-2", 1_700_000_100);
    session_assign_tags_impl(&app, &vs, "daisy-1", vec![a.clone()]).unwrap();
    session_assign_tags_impl(&app, &vs, "daisy-2", vec![a.clone()]).unwrap();
    assert_eq!(use_count(&app, &a), 2);
    session_assign_tags_impl(&app, &vs, "daisy-1", vec![]).unwrap();
    assert_eq!(use_count(&app, &a), 1, "clearing one session leaves the other's count");
}

#[test]
fn deleting_a_session_gives_back_the_counts_its_tags_held() {
    let (app, vs, _d) = unlocked();
    let a = tag(&app, "Northwind");
    let b = tag(&app, "Weekly");
    synth_session(&app, "daisy-1", 1_700_000_000);
    synth_session(&app, "daisy-2", 1_700_000_100);
    session_assign_tags_impl(&app, &vs, "daisy-1", vec![a.clone(), b.clone()]).unwrap();
    session_assign_tags_impl(&app, &vs, "daisy-2", vec![a.clone()]).unwrap();
    assert_eq!((use_count(&app, &a), use_count(&app, &b)), (2, 1));

    delete_session_impl(&app, &vs, "daisy-1").unwrap();
    assert_eq!(
        (use_count(&app, &a), use_count(&app, &b)),
        (1, 0),
        "a deleted session is one fewer use of every tag it held",
    );
    delete_session_impl(&app, &vs, "daisy-2").unwrap();
    assert_eq!(
        (use_count(&app, &a), use_count(&app, &b)),
        (0, 0),
        "the counts land back where they started, not below",
    );
}

#[test]
fn tagging_a_session_that_is_not_there_is_an_error() {
    let (app, vs, _d) = unlocked();
    let a = tag(&app, "Northwind");
    assert!(session_assign_tags_impl(&app, &vs, "daisy-missing", vec![a.clone()]).is_err());
    assert_eq!(use_count(&app, &a), 0, "and moves no counts on the way out");
}

#[test]
fn a_tag_id_that_matches_no_tag_is_recorded_without_a_count() {
    let (app, vs, _d) = unlocked();
    synth_session(&app, "daisy-1", 1_700_000_000);
    session_assign_tags_impl(&app, &vs, "daisy-1", vec!["ghost-tag".into()]).unwrap();
    assert_eq!(
        read_manifest(&app, "daisy-1")["tag_ids"],
        serde_json::json!(["ghost-tag"]),
        "the manifest keeps what it was told"
    );
    assert!(
        list_tags_impl(&app).unwrap().is_empty(),
        "and no tag is conjured to hold the count"
    );
}

// ---- the tag search box ----------------------------------------------------

#[test]
fn tag_search_matches_any_part_of_the_name_in_any_case() {
    let (app, _vs, _d) = unlocked();
    tag(&app, "Northwind Logistics");
    tag(&app, "northwind retro");
    tag(&app, "Kickoff");

    let names = |q: &str| {
        let mut v: Vec<String> = search_tags_impl(&app, q)
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        v.sort();
        v
    };

    assert_eq!(names("north"), ["Northwind Logistics", "northwind retro"]);
    assert_eq!(names("NORTH"), ["Northwind Logistics", "northwind retro"]);
    assert_eq!(names("retro"), ["northwind retro"], "a match in the middle counts");
    assert_eq!(names("kick"), ["Kickoff"]);
    assert!(names("zzz").is_empty());
    assert_eq!(
        names("").len(),
        3,
        "an empty box offers everything rather than nothing"
    );
}

// ---- the people dropdown ---------------------------------------------------

fn contact(id: &str, name: &str) -> Contact {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "display_name": name,
        "emails": [],
        "created_at_unix_seconds": 1_700_000_000i64,
    }))
    .unwrap()
}

#[test]
fn contacts_are_offered_most_met_first_then_alphabetically() {
    let (app, _vs, _d) = unlocked();
    save_contacts(
        &app,
        &[
            contact("c-quiet", "Zoe Winters"),
            contact("c-often", "Rowan Hale"),
            contact("c-abel", "Abel Okafor"),
        ],
    )
    .unwrap();

    // Rowan is in two meetings, Abel in none, Zoe in none.
    for (sid, ts) in [("daisy-1", 1_700_000_000), ("daisy-2", 1_700_000_100)] {
        synth_session(&app, sid, ts);
        let mut m = read_manifest(&app, sid);
        m["speaker_map"] = serde_json::json!([{
            "cluster_id": 0, "display_name": "Rowan Hale", "contact_id": "c-often",
        }]);
        write_manifest(&app, sid, &m);
    }

    let order: Vec<String> = list_contacts_impl(&app)
        .unwrap()
        .into_iter()
        .map(|c| c.display_name)
        .collect();
    assert_eq!(
        order,
        ["Rowan Hale", "Abel Okafor", "Zoe Winters"],
        "the person you meet most is first; the rest sort by name"
    );
}

#[test]
fn a_contact_named_only_as_an_attendee_still_counts_as_met() {
    let (app, _vs, _d) = unlocked();
    save_contacts(&app, &[contact("c-1", "Rowan Hale"), contact("c-2", "Abel Okafor")]).unwrap();
    synth_session(&app, "daisy-1", 1_700_000_000);
    let mut m = read_manifest(&app, "daisy-1");
    m["attendees"] = serde_json::json!([{ "display_name": "Rowan Hale", "role": "other" }]);
    write_manifest(&app, "daisy-1", &m);

    let order: Vec<String> = list_contacts_impl(&app)
        .unwrap()
        .into_iter()
        .map(|c| c.display_name)
        .collect();
    assert_eq!(order, ["Rowan Hale", "Abel Okafor"]);
}

#[test]
fn an_unreadable_session_does_not_hide_the_contact_list() {
    let (app, _vs, _d) = unlocked();
    save_contacts(&app, &[contact("c-1", "Rowan Hale")]).unwrap();
    let dir = synth_session(&app, "daisy-1", 1_700_000_000);
    std::fs::write(dir.join("manifest.json"), b"{ truncated").unwrap();
    assert_eq!(
        list_contacts_impl(&app).unwrap().len(),
        1,
        "one broken session must not empty the people dropdown"
    );
}

// ---- the storage figures ---------------------------------------------------

#[test]
fn a_profile_with_no_recordings_reports_zeroes() {
    let (app, _vs, _d) = unlocked();
    let s = recordings_stats_impl(&app).unwrap();
    assert_eq!(s.session_count, 0);
    assert_eq!(s.wav_bytes, 0);
    assert_eq!(s.opus_bytes, 0);
    assert_eq!(s.deletable_session_count, 0);
    assert_eq!(s.deletable_bytes, 0);
}

#[test]
fn only_transcribed_sessions_count_as_clearable_and_the_archive_is_not_counted() {
    let (app, _vs, _d) = unlocked();
    let a = synth_session(&app, "daisy-1", 1_700_000_000);
    std::fs::write(a.join("chunks/0001/mic.wav"), vec![0u8; 5_000]).unwrap();
    std::fs::write(a.join("chunks/0001/system.wav"), vec![0u8; 3_000]).unwrap();
    std::fs::write(a.join("meeting.opus"), vec![0u8; 900]).unwrap();
    std::fs::write(a.join("transcript.json"), b"{}").unwrap();

    // Recorded but never transcribed: its wav is still needed.
    let b = synth_session(&app, "daisy-2", 1_700_000_100);
    std::fs::write(b.join("chunks/0001/mic.wav"), vec![0u8; 7_000]).unwrap();

    let s = recordings_stats_impl(&app).unwrap();
    assert_eq!(s.session_count, 2);
    assert_eq!(s.wav_bytes, 15_000, "every chunk wav is counted");
    assert_eq!(s.opus_bytes, 900);
    assert_eq!(s.deletable_session_count, 1);
    assert_eq!(
        s.deletable_bytes, 8_000,
        "clearing frees the wavs and keeps meeting.opus"
    );
}

#[test]
fn a_session_with_a_broken_manifest_is_offered_for_clearing() {
    let (app, _vs, _d) = unlocked();
    let a = synth_session(&app, "daisy-1", 1_700_000_000);
    std::fs::write(a.join("chunks/0001/mic.wav"), vec![0u8; 2_000]).unwrap();
    std::fs::write(a.join("manifest.json"), b"<not json>").unwrap();
    let s = recordings_stats_impl(&app).unwrap();
    assert_eq!(
        s.deletable_session_count, 1,
        "an orphan is exactly what the clear button is for"
    );
    assert_eq!(s.deletable_bytes, 2_000);
}

// ---- playback --------------------------------------------------------------

fn write_meeting_opus(dir: &std::path::Path, seconds: usize) {
    let n = 16_000 * seconds;
    let mut state = 0x2545_F491u32;
    let pcm: Vec<i16> = (0..n)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            ((state >> 16) as i16) / 8
        })
        .collect();
    recording::compress::encode_mono_pcm16(
        &pcm,
        16_000,
        &recording::compress::CompressParams::default(),
        &dir.join("meeting.opus"),
    )
    .unwrap();
}

#[test]
fn playback_is_offered_only_for_a_session_that_has_the_archive() {
    let (app, vs, _d) = unlocked();
    let a = synth_session(&app, "daisy-1", 1_700_000_000);
    write_meeting_opus(&a, 1);
    synth_session(&app, "daisy-2", 1_700_000_100);

    assert!(session_has_playback_audio_impl(&app, &vs, "daisy-1"));
    assert!(
        !session_has_playback_audio_impl(&app, &vs, "daisy-2"),
        "no archive, no play button"
    );
    assert!(!session_has_playback_audio_impl(&app, &vs, "daisy-missing"));
}

#[test]
fn a_locked_vault_offers_no_playback_and_serves_no_bytes() {
    let (app, vs, _d) = unlocked();
    let a = synth_session(&app, "daisy-1", 1_700_000_000);
    write_meeting_opus(&a, 1);
    lock_vault_impl(&app, &vs).unwrap();

    assert!(!session_has_playback_audio_impl(&app, &vs, "daisy-1"));
    assert!(matches!(
        session_playback_audio_bytes_impl(&app, &vs, "daisy-1"),
        Err(AppError::VaultLocked)
    ));
    if cfg!(target_os = "linux") {
        assert!(matches!(
            session_playback_audio_url_impl(&app, &vs, "daisy-1"),
            Err(AppError::VaultLocked)
        ));
    }
}

/// The URL the player gets: the canonical archive path as a file:// URL on
/// Linux, none on other platforms.
fn assert_playback_url(app: &AppState, vs: &VaultState, id: &str, archive: &std::path::Path) {
    let url = session_playback_audio_url_impl(app, vs, id).unwrap();
    if cfg!(target_os = "linux") {
        assert_eq!(url, Some(file_url(&archive.canonicalize().unwrap())));
    } else {
        assert_eq!(url, None);
    }
}

#[test]
fn a_session_id_that_is_a_path_is_refused_by_both_playback_calls() {
    let (app, vs, _d) = unlocked();
    for id in [
        "../../../../etc/passwd",
        "daisy-1/../../escape",
        "/absolute",
        "",
        "daisy 1",
    ] {
        assert!(
            !session_has_playback_audio_impl(&app, &vs, id),
            "{id:?} must not report audio"
        );
        assert!(
            session_playback_audio_bytes_impl(&app, &vs, id).is_err(),
            "{id:?} must not serve bytes"
        );
        if cfg!(target_os = "linux") {
            assert!(
                session_playback_audio_url_impl(&app, &vs, id).is_err(),
                "{id:?} must not get a URL"
            );
        }
    }
}

/// A profile reached through a symlink (data moved to another disk) plays.
#[cfg(unix)]
#[test]
fn playback_serves_a_profile_that_is_itself_a_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("other disk/daisy");
    std::fs::create_dir_all(&real).unwrap();
    let link = dir.path().join("daisy");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let app = AppState::new(ProfileDir::at(link).unwrap());
    let vs = VaultState::new();
    init_vault_impl(&app, &vs, PASS).unwrap();
    let a = synth_session(&app, "daisy-1", 1_700_000_000);
    write_meeting_opus(&a, 1);

    assert!(session_has_playback_audio_impl(&app, &vs, "daisy-1"));
    assert!(session_playback_audio_bytes_impl(&app, &vs, "daisy-1").is_ok());
    assert_playback_url(&app, &vs, "daisy-1", &a.join("meeting.opus"));
}

/// A sessions/ directory that is a symlink to another location plays.
#[cfg(unix)]
#[test]
fn playback_serves_a_sessions_dir_that_is_a_symlink() {
    let (app, vs, d) = unlocked();
    let sessions = app.profile.sessions_dir();
    let moved = d.path().join("moved-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::rename(&sessions, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &sessions).unwrap();
    let a = synth_session(&app, "daisy-1", 1_700_000_000);
    write_meeting_opus(&a, 1);

    assert!(session_has_playback_audio_impl(&app, &vs, "daisy-1"));
    assert!(session_playback_audio_bytes_impl(&app, &vs, "daisy-1").is_ok());
    assert_playback_url(&app, &vs, "daisy-1", &a.join("meeting.opus"));
}

/// A session directory that links out of sessions/ is refused.
#[cfg(unix)]
#[test]
fn playback_refuses_a_session_dir_that_links_out_of_sessions() {
    let (app, vs, d) = unlocked();
    std::fs::create_dir_all(app.profile.sessions_dir()).unwrap();
    let outside = d.path().join("outside/daisy-1");
    std::fs::create_dir_all(&outside).unwrap();
    write_meeting_opus(&outside, 1);
    std::os::unix::fs::symlink(&outside, app.profile.session_path("daisy-1")).unwrap();

    assert!(!session_has_playback_audio_impl(&app, &vs, "daisy-1"));
    assert!(session_playback_audio_bytes_impl(&app, &vs, "daisy-1").is_err());
    if cfg!(target_os = "linux") {
        assert!(session_playback_audio_url_impl(&app, &vs, "daisy-1").is_err());
    }
}

#[test]
fn playback_bytes_come_back_in_a_format_the_webview_can_open() {
    let (app, vs, _d) = unlocked();
    let a = synth_session(&app, "daisy-1", 1_700_000_000);
    write_meeting_opus(&a, 1);

    let bytes = session_playback_audio_bytes_impl(&app, &vs, "daisy-1").unwrap();
    assert!(!bytes.is_empty());
    // The frontend sniffs the magic bytes to pick the Blob type: WKWebView
    // gets a RIFF/WAVE, every other webview gets the Ogg stream itself.
    if cfg!(target_os = "macos") {
        assert_eq!(&bytes[0..4], b"RIFF", "macOS has no Ogg-Opus decoder");
        assert_eq!(&bytes[8..12], b"WAVE");
    } else {
        assert_eq!(&bytes[0..4], b"OggS");
    }
}

#[test]
fn an_archive_that_is_not_audio_fails_instead_of_returning_junk() {
    let (app, vs, _d) = unlocked();
    let a = synth_session(&app, "daisy-1", 1_700_000_000);
    std::fs::write(a.join("meeting.opus"), b"this is not an ogg stream").unwrap();

    assert!(
        session_has_playback_audio_impl(&app, &vs, "daisy-1"),
        "the file is there, so the button is offered"
    );
    if cfg!(target_os = "macos") {
        assert!(
            session_playback_audio_bytes_impl(&app, &vs, "daisy-1").is_err(),
            "and pressing it reports the decode failure"
        );
    }
}

// ---- chapters --------------------------------------------------------------

#[test]
fn chapters_are_absent_until_they_are_extracted() {
    let (app, _vs, _d) = unlocked();
    synth_session(&app, "daisy-1", 1_700_000_000);
    assert!(
        load_session_chapters_impl(&app, "daisy-1").unwrap().is_none(),
        "no chapters is a state, not a failure"
    );
    assert!(
        load_session_chapters_impl(&app, "daisy-missing").unwrap().is_none(),
        "and neither is a session that is gone"
    );
}

#[test]
fn chapters_written_to_disk_are_read_back_whole() {
    let (app, _vs, _d) = unlocked();
    let dir = synth_session(&app, "daisy-1", 1_700_000_000);
    let on_disk = serde_json::json!({
        "schema_version": 1,
        "session_id": "daisy-1",
        "model": "qwen3.5-4b",
        "generated_at_unix_seconds": 1_700_000_500i64,
        "chapters": [
            { "title": "Budget", "start_hms": "00:00:00", "summary": "the numbers" },
            { "title": "Hiring", "start_hms": "00:04:00", "summary": "two roles" },
        ],
    });
    std::fs::write(
        dir.join("chapters.json"),
        serde_json::to_vec_pretty(&on_disk).unwrap(),
    )
    .unwrap();

    let got = load_session_chapters_impl(&app, "daisy-1").unwrap().unwrap();
    assert_eq!(got.session_id, "daisy-1");
    assert_eq!(got.model, "qwen3.5-4b");
    assert_eq!(got.chapters.len(), 2);
    assert_eq!(got.chapters[1].title, "Hiring");
}

#[test]
fn a_half_written_chapters_file_says_so() {
    let (app, _vs, _d) = unlocked();
    let dir = synth_session(&app, "daisy-1", 1_700_000_000);
    std::fs::write(dir.join("chapters.json"), b"{\"schema_version\":1,").unwrap();
    let e = load_session_chapters_impl(&app, "daisy-1").unwrap_err();
    assert!(
        format!("{e}").contains("chapters.json"),
        "the message names the file to look at: {e}"
    );
}

// ---- finishing a session by hand -------------------------------------------

#[test]
fn marking_a_session_complete_stamps_it_once_and_is_safe_to_repeat() {
    let (app, _vs, _d) = unlocked();
    synth_session(&app, "daisy-1", 1_700_000_000);
    let mut m = read_manifest(&app, "daisy-1");
    m["finalized_at_unix_seconds"] = serde_json::Value::Null;
    write_manifest(&app, "daisy-1", &m);

    mark_session_complete_impl(&app, "daisy-1").unwrap();
    let stamped = read_manifest(&app, "daisy-1");
    let at = stamped["finalized_at_unix_seconds"]
        .as_i64()
        .expect("the session is now finished");

    mark_session_complete_impl(&app, "daisy-1").unwrap();
    assert_eq!(
        read_manifest(&app, "daisy-1")["finalized_at_unix_seconds"]
            .as_i64()
            .unwrap(),
        at,
        "a second press does not move the finish time"
    );
}

#[test]
fn marking_a_session_that_is_gone_is_not_an_error() {
    let (app, _vs, _d) = unlocked();
    mark_session_complete_impl(&app, "daisy-missing").unwrap();
}

// ---- save as ---------------------------------------------------------------

#[test]
fn saving_a_file_creates_the_folders_the_user_picked() {
    let (_app, dir) = temp_app();
    let target = dir.path().join("exports/2026/notes.md");
    save_text_file_impl(&target, "# Notes\n\nRowan: ship it\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "# Notes\n\nRowan: ship it\n"
    );
}

#[test]
fn saving_over_a_file_replaces_it_rather_than_appending() {
    let (_app, dir) = temp_app();
    let target = dir.path().join("notes.md");
    save_text_file_impl(&target, "first draft, quite long indeed").unwrap();
    save_text_file_impl(&target, "second").unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "second");
}

#[test]
fn saving_keeps_text_that_is_not_ascii_intact() {
    let (_app, dir) = temp_app();
    let target = dir.path().join("메모.md");
    let body = "회의 요약 — 예산 검토 ✅\n";
    save_text_file_impl(&target, body).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), body);
}

#[test]
fn saving_into_a_folder_that_cannot_be_made_reports_the_failure() {
    let (_app, dir) = temp_app();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"I am a file, not a folder").unwrap();
    let e = save_text_file_impl(&blocker.join("under/notes.md"), "body").unwrap_err();
    assert!(
        matches!(e, AppError::Config(ref m) if m.contains("create parent dir")),
        "the dialog can say why nothing was written: {e:?}"
    );
}

#[test]
fn two_notes_typed_in_the_same_second_are_two_notes() {
    let (app, _vs, _d) = unlocked();
    let first = create_note_session_impl(
        &app,
        CreateNoteRequest {
            title: Some("First".into()),
            notes_md: "the first thing I typed".into(),
            tag_ids: vec![],
        },
    )
    .unwrap();
    let second = create_note_session_impl(
        &app,
        CreateNoteRequest {
            title: Some("Second".into()),
            notes_md: "the second thing I typed".into(),
            tag_ids: vec![],
        },
    )
    .unwrap();

    assert_ne!(first, second, "the second note must not land on the first");
    assert_eq!(
        std::fs::read_to_string(app.profile.session_path(&first).join("notes.md")).unwrap(),
        "the first thing I typed",
        "the first note still says what the user typed"
    );
    assert_eq!(
        list_sessions_impl(&app).unwrap().len(),
        2,
        "both notes are in the library"
    );
}

#[test]
fn two_text_imports_in_the_same_second_are_two_sessions() {
    let (app, _vs, _d) = unlocked();
    let mk = |body: &str| {
        import_session_impl(
            &app,
            ImportSessionRequest {
                title: Some("Imported".into()),
                occurred_at: Some(1_700_000_000),
                transcript_md: Some(body.to_string()),
                summary_md: None,
                notes_md: None,
                tag_ids: vec![],
            },
        )
        .unwrap()
    };
    let first = mk("the first transcript");
    let second = mk("the second transcript");

    assert_ne!(first, second);
    assert_eq!(
        std::fs::read_to_string(app.profile.session_path(&first).join("transcript.md")).unwrap(),
        "the first transcript",
        "importing a second file must not overwrite the first"
    );
    assert_eq!(list_sessions_impl(&app).unwrap().len(), 2);
}

#[test]
fn the_second_id_of_a_second_reads_as_a_session_id_everywhere() {
    let (app, _vs, _d) = unlocked();
    let mut ids = Vec::new();
    for _ in 0..3 {
        ids.push(
            create_note_session_impl(
                &app,
                CreateNoteRequest {
                    title: None,
                    notes_md: "body".into(),
                    tag_ids: vec![],
                },
            )
            .unwrap(),
        );
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 3, "three notes, three ids");
    for id in &ids {
        assert!(
            is_safe_session_id(id),
            "{id} must pass the id check every read path applies"
        );
        assert!(id.starts_with("daisy-note-"), "{id} keeps the prefix the app sorts on");
    }
}

#[test]
fn a_session_whose_audio_is_already_cleared_is_not_offered_again() {
    let (app, _vs, _d) = unlocked();
    let a = synth_session(&app, "daisy-1", 1_700_000_000);
    std::fs::write(a.join("transcript.json"), b"{}").unwrap();
    std::fs::write(a.join("meeting.opus"), vec![0u8; 700]).unwrap();
    // chunks/0001 exists but holds no wav: this is what a cleared session
    // looks like on disk.

    let s = recordings_stats_impl(&app).unwrap();
    assert_eq!(s.session_count, 1, "it is still a meeting");
    assert_eq!(s.opus_bytes, 700);
    assert_eq!(
        s.deletable_session_count, 0,
        "there is nothing left to free, so nothing is offered"
    );
    assert_eq!(s.deletable_bytes, 0);
}
