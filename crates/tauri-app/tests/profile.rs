use tauri_app_core::profile::{ProfileDir, expand_home};
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn expand_home_handles_tilde_only() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home".into());
    assert_eq!(expand_home("~"), PathBuf::from(&home));
}

#[test]
fn expand_home_handles_tilde_slash_prefix() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home".into());
    assert_eq!(
        expand_home("~/Insync/Daisy"),
        PathBuf::from(home).join("Insync").join("Daisy")
    );
}

#[test]
fn expand_home_passes_through_absolute() {
    let p = "/var/data/daisy";
    assert_eq!(expand_home(p), PathBuf::from(p));
}

#[test]
fn expand_home_passes_through_relative_without_tilde() {
    assert_eq!(expand_home("foo/bar"), PathBuf::from("foo/bar"));
}

#[test]
fn expand_home_does_not_expand_mid_path_tilde() {
    // "~user/x" is shell-style alternate-user syntax; it passes through
    // unchanged.
    assert_eq!(expand_home("~user/x"), PathBuf::from("~user/x"));
}

#[test]
fn at_creates_sessions_subdir() {
    let td = TempDir::new().unwrap();
    let p = ProfileDir::at(td.path().join("daisy")).unwrap();
    assert!(p.sessions_dir().is_dir());
}

#[test]
fn session_path_resolves_under_sessions() {
    let td = TempDir::new().unwrap();
    let p = ProfileDir::at(td.path().join("daisy")).unwrap();
    let s = p.session_path("abc-123");
    assert!(s.ends_with("sessions/abc-123") || s.ends_with("sessions\\abc-123"));
    assert!(s.starts_with(p.root()));
}

#[test]
fn resolve_honors_env_var() {
    let td = TempDir::new().unwrap();
    std::env::set_var("DAISY_PROFILE_DIR", td.path());
    let p = ProfileDir::resolve().unwrap();
    assert_eq!(p.root(), td.path());
    std::env::remove_var("DAISY_PROFILE_DIR");
}
