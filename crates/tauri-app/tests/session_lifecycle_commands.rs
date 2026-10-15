// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Ending a meeting's life on this desktop: the in-call chat thread it may
//! carry, deleting the session itself, and the "clear recordings" sweep the
//! storage card offers.
use tauri::Manager as _;
use tauri_app_core::commands::lifecycle::{init_vault_impl, lock_vault_impl};
use tauri_app_core::commands::live_chat::{live_chat_delete_impl, live_chat_load_impl};
use tauri_app_core::commands::recordings::{delete_recordings_impl, recordings_stats_impl};
use tauri_app_core::commands::session::delete_session_impl;
use tauri_app_core::error::AppError;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, VaultState};

const PASS: &str = "correct-horse-battery-staple-extended-22";

fn unlocked() -> (AppState, VaultState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let app = AppState::new(profile);
    let vs = VaultState::new();
    init_vault_impl(&app, &vs, PASS).unwrap();
    (app, vs, dir)
}

/// A finished meeting. `origin` set makes it one that came from a phone.
fn synth_session(app: &AppState, id: &str, origin: Option<&str>) -> std::path::PathBuf {
    let dir = app.profile.session_path(id);
    std::fs::create_dir_all(dir.join("chunks/0001")).unwrap();
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
        "origin_device": origin,
    });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    dir
}

fn write_chat(app: &AppState, id: &str, body: &serde_json::Value) {
    std::fs::write(
        app.profile.session_path(id).join("call-chat.json"),
        serde_json::to_vec_pretty(body).unwrap(),
    )
    .unwrap();
}

// ---- the in-call chat thread -----------------------------------------------

#[test]
fn a_meeting_with_no_chat_opens_an_empty_thread() {
    let (app, _vs, _d) = unlocked();
    synth_session(&app, "daisy-1", None);
    let chat = live_chat_load_impl(&app, "daisy-1").unwrap();
    assert!(chat.messages.is_empty(), "an empty thread, not an error");
    assert_eq!(chat.transcript_cursor_ms, 0);
    assert!(!chat.transcript_seeded);
}

#[test]
fn a_session_that_never_existed_also_opens_an_empty_thread() {
    let (app, _vs, _d) = unlocked();
    assert!(live_chat_load_impl(&app, "daisy-missing")
        .unwrap()
        .messages
        .is_empty());
}

#[test]
fn a_saved_thread_comes_back_with_its_turns_and_its_cursor() {
    let (app, _vs, _d) = unlocked();
    synth_session(&app, "daisy-1", None);
    write_chat(
        &app,
        "daisy-1",
        &serde_json::json!({
            "messages": [
                { "role": "user", "content": "what did they decide?", "ts": 1_700_000_001_000u64 },
                { "role": "assistant", "content": "to ship it", "ts": 1_700_000_002_000u64 },
            ],
            "transcript_cursor_ms": 91_000,
            "transcript_seeded": true,
        }),
    );

    let chat = live_chat_load_impl(&app, "daisy-1").unwrap();
    assert_eq!(chat.messages.len(), 2);
    assert_eq!(chat.messages[0].role, "user");
    assert_eq!(chat.messages[1].content, "to ship it");
    assert_eq!(
        chat.transcript_cursor_ms, 91_000,
        "the cursor is what stops the next turn re-feeding the whole transcript"
    );
    assert!(chat.transcript_seeded);
}

#[test]
fn a_half_written_thread_says_so_rather_than_showing_nothing() {
    let (app, _vs, _d) = unlocked();
    synth_session(&app, "daisy-1", None);
    std::fs::write(
        app.profile.session_path("daisy-1").join("call-chat.json"),
        b"{\"messages\": [",
    )
    .unwrap();
    let e = live_chat_load_impl(&app, "daisy-1").unwrap_err();
    assert!(
        format!("{e}").contains("call-chat"),
        "the message names the file: {e}"
    );
}

#[test]
fn deleting_the_thread_leaves_the_meeting_alone() {
    let (app, _vs, _d) = unlocked();
    let dir = synth_session(&app, "daisy-1", None);
    std::fs::write(dir.join("transcript.md"), "[00:00:00] **Rowan**: hi\n").unwrap();
    write_chat(
        &app,
        "daisy-1",
        &serde_json::json!({ "messages": [{ "role": "user", "content": "hi", "ts": 1u64 }] }),
    );

    live_chat_delete_impl(&app, "daisy-1").unwrap();
    assert!(!dir.join("call-chat.json").exists());
    assert!(dir.join("transcript.md").is_file(), "the transcript stays");
    assert!(dir.join("manifest.json").is_file(), "and so does the meeting");
    assert!(live_chat_load_impl(&app, "daisy-1").unwrap().messages.is_empty());
}

#[test]
fn deleting_a_thread_that_is_not_there_is_not_an_error() {
    let (app, _vs, _d) = unlocked();
    synth_session(&app, "daisy-1", None);
    live_chat_delete_impl(&app, "daisy-1").unwrap();
    live_chat_delete_impl(&app, "daisy-1").unwrap();
    live_chat_delete_impl(&app, "daisy-missing").unwrap();
}

#[test]
fn a_session_id_that_is_a_path_reaches_no_chat_file() {
    let (app, _vs, _d) = unlocked();
    for id in ["../escape", "a/b", "a\\b", ".hidden", ""] {
        assert!(live_chat_load_impl(&app, id).is_err(), "{id:?} load");
        assert!(live_chat_delete_impl(&app, id).is_err(), "{id:?} delete");
    }
}

// ---- deleting a session ----------------------------------------------------

#[test]
fn deleting_a_session_takes_its_whole_directory_and_nothing_else() {
    let (app, vs, _d) = unlocked();
    let gone = synth_session(&app, "daisy-1", None);
    let kept = synth_session(&app, "daisy-2", None);
    std::fs::write(gone.join("chunks/0001/mic.wav"), vec![0u8; 100]).unwrap();

    delete_session_impl(&app, &vs, "daisy-1").unwrap();
    assert!(!gone.exists());
    assert!(kept.is_dir(), "the other meeting is untouched");

    assert!(matches!(
        delete_session_impl(&app, &vs, "daisy-1"),
        Err(AppError::SessionNotFound(_))
    ));
}

#[test]
fn a_session_id_that_is_a_path_deletes_nothing() {
    let (app, vs, _d) = unlocked();
    let kept = synth_session(&app, "daisy-1", None);
    let outside = kept.parent().unwrap().parent().unwrap().join("precious.txt");
    std::fs::write(&outside, b"not part of any session").unwrap();

    for id in [
        "../../../../etc/passwd",
        "daisy-1/../../precious.txt",
        "/absolute",
        "..",
        "",
        "a b",
    ] {
        let e = delete_session_impl(&app, &vs, id).unwrap_err();
        assert!(
            matches!(e, AppError::Config(_) | AppError::SessionNotFound(_)),
            "{id:?} -> {e:?}"
        );
    }
    assert!(kept.is_dir());
    assert!(outside.is_file(), "nothing outside the sessions folder was touched");
}

#[cfg(unix)]
#[test]
fn a_symlink_pointing_out_of_the_sessions_folder_is_refused() {
    let (app, vs, d) = unlocked();
    let outside = d.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("precious.txt"), b"keep me").unwrap();
    std::os::unix::fs::symlink(&outside, app.profile.session_path("daisy-escape")).unwrap();

    let e = delete_session_impl(&app, &vs, "daisy-escape").unwrap_err();
    assert!(matches!(e, AppError::Config(ref m) if m.contains("escapes")), "{e:?}");
    assert!(outside.join("precious.txt").is_file());
}

#[test]
fn deleting_a_phone_session_leaves_a_tombstone_so_it_does_not_come_back() {
    let (app, vs, _d) = unlocked();
    synth_session(&app, "daisy-1", Some("phone-device-a"));
    delete_session_impl(&app, &vs, "daisy-1").unwrap();

    let inbox = tauri_app_core::sync::server::inbox_of(&app);
    assert!(
        inbox.is_entombed("daisy-1", "phone-device-a"),
        "without the tombstone the phone uploads it again at the next hello"
    );
}

#[test]
fn deleting_a_desktop_session_leaves_no_tombstone() {
    let (app, vs, _d) = unlocked();
    synth_session(&app, "daisy-1", None);
    delete_session_impl(&app, &vs, "daisy-1").unwrap();
    let inbox = tauri_app_core::sync::server::inbox_of(&app);
    assert!(
        !inbox.is_entombed("daisy-1", "phone-device-a"),
        "a session this desktop recorded has no phone to tell"
    );
}

#[test]
fn a_locked_vault_still_deletes_the_session_it_was_asked_to_delete() {
    let (app, vs, _d) = unlocked();
    let dir = synth_session(&app, "daisy-1", Some("phone-device-a"));
    lock_vault_impl(&app, &vs).unwrap();
    delete_session_impl(&app, &vs, "daisy-1").expect("the delete the user asked for happens");
    assert!(!dir.exists());
}

// ---- clearing the recordings ------------------------------------------------

/// `delete_recordings_impl` emits progress events and takes a handle.
fn mock_app_for(state: AppState) -> tauri::App<tauri::test::MockRuntime> {
    let app = tauri::test::mock_app();
    app.manage(state);
    app
}

#[test]
fn clearing_recordings_frees_the_chunks_and_keeps_the_meeting() {
    let (app, _vs, _d) = unlocked();
    let a = synth_session(&app, "daisy-1", None);
    std::fs::write(a.join("chunks/0001/mic.wav"), vec![0u8; 4_000]).unwrap();
    std::fs::write(a.join("meeting.opus"), vec![0u8; 700]).unwrap();
    std::fs::write(a.join("transcript.json"), b"{}").unwrap();
    std::fs::write(a.join("transcript.md"), b"# hi").unwrap();

    let mock = mock_app_for(app);
    let state = mock.state::<AppState>();
    let summary = delete_recordings_impl(&state, mock.handle()).unwrap();

    assert_eq!(summary.deleted_sessions, 1);
    assert_eq!(summary.freed_bytes, 4_000);
    assert!(!a.join("chunks/0001/mic.wav").exists(), "the audio is gone");
    assert!(a.join("transcript.md").is_file(), "the transcript stays");
    assert!(a.join("manifest.json").is_file(), "and the meeting stays");
    assert!(
        a.join("meeting.opus").is_file(),
        "the archive is what playback needs, so it is kept"
    );
}

#[test]
fn clearing_skips_a_meeting_that_has_not_been_transcribed_yet() {
    let (app, _vs, _d) = unlocked();
    let untranscribed = synth_session(&app, "daisy-2", None);
    std::fs::write(untranscribed.join("chunks/0001/mic.wav"), vec![0u8; 9_000]).unwrap();

    let mock = mock_app_for(app);
    let state = mock.state::<AppState>();
    let summary = delete_recordings_impl(&state, mock.handle()).unwrap();

    assert_eq!(summary.deleted_sessions, 0);
    assert_eq!(summary.freed_bytes, 0);
    assert!(
        untranscribed.join("chunks/0001/mic.wav").is_file(),
        "the only copy of the words is still in that audio"
    );
}

#[test]
fn clearing_an_empty_profile_reports_nothing_freed() {
    let (app, _vs, _d) = unlocked();
    let mock = mock_app_for(app);
    let state = mock.state::<AppState>();
    let summary = delete_recordings_impl(&state, mock.handle()).unwrap();
    assert_eq!(summary.deleted_sessions, 0);
    assert_eq!(summary.freed_bytes, 0);
}

#[test]
fn what_the_storage_card_offered_is_what_clearing_frees() {
    let (app, _vs, _d) = unlocked();
    for (id, wav, transcribed) in [
        ("daisy-1", 4_000usize, true),
        ("daisy-2", 9_000, false),
        ("daisy-3", 1_500, true),
    ] {
        let dir = synth_session(&app, id, None);
        std::fs::write(dir.join("chunks/0001/mic.wav"), vec![0u8; wav]).unwrap();
        if transcribed {
            std::fs::write(dir.join("transcript.json"), b"{}").unwrap();
        }
    }
    let offered = recordings_stats_impl(&app).unwrap();

    let mock = mock_app_for(app);
    let state = mock.state::<AppState>();
    let summary = delete_recordings_impl(&state, mock.handle()).unwrap();

    assert_eq!(summary.deleted_sessions, offered.deletable_session_count);
    assert_eq!(
        summary.freed_bytes, offered.deletable_bytes,
        "the number on the button is the number that comes back"
    );

    // And a second press has nothing left to do.
    let again = delete_recordings_impl(&state, mock.handle()).unwrap();
    assert_eq!(again.deleted_sessions, 0);
    assert_eq!(again.freed_bytes, 0);
}
