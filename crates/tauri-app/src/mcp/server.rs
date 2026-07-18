//! Loopback HTTP listener for the MCP server (streamable-HTTP transport,
//! stateless). POST /mcp with `Authorization: Bearer <token>`; one JSON-RPC
//! message per request. GET (SSE streaming) answers 405.

use crate::error::{AppError, Result};
use crate::mcp::{protocol, token, tools::AppHost};
use crate::settings::Settings;
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
    shutdown: tokio::sync::oneshot::Sender<()>,
}

struct Ctx {
    app_handle: tauri::AppHandle,
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
fn vault_token(app_handle: &tauri::AppHandle) -> Result<Option<String>> {
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

/// Reconciles the listener with settings: starts when enabled+stopped, stops
/// when disabled+running, restarts when the port changed. Called from the
/// setup hook, after MCP settings changes, and on vault unlock.
pub fn apply_mcp_state(app_handle: &tauri::AppHandle) -> Result<()> {
    let state = app_handle.state::<AppState>();
    let settings = Settings::load_or_default(&state.profile.settings_path());
    let handle = app_handle.state::<McpHandle>();
    let mut running = handle.inner.lock().unwrap();

    let port_matches = running.as_ref().map(|r| r.port) == Some(settings.mcp_port);
    if settings.mcp_enabled && port_matches {
        return Ok(()); // already in the desired state
    }
    // Stop whatever is running (disabled, or port changed).
    if let Some(r) = running.take() {
        let _ = r.shutdown.send(());
    }
    if !settings.mcp_enabled {
        return Ok(());
    }

    let tok = match vault_token(app_handle)? {
        Some(t) => t,
        None => {
            // Vault locked — defer. The unlock path re-invokes apply_mcp_state.
            log::info!("mcp: enabled but vault locked; deferring start until unlock");
            return Ok(());
        }
    };
    // The bind happens synchronously; a port conflict surfaces to the caller
    // as an error.
    let std_listener = std::net::TcpListener::bind(("127.0.0.1", settings.mcp_port))
        .map_err(|e| AppError::Io(format!("mcp: bind 127.0.0.1:{}: {e}", settings.mcp_port)))?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| AppError::Io(format!("mcp: set_nonblocking: {e}")))?;

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let port = settings.mcp_port;
    let ctx = Arc::new(Ctx { app_handle: app_handle.clone(), token: tok });

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

    *running = Some(Running { port, shutdown: tx });
    Ok(())
}

/// True if `port` can host the MCP listener: either the listener is already
/// bound to it, or a fresh loopback bind succeeds. Advisory only; the
/// authoritative bind is in `apply_mcp_state`.
pub fn mcp_port_available_impl(app_handle: &tauri::AppHandle, port: u16) -> bool {
    let handle = app_handle.state::<McpHandle>();
    if handle.inner.lock().unwrap().as_ref().map(|r| r.port) == Some(port) {
        return true; // already ours
    }
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

async fn handle_post(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, body: Bytes) -> Response {
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

pub fn mcp_status_impl(app_handle: &tauri::AppHandle) -> Result<McpStatus> {
    let state = app_handle.state::<AppState>();
    let settings = Settings::load_or_default(&state.profile.settings_path());
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
pub fn mcp_regenerate_token_impl(app_handle: &tauri::AppHandle) -> Result<McpStatus> {
    let app = app_handle.state::<AppState>();
    let vault = app_handle.state::<VaultState>();
    {
        let mut guard = vault.keys.lock().unwrap();
        let keys = guard.as_mut().ok_or(AppError::VaultLocked)?;
        keys.mcp_token = Some(token::generate());
    }
    crate::commands::lifecycle::re_encrypt_keys(&app, &vault)?;
    // The old listener (holding the previous token in memory) is dropped,
    // then restarted.
    let handle = app_handle.state::<McpHandle>();
    if let Some(r) = handle.inner.lock().unwrap().take() {
        let _ = r.shutdown.send(());
    }
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
