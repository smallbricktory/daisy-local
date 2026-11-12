// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Outbound destinations: the rows in Settings → Integrations, and the push
//! that sends a meeting to one.
//!
//! Two things matter beyond the CRUD: the secret a destination holds must
//! never come back out to the frontend, and a push has to say exactly what
//! went wrong when the far end refuses it. Pushes run against a local mock
//! server, which the URL rule allows over plain http.
use tauri_app_core::commands::integrations::{
    delete_integration_impl, integration_push_impl, list_integrations_impl,
    upsert_integration_impl, UpsertIntegration, WebhookAuthInput,
};
use tauri_app_core::commands::lifecycle::{init_vault_impl, lock_vault_impl, unlock_vault_impl};
use tauri_app_core::error::AppError;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, PayloadSelection, VaultState};

const PASS: &str = "correct-horse-battery-staple-extended-22";

fn unlocked() -> (AppState, VaultState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let app = AppState::new(profile);
    let vs = VaultState::new();
    init_vault_impl(&app, &vs, PASS).unwrap();
    (app, vs, dir)
}

fn all_payloads() -> PayloadSelection {
    PayloadSelection {
        summary: true,
        notes: true,
        transcript: true,
    }
}

fn new_dest(name: &str, url: &str, auth: WebhookAuthInput) -> UpsertIntegration {
    UpsertIntegration {
        id: None,
        name: name.into(),
        enabled: true,
        webhook_url: url.into(),
        auth,
        payloads: all_payloads(),
    }
}

/// A finished meeting with all three text payloads on disk.
fn synth_session(app: &AppState, id: &str) -> std::path::PathBuf {
    let dir = app.profile.session_path(id);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = serde_json::json!({
        "schema_version": 4,
        "session_id": id,
        "created_at_unix_seconds": 1_700_000_000i64,
        "sample_rate": 16000,
        "channels": 1,
        "mic_source_id": 1,
        "mic_source_node_name": "m",
        "mic_source_description": "M",
        "system_source_id": 2,
        "system_source_node_name": "s",
        "system_source_description": "S",
        "aec_mode": "disabled",
        "chunks": [{
            "index": 1,
            "started_at_unix_seconds": 1_700_000_000i64,
            "ended_at_unix_seconds": 1_700_000_120i64,
            "duration_seconds": 120,
            "mic_wav_relative": "chunks/0001/mic.wav",
            "system_wav_relative": "chunks/0001/system.wav",
            "mic_aec_wav_relative": null,
        }],
        "finalized_at_unix_seconds": 1_700_000_130i64,
        "title": "Northwind kickoff",
        "meeting_id": "11111111-2222-4333-8444-555555555555",
        "tag_ids": ["tag-nwnd"],
        "attendees": [{ "display_name": "Rowan Hale", "role": "self" }],
    });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("summary.md"), "## Decisions\n\n- ship it\n").unwrap();
    std::fs::write(dir.join("notes.md"), "my own notes\n").unwrap();
    std::fs::write(dir.join("transcript.md"), "[00:00:00] **Rowan**: hi\n").unwrap();
    dir
}

// ---- the destination list --------------------------------------------------

#[test]
fn there_are_no_destinations_until_one_is_added() {
    let (_app, vs, _d) = unlocked();
    assert!(list_integrations_impl(&vs).unwrap().is_empty());
}

#[test]
fn a_destination_round_trips_through_a_locked_vault() {
    let (app, vs, _d) = unlocked();
    let made = upsert_integration_impl(
        &app,
        &vs,
        new_dest(
            "Northwind hooks",
            "https://hooks.example.org/daisy",
            WebhookAuthInput::Bearer {
                token: "s3cret-token".into(),
            },
        ),
    )
    .unwrap();
    assert_eq!(made.name, "Northwind hooks");
    assert_eq!(made.kind, "webhook");
    assert_eq!(made.auth_kind, "bearer");
    assert!(made.enabled);

    lock_vault_impl(&app, &vs).unwrap();
    unlock_vault_impl(&app, &vs, PASS).unwrap();
    let back = list_integrations_impl(&vs).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].id, made.id);
    assert_eq!(back[0].webhook_url.as_deref(), Some("https://hooks.example.org/daisy"));
}

#[test]
fn the_secret_never_comes_back_to_the_frontend() {
    let (app, vs, _d) = unlocked();
    let bearer = upsert_integration_impl(
        &app,
        &vs,
        new_dest(
            "Bearer dest",
            "https://hooks.example.org/a",
            WebhookAuthInput::Bearer {
                token: "token-must-not-leak".into(),
            },
        ),
    )
    .unwrap();
    let header = upsert_integration_impl(
        &app,
        &vs,
        new_dest(
            "Header dest",
            "https://hooks.example.org/b",
            WebhookAuthInput::Header {
                name: "X-API-Key".into(),
                value: "value-must-not-leak".into(),
            },
        ),
    )
    .unwrap();

    let as_json = serde_json::to_string(&list_integrations_impl(&vs).unwrap()).unwrap();
    assert!(!as_json.contains("token-must-not-leak"));
    assert!(!as_json.contains("value-must-not-leak"));
    assert!(
        as_json.contains("X-API-Key"),
        "the header NAME is shown so the row can describe itself"
    );
    assert_eq!(bearer.auth_header_name, None);
    assert_eq!(header.auth_header_name.as_deref(), Some("X-API-Key"));
}

#[test]
fn editing_a_destination_without_retyping_the_secret_keeps_it() {
    let (app, vs, _d) = unlocked();
    let made = upsert_integration_impl(
        &app,
        &vs,
        new_dest(
            "Northwind hooks",
            "https://hooks.example.org/daisy",
            WebhookAuthInput::Bearer {
                token: "keep-me".into(),
            },
        ),
    )
    .unwrap();

    let edited = upsert_integration_impl(
        &app,
        &vs,
        UpsertIntegration {
            id: Some(made.id.clone()),
            name: "Northwind hooks (prod)".into(),
            enabled: false,
            webhook_url: "https://hooks.example.org/prod".into(),
            auth: WebhookAuthInput::Keep,
            payloads: PayloadSelection {
                summary: true,
                notes: false,
                transcript: false,
            },
        },
    )
    .unwrap();

    assert_eq!(edited.id, made.id, "editing does not mint a new destination");
    assert_eq!(edited.name, "Northwind hooks (prod)");
    assert!(!edited.enabled);
    assert_eq!(edited.auth_kind, "bearer", "the stored token is still there");
    assert!(!edited.payloads.notes);
    assert_eq!(list_integrations_impl(&vs).unwrap().len(), 1);
}

#[test]
fn clearing_the_auth_leaves_a_destination_with_none() {
    let (app, vs, _d) = unlocked();
    let made = upsert_integration_impl(
        &app,
        &vs,
        new_dest(
            "Northwind hooks",
            "https://hooks.example.org/daisy",
            WebhookAuthInput::Bearer {
                token: "drop-me".into(),
            },
        ),
    )
    .unwrap();
    let edited = upsert_integration_impl(
        &app,
        &vs,
        UpsertIntegration {
            id: Some(made.id),
            name: "Northwind hooks".into(),
            enabled: true,
            webhook_url: "https://hooks.example.org/daisy".into(),
            auth: WebhookAuthInput::None,
            payloads: all_payloads(),
        },
    )
    .unwrap();
    assert_eq!(edited.auth_kind, "none");
}

#[test]
fn a_destination_needs_a_name_and_a_url_that_can_be_trusted() {
    let (app, vs, _d) = unlocked();
    for name in ["", "   "] {
        assert!(
            upsert_integration_impl(
                &app,
                &vs,
                new_dest(name, "https://hooks.example.org/a", WebhookAuthInput::None),
            )
            .is_err(),
            "{name:?} is not a name"
        );
    }
    for url in [
        "",
        "  ",
        "http://hooks.example.org/a",
        "ftp://hooks.example.org/a",
        "hooks.example.org/a",
        "javascript:alert(1)",
        "https://hooks.example.org/a ", // trailing space is not trimmed away first
    ] {
        let refused = upsert_integration_impl(
            &app,
            &vs,
            new_dest("Dest", url, WebhookAuthInput::None),
        );
        if url.trim_end() == "https://hooks.example.org/a" && !url.is_empty() {
            // A trailing space still points at https, so it is accepted.
            assert!(refused.is_ok());
        } else {
            assert!(refused.is_err(), "{url:?} must not be accepted");
        }
    }
    // Plain http is allowed only for a service on this machine.
    assert!(upsert_integration_impl(
        &app,
        &vs,
        new_dest("Local", "http://localhost:9999/hook", WebhookAuthInput::None),
    )
    .is_ok());
    assert!(upsert_integration_impl(
        &app,
        &vs,
        new_dest("Loopback", "http://127.0.0.1:9999/hook", WebhookAuthInput::None),
    )
    .is_ok());
}

#[test]
fn an_auth_kind_with_nothing_filled_in_is_refused() {
    let (app, vs, _d) = unlocked();
    assert!(upsert_integration_impl(
        &app,
        &vs,
        new_dest(
            "Dest",
            "https://hooks.example.org/a",
            WebhookAuthInput::Bearer { token: "  ".into() },
        ),
    )
    .is_err());
    assert!(upsert_integration_impl(
        &app,
        &vs,
        new_dest(
            "Dest",
            "https://hooks.example.org/a",
            WebhookAuthInput::Header {
                name: " ".into(),
                value: "v".into(),
            },
        ),
    )
    .is_err());
    assert!(
        list_integrations_impl(&vs).unwrap().is_empty(),
        "a refused destination is not half-created"
    );
}

#[test]
fn deleting_a_destination_removes_only_that_one() {
    let (app, vs, _d) = unlocked();
    let a = upsert_integration_impl(
        &app,
        &vs,
        new_dest("A", "https://hooks.example.org/a", WebhookAuthInput::None),
    )
    .unwrap();
    upsert_integration_impl(
        &app,
        &vs,
        new_dest("B", "https://hooks.example.org/b", WebhookAuthInput::None),
    )
    .unwrap();

    delete_integration_impl(&app, &vs, &a.id).unwrap();
    let left = list_integrations_impl(&vs).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].name, "B");

    assert!(delete_integration_impl(&app, &vs, &a.id).is_err());
    assert!(delete_integration_impl(&app, &vs, "no-such-id").is_err());

    lock_vault_impl(&app, &vs).unwrap();
    unlock_vault_impl(&app, &vs, PASS).unwrap();
    assert_eq!(list_integrations_impl(&vs).unwrap().len(), 1);
}

#[test]
fn a_locked_vault_hides_the_destinations_and_refuses_every_change() {
    let (app, vs, _d) = unlocked();
    let made = upsert_integration_impl(
        &app,
        &vs,
        new_dest("A", "https://hooks.example.org/a", WebhookAuthInput::None),
    )
    .unwrap();
    lock_vault_impl(&app, &vs).unwrap();

    assert!(matches!(
        list_integrations_impl(&vs),
        Err(AppError::VaultLocked)
    ));
    assert!(matches!(
        delete_integration_impl(&app, &vs, &made.id),
        Err(AppError::VaultLocked)
    ));
    assert!(matches!(
        upsert_integration_impl(
            &app,
            &vs,
            new_dest("B", "https://hooks.example.org/b", WebhookAuthInput::None),
        ),
        Err(AppError::VaultLocked)
    ));
}

// ---- the push --------------------------------------------------------------

#[test]
fn a_push_sends_the_meeting_and_the_payloads_that_were_selected() {
    let mut server = mockito::Server::new();
    let hook = server
        .mock("POST", "/hook")
        .match_header("content-type", "application/json")
        .with_status(200)
        .with_body("ok")
        .create();

    let (app, vs, _d) = unlocked();
    synth_session(&app, "daisy-1");
    let dest = upsert_integration_impl(
        &app,
        &vs,
        new_dest(
            "Local",
            &format!("{}/hook", server.url()),
            WebhookAuthInput::None,
        ),
    )
    .unwrap();

    integration_push_impl(&app, &vs, "daisy-1", &dest.id, false).unwrap();
    hook.assert();
}

#[test]
fn the_body_carries_the_meeting_and_the_three_texts() {
    let mut server = mockito::Server::new();
    let hook = server
        .mock("POST", "/hook")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "source": "daisy",
            "meeting": {
                "session_id": "daisy-1",
                "title": "Northwind kickoff",
                "started_at_unix_seconds": 1_700_000_000i64,
                "duration_seconds": 120,
                "tag_ids": ["tag-nwnd"],
            },
            "summary": "## Decisions\n\n- ship it\n",
            "notes": "my own notes\n",
            "transcript": "[00:00:00] **Rowan**: hi\n",
        })))
        .with_status(200)
        .create();

    let (app, vs, _d) = unlocked();
    synth_session(&app, "daisy-1");
    let dest = upsert_integration_impl(
        &app,
        &vs,
        new_dest("Local", &format!("{}/hook", server.url()), WebhookAuthInput::None),
    )
    .unwrap();
    integration_push_impl(&app, &vs, "daisy-1", &dest.id, false).unwrap();
    hook.assert();
}

#[test]
fn a_payload_the_user_turned_off_is_not_sent() {
    let mut server = mockito::Server::new();
    let hook = server
        .mock("POST", "/hook")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "summary": "## Decisions\n\n- ship it\n",
            "transcript": null,
            "notes": null,
        })))
        .with_status(200)
        .create();

    let (app, vs, _d) = unlocked();
    synth_session(&app, "daisy-1");
    let dest = upsert_integration_impl(
        &app,
        &vs,
        UpsertIntegration {
            id: None,
            name: "Summary only".into(),
            enabled: true,
            webhook_url: format!("{}/hook", server.url()),
            auth: WebhookAuthInput::None,
            payloads: PayloadSelection {
                summary: true,
                notes: false,
                transcript: false,
            },
        },
    )
    .unwrap();
    integration_push_impl(&app, &vs, "daisy-1", &dest.id, false).unwrap();
    hook.assert();
}

#[test]
fn the_stored_auth_is_what_goes_on_the_wire() {
    let mut server = mockito::Server::new();
    let bearer = server
        .mock("POST", "/bearer")
        .match_header("authorization", "Bearer s3cret-token")
        .with_status(200)
        .create();
    let header = server
        .mock("POST", "/header")
        .match_header("x-api-key", "s3cret-value")
        .with_status(200)
        .create();

    let (app, vs, _d) = unlocked();
    synth_session(&app, "daisy-1");
    let b = upsert_integration_impl(
        &app,
        &vs,
        new_dest(
            "Bearer",
            &format!("{}/bearer", server.url()),
            WebhookAuthInput::Bearer {
                token: "s3cret-token".into(),
            },
        ),
    )
    .unwrap();
    let h = upsert_integration_impl(
        &app,
        &vs,
        new_dest(
            "Header",
            &format!("{}/header", server.url()),
            WebhookAuthInput::Header {
                name: "X-API-Key".into(),
                value: "s3cret-value".into(),
            },
        ),
    )
    .unwrap();

    integration_push_impl(&app, &vs, "daisy-1", &b.id, false).unwrap();
    integration_push_impl(&app, &vs, "daisy-1", &h.id, false).unwrap();
    bearer.assert();
    header.assert();
}

#[test]
fn a_refusal_from_the_far_end_is_reported_with_its_reason() {
    let mut server = mockito::Server::new();
    let hook = server
        .mock("POST", "/hook")
        .with_status(403)
        .with_body("workspace token revoked")
        .create();

    let (app, vs, _d) = unlocked();
    synth_session(&app, "daisy-1");
    let dest = upsert_integration_impl(
        &app,
        &vs,
        new_dest("Local", &format!("{}/hook", server.url()), WebhookAuthInput::None),
    )
    .unwrap();

    let e = integration_push_impl(&app, &vs, "daisy-1", &dest.id, false).unwrap_err();
    let msg = format!("{e}");
    assert!(msg.contains("403"), "the status is in the message: {msg}");
    assert!(
        msg.contains("workspace token revoked"),
        "and so is what the far end said: {msg}"
    );
    hook.assert();
}

#[test]
fn a_server_error_is_retried_once_before_it_is_reported() {
    let mut server = mockito::Server::new();
    let failing = server
        .mock("POST", "/hook")
        .with_status(503)
        .expect(2)
        .create();

    let (app, vs, _d) = unlocked();
    synth_session(&app, "daisy-1");
    let dest = upsert_integration_impl(
        &app,
        &vs,
        new_dest("Local", &format!("{}/hook", server.url()), WebhookAuthInput::None),
    )
    .unwrap();

    assert!(integration_push_impl(&app, &vs, "daisy-1", &dest.id, false).is_err());
    failing.assert();
}

#[test]
fn a_second_try_that_succeeds_is_a_success() {
    let mut server = mockito::Server::new();
    let first = server.mock("POST", "/hook").with_status(429).expect(1).create();
    let (app, vs, _d) = unlocked();
    synth_session(&app, "daisy-1");
    let dest = upsert_integration_impl(
        &app,
        &vs,
        new_dest("Local", &format!("{}/hook", server.url()), WebhookAuthInput::None),
    )
    .unwrap();

    // The first mock answers 429 once; the retry finds this one.
    let second = server.mock("POST", "/hook").with_status(200).expect(1).create();
    integration_push_impl(&app, &vs, "daisy-1", &dest.id, false).unwrap();
    first.assert();
    second.assert();
}

#[test]
fn pushing_a_session_that_is_not_there_is_refused_before_any_request() {
    let mut server = mockito::Server::new();
    let hook = server.mock("POST", "/hook").expect(0).create();

    let (app, vs, _d) = unlocked();
    let dest = upsert_integration_impl(
        &app,
        &vs,
        new_dest("Local", &format!("{}/hook", server.url()), WebhookAuthInput::None),
    )
    .unwrap();

    assert!(matches!(
        integration_push_impl(&app, &vs, "daisy-missing", &dest.id, false),
        Err(AppError::SessionNotFound(_))
    ));
    hook.assert();
}

#[test]
fn pushing_to_a_destination_that_is_not_there_is_refused() {
    let (app, vs, _d) = unlocked();
    synth_session(&app, "daisy-1");
    assert!(integration_push_impl(&app, &vs, "daisy-1", "no-such-dest", false).is_err());
}

#[test]
fn a_locked_vault_cannot_push() {
    let (app, vs, _d) = unlocked();
    synth_session(&app, "daisy-1");
    let dest = upsert_integration_impl(
        &app,
        &vs,
        new_dest("Local", "https://hooks.example.org/a", WebhookAuthInput::None),
    )
    .unwrap();
    lock_vault_impl(&app, &vs).unwrap();
    assert!(matches!(
        integration_push_impl(&app, &vs, "daisy-1", &dest.id, false),
        Err(AppError::VaultLocked)
    ));
}

#[test]
fn a_meeting_with_none_of_the_selected_texts_still_sends_the_meeting() {
    let mut server = mockito::Server::new();
    let hook = server
        .mock("POST", "/hook")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "meeting": { "session_id": "daisy-1" },
            "summary": null,
            "notes": null,
            "transcript": null,
        })))
        .with_status(200)
        .create();

    let (app, vs, _d) = unlocked();
    let dir = synth_session(&app, "daisy-1");
    for f in ["summary.md", "notes.md", "transcript.md"] {
        std::fs::write(dir.join(f), "   \n").unwrap();
    }
    let dest = upsert_integration_impl(
        &app,
        &vs,
        new_dest("Local", &format!("{}/hook", server.url()), WebhookAuthInput::None),
    )
    .unwrap();
    integration_push_impl(&app, &vs, "daisy-1", &dest.id, false).unwrap();
    hook.assert();
}
