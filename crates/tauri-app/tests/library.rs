use tauri_app_core::commands::library::list_sessions_impl;
use tauri_app_core::commands::session::read_session_impl;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::AppState;
use tempfile::TempDir;

fn synth_session(p: &ProfileDir, id: &str, ts: i64, with_transcript: bool) {
    let dir = p.session_path(id);
    std::fs::create_dir_all(dir.join("chunks/0001")).unwrap();
    let manifest = serde_json::json!({
        "schema_version": 1,
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
        "chunks": [{
            "index": 1,
            "started_at_unix_seconds": ts,
            "ended_at_unix_seconds": ts + 120,
            "duration_seconds": 120,
            "mic_wav_relative": "chunks/0001/mic.wav",
            "system_wav_relative": "chunks/0001/system.wav",
            "mic_aec_wav_relative": null
        }],
        "finalized_at_unix_seconds": ts + 130
    });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    ).unwrap();
    if with_transcript {
        std::fs::write(dir.join("transcript.md"), "# Test\n\n[00:00:00] **Me**: hi\n").unwrap();
        std::fs::write(dir.join("transcript.json"), b"{}").unwrap();
    }
}

#[test]
fn lists_sessions_sorted_newest_first() {
    let td = TempDir::new().unwrap();
    let p = ProfileDir::at(td.path().join("daisy")).unwrap();
    synth_session(&p, "alpha", 1_700_000_000, false);
    synth_session(&p, "beta", 1_800_000_000, true);

    let app = AppState::new(p);
    let list = list_sessions_impl(&app).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].session_id, "beta", "newer first");
    assert_eq!(list[1].session_id, "alpha");
    assert_eq!(list[0].duration_seconds, Some(120));
    assert!(list[0].has_transcript, "beta has transcript.json");
    assert!(!list[1].has_transcript, "alpha doesn't");
}

#[test]
fn empty_profile_returns_empty_list() {
    let td = TempDir::new().unwrap();
    let p = ProfileDir::at(td.path().join("daisy")).unwrap();
    let app = AppState::new(p);
    let list = list_sessions_impl(&app).unwrap();
    assert!(list.is_empty());
}

#[test]
fn read_session_returns_manifest_and_optional_transcript() {
    let td = TempDir::new().unwrap();
    let p = ProfileDir::at(td.path().join("daisy")).unwrap();
    synth_session(&p, "with-md", 1_800_000_000, true);
    let app = AppState::new(p);
    let v = read_session_impl(&app, "with-md").unwrap();
    assert_eq!(v.session_id, "with-md");
    assert!(v.transcript_md.unwrap().contains("**Me**: hi"));
    assert!(v.has_transcript);
}

#[test]
fn read_session_errors_on_missing_id() {
    let td = TempDir::new().unwrap();
    let p = ProfileDir::at(td.path().join("daisy")).unwrap();
    let app = AppState::new(p);
    let err = read_session_impl(&app, "nope").unwrap_err();
    match err {
        tauri_app_core::error::AppError::SessionNotFound(id) => assert_eq!(id, "nope"),
        other => panic!("expected SessionNotFound, got {other:?}"),
    }
}
