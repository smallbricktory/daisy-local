//! Settings persistence across the profile/machine split.
//!
//! `settings.json` syncs between machines; `settings.<MACHINE>.json` does
//! not. Device ids, AEC mode and the diarizer are machine-side.
//!
//! Built by `cargo test -p tauri-app`, not by `--lib`.

use tauri_app_core::commands::settings::read_settings_impl;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::settings::{machine_settings_path, AecModeOverride, MachineSettings, Settings};
use tauri_app_core::state::{AppState, ProviderId};
use tempfile::TempDir;

fn fresh_app(td: &TempDir) -> AppState {
    let p = ProfileDir::at(td.path().join("daisy")).unwrap();
    AppState::new(p)
}

#[test]
fn defaults_when_settings_file_missing() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let s = read_settings_impl(&app).unwrap();
    assert_eq!(s.schema_version, Settings::SCHEMA);
    assert_eq!(s.default_summary_provider, None);

    let m = MachineSettings::load(&app.profile.settings_path());
    assert_eq!(m.default_mic_source_id, None);
    assert_eq!(m.aec_mode_override, AecModeOverride::Auto);
}

#[test]
fn write_then_read_roundtrip() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    // The legacy_* fields are private, so an out-of-crate caller cannot
    // spread defaults; the equality below covers every field.
    let mut s = Settings::defaults();
    s.default_summary_provider = Some(ProviderId::Openai);
    s.user_display_name = Some("Wren Alcott".into());
    s.save(&app.profile.settings_path()).unwrap();

    let back = read_settings_impl(&app).unwrap();
    assert_eq!(back, s);
}

#[test]
fn machine_settings_roundtrip_to_their_own_file() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let path = app.profile.settings_path();
    let mut m = MachineSettings::load(&path);
    m.default_mic_source_id = Some(77);
    m.aec_mode_override = AecModeOverride::Always;
    m.save(&path).unwrap();

    assert!(machine_settings_path(&path).is_file(), "must not land in settings.json");
    assert_eq!(MachineSettings::load(&path), m);
}

#[test]
fn corrupted_file_falls_back_to_defaults_and_is_kept() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    std::fs::write(app.profile.settings_path(), b"this is not json").unwrap();
    let s = read_settings_impl(&app).unwrap();
    assert_eq!(s.schema_version, Settings::SCHEMA);
    // Defaults are about to be written over it, so the bytes we could not
    // read are moved aside first.
    let backup = app.profile.settings_path().with_file_name("settings.json.corrupt");
    assert!(backup.is_file(), "unreadable settings.json was not kept");
    assert_eq!(std::fs::read(&backup).unwrap(), b"this is not json");
}

#[test]
fn an_older_file_is_reconciled_not_reset() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    // A v1 file: one property this build still has, one it never had, and
    // none of the ones added since.
    let old = serde_json::json!({
        "schema_version": 1,
        "default_summary_provider": "openai",
        "some_retired_option": 42
    });
    std::fs::write(app.profile.settings_path(), serde_json::to_vec(&old).unwrap()).unwrap();
    let s = read_settings_impl(&app).unwrap();

    // Kept: what the user set. Filled: what this build added. Neither is
    // a reset, and nothing was moved aside.
    assert_eq!(s.default_summary_provider, Some(ProviderId::Openai));
    assert_eq!(s.schema_version, Settings::SCHEMA, "version stamped forward");
    assert!(s.auto_update_check, "property added since v1 takes its default");
    assert_eq!(s.unknown.get("some_retired_option").unwrap(), 42);
    let backup = app.profile.settings_path().with_file_name("settings.json.pre-reset");
    assert!(!backup.exists(), "an older file must not be reset");

    // The next write carries the unrecognised property through.
    s.save(&app.profile.settings_path()).unwrap();
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(app.profile.settings_path()).unwrap()).unwrap();
    assert_eq!(written["schema_version"], serde_json::json!(Settings::SCHEMA));
    assert_eq!(written["some_retired_option"], serde_json::json!(42));
}

#[test]
fn a_file_with_no_schema_version_still_loads() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let old = serde_json::json!({ "user_display_name": "Rosalind Quill" });
    std::fs::write(app.profile.settings_path(), serde_json::to_vec(&old).unwrap()).unwrap();
    let s = read_settings_impl(&app).unwrap();
    assert_eq!(s.user_display_name.as_deref(), Some("Rosalind Quill"));
    assert_eq!(s.schema_version, Settings::SCHEMA);
}

#[test]
fn newer_schema_uses_defaults_but_leaves_the_file_alone() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let newer = serde_json::json!({
        "schema_version": 99,
        "default_summary_provider": "openai"
    });
    std::fs::write(app.profile.settings_path(), serde_json::to_vec(&newer).unwrap()).unwrap();
    let s = read_settings_impl(&app).unwrap();
    assert_eq!(s.schema_version, Settings::SCHEMA, "v99 file should be ignored");
    assert_eq!(s.default_summary_provider, None);
    // The newer build's file stays in place — no rename, no backup.
    assert!(app.profile.settings_path().is_file(), "newer settings.json must survive");
    let kept: serde_json::Value =
        serde_json::from_slice(&std::fs::read(app.profile.settings_path()).unwrap()).unwrap();
    assert_eq!(kept["schema_version"], serde_json::json!(99));
    let backup = app.profile.settings_path().with_file_name("settings.json.pre-reset");
    assert!(!backup.exists(), "no pre-reset backup for a newer-schema file");
}

#[test]
fn ui_machine_settings_write_preserves_backend_owned_fields() {
    use tauri_app_core::commands::settings::apply_ui_machine_settings;
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);

    // Backend writes the active model + diarizer (as set_active_asr_model
    // and set_diarizer do).
    app.update_machine_settings(|m| {
        m.asr.active = Some("parakeet-tdt-0.6b-v2-int8".into());
        m.diarizer = "speakrs".into();
    })
    .unwrap();

    // The UI sends a snapshot taken BEFORE those writes, with a toggle
    // flipped. UI-owned fields land; backend-owned fields survive.
    let mut stale = MachineSettings::default();
    stale.denoise_enabled = true;
    stale.aec_mode_override = AecModeOverride::Never;
    stale.show_live_captions = false;
    stale.asr.faint_audio_marker = false;
    let merged = apply_ui_machine_settings(&app, &stale).unwrap();

    assert!(merged.denoise_enabled);
    assert_eq!(merged.aec_mode_override, AecModeOverride::Never);
    assert!(!merged.show_live_captions);
    assert!(!merged.asr.faint_audio_marker, "tunables are UI-owned");
    assert_eq!(
        merged.asr.active.as_deref(),
        Some("parakeet-tdt-0.6b-v2-int8"),
        "active model survives a stale UI snapshot"
    );
    assert_eq!(merged.diarizer, "speakrs");
    // And the merge is what got persisted.
    assert_eq!(app.machine_settings(), merged);
}

#[test]
fn aec_mode_override_serializes_lowercase() {
    use serde_json::json;
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let mut m = MachineSettings::load(&app.profile.settings_path());
    m.aec_mode_override = AecModeOverride::Never;
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(v["aec_mode_override"], json!("never"));
}

#[test]
fn a_machine_file_keeps_properties_this_build_does_not_know() {
    use tauri_app_core::commands::settings::apply_ui_machine_settings;
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let path = app.profile.settings_path();

    // Written by a newer build: one property this build knows, one it does
    // not. Running the older build must not cost the user the second.
    let newer = serde_json::json!({
        "ui_zoom": 1.25,
        "some_later_option": { "enabled": true }
    });
    std::fs::write(machine_settings_path(&path), serde_json::to_vec(&newer).unwrap()).unwrap();

    let loaded = MachineSettings::load(&path);
    assert_eq!(loaded.ui_zoom, 1.25);
    assert_eq!(loaded.unknown["some_later_option"]["enabled"], true);

    // A UI write touches only the properties the UI owns.
    let mut incoming = loaded.clone();
    incoming.ui_zoom = 1.5;
    apply_ui_machine_settings(&app, &incoming).unwrap();

    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(machine_settings_path(&path)).unwrap()).unwrap();
    assert_eq!(after["ui_zoom"], serde_json::json!(1.5));
    assert_eq!(after["some_later_option"]["enabled"], true);
}

#[test]
fn a_ui_write_cannot_delete_properties_the_ui_never_saw() {
    use tauri_app_core::commands::settings::write_settings_impl;
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let path = app.profile.settings_path();
    let newer = serde_json::json!({
        "schema_version": Settings::SCHEMA,
        "user_display_name": "Rosalind Quill",
        "some_later_option": 7
    });
    std::fs::write(&path, serde_json::to_vec(&newer).unwrap()).unwrap();

    // The UI round-trips only what it knows: read, change one field, write.
    let mut s = read_settings_impl(&app).unwrap();
    s.unknown.clear();
    s.user_display_name = Some("Rosalind Quill-Adeyemi".into());
    write_settings_impl(&app, s).unwrap();

    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(after["user_display_name"], "Rosalind Quill-Adeyemi");
    assert_eq!(after["some_later_option"], serde_json::json!(7));
}
