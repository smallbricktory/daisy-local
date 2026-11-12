// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Settings persistence across the profile/machine split.
//!
//! `settings.json` syncs between machines; `settings.<MACHINE>.json` does
//! not. Device ids and AEC mode are machine-side.
//!
//! Built by `cargo test -p tauri-app`, not by `--lib`.

use tauri_app_core::commands::settings::{
    apply_machine_patch, update_settings_impl, MachinePatch, SettingsPatch,
};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::settings::{machine_settings_path, AecModeOverride, MachineSettings, Settings};
use tauri_app_core::state::{AppState, ProviderId};
use tempfile::TempDir;

fn fresh_app(td: &TempDir) -> AppState {
    let p = ProfileDir::at(td.path().join("daisy")).unwrap();
    AppState::new(p)
}

/// A machine patch as the frontend sends it.
fn machine_patch(v: serde_json::Value) -> MachinePatch {
    serde_json::from_value(v).unwrap()
}

/// A settings patch as the frontend sends it.
fn settings_patch(v: serde_json::Value) -> SettingsPatch {
    serde_json::from_value(v).unwrap()
}

#[test]
fn a_fresh_machine_has_live_speaker_names_on() {
    assert!(tauri_app_core::settings::MachineSettings::default().live_speaker_names);
}

#[test]
fn defaults_when_settings_file_missing() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let s = app.settings();
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

    let back = app.settings();
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
    let s = app.settings();
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
    let s = app.settings();

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
    let s = app.settings();
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
    let s = app.settings();
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
fn a_machine_patch_changes_only_the_fields_it_carries() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);

    // Backend writes the active model (as set_active_asr_model does), and
    // another window stores a zoom.
    app.update_machine_settings(|m| {
        m.asr.active = Some(providers_local::registry::NEMOTRON_ID.into());
        m.ui_zoom = 1.25;
    })
    .unwrap();

    let merged = apply_machine_patch(
        &app,
        machine_patch(serde_json::json!({
            "denoise_enabled": true,
            "aec_mode_override": "never",
            "show_live_captions": false,
            "live_speaker_names": false,
            "asr": {
                "active": null,
                "whisper": { "language": null, "window_cap_s": 45, "threads": 0, "hop_ladder_ms": null },
            },
        })),
    )
    .unwrap();

    assert!(merged.denoise_enabled);
    assert_eq!(merged.aec_mode_override, AecModeOverride::Never);
    assert!(!merged.show_live_captions);
    assert!(!merged.live_speaker_names);
    assert_eq!(merged.asr.whisper.window_cap_s, 45, "tunables are UI-owned");
    assert_eq!(
        merged.asr.active.as_deref(),
        Some(providers_local::registry::NEMOTRON_ID),
        "the active model is never taken from a patch"
    );
    assert_eq!(merged.ui_zoom, 1.25, "a field the patch does not carry keeps its value");
    assert_eq!(app.machine_settings(), merged);
}

#[test]
fn a_machine_patch_refuses_a_backend_owned_field() {
    for field in ["ai_provider", "default_mic_key", "some_later_option"] {
        let v = serde_json::json!({ field: null });
        assert!(
            serde_json::from_value::<MachinePatch>(v).is_err(),
            "{field} was accepted from the UI"
        );
    }
    assert!(serde_json::from_value::<SettingsPatch>(serde_json::json!({ "schema_version": 9 })).is_err());
}

#[test]
fn the_mic_arrives_as_a_pick_a_clear_or_nothing() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    app.update_machine_settings(|m| m.default_mic_key = Some("usb-headset".into())).unwrap();

    // Absent: the stored key stays, however stale the picker list is.
    apply_machine_patch(&app, machine_patch(serde_json::json!({ "nerd_mode": true }))).unwrap();
    assert_eq!(app.machine_settings().default_mic_key.as_deref(), Some("usb-headset"));

    // An id no device answers to never destroys the stored choice.
    apply_machine_patch(&app, machine_patch(serde_json::json!({ "default_mic_source_id": 4_000_000_000u32 })))
        .unwrap();
    assert_eq!(app.machine_settings().default_mic_key.as_deref(), Some("usb-headset"));

    // null clears it.
    apply_machine_patch(&app, machine_patch(serde_json::json!({ "default_mic_source_id": null }))).unwrap();
    assert_eq!(app.machine_settings().default_mic_key, None);
}

#[test]
fn every_settings_write_is_announced_with_the_file_it_changed() {
    use std::sync::{Arc, Mutex};
    use tauri_app_core::state::SettingsFile;
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let heard = Arc::new(Mutex::new(Vec::new()));
    let sink = heard.clone();
    app.on_settings_changed(move |w| sink.lock().unwrap().push(w));

    apply_machine_patch(&app, machine_patch(serde_json::json!({ "ui_zoom": 1.1 }))).unwrap();
    update_settings_impl(&app, settings_patch(serde_json::json!({ "nav_order": ["search"] }))).unwrap();
    app.set_ai_provider(Some(ProviderId::Local)).unwrap();
    app.update_settings(|s| s.default_summary_prompt_id = Some("builtin:daisy".into())).unwrap();

    assert_eq!(
        *heard.lock().unwrap(),
        vec![SettingsFile::Machine, SettingsFile::Profile, SettingsFile::Machine, SettingsFile::Profile]
    );
}

#[test]
fn a_settings_patch_changes_only_the_fields_it_carries() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    update_settings_impl(&app, settings_patch(serde_json::json!({ "nav_order": ["search", "record"] })))
        .unwrap();
    let after = update_settings_impl(
        &app,
        settings_patch(serde_json::json!({ "auto_update_check": false, "user_display_name": null })),
    )
    .unwrap();
    assert_eq!(after.nav_order, vec!["search".to_string(), "record".to_string()]);
    assert!(!after.auto_update_check);
    assert_eq!(after.user_display_name, None);
    assert_eq!(app.settings(), after);
}

#[test]
fn a_machine_ai_provider_wins_over_the_profile() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let path = app.profile.settings_path();
    let mut s = Settings::load_or_default(&path);
    s.default_summary_provider = Some(ProviderId::Openai);
    s.save(&path).unwrap();
    assert_eq!(app.ai_provider(), Some(ProviderId::Openai), "a machine with no choice reads the profile");

    app.set_ai_provider(Some(ProviderId::Local)).unwrap();
    assert_eq!(app.ai_provider(), Some(ProviderId::Local));
    app.set_ai_provider(None).unwrap();
    assert_eq!(app.ai_provider(), None, "none on this machine wins over the profile");
    assert_eq!(Settings::load_or_default(&path).default_summary_provider, Some(ProviderId::Openai));

    let own: serde_json::Value =
        serde_json::from_slice(&std::fs::read(machine_settings_path(&path)).unwrap()).unwrap();
    assert_eq!(own.get("ai_provider"), Some(&serde_json::Value::Null));
    let fresh = serde_json::to_value(MachineSettings::default()).unwrap();
    assert!(fresh.get("ai_provider").is_none(), "no choice writes no property");

    // A UI patch carries no provider; the stored choice survives it.
    apply_machine_patch(&app, machine_patch(serde_json::json!({ "denoise_enabled": true }))).unwrap();
    assert_eq!(app.ai_provider(), None);
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
    apply_machine_patch(&app, machine_patch(serde_json::json!({ "ui_zoom": 1.5 }))).unwrap();

    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(machine_settings_path(&path)).unwrap()).unwrap();
    assert_eq!(after["ui_zoom"], serde_json::json!(1.5));
    assert_eq!(after["some_later_option"]["enabled"], true);
}

#[test]
fn an_unknown_ai_provider_keeps_the_rest_of_the_machine_file() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let path = app.profile.settings_path();

    // Written by a newer build whose provider list is longer than this one's.
    let newer = serde_json::json!({
        "ai_provider": "orbital_relay",
        "diarizer": "kmeans",
        "ui_zoom": 1.75,
        "css_theme": "dusk",
        "appearance": "dark",
        "mcp_port": 40001,
        "some_later_option": 7,
    });
    std::fs::write(machine_settings_path(&path), serde_json::to_vec(&newer).unwrap()).unwrap();

    let m = MachineSettings::load(&path);
    assert_eq!(m.ui_zoom, 1.75);
    assert_eq!(m.css_theme.as_deref(), Some("dusk"));
    assert_eq!(m.appearance, "dark");
    assert_eq!(m.mcp_port, 40001);
    assert_eq!(m.unknown["diarizer"], serde_json::json!("kmeans"), "a retired key round-trips as unknown");
    assert_eq!(m.unknown["some_later_option"], serde_json::json!(7));

    // The id means nothing to this build, so this machine reads as having
    // made no choice and follows the profile.
    let mut s = Settings::defaults();
    s.default_summary_provider = Some(ProviderId::Openai);
    s.save(&path).unwrap();
    assert_eq!(app.ai_provider(), Some(ProviderId::Openai));

    // The newer build's choice survives this build's next write.
    m.save(&path).unwrap();
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(machine_settings_path(&path)).unwrap()).unwrap();
    assert_eq!(after["ai_provider"], "orbital_relay");
    assert_eq!(after["ui_zoom"], serde_json::json!(1.75));
    assert_eq!(after["some_later_option"], serde_json::json!(7));
}

#[test]
fn an_unreadable_machine_file_is_kept_beside_the_new_one() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let path = app.profile.settings_path();
    let own = machine_settings_path(&path);
    std::fs::write(&own, b"{ this is not json").unwrap();

    let m = MachineSettings::load(&path);
    assert_eq!(m.ui_zoom, 1.0);
    let corrupt =
        own.with_file_name(format!("{}.corrupt", own.file_name().unwrap().to_str().unwrap()));
    assert!(corrupt.is_file(), "the unreadable file was overwritten instead of kept");
    m.save(&path).unwrap();
    assert!(corrupt.is_file());
}

#[test]
fn a_ui_write_cannot_delete_properties_the_ui_never_saw() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let path = app.profile.settings_path();
    let newer = serde_json::json!({
        "schema_version": Settings::SCHEMA,
        "user_display_name": "Rosalind Quill",
        "some_later_option": 7
    });
    std::fs::write(&path, serde_json::to_vec(&newer).unwrap()).unwrap();

    update_settings_impl(
        &app,
        settings_patch(serde_json::json!({ "user_display_name": "Rosalind Quill-Adeyemi" })),
    )
    .unwrap();

    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(after["user_display_name"], "Rosalind Quill-Adeyemi");
    assert_eq!(after["some_later_option"], serde_json::json!(7));
}

#[test]
fn tray_icon_defaults_on_and_is_ui_owned() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let path = app.profile.settings_path();

    // A machine file without the property.
    let older = serde_json::json!({ "ui_zoom": 1.0 });
    std::fs::write(machine_settings_path(&path), serde_json::to_vec(&older).unwrap()).unwrap();
    assert!(MachineSettings::load(&path).show_tray_icon);
    assert!(MachineSettings::default().show_tray_icon);

    let merged =
        apply_machine_patch(&app, machine_patch(serde_json::json!({ "show_tray_icon": false }))).unwrap();
    assert!(!merged.show_tray_icon);
    assert!(!MachineSettings::load(&path).show_tray_icon);
}

#[test]
fn nerd_mode_defaults_off_and_is_ui_owned() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let path = app.profile.settings_path();

    // A machine file without the property.
    let older = serde_json::json!({ "ui_zoom": 1.0 });
    std::fs::write(machine_settings_path(&path), serde_json::to_vec(&older).unwrap()).unwrap();
    assert!(!MachineSettings::load(&path).nerd_mode);
    assert!(!MachineSettings::default().nerd_mode);

    // The UI owns it: a patch turning it on is saved as on.
    let merged = apply_machine_patch(&app, machine_patch(serde_json::json!({ "nerd_mode": true }))).unwrap();
    assert!(merged.nerd_mode);
    assert!(MachineSettings::load(&path).nerd_mode);
}

#[test]
fn a_machine_patch_carries_the_record_hotkey() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);

    let merged =
        apply_machine_patch(&app, machine_patch(serde_json::json!({ "record_hotkey": "Shift+Alt+F9" })))
            .unwrap();
    assert_eq!(merged.record_hotkey.as_deref(), Some("Shift+Alt+F9"));

    let merged = apply_machine_patch(&app, machine_patch(serde_json::json!({ "denoise_enabled": true })))
        .unwrap();
    assert_eq!(merged.record_hotkey.as_deref(), Some("Shift+Alt+F9"), "absent keeps it");

    let merged =
        apply_machine_patch(&app, machine_patch(serde_json::json!({ "record_hotkey": null }))).unwrap();
    assert_eq!(merged.record_hotkey, None);
    assert_eq!(app.machine_settings().record_hotkey, None);
}

#[test]
fn minimize_on_close_defaults_to_none() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let path = app.profile.settings_path();

    let older = serde_json::json!({ "ui_zoom": 1.0 });
    std::fs::write(machine_settings_path(&path), serde_json::to_vec(&older).unwrap()).unwrap();
    assert_eq!(MachineSettings::load(&path).minimize_on_close, None);
    assert_eq!(MachineSettings::default().minimize_on_close, None);
}

#[test]
fn minimize_on_close_roundtrips_through_the_machine_file() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let path = app.profile.settings_path();

    for v in [Some(true), Some(false), None] {
        app.update_machine_settings(|m| m.minimize_on_close = v).unwrap();
        assert_eq!(MachineSettings::load(&path).minimize_on_close, v);
    }
}

#[test]
fn a_machine_patch_carrying_minimize_on_close_changes_only_it() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let path = app.profile.settings_path();
    let before = MachineSettings::load(&path);

    let merged =
        apply_machine_patch(&app, machine_patch(serde_json::json!({ "minimize_on_close": true }))).unwrap();
    assert_eq!(merged.minimize_on_close, Some(true));
    let expected = MachineSettings { minimize_on_close: Some(true), ..before };
    assert_eq!(MachineSettings::load(&path), expected);

    let kept = apply_machine_patch(&app, machine_patch(serde_json::json!({ "nerd_mode": true }))).unwrap();
    assert_eq!(kept.minimize_on_close, Some(true));
}
