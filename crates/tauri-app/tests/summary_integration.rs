use std::fs;
use tauri_app_core::commands::summary::{summary_load_impl, summary_save_edit_impl};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::AppState;

fn temp_app_with_summary() -> (AppState, tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let app = AppState::new(profile.clone());
    let sid = "daisy-x".to_string();
    let sdir = profile.session_path(&sid);
    fs::create_dir_all(&sdir).unwrap();
    let s = serde_json::json!({
        "schema_version": 1,
        "session_id": sid,
        "provider": "anthropic",
        "model": "claude-haiku-4-5-20251001",
        "generated_at_unix_seconds": 1,
        "source_inputs_hash": "abc",
        "structured": {
            "tldr": "x",
            "action_items": [],
            "decisions": [],
            "open_questions": [],
            "key_topics": []
        },
        "markdown": "# X\n\n**TL;DR.** x\n",
        "user_edited": false
    });
    fs::write(
        sdir.join("summary.json"),
        serde_json::to_vec_pretty(&s).unwrap(),
    )
    .unwrap();
    (app, dir, sid)
}

#[test]
fn load_then_edit_sets_user_edited() {
    let (app, _d, sid) = temp_app_with_summary();
    assert!(!summary_load_impl(&app, &sid).unwrap().unwrap().user_edited);
    summary_save_edit_impl(&app, &sid, "# X\n\nedited by hand\n").unwrap();
    let s1 = summary_load_impl(&app, &sid).unwrap().unwrap();
    assert!(s1.user_edited);
    assert_eq!(s1.markdown, "# X\n\nedited by hand\n");
    assert_eq!(
        fs::read_to_string(app.profile.session_path(&sid).join("summary.md")).unwrap(),
        "# X\n\nedited by hand\n"
    );
}
