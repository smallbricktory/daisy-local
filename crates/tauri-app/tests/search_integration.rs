use std::fs;
use tauri_app_core::commands::search::{search_sessions_impl, SearchRequest};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::AppState;

#[test]
fn search_filters_by_tag_date_and_text() {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path()).unwrap();
    let app = AppState::new(profile.clone());
    let s = profile.sessions_dir();
    let a = s.join("a");
    fs::create_dir_all(&a).unwrap();
    fs::write(a.join("manifest.json"), r#"{"schema_version":2,"session_id":"a","created_at_unix_seconds":1000,"sample_rate":16000,"channels":1,"mic_source_id":1,"mic_source_node_name":"m","mic_source_description":"m","system_source_id":2,"system_source_node_name":"s","system_source_description":"s","aec_mode":"disabled","chunks":[],"finalized_at_unix_seconds":2000,"title":"Northwind onboarding","meeting_id":"m","tag_ids":["TAG1"],"notes_md_relative":null,"attendees":[],"calendar":null,"recording_segments":[]}"#).unwrap();
    let b = s.join("b");
    fs::create_dir_all(&b).unwrap();
    fs::write(b.join("manifest.json"), r#"{"schema_version":2,"session_id":"b","created_at_unix_seconds":5000,"sample_rate":16000,"channels":1,"mic_source_id":1,"mic_source_node_name":"m","mic_source_description":"m","system_source_id":2,"system_source_node_name":"s","system_source_description":"s","aec_mode":"disabled","chunks":[],"finalized_at_unix_seconds":6000,"title":"Standup","meeting_id":"m","tag_ids":[],"notes_md_relative":null,"attendees":[],"calendar":null,"recording_segments":[]}"#).unwrap();
    let h = search_sessions_impl(
        &app,
        SearchRequest {
            query: Some("northwind".into()),
            tag_ids: None,
            contact_ids: None,
            date_from: None,
            date_to: None,
        },
    )
    .unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].session_id, "a");
    assert_eq!(h[0].match_source, "title");
    let h = search_sessions_impl(
        &app,
        SearchRequest {
            query: None,
            tag_ids: Some(vec!["TAG1".into()]),
            contact_ids: None,
            date_from: None,
            date_to: None,
        },
    )
    .unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].session_id, "a");
    let h = search_sessions_impl(
        &app,
        SearchRequest {
            query: None,
            tag_ids: None,
            contact_ids: None,
            date_from: Some(3000),
            date_to: None,
        },
    )
    .unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].session_id, "b");
    let h = search_sessions_impl(
        &app,
        SearchRequest {
            query: None,
            tag_ids: None,
            contact_ids: None,
            date_from: None,
            date_to: None,
        },
    )
    .unwrap();
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].session_id, "b");
}
