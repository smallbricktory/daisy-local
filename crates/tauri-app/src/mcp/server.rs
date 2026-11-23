// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Loopback HTTP listener for the MCP server (streamable-HTTP transport,
//! stateless). POST /mcp with `Authorization: Bearer <token>`; one JSON-RPC
//! message per request. GET (SSE streaming) answers 405.

use crate::error::{AppError, Result};
use crate::mcp::{protocol, token, tools::AppHost};

use crate::state::{AppState, VaultState};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tauri::Manager as _;

/// Managed by Tauri (`.manage(McpHandle::default())`). Tracks the running
/// listener; settings changes stop/start it without an app restart.
#[derive(Default)]
pub struct McpHandle {
    inner: Mutex<Option<Running>>,
}

struct Running {
    port: u16,
    /// The token the listener accepts.
    token: String,
    shutdown: tokio::sync::oneshot::Sender<()>,
}

/// How long a restart on the same port waits for the stopped listener to let
/// go of it.
const REBIND_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Binds the loopback port. `just_stopped` is true when this process stopped
/// a listener on the same port a moment ago; the bind then retries while
/// that listener lets go.
fn bind_loopback(port: u16, just_stopped: bool) -> std::io::Result<std::net::TcpListener> {
    let deadline = std::time::Instant::now() + REBIND_WAIT;
    loop {
        match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Err(e)
                if just_stopped
                    && e.kind() == std::io::ErrorKind::AddrInUse
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            r => return r,
        }
    }
}

struct Ctx<R: tauri::Runtime> {
    app_handle: tauri::AppHandle<R>,
    token: String,
}

#[derive(Debug, Serialize)]
pub struct McpStatus {
    pub enabled: bool,
    pub running: bool,
    pub port: u16,
    pub token: String,
    pub endpoint: String,
    /// Paste-ready `claude mcp add …` command.
    pub claude_command: String,
}

/// Fetches the MCP bearer token from the unlocked vault, minting +
/// persisting one on first use. Returns `Ok(None)` when the vault is locked.
fn vault_token<R: tauri::Runtime>(app_handle: &tauri::AppHandle<R>) -> Result<Option<String>> {
    let app = app_handle.state::<AppState>();
    let vault = app_handle.state::<VaultState>();
    let (tok, generated) = {
        let mut guard = vault.keys.lock().unwrap();
        match guard.as_mut() {
            Some(keys) => crate::state::get_or_create_mcp_token(keys),
            None => return Ok(None),
        }
    };
    // A freshly-minted token is persisted before being handed out.
    if generated {
        crate::commands::lifecycle::re_encrypt_keys(&app, &vault)?;
    }
    Ok(Some(tok))
}

/// Reconciles the listener with settings and the vault: it runs only while
/// MCP is enabled and the vault is open, on the configured port, taking the
/// vault's current token. Anything else about it is stopped or restarted.
/// Called after MCP settings changes and on every vault transition.
pub fn apply_mcp_state<R: tauri::Runtime>(app_handle: &tauri::AppHandle<R>) -> Result<()> {
    let state = app_handle.state::<AppState>();
    let settings = state.machine_settings();
    let handle = app_handle.state::<McpHandle>();
    let mut running = handle.inner.lock().unwrap();

    let tok = if settings.mcp_enabled { vault_token(app_handle)? } else { None };
    if let (Some(r), Some(t)) = (running.as_ref(), tok.as_ref()) {
        if r.port == settings.mcp_port && r.token == *t {
            return Ok(());
        }
    }
    let stopped_port = running.take().map(|r| {
        let _ = r.shutdown.send(());
        r.port
    });
    let Some(tok) = tok else {
        if settings.mcp_enabled {
            log::info!("mcp: enabled but vault locked; starts when it opens");
        }
        return Ok(());
    };
    // The bind happens synchronously; a port conflict surfaces to the caller
    // as an error.
    let std_listener = bind_loopback(settings.mcp_port, stopped_port == Some(settings.mcp_port))
        .map_err(|e| AppError::Io(format!("mcp: bind 127.0.0.1:{}: {e}", settings.mcp_port)))?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| AppError::Io(format!("mcp: set_nonblocking: {e}")))?;

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let port = settings.mcp_port;
    let ctx = Arc::new(Ctx { app_handle: app_handle.clone(), token: tok.clone() });

    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(std_listener) {
            Ok(l) => l,
            Err(e) => {
                log::warn!("mcp: listener conversion failed: {e}");
                return;
            }
        };
        let router = Router::new()
            .route(
                "/mcp",
                post(handle_post).get(|| async { StatusCode::METHOD_NOT_ALLOWED }),
            )
            .with_state(ctx);
        log::info!("mcp: listening on http://127.0.0.1:{port}/mcp");
        let serve = axum::serve(listener, router).with_graceful_shutdown(async {
            rx.await.ok();
        });
        if let Err(e) = serve.await {
            log::warn!("mcp: server error: {e}");
        }
        log::info!("mcp: stopped");
    });

    *running = Some(Running { port, token: tok, shutdown: tx });
    Ok(())
}

/// True if `port` can host the MCP listener: either the listener is already
/// bound to it, or a fresh loopback bind succeeds. Advisory only; the
/// authoritative bind is in `apply_mcp_state`.
pub fn mcp_port_available_impl<R: tauri::Runtime>(app_handle: &tauri::AppHandle<R>, port: u16) -> bool {
    let handle = app_handle.state::<McpHandle>();
    if handle.inner.lock().unwrap().as_ref().map(|r| r.port) == Some(port) {
        return true; // already ours
    }
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

async fn handle_post<R: tauri::Runtime>(
    State(ctx): State<Arc<Ctx<R>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&headers, &ctx.token) {
        return (StatusCode::UNAUTHORIZED, "missing or wrong bearer token").into_response();
    }
    // Tool calls do blocking fs + embedding work; they run on a blocking
    // thread, off the async workers.
    let host = AppHost { app_handle: ctx.app_handle.clone() };
    let bytes = body.to_vec();
    match tauri::async_runtime::spawn_blocking(move || protocol::handle_message(&host, &bytes))
        .await
    {
        Ok(Some(resp)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            resp.to_string(),
        )
            .into_response(),
        Ok(None) => StatusCode::ACCEPTED.into_response(), // notification
        Err(e) => {
            log::warn!("mcp: handler panic: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn authorized(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|presented| token::token_matches(expected, presented))
}

pub fn mcp_status_impl<R: tauri::Runtime>(app_handle: &tauri::AppHandle<R>) -> Result<McpStatus> {
    let state = app_handle.state::<AppState>();
    let settings = state.machine_settings();
    let handle = app_handle.state::<McpHandle>();
    let running = handle.inner.lock().unwrap().is_some();
    // The token is empty when disabled or when the vault is locked.
    let token = if settings.mcp_enabled {
        vault_token(app_handle)?.unwrap_or_default()
    } else {
        String::new()
    };
    let endpoint = format!("http://127.0.0.1:{}/mcp", settings.mcp_port);
    let claude_command = if token.is_empty() {
        String::new()
    } else {
        format!(
            "claude mcp add --transport http daisy {endpoint} --header \"Authorization: Bearer {token}\""
        )
    };
    Ok(McpStatus {
        enabled: settings.mcp_enabled,
        running,
        port: settings.mcp_port,
        token,
        endpoint,
        claude_command,
    })
}

/// Rotates the token in the vault and restarts the listener. Requires an
/// unlocked vault. Previously configured clients stop working.
pub fn mcp_regenerate_token_impl<R: tauri::Runtime>(app_handle: &tauri::AppHandle<R>) -> Result<McpStatus> {
    let app = app_handle.state::<AppState>();
    let vault = app_handle.state::<VaultState>();
    {
        let mut guard = vault.keys.lock().unwrap();
        let keys = guard.as_mut().ok_or(AppError::VaultLocked)?;
        keys.mcp_token = Some(token::generate());
    }
    crate::commands::lifecycle::re_encrypt_keys(&app, &vault)?;
    // The listener holding the previous token restarts on the new one.
    apply_mcp_state(app_handle)?;
    mcp_status_impl(app_handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_header_paths() {
        let mut h = HeaderMap::new();
        assert!(!authorized(&h, "tok"), "missing header");
        h.insert(header::AUTHORIZATION, "Bearer tok".parse().unwrap());
        assert!(authorized(&h, "tok"));
        h.insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
        assert!(!authorized(&h, "tok"));
        h.insert(header::AUTHORIZATION, "tok".parse().unwrap());
        assert!(!authorized(&h, "tok"), "no Bearer prefix");
    }
}
