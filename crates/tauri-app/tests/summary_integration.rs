// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
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

#[test]
fn edit_without_summary_creates_a_user_summary() {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let app = AppState::new(profile.clone());
    let sid = "daisy-y".to_string();
    fs::create_dir_all(profile.session_path(&sid)).unwrap();
    assert!(summary_load_impl(&app, &sid).unwrap().is_none());

    summary_save_edit_impl(&app, &sid, "# Pasted\n\nfrom another LLM\n").unwrap();

    let s = summary_load_impl(&app, &sid).unwrap().unwrap();
    assert!(s.user_edited);
    assert_eq!(s.markdown, "# Pasted\n\nfrom another LLM\n");
    assert_eq!(s.session_id, sid);
    assert_eq!(
        fs::read_to_string(profile.session_path(&sid).join("summary.md")).unwrap(),
        "# Pasted\n\nfrom another LLM\n"
    );
}

// ---- which provider the summary card reports ------------------------------

/// A profile with no settings file written at all: the first-run shape.
fn fresh_profile() -> (AppState, tauri_app_core::state::VaultState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(dir.path().join("daisy")).unwrap());
    let vs = tauri_app_core::state::VaultState::default();
    *vs.keys.lock().unwrap() = Some(tauri_app_core::state::DecryptedKeys::default());
    (app, vs, dir)
}

#[test]
fn a_fresh_profile_reports_no_provider_and_points_at_the_copy_buttons() {
    let (app, vs, _d) = fresh_profile();
    let s = tauri_app_core::builtin_model::status(&app, &vs).provider;
    assert_eq!(s.state, "None");
    assert!(s.provider.is_none(), "no provider is named: {:?}", s.provider);
    let hint = s.hint.unwrap_or_default();
    assert!(hint.contains("Settings → Providers"), "hint names where to go: {hint}");
    assert!(hint.contains("Copy"), "hint offers the no-provider path: {hint}");
}

#[test]
fn the_selected_provider_is_the_one_reported() {
    let (app, vs, _d) = fresh_profile();
    let mut settings = tauri_app_core::settings::Settings::load_or_default(&app.profile.settings_path());
    settings.default_summary_provider = Some(tauri_app_core::state::ProviderId::LmStudio);
    settings.save(&app.profile.settings_path()).unwrap();

    let s = tauri_app_core::builtin_model::status(&app, &vs).provider;
    assert_eq!(s.state, "Configured");
    assert_eq!(s.provider, Some(tauri_app_core::state::ProviderId::LmStudio));
}

#[test]
fn a_locked_vault_cannot_answer_for_the_selected_provider() {
    let (app, _v, _d) = fresh_profile();
    let mut settings = tauri_app_core::settings::Settings::load_or_default(&app.profile.settings_path());
    settings.default_summary_provider = Some(tauri_app_core::state::ProviderId::Anthropic);
    settings.save(&app.profile.settings_path()).unwrap();

    let locked = tauri_app_core::state::VaultState::default();
    let s = tauri_app_core::builtin_model::status(&app, &locked).provider;
    assert_eq!(s.state, "VaultLocked");
    assert_eq!(s.provider, Some(tauri_app_core::state::ProviderId::Anthropic));
}
