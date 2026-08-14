use std::fs;
use tauri_app_core::commands::meeting::*;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::AppState;

fn temp_app_with_session() -> (AppState, tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let app = AppState::new(profile.clone());
    let sid = "daisy-test".to_string();
    let sdir = profile.session_path(&sid);
    fs::create_dir_all(&sdir).unwrap();
    fs::write(
        sdir.join("manifest.json"),
        r#"{"schema_version":2,"session_id":"daisy-test","created_at_unix_seconds":1000,"sample_rate":16000,"channels":1,"mic_source_id":1,"mic_source_node_name":"m","mic_source_description":"m","system_source_id":2,"system_source_node_name":"s","system_source_description":"s","aec_mode":"disabled","chunks":[],"finalized_at_unix_seconds":2000,"title":"Original","meeting_id":"mid","tag_ids":[],"notes_md_relative":null,"attendees":[],"calendar":null,"recording_segments":[]}"#,
    )
    .unwrap();
    (app, dir, sid)
}

#[test]
fn meta_get_update_and_notes_roundtrip() {
    let (app, _d, sid) = temp_app_with_session();
    assert_eq!(
        session_meta_get_impl(&app, &sid).unwrap().title.as_deref(),
        Some("Original")
    );
    session_meta_update_impl(
        &app,
        SessionMetaUpdate {
            session_id: sid.clone(),
            title: Some(Some("Renamed".into())),
            tag_ids: Some(vec!["t1".into()]),
            attendees: None,
            created_at_unix_seconds: None,
        },
    )
    .unwrap();
    let m = session_meta_get_impl(&app, &sid).unwrap();
    assert_eq!(m.title.as_deref(), Some("Renamed"));
    assert_eq!(m.tag_ids, vec!["t1".to_string()]);
    assert_eq!(session_notes_load_impl(&app, &sid).unwrap(), "");
    session_notes_save_impl(&app, &sid, "## hi\n- a").unwrap();
    assert_eq!(session_notes_load_impl(&app, &sid).unwrap(), "## hi\n- a");
    assert!(session_meta_get_impl(&app, &sid).unwrap().has_notes);
    assert_eq!(
        sessions_referencing_tag(&app, "t1").unwrap(),
        vec![sid.clone()]
    );
    detach_tag_from_session(&app, &sid, "t1").unwrap();
    assert!(sessions_referencing_tag(&app, "t1").unwrap().is_empty());
}
