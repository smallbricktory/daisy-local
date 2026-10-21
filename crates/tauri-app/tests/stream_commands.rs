// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The two streaming commands: `live_chat_send_stream` and `qa_ask_stream`.
//!
//! Both hand a prompt to an AI provider and surface the answer one delta at a
//! time. The provider here is a local HTTP server speaking the OpenAI SSE
//! shape, so the request the app builds, the stream it decodes, and what it
//! writes back to the session are all exercised.
use std::fs;
use std::sync::{Arc, Mutex};

use tauri_app_core::commands::live_chat::{live_chat_send_streaming_impl, LiveChatSendRequest};
use tauri_app_core::commands::qa::{qa_ask_streaming_impl, QaAskRequest};
use tauri_app_core::error::AppError;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, DecryptedKeys, ProviderConfig, ProviderId, VaultState};

const SESSION: &str = "daisy-1788200000";

/// A session directory with a finalized transcript, which is what the send
/// path seeds the first turn from when no live tail is supplied.
fn app() -> (AppState, VaultState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(dir.path().join("daisy")).unwrap());
    let root = app.profile.session_path(SESSION);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("transcript.md"),
        "[00:00:00] Me: the harbour survey came back short two soundings\n",
    )
    .unwrap();
    let vault = VaultState::default();
    *vault.keys.lock().unwrap() = Some(DecryptedKeys::default());
    (app, vault, dir)
}

/// A vault with no keys: every send must stop at the lock, not at the wire.
fn locked() -> (AppState, VaultState, tempfile::TempDir) {
    let (app, _v, dir) = app();
    (app, VaultState::default(), dir)
}

fn provider_at(vault: &VaultState, url: &str) {
    let mut guard = vault.keys.lock().unwrap();
    let keys = guard.as_mut().unwrap();
    keys.providers.insert(
        ProviderId::LmStudio,
        ProviderConfig {
            api_key: None,
            model: Some("qwen3.5-4b".into()),
            base_url: Some(format!("{url}/v1")),
            context_tokens: None,
        },
    );
}

/// An OpenAI-shaped SSE stream carrying `deltas`, then `[DONE]`.
fn sse_body(deltas: &[&str]) -> String {
    let mut s = String::new();
    for d in deltas {
        s.push_str(&format!(
            "data: {}\n\n",
            serde_json::json!({ "choices": [{ "delta": { "content": d } }] })
        ));
    }
    s.push_str("data: [DONE]\n\n");
    s
}

fn send_req(user_text: &str) -> LiveChatSendRequest {
    LiveChatSendRequest {
        session_id: SESSION.into(),
        user_text: user_text.into(),
        transcript_tail: String::new(),
        tail_end_ms: 0,
        provider: Some(ProviderId::LmStudio),
    }
}

// ---- guards ----------------------------------------------------------------

#[test]
fn an_empty_message_is_refused_before_the_vault_is_consulted() {
    // Locked vault AND blank text: the blank text is the answer, which pins
    // the order — no provider is resolved and no request is built.
    let (app, _v, dir) = locked();
    let vault = VaultState::default();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let e = rt
        .block_on(live_chat_send_streaming_impl(
            &app,
            &vault,
            send_req("   "),
            |_| {},
        ))
        .unwrap_err();
    assert!(
        matches!(&e, AppError::Config(m) if m == "message is empty"),
        "got {e:?}"
    );
    drop(dir);
}

#[test]
fn a_locked_vault_refuses_the_send() {
    let (app, vault, _d) = locked();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let e = rt
        .block_on(live_chat_send_streaming_impl(
            &app,
            &vault,
            send_req("what did we decide about the harbour?"),
            |_| {},
        ))
        .unwrap_err();
    assert!(matches!(e, AppError::VaultLocked), "got {e:?}");
}

#[test]
fn an_empty_question_is_refused_before_the_vault_is_consulted() {
    let (app, _v, dir) = locked();
    let vault = VaultState::default();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let e = rt
        .block_on(qa_ask_streaming_impl(
            &app,
            &vault,
            QaAskRequest { query: "  ".into(), provider: Some(ProviderId::LmStudio) },
            |_| {},
        ))
        .unwrap_err();
    assert!(
        matches!(&e, AppError::Config(m) if m == "query is empty"),
        "got {e:?}"
    );
    drop(dir);
}

#[test]
fn a_locked_vault_refuses_the_question() {
    let (app, vault, _d) = locked();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let e = rt
        .block_on(qa_ask_streaming_impl(
            &app,
            &vault,
            QaAskRequest {
                query: "what did we decide about the harbour?".into(),
                provider: Some(ProviderId::LmStudio),
            },
            |_| {},
        ))
        .unwrap_err();
    assert!(matches!(e, AppError::VaultLocked), "got {e:?}");
}

// ---- the stream ------------------------------------------------------------

#[test]
fn the_reply_arrives_delta_by_delta_and_the_whole_turn_is_persisted() {
    let (app, vault, _d) = app();
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_body(&["Two soundings", " are missing."]))
        .expect(1)
        .create();
    provider_at(&vault, &server.url());

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let reply = rt
        .block_on(live_chat_send_streaming_impl(
            &app,
            &vault,
            send_req("what came back short?"),
            move |tok| sink.lock().unwrap().push(tok.to_string()),
        ))
        .unwrap();

    mock.assert();
    // Each delta surfaced separately — the point of the streaming command.
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["Two soundings".to_string(), " are missing.".to_string()]
    );
    assert_eq!(reply.reply, "Two soundings are missing.");

    // Both turns are in the returned chat and on disk, in order.
    let roles: Vec<&str> = reply.chat.messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, vec!["user", "assistant"]);
    assert_eq!(reply.chat.messages[0].content, "what came back short?");
    assert_eq!(reply.chat.messages[1].content, "Two soundings are missing.");

    let on_disk = tauri_app_core::commands::live_chat::live_chat_load_impl(&app, SESSION).unwrap();
    assert_eq!(on_disk.messages.len(), 2);
    assert_eq!(on_disk.messages[1].content, "Two soundings are missing.");
}

#[test]
fn a_mid_stream_error_object_fails_the_send_and_writes_nothing() {
    let (app, vault, _d) = app();
    let mut server = mockito::Server::new();
    let _mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Two\"}}]}\n\n\
             data: {\"error\":{\"message\":\"context length exceeded\"}}\n\n",
        )
        .create();
    provider_at(&vault, &server.url());

    let rt = tokio::runtime::Runtime::new().unwrap();
    let e = rt
        .block_on(live_chat_send_streaming_impl(
            &app,
            &vault,
            send_req("what came back short?"),
            |_| {},
        ))
        .unwrap_err();
    assert!(
        format!("{e:?}").contains("context length exceeded"),
        "the provider's reason should survive: {e:?}"
    );
    // A failed send leaves no half-written turn behind.
    let on_disk = tauri_app_core::commands::live_chat::live_chat_load_impl(&app, SESSION).unwrap();
    assert!(
        on_disk.messages.is_empty(),
        "a failed send persisted a turn: {:?}",
        on_disk.messages
    );
}
