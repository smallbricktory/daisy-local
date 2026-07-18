use tauri_app_core::commands::settings::read_settings_impl;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::settings::{AecModeOverride, Settings};
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
    assert_eq!(s.default_mic_source_id, None);
    assert_eq!(s.aec_mode_override, AecModeOverride::Auto);
}

#[test]
fn write_then_read_roundtrip() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    // Overrides a few fields off their defaults; the rest spread from
    // Settings::defaults(). The `back == s` equality below roundtrip-checks
    // all fields.
    let s = Settings {
        default_mic_source_id: Some(77),
        aec_mode_override: AecModeOverride::Always,
        default_summary_provider: Some(ProviderId::Openai),
        ..Settings::defaults()
    };
    s.save(&app.profile.settings_path()).unwrap();

    let back = read_settings_impl(&app).unwrap();
    assert_eq!(back, s);
}

#[test]
fn corrupted_file_falls_back_to_defaults() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    std::fs::write(app.profile.settings_path(), b"this is not json").unwrap();
    let s = read_settings_impl(&app).unwrap();
    assert_eq!(s.schema_version, Settings::SCHEMA);
}

#[test]
fn schema_mismatch_falls_back_to_defaults() {
    let td = TempDir::new().unwrap();
    let app = fresh_app(&td);
    let bad = serde_json::json!({
        "schema_version": 99,
        "default_mic_source_id": null,
        "aec_mode_override": "auto"
    });
    std::fs::write(app.profile.settings_path(), serde_json::to_vec(&bad).unwrap()).unwrap();
    let s = read_settings_impl(&app).unwrap();
    assert_eq!(s.schema_version, Settings::SCHEMA, "v99 file should be ignored");
}

#[test]
fn aec_mode_override_serializes_lowercase() {
    use serde_json::json;
    let s = Settings {
        aec_mode_override: AecModeOverride::Never,
        ..Settings::defaults()
    };
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["aec_mode_override"], json!("never"));
}
