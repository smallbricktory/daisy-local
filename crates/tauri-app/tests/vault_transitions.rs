// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! What follows each vault transition, driven the way the commands drive it:
//! init / unlock end in `vault_opened`, lock / reset end in `vault_closed`.
//! The MCP listener, the phone-sync listener and the built-in model follow
//! the vault. Runs against Tauri's mock runtime; ports are bound for real, so
//! the tests take turns.
use tauri::Manager as _;
use tauri_app_core::builtin_model;
use tauri_app_core::commands::lifecycle::{
    init_vault_impl, reset_vault_impl, unlock_vault_impl, vault_closed, vault_opened,
};
use tauri_app_core::mcp::server::{mcp_status_impl, McpHandle};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, ProviderId, VaultState};

const PASS: &str = "correct-horse-battery-staple-extended-22";

fn alone() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    l.local_addr().unwrap().port()
}

/// `cargo test` builds every bin target of the workspace member under test's
/// dependencies into the same profile dir as this test binary.
fn fake_worker() -> String {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    let exe = if cfg!(windows) { "fake-llm-worker.exe" } else { "fake-llm-worker" };
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let target = std::env::var("CARGO_TARGET_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| p.join("target"));
    let path = target.join(profile).join(exe);
    assert!(path.is_file(), "build it first: cargo build -p summarize --bin fake-llm-worker ({})", path.display());
    path.to_string_lossy().into_owned()
}

struct Harness {
    app: tauri::App<tauri::test::MockRuntime>,
    _dir: tempfile::TempDir,
}

impl Harness {
    /// A running app on a fresh profile with no vault yet.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
        let app = tauri::test::mock_app();
        app.manage(AppState::new(profile));
        app.manage(VaultState::new());
        app.manage(McpHandle::default());
        app.manage(tauri_app_core::sync::server::SyncServerHandle::on_port(0));
        app.manage(tauri_app_core::sync::peripheral::PeripheralHandle::default());
        Self { app, _dir: dir }
    }

    fn handle(&self) -> tauri::AppHandle<tauri::test::MockRuntime> {
        self.app.handle().clone()
    }

    /// The port the sync listener holds.
    fn sync_port(&self) -> u16 {
        self.app.state::<tauri_app_core::sync::server::SyncServerHandle>().bound_port().expect("the listener is up")
    }

    fn state(&self) -> tauri::State<'_, AppState> {
        self.app.state::<AppState>()
    }

    fn vault(&self) -> tauri::State<'_, VaultState> {
        self.app.state::<VaultState>()
    }

    /// What the init command does.
    fn init(&self) {
        init_vault_impl(&self.state(), &self.vault(), PASS).unwrap();
        vault_opened(&self.handle());
    }

    /// What the unlock command does.
    fn unlock(&self) {
        unlock_vault_impl(&self.state(), &self.vault(), PASS).unwrap();
        vault_opened(&self.handle());
    }

    /// What the lock command does.
    fn lock(&self) {
        vault_closed(&self.handle());
    }

    /// What the reset command does.
    fn reset(&self) {
        reset_vault_impl(&self.state(), &self.vault()).unwrap();
        vault_closed(&self.handle());
    }

    fn enable_mcp(&self, port: u16) {
        self.state()
            .update_machine_settings(|m| {
                m.mcp_enabled = true;
                m.mcp_port = port;
            })
            .unwrap();
    }
}

/// The HTTP status the MCP listener on `port` answers a ping with,
/// presenting `token`. Zero when nothing answers.
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

/// Polls until nothing accepts on `port`. A connect that times out counts as
/// nothing accepting.
fn closes(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(250)).is_err() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

#[test]
fn a_new_vault_runs_the_migrations_and_starts_mcp() {
    let _alone = alone();
    let h = Harness::new();
    let port = free_port();
    h.enable_mcp(port);
    h.init();
    let migrations: serde_json::Value = serde_json::from_slice(
        &std::fs::read(h.state().profile.root().join("migrations.json")).unwrap(),
    )
    .unwrap();
    assert!(
        migrations["applied"].as_array().unwrap().iter().any(|v| v == "2026-05-vault-v3-tags-out"),
        "{migrations}"
    );
    let token = mcp_status_impl(&h.handle()).unwrap().token;
    assert_eq!(ping_status(port, &token), 200);
}

#[test]
fn a_lock_takes_the_mcp_listener_down_and_an_unlock_brings_it_back() {
    let _alone = alone();
    let h = Harness::new();
    let port = free_port();
    h.enable_mcp(port);
    h.init();
    let token = mcp_status_impl(&h.handle()).unwrap().token;
    assert_eq!(ping_status(port, &token), 200);

    h.lock();
    assert!(!mcp_status_impl(&h.handle()).unwrap().running);
    assert!(closes(port), "a locked vault leaves nothing listening");

    h.unlock();
    assert_eq!(ping_status(port, &token), 200, "the same token serves again");
}

#[test]
fn a_reset_and_a_new_vault_serve_the_new_token() {
    let _alone = alone();
    let h = Harness::new();
    let port = free_port();
    h.enable_mcp(port);
    h.init();
    let old = mcp_status_impl(&h.handle()).unwrap().token;

    h.reset();
    assert!(closes(port), "a reset vault leaves nothing listening");
    h.init();
    let new = mcp_status_impl(&h.handle()).unwrap().token;
    assert_ne!(new, old);
    assert_eq!(ping_status(port, &new), 200);
    assert_eq!(ping_status(port, &old), 401);
}

/// The phone-sync listener is up while a pairing code shows, and a reset
/// takes it down with the vault.
#[test]
fn a_reset_takes_the_sync_listener_down() {
    let _alone = alone();
    let h = Harness::new();
    h.init();
    tauri_app_core::commands::sync::sync_pairing_begin_impl(
        &h.state(),
        &h.vault(),
        "127.0.0.1".into(),
        tauri_app_core::now_unix(),
    )
    .unwrap();
    assert!(tauri_app_core::sync::server::apply_sync_state(&h.handle()));
    let port = h.sync_port();
    assert_ne!(port, tauri_app_core::commands::sync::SYNC_PORT);
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());

    h.reset();
    assert!(h.state().sync_window.lock().unwrap().is_none(), "the pairing window closes with the vault");
    assert!(closes(port), "a reset vault leaves the sync port closed");
}

/// A pairing key that runs out with no phone paired takes the listener down
/// with it, whether or not the settings screen reads the status again.
#[test]
fn an_expired_pairing_key_takes_the_sync_listener_down() {
    let _alone = alone();
    let h = Harness::new();
    h.init();
    tauri_app_core::commands::sync::sync_pairing_begin_impl(
        &h.state(),
        &h.vault(),
        "127.0.0.1".into(),
        tauri_app_core::now_unix(),
    )
    .unwrap();
    assert!(tauri_app_core::sync::server::apply_sync_state(&h.handle()));
    let port = h.sync_port();
    assert_ne!(port, tauri_app_core::commands::sync::SYNC_PORT);
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());

    if let Some(tauri_app_core::commands::sync::SyncWindow::Live(open)) = h.state().sync_window.lock().unwrap().as_mut() {
        open.key.expires_unix = tauri_app_core::now_unix();
    }
    tauri_app_core::sync::server::end_expired_key(&h.handle());
    assert!(closes(port), "no phone is paired and no key is live, so the sync port closes");
}

/// Locking the vault ends a request running on the built-in model, and the
/// model takes no new one until the vault opens again.
#[test]
fn a_lock_stops_the_built_in_model_mid_request() {
    let _alone = alone();
    std::env::set_var("DAISY_LLM_BIN", fake_worker());
    let h = Harness::new();
    h.init();
    let dir = h.state().profile.models_dir().join("llm").join(builtin_model::row().id);
    std::fs::create_dir_all(&dir).unwrap();
    for (n, _, size) in builtin_model::row().files {
        std::fs::File::create(dir.join(n)).unwrap().set_len(*size).unwrap();
    }
    h.state().set_ai_provider(Some(ProviderId::Local)).unwrap();

    let chat = builtin_model::chat(&h.state(), &h.vault()).unwrap();
    let caller = chat.clone();
    let request = std::thread::spawn(move || caller.complete_text("", &[("user".into(), "hang".into())], 16));
    std::thread::sleep(std::time::Duration::from_millis(500));

    let started = std::time::Instant::now();
    h.lock();
    assert!(request.join().unwrap().is_err(), "the request in flight ends");
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "{:?}", started.elapsed());
    assert!(!chat.has_worker());
    assert!(matches!(
        builtin_model::chat(&h.state(), &h.vault()),
        Err(tauri_app_core::error::AppError::VaultLocked)
    ));

    h.unlock();
    let again = builtin_model::chat(&h.state(), &h.vault()).unwrap();
    assert!(again.complete_text("", &[("user".into(), "free".into())], 16).is_ok());
    std::env::remove_var("DAISY_LLM_BIN");
}
