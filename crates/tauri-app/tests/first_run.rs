// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The walk a brand-new install takes before anything exists: where the
//! profile goes, the two acknowledgements, the vault it makes, and the
//! binding that decides whether this profile is this install's to open.
//!
//! Its own test binary because every one of these reads `DAISY_CONFIG_DIR`,
//! which is process-global. Inside it the tests take turns for the same
//! reason.
use tauri_app_core::commands::binding::{profile_binding_check_impl, BindingState};
use tauri_app_core::commands::bootstrap::{bootstrap_set_impl, bootstrap_status_impl};
use tauri_app_core::commands::lifecycle::{
    init_vault_impl, init_vault_machine_mode_impl, lock_vault_impl, read_vault_kind,
    read_vault_trust, reset_vault_impl, switch_vault_mode_impl, unlock_if_machine_mode_impl,
    unlock_vault_impl, vault_status_impl,
};
use tauri_app_core::commands::settings::{list_css_themes_impl, read_css_theme_impl};
use tauri_app_core::error::AppError;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, VaultState};

const PASS: &str = "correct-horse-battery-staple-extended-22";

fn alone() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A machine that has never run Daisy: an empty config dir and no profile.
///
/// The redirect is only honoured when `license` carries its `ui-test`
/// feature. Without it these tests write to the config directory of the
/// machine running them, so the redirect is checked rather than assumed.
fn fresh_machine() -> tempfile::TempDir {
    let td = tempfile::tempdir().unwrap();
    std::env::set_var("DAISY_CONFIG_DIR", td.path());
    std::env::remove_var("DAISY_PROFILE_DIR");
    let redirected = license::config_dir_path().unwrap();
    assert_eq!(
        redirected.canonicalize().unwrap(),
        td.path().canonicalize().unwrap(),
        "DAISY_CONFIG_DIR is being ignored — these tests would write to the real \
         config directory. tauri-app needs license/ui-test in its dev-dependencies."
    );
    td
}

fn app_at(dir: &std::path::Path) -> AppState {
    AppState::new(ProfileDir::at(dir.join("daisy")).unwrap())
}

// ---- where the profile goes ------------------------------------------------

#[test]
fn a_machine_that_has_never_run_daisy_has_no_bootstrap_but_knows_where_one_would_go() {
    let _alone = alone();
    let _td = fresh_machine();
    let status = bootstrap_status_impl().unwrap();
    assert!(!status.has_bootstrap, "nothing has been chosen yet");
    assert_eq!(status.profile_dir, None);
    assert_eq!(status.env_override, None);
    assert!(
        status.platform_default.is_absolute(),
        "the screen can offer a real default: {}",
        status.platform_default.display()
    );
}

#[test]
fn choosing_a_profile_creates_it_and_the_next_launch_reads_it_back() {
    let _alone = alone();
    let td = fresh_machine();
    let chosen = td.path().join("Documents/Daisy");
    assert!(!chosen.exists());

    bootstrap_set_impl(chosen.clone()).unwrap();
    assert!(chosen.is_dir(), "the directory is made, not just recorded");

    let status = bootstrap_status_impl().unwrap();
    assert!(status.has_bootstrap);
    assert_eq!(status.profile_dir.as_deref(), Some(chosen.as_path()));

    // Choosing again moves it, and the old directory is left where it was.
    let second = td.path().join("Elsewhere/Daisy");
    bootstrap_set_impl(second.clone()).unwrap();
    assert_eq!(
        bootstrap_status_impl().unwrap().profile_dir.as_deref(),
        Some(second.as_path())
    );
    assert!(chosen.is_dir(), "the first location is not deleted out from under its files");
}

#[test]
fn the_environment_override_is_reported_beside_the_saved_choice() {
    let _alone = alone();
    let td = fresh_machine();
    let saved = td.path().join("Saved");
    bootstrap_set_impl(saved.clone()).unwrap();

    let forced = td.path().join("Forced");
    std::env::set_var("DAISY_PROFILE_DIR", &forced);
    let status = bootstrap_status_impl().unwrap();
    assert_eq!(
        status.profile_dir.as_deref(),
        Some(saved.as_path()),
        "the saved choice is still shown"
    );
    assert_eq!(
        status.env_override.as_deref(),
        Some(forced.as_path()),
        "and so is what is overriding it this session"
    );
    std::env::remove_var("DAISY_PROFILE_DIR");
}

#[test]
fn a_bootstrap_file_that_will_not_parse_is_an_error_rather_than_a_silent_default() {
    let _alone = alone();
    let td = fresh_machine();
    bootstrap_set_impl(td.path().join("Daisy")).unwrap();
    let path = license::config_dir().unwrap().join("bootstrap.json");
    std::fs::write(&path, b"{ not json").unwrap();
    let e = bootstrap_status_impl().unwrap_err();
    assert!(
        matches!(e, AppError::Io(_)),
        "a corrupt pointer must be reported, not read as a fresh install: {e:?}"
    );
}

// ---- the two acknowledgements ---------------------------------------------

#[test]
fn consent_and_the_eula_are_unaccepted_until_they_are_accepted() {
    let _alone = alone();
    let _td = fresh_machine();
    use tauri_app_core::bootstrap::{Consent, Eula};

    assert!(!Consent::is_accepted(), "a new machine has not agreed to anything");
    assert!(!Eula::is_accepted());

    Consent::accept().unwrap();
    assert!(Consent::is_accepted());
    assert!(!Eula::is_accepted(), "one does not stand in for the other");

    Eula::accept().unwrap();
    assert!(Eula::is_accepted());

    // Accepting twice is not an error and does not undo it.
    Consent::accept().unwrap();
    Eula::accept().unwrap();
    assert!(Consent::is_accepted() && Eula::is_accepted());
}

#[test]
fn an_acknowledgement_file_that_will_not_parse_re_prompts() {
    let _alone = alone();
    let _td = fresh_machine();
    use tauri_app_core::bootstrap::{Consent, Eula};
    Consent::accept().unwrap();
    Eula::accept().unwrap();

    for name in ["consent.json", "eula.json"] {
        std::fs::write(license::config_dir().unwrap().join(name), b"<not json>").unwrap();
    }
    assert!(
        !Consent::is_accepted() && !Eula::is_accepted(),
        "an unreadable record asks again rather than assuming yes"
    );
}

// ---- the vault -------------------------------------------------------------

#[test]
fn the_first_vault_is_made_unlocked_and_survives_a_lock() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());
    let vs = VaultState::new();

    let before = vault_status_impl(&app, &vs).unwrap();
    assert!(!before.vault_exists, "there is no vault yet");
    assert!(!before.unlocked);

    init_vault_impl(&app, &vs, PASS).unwrap();
    let after = vault_status_impl(&app, &vs).unwrap();
    assert!(after.vault_exists && after.unlocked, "making one leaves it open");

    assert!(
        init_vault_impl(&app, &vs, PASS).is_err(),
        "a second vault must not overwrite the first"
    );

    lock_vault_impl(&app, &vs).unwrap();
    assert!(!vault_status_impl(&app, &vs).unwrap().unlocked);

    assert!(
        unlock_vault_impl(&app, &vs, "the wrong passphrase entirely").is_err(),
        "the wrong passphrase does not open it"
    );
    assert!(
        !vault_status_impl(&app, &vs).unwrap().unlocked,
        "and leaves it locked"
    );

    unlock_vault_impl(&app, &vs, PASS).unwrap();
    assert!(vault_status_impl(&app, &vs).unwrap().unlocked);
}

#[test]
fn unlocking_a_vault_that_is_not_there_says_where_to_make_one() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());
    let vs = VaultState::new();
    let e = unlock_vault_impl(&app, &vs, PASS).unwrap_err();
    assert!(
        matches!(e, AppError::Config(ref m) if m.contains("Welcome")),
        "the message points at the screen that makes one: {e:?}"
    );
}

#[test]
fn a_machine_mode_vault_opens_itself_on_the_next_launch() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());
    let vs = VaultState::new();

    init_vault_machine_mode_impl(&app, &vs).unwrap();
    let status = vault_status_impl(&app, &vs).unwrap();
    assert!(status.vault_exists && status.unlocked);
    assert_eq!(read_vault_kind(&app), "machine", "the mode is recorded on disk");

    // Locking and coming back is the launch after a restart: startup calls
    // this, and nothing is typed.
    lock_vault_impl(&app, &vs).unwrap();
    assert!(!vault_status_impl(&app, &vs).unwrap().unlocked);
    let vs2 = VaultState::new();
    assert!(
        unlock_if_machine_mode_impl(&app, &vs2).unwrap(),
        "startup reports that it opened the vault"
    );
    assert!(vault_status_impl(&app, &vs2).unwrap().unlocked);
}

#[test]
fn startup_leaves_a_passphrase_vault_alone() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());
    let vs = VaultState::new();
    init_vault_impl(&app, &vs, PASS).unwrap();
    assert_eq!(read_vault_kind(&app), "passphrase", "the default needs no marker");

    lock_vault_impl(&app, &vs).unwrap();
    assert!(
        !unlock_if_machine_mode_impl(&app, &vs).unwrap(),
        "a passphrase vault is not opened by the machine key"
    );
    assert!(
        !vault_status_impl(&app, &vs).unwrap().unlocked,
        "and stays locked for the passphrase screen"
    );
}

// ---- the binding -----------------------------------------------------------

#[test]
fn a_machine_mode_vault_names_the_computer_that_opens_it() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());
    let vs = VaultState::new();
    init_vault_machine_mode_impl(&app, &vs).unwrap();

    let here = tauri_app_core::hardware::machine_name();
    let trust = read_vault_trust(&app);
    assert_eq!(trust.kind, "machine");
    assert_eq!(trust.trusted_on.as_deref(), Some(here.as_str()));
    assert_eq!(trust.this_machine, here);
}

#[test]
fn the_machine_marker_stays_the_single_line_older_builds_read() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());
    let vs = VaultState::new();
    init_vault_machine_mode_impl(&app, &vs).unwrap();
    let marker = std::fs::read_to_string(app.profile.root().join("keys.vault.kind")).unwrap();
    assert_eq!(marker, "machine\n", "an older build trims the whole file and compares");
}

#[test]
fn a_two_line_marker_from_a_dev_build_still_opens_and_is_rewritten() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());
    let vs = VaultState::new();
    init_vault_machine_mode_impl(&app, &vs).unwrap();
    let root = app.profile.root();
    std::fs::write(root.join("keys.vault.kind"), "machine\nfictional-desktop\n").unwrap();
    std::fs::remove_file(root.join("keys.vault.owner")).unwrap();

    assert_eq!(read_vault_kind(&app), "machine");
    assert_eq!(read_vault_trust(&app).trusted_on.as_deref(), Some("fictional-desktop"));

    lock_vault_impl(&app, &vs).unwrap();
    assert!(unlock_if_machine_mode_impl(&app, &VaultState::new()).unwrap());
    assert_eq!(std::fs::read_to_string(root.join("keys.vault.kind")).unwrap(), "machine\n");
    let here = tauri_app_core::hardware::machine_name();
    assert_eq!(read_vault_trust(&app).trusted_on.as_deref(), Some(here.as_str()));
}

#[test]
fn opening_the_vault_after_a_rename_records_the_new_name() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());
    let vs = VaultState::new();
    init_vault_machine_mode_impl(&app, &vs).unwrap();
    std::fs::write(app.profile.root().join("keys.vault.owner"), "fictional-old-name\n").unwrap();

    lock_vault_impl(&app, &vs).unwrap();
    assert!(unlock_if_machine_mode_impl(&app, &VaultState::new()).unwrap());
    let here = tauri_app_core::hardware::machine_name();
    assert_eq!(read_vault_trust(&app).trusted_on.as_deref(), Some(here.as_str()));
}

/// A profile folder shared from another computer: its machine-mode vault is
/// sealed with that computer's key and its marker names that computer.
fn profile_trusted_on_another_computer(app: &AppState) {
    let owner = VaultState::new();
    init_vault_impl(app, &owner, OTHER_COMPUTER_KEY).unwrap();
    std::fs::write(app.profile.root().join("keys.vault.kind"), "machine\n").unwrap();
    std::fs::write(app.profile.root().join("keys.vault.owner"), "fictional-laptop\n").unwrap();
}

const OTHER_COMPUTER_KEY: &str = "daisy.v1.machine:00000000-fictional-laptop";

#[test]
fn a_profile_trusted_on_another_computer_stays_locked_and_names_that_computer() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());
    profile_trusted_on_another_computer(&app);

    let vs = VaultState::new();
    assert!(
        unlock_if_machine_mode_impl(&app, &vs).is_err(),
        "this computer's key does not open the other computer's vault"
    );
    assert!(!vault_status_impl(&app, &vs).unwrap().unlocked);
    let trust = read_vault_trust(&app);
    assert_eq!(trust.kind, "machine");
    assert_eq!(trust.trusted_on.as_deref(), Some("fictional-laptop"));
}

#[test]
fn a_passphrase_added_on_the_trusted_computer_opens_the_profile_here() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());
    profile_trusted_on_another_computer(&app);

    // On the trusted computer: unlock with its key, then add a passphrase.
    let owner = VaultState::new();
    unlock_vault_impl(&app, &owner, OTHER_COMPUTER_KEY).unwrap();
    assert_eq!(switch_vault_mode_impl(&app, &owner, Some(PASS)).unwrap(), "passphrase");

    // Here: the unlock screen asks for that passphrase, and it opens.
    let trust = read_vault_trust(&app);
    assert_eq!(trust.kind, "passphrase");
    assert_eq!(trust.trusted_on, None);
    assert!(
        !app.profile.root().join("keys.vault.owner").exists(),
        "passphrase mode keeps no owner record"
    );
    let vs = VaultState::new();
    unlock_vault_impl(&app, &vs, PASS).unwrap();
    assert!(vault_status_impl(&app, &vs).unwrap().unlocked);
}

#[test]
fn a_machine_marker_without_a_computer_name_reads_as_unnamed() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());
    std::fs::create_dir_all(app.profile.root()).unwrap();
    std::fs::write(app.profile.root().join("keys.vault.kind"), "machine\n").unwrap();
    let trust = read_vault_trust(&app);
    assert_eq!(trust.kind, "machine");
    assert_eq!(trust.trusted_on, None);
    assert_eq!(read_vault_kind(&app), "machine");
}

#[test]
fn resetting_the_vault_forgets_the_machine_marker() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());
    profile_trusted_on_another_computer(&app);

    let vs = VaultState::new();
    reset_vault_impl(&app, &vs).unwrap();
    assert!(!vault_status_impl(&app, &vs).unwrap().vault_exists);
    assert_eq!(read_vault_kind(&app), "passphrase");
    assert_eq!(read_vault_trust(&app).trusted_on, None);
    assert!(!app.profile.root().join("keys.vault.owner").exists());
}

#[test]
fn a_fresh_profile_is_claimed_by_the_install_that_opens_it() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());

    assert!(matches!(
        profile_binding_check_impl(&app).unwrap(),
        BindingState::Ok
    ));
    let binding = app.profile.root().join("binding.json");
    assert!(binding.is_file(), "the claim is written down");

    // Opening it again is the same install, so it stays allowed.
    assert!(matches!(
        profile_binding_check_impl(&app).unwrap(),
        BindingState::Ok
    ));
}

#[test]
fn a_profile_claimed_by_another_install_is_foreign() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());
    profile_binding_check_impl(&app).unwrap();

    // What a copied profile directory looks like: someone else's claim.
    let binding = app.profile.root().join("binding.json");
    let mut file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&binding).unwrap()).unwrap();
    file["owner"] = serde_json::json!("install:0000000000000000000000000000beef");
    std::fs::write(&binding, serde_json::to_vec(&file).unwrap()).unwrap();

    assert!(
        matches!(profile_binding_check_impl(&app).unwrap(), BindingState::Foreign),
        "an unlicensed install does not open a profile another install claimed"
    );
}

#[test]
fn a_binding_left_by_a_licensed_install_is_opened_on_trial() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());
    let binding = app.profile.root().join("binding.json");
    std::fs::create_dir_all(binding.parent().unwrap()).unwrap();
    std::fs::write(
        &binding,
        serde_json::to_vec(&serde_json::json!({
            "profile_id": "b0e6a0c2-0000-4000-8000-000000000000",
            "owner": "license:rowan@example.org",
        }))
        .unwrap(),
    )
    .unwrap();

    assert!(
        matches!(profile_binding_check_impl(&app).unwrap(), BindingState::Ok),
        "a licence expiring must not lock the owner out of their own profile"
    );
}

// ---- the themes the settings screen offers ---------------------------------

#[test]
fn the_built_in_themes_are_listed_and_read_and_nothing_else_is() {
    let _alone = alone();
    let td = fresh_machine();
    let app = app_at(td.path());

    let themes = list_css_themes_impl(&app).unwrap();
    for name in &themes {
        let css = read_css_theme_impl(&app, name).unwrap();
        assert!(!css.is_empty(), "{name} is an empty stylesheet");
    }

    // A theme name is a name, not a path into the filesystem.
    for name in [
        "../../../../etc/passwd",
        "..\\..\\windows\\system32",
        "/etc/passwd",
        "nope",
    ] {
        assert!(
            read_css_theme_impl(&app, name).is_err(),
            "{name:?} must not be readable as a theme"
        );
    }
}
