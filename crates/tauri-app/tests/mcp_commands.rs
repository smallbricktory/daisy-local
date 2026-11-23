// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The MCP listener the Settings screen switches on: its status card, the
//! token it hands to a client, and the listener starting, moving and
//! stopping as the setting changes.
//!
//! These need an `AppHandle` for the managed state, so they run against
//! Tauri's mock runtime. Ports are bound for real, so each test takes its own
//! and the tests take turns.
use tauri_app_core::commands::lifecycle::{init_vault_impl, lock_vault_impl, unlock_vault_impl};
use tauri_app_core::error::AppError;
use tauri_app_core::mcp::server::{
    apply_mcp_state, mcp_port_available_impl, mcp_regenerate_token_impl, mcp_status_impl, McpHandle,
};
use tauri_app_core::profile::ProfileDir;
use tauri::Manager as _;
use tauri_app_core::state::{AppState, VaultState};

const PASS: &str = "correct-horse-battery-staple-extended-22";

fn alone() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A port nothing else is on: bound, read back, released. A later bind can
/// still lose the race, which is why the tests take turns.
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    l.local_addr().unwrap().port()
}

struct Harness {
    app: tauri::App<tauri::test::MockRuntime>,
    _dir: tempfile::TempDir,
}

impl Harness {
    /// A running app with an unlocked vault and MCP switched off.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
        let state = AppState::new(profile);
        let vault = VaultState::new();
        init_vault_impl(&state, &vault, PASS).unwrap();

        let app = tauri::test::mock_app();
        app.manage(state);
        app.manage(vault);
        app.manage(McpHandle::default());
        Self { app, _dir: dir }
    }

    fn handle(&self) -> tauri::AppHandle<tauri::test::MockRuntime> {
        self.app.handle().clone()
    }

    fn state(&self) -> tauri::State<'_, AppState> {
        self.app.state::<AppState>()
    }

    fn vault(&self) -> tauri::State<'_, VaultState> {
        self.app.state::<VaultState>()
    }

    fn enable(&self, port: u16) {
        self.state()
            .update_machine_settings(|m| {
                m.mcp_enabled = true;
                m.mcp_port = port;
            })
            .unwrap();
    }

    fn disable(&self) {
        self.state()
            .update_machine_settings(|m| m.mcp_enabled = false)
            .unwrap();
    }
}

// ---- the status card -------------------------------------------------------

#[test]
fn mcp_is_off_and_silent_until_it_is_switched_on() {
    let _alone = alone();
    let h = Harness::new();
    let s = mcp_status_impl(&h.handle()).unwrap();
    assert!(!s.enabled);
    assert!(!s.running);
    assert!(s.token.is_empty(), "no token is minted for a feature that is off");
    assert!(
        s.claude_command.is_empty(),
        "and nothing is offered to paste"
    );
    assert!(
        s.endpoint.starts_with("http://127.0.0.1:"),
        "the address is still shown so the port field means something: {}",
        s.endpoint
    );
}

#[test]
fn switching_it_on_starts_a_listener_and_offers_a_command_to_paste() {
    let _alone = alone();
    let h = Harness::new();
    let port = free_port();
    h.enable(port);
    apply_mcp_state(&h.handle()).unwrap();

    let s = mcp_status_impl(&h.handle()).unwrap();
    assert!(s.enabled && s.running);
    assert_eq!(s.port, port);
    assert_eq!(s.endpoint, format!("http://127.0.0.1:{port}/mcp"));
    assert!(!s.token.is_empty(), "a client needs a token");
    assert!(
        s.claude_command.contains(&s.endpoint) && s.claude_command.contains(&s.token),
        "the pasteable command carries both: {}",
        s.claude_command
    );
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok(),
        "something is actually listening"
    );

    h.disable();
    apply_mcp_state(&h.handle()).unwrap();
    assert!(!mcp_status_impl(&h.handle()).unwrap().running);
}

#[test]
fn the_token_is_minted_once_and_survives_a_lock() {
    let _alone = alone();
    let h = Harness::new();
    h.enable(free_port());
    apply_mcp_state(&h.handle()).unwrap();

    let first = mcp_status_impl(&h.handle()).unwrap().token;
    let second = mcp_status_impl(&h.handle()).unwrap().token;
    assert_eq!(first, second, "reading the card does not rotate the token");

    lock_vault_impl(&h.state(), &h.vault()).unwrap();
    unlock_vault_impl(&h.state(), &h.vault(), PASS).unwrap();
    assert_eq!(
        mcp_status_impl(&h.handle()).unwrap().token,
        first,
        "the token lives in the vault, so it comes back with it"
    );
}

#[test]
fn a_locked_vault_shows_the_setting_without_the_token() {
    let _alone = alone();
    let h = Harness::new();
    h.enable(free_port());
    apply_mcp_state(&h.handle()).unwrap();
    assert!(!mcp_status_impl(&h.handle()).unwrap().token.is_empty());

    lock_vault_impl(&h.state(), &h.vault()).unwrap();
    let s = mcp_status_impl(&h.handle()).unwrap();
    assert!(s.enabled, "the setting is still on");
    assert!(s.token.is_empty(), "but the secret is not readable");
    assert!(s.claude_command.is_empty());
}

// ---- rotating the token ----------------------------------------------------

#[test]
fn regenerating_the_token_replaces_it_and_keeps_serving() {
    let _alone = alone();
    let h = Harness::new();
    let port = free_port();
    h.enable(port);
    apply_mcp_state(&h.handle()).unwrap();
    let before = mcp_status_impl(&h.handle()).unwrap().token;

    let after = mcp_regenerate_token_impl(&h.handle()).unwrap();
    assert_ne!(after.token, before, "the old token stops working");
    assert!(!after.token.is_empty());
    assert!(after.running, "the listener comes back up on the same port");
    assert_eq!(after.port, port);

    lock_vault_impl(&h.state(), &h.vault()).unwrap();
    unlock_vault_impl(&h.state(), &h.vault(), PASS).unwrap();
    assert_eq!(
        mcp_status_impl(&h.handle()).unwrap().token,
        after.token,
        "the new token was written to the vault, not only held in memory"
    );
}

#[test]
fn regenerating_needs_an_unlocked_vault() {
    let _alone = alone();
    let h = Harness::new();
    h.enable(free_port());
    apply_mcp_state(&h.handle()).unwrap();
    lock_vault_impl(&h.state(), &h.vault()).unwrap();
    assert!(matches!(
        mcp_regenerate_token_impl(&h.handle()),
        Err(AppError::VaultLocked)
    ));
}

// ---- moving and restarting -------------------------------------------------

#[test]
fn changing_the_port_moves_the_listener() {
    let _alone = alone();
    let h = Harness::new();
    let first = free_port();
    h.enable(first);
    apply_mcp_state(&h.handle()).unwrap();
    assert!(std::net::TcpStream::connect(("127.0.0.1", first)).is_ok());

    let second = free_port();
    h.enable(second);
    apply_mcp_state(&h.handle()).unwrap();

    assert_eq!(mcp_status_impl(&h.handle()).unwrap().port, second);
    assert!(std::net::TcpStream::connect(("127.0.0.1", second)).is_ok());
}

#[test]
fn applying_the_same_state_twice_leaves_the_listener_where_it_is() {
    let _alone = alone();
    let h = Harness::new();
    let port = free_port();
    h.enable(port);
    apply_mcp_state(&h.handle()).unwrap();
    apply_mcp_state(&h.handle()).unwrap();
    apply_mcp_state(&h.handle()).unwrap();
    let s = mcp_status_impl(&h.handle()).unwrap();
    assert!(s.running);
    assert_eq!(s.port, port);
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
}

#[test]
fn a_port_someone_else_holds_is_reported_rather_than_taken() {
    let _alone = alone();
    let h = Harness::new();
    let squatter = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = squatter.local_addr().unwrap().port();

    h.enable(port);
    let e = apply_mcp_state(&h.handle()).unwrap_err();
    assert!(
        format!("{e}").contains(&port.to_string()),
        "the message names the port to change: {e}"
    );
    assert!(
        !mcp_status_impl(&h.handle()).unwrap().running,
        "and nothing is left half-started"
    );
}

#[test]
fn switching_on_with_a_locked_vault_waits_rather_than_failing() {
    let _alone = alone();
    let h = Harness::new();
    let port = free_port();
    lock_vault_impl(&h.state(), &h.vault()).unwrap();
    h.enable(port);

    apply_mcp_state(&h.handle()).expect("a locked vault is not an error at startup");
    assert!(
        !mcp_status_impl(&h.handle()).unwrap().running,
        "it cannot start without the token"
    );

    // Unlocking is what the app calls next, and that is when it comes up.
    unlock_vault_impl(&h.state(), &h.vault(), PASS).unwrap();
    apply_mcp_state(&h.handle()).unwrap();
    assert!(mcp_status_impl(&h.handle()).unwrap().running);
}

// ---- the port field's own check --------------------------------------------

#[test]
fn a_free_port_reads_as_available_and_a_taken_one_does_not() {
    let _alone = alone();
    let h = Harness::new();
    let squatter = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let taken = squatter.local_addr().unwrap().port();

    assert!(mcp_port_available_impl(&h.handle(), free_port()));
    assert!(!mcp_port_available_impl(&h.handle(), taken));
}

#[test]
fn the_port_we_are_already_on_reads_as_available() {
    let _alone = alone();
    let h = Harness::new();
    let port = free_port();
    h.enable(port);
    apply_mcp_state(&h.handle()).unwrap();
    assert!(
        mcp_port_available_impl(&h.handle(), port),
        "the field must not warn about the port the user is already using"
    );
}

// ---- vault transitions -----------------------------------------------------

/// The HTTP status the listener on `port` answers a ping with, presenting
/// `token`. Zero when nothing answers.
fn ping_status(port: u16, token: &str) -> u16 {
    use std::io::{Read as _, Write as _};
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) else { return 0 };
    s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    write!(
        s,
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0)
}

#[test]
fn a_reset_and_a_new_vault_serve_the_new_token() {
    let _alone = alone();
    let h = Harness::new();
    let port = free_port();
    h.enable(port);
    apply_mcp_state(&h.handle()).unwrap();
    let old = mcp_status_impl(&h.handle()).unwrap().token;
    assert_eq!(ping_status(port, &old), 200);

    tauri_app_core::commands::lifecycle::reset_vault_impl(&h.state(), &h.vault()).unwrap();
    init_vault_impl(&h.state(), &h.vault(), PASS).unwrap();
    apply_mcp_state(&h.handle()).unwrap();

    let new = mcp_status_impl(&h.handle()).unwrap().token;
    assert_ne!(new, old, "a new vault mints its own token");
    assert_eq!(ping_status(port, &new), 200, "the token the card shows is the one the listener takes");
    assert_eq!(ping_status(port, &old), 401, "the reset vault's token no longer works");
}
