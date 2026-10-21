// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Outbound integrations: manage destinations (stored in the vault) and push a
//! meeting's selected payloads to one. Webhook destinations only: a single
//! JSON POST.

use crate::commands::history::{append_history, HistoryEntry};
use crate::commands::lifecycle::re_encrypt_keys;
use crate::error::{AppError, Result};
use crate::state::{
    AppState, Integration, IntegrationKind, PayloadSelection, VaultState, WebhookAuth,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

// ---- redacted views for the frontend ----------------------------------------

#[derive(Debug, Serialize)]
pub struct IntegrationPublic {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// Destination kind, e.g. "webhook".
    pub kind: String,
    pub webhook_url: Option<String>,
    /// "none" | "header" | "bearer".
    pub auth_kind: String,
    /// The header NAME (never the value) — set only for the "header" auth kind.
    pub auth_header_name: Option<String>,
    pub payloads: PayloadSelection,
}

fn to_public(i: &Integration) -> IntegrationPublic {
    match &i.kind {
        IntegrationKind::Webhook { url, auth } => {
            let (auth_kind, auth_header_name) = match auth {
                WebhookAuth::None => ("none".to_string(), None),
                WebhookAuth::Header { name, .. } => ("header".to_string(), Some(name.clone())),
                WebhookAuth::Bearer { .. } => ("bearer".to_string(), None),
            };
            IntegrationPublic {
                id: i.id.clone(),
                name: i.name.clone(),
                enabled: i.enabled,
                kind: "webhook".to_string(),
                webhook_url: Some(url.clone()),
                auth_kind,
                auth_header_name,
                payloads: i.payloads,
            }
        }
    }
}

// ---- request shapes ----------------------------------------------------------

/// Auth as the frontend sends it. `Keep` = leave the stored auth unchanged.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebhookAuthInput {
    Keep,
    None,
    Header { name: String, value: String },
    Bearer { token: String },
}

#[derive(Debug, Deserialize)]
pub struct UpsertIntegration {
    /// `Some` = update that destination; `None` = create a new one.
    pub id: Option<String>,
    pub name: String,
    pub enabled: bool,
    pub webhook_url: String,
    pub auth: WebhookAuthInput,
    pub payloads: PayloadSelection,
}

// ---- vault CRUD --------------------------------------------------------------

fn validate_url(url: &str) -> Result<()> {
    let u = url.trim();
    if u.is_empty() {
        return Err(AppError::Config("webhook URL is empty".into()));
    }
    let https = u.starts_with("https://");
    let local = u.starts_with("http://localhost") || u.starts_with("http://127.0.0.1");
    if !https && !local {
        return Err(AppError::Config(
            "webhook URL must be https:// (http:// is only allowed for localhost / 127.0.0.1)"
                .into(),
        ));
    }
    Ok(())
}

fn resolve_auth(input: &WebhookAuthInput, existing: Option<&WebhookAuth>) -> Result<WebhookAuth> {
    Ok(match input {
        WebhookAuthInput::Keep => existing.cloned().unwrap_or(WebhookAuth::None),
        WebhookAuthInput::None => WebhookAuth::None,
        WebhookAuthInput::Header { name, value } => {
            if name.trim().is_empty() {
                return Err(AppError::Config("auth header name is empty".into()));
            }
            WebhookAuth::Header {
                name: name.trim().to_string(),
                value: value.clone(),
            }
        }
        WebhookAuthInput::Bearer { token } => {
            if token.trim().is_empty() {
                return Err(AppError::Config("bearer token is empty".into()));
            }
            WebhookAuth::Bearer {
                token: token.clone(),
            }
        }
    })
}

pub fn list_integrations_impl(vs: &VaultState) -> Result<Vec<IntegrationPublic>> {
    let guard = vs.keys.lock().unwrap();
    let keys = guard
        .as_ref()
        .ok_or(AppError::VaultLocked)?;
    Ok(keys.integrations.iter().map(to_public).collect())
}

pub fn upsert_integration_impl(
    app: &AppState,
    vs: &VaultState,
    req: UpsertIntegration,
) -> Result<IntegrationPublic> {
    if req.name.trim().is_empty() {
        return Err(AppError::Config("destination name is empty".into()));
    }
    validate_url(&req.webhook_url)?;
    let result;
    {
        let mut guard = vs.keys.lock().unwrap();
        let keys = guard
            .as_mut()
            .ok_or(AppError::VaultLocked)?;
        match &req.id {
            Some(id) => {
                let existing_auth = keys.integrations.iter().find(|i| &i.id == id).map(|i| {
                    match &i.kind {
                        IntegrationKind::Webhook { auth, .. } => auth.clone(),
                    }
                });
                let auth = resolve_auth(&req.auth, existing_auth.as_ref())?;
                let it = keys
                    .integrations
                    .iter_mut()
                    .find(|i| &i.id == id)
                    .ok_or_else(|| AppError::Config(format!("no such destination: {id}")))?;
                it.name = req.name.trim().to_string();
                it.enabled = req.enabled;
                it.kind = IntegrationKind::Webhook {
                    url: req.webhook_url.trim().to_string(),
                    auth,
                };
                it.payloads = req.payloads;
                result = to_public(it);
            }
            None => {
                let auth = resolve_auth(&req.auth, None)?;
                let it = Integration {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: req.name.trim().to_string(),
                    enabled: req.enabled,
                    kind: IntegrationKind::Webhook {
                        url: req.webhook_url.trim().to_string(),
                        auth,
                    },
                    payloads: req.payloads,
                };
                result = to_public(&it);
                keys.integrations.push(it);
            }
        }
    }
    re_encrypt_keys(app, vs)?;
    Ok(result)
}

pub fn delete_integration_impl(app: &AppState, vs: &VaultState, id: &str) -> Result<()> {
    {
        let mut guard = vs.keys.lock().unwrap();
        let keys = guard
            .as_mut()
            .ok_or(AppError::VaultLocked)?;
        let before = keys.integrations.len();
        keys.integrations.retain(|i| i.id != id);
        if keys.integrations.len() == before {
            return Err(AppError::Config(format!("no such destination: {id}")));
        }
    }
    re_encrypt_keys(app, vs)
}

// ---- push --------------------------------------------------------------------

fn read_nonempty(path: &Path) -> Option<String> {
    syncsafe::read_to_string(path)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Builds the JSON body and reports which payload keys it actually included.
fn build_payload(
    session_dir: &Path,
    sel: &PayloadSelection,
) -> Result<(serde_json::Value, Vec<String>)> {
    #[derive(serde::Deserialize)]
    struct Att {
        display_name: String,
        #[serde(default)]
        role: serde_json::Value,
    }
    #[derive(serde::Deserialize)]
    struct C {
        #[serde(default, deserialize_with = "recording::manifest::whole_seconds")]
        duration_seconds: Option<u64>,
        /// One chunk per index counts, the first, as a full manifest read
        /// keeps.
        #[serde(default)]
        index: Option<u32>,
    }
    #[derive(serde::Deserialize)]
    struct M {
        session_id: String,
        created_at_unix_seconds: i64,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        meeting_id: String,
        #[serde(default)]
        tag_ids: Vec<String>,
        #[serde(default)]
        attendees: Vec<Att>,
        #[serde(default)]
        chunks: Vec<C>,
    }
    let m: M = serde_json::from_slice(&syncsafe::read(session_dir.join("manifest.json"))?)?;
    let mut seen = std::collections::HashSet::new();
    let dur: u64 = m
        .chunks
        .iter()
        .filter(|c| c.index.is_none_or(|i| seen.insert(i)))
        .filter_map(|c| c.duration_seconds)
        .sum();

    let mut included: Vec<String> = Vec::new();
    let mut take = |on: bool, key: &str, file: &str| -> Option<String> {
        if !on {
            return None;
        }
        let v = read_nonempty(&session_dir.join(file));
        if v.is_some() {
            included.push(key.to_string());
        }
        v
    };
    let summary = take(sel.summary, "summary", "summary.md");
    let notes = take(sel.notes, "notes", "notes.md");
    let transcript = take(sel.transcript, "transcript", "transcript.md");

    let attendees: Vec<serde_json::Value> = m
        .attendees
        .iter()
        .map(|a| serde_json::json!({ "display_name": a.display_name, "role": a.role }))
        .collect();

    let payload = serde_json::json!({
        "source": "daisy",
        "meeting": {
            "id": m.meeting_id,
            "session_id": m.session_id,
            "title": m.title,
            "started_at_unix_seconds": m.created_at_unix_seconds,
            "duration_seconds": if dur > 0 { Some(dur) } else { None },
            "tag_ids": m.tag_ids,
            "attendees": attendees,
        },
        "summary": summary,
        "notes": notes,
        "transcript": transcript,
    });
    Ok((payload, included))
}

pub(crate) fn push_webhook(url: &str, auth: &WebhookAuth, body: &serde_json::Value) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::Provider(format!("http client: {e}")))?;
    for attempt in 1..=2 {
        let mut rb = client.post(url).json(body);
        match auth {
            WebhookAuth::None => {}
            WebhookAuth::Header { name, value } => rb = rb.header(name.as_str(), value.as_str()),
            WebhookAuth::Bearer { token } => rb = rb.bearer_auth(token),
        }
        let resp = rb
            .send()
            .map_err(|e| AppError::Provider(format!("POST {url}: {e}")))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        if attempt == 1 && (status.as_u16() == 429 || status.is_server_error()) {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        let body_txt = resp.text().unwrap_or_default();
        let snippet: String = body_txt.chars().take(300).collect();
        return Err(AppError::Provider(format!(
            "webhook returned {status}: {snippet}"
        )));
    }
    unreachable!()
}

use crate::now_unix;

/// Clone a destination out of the vault; the lock is released before return.
pub(crate) fn get_integration(vs: &VaultState, integration_id: &str) -> Result<Integration> {
    let guard = vs.keys.lock().unwrap();
    let keys = guard.as_ref().ok_or(AppError::VaultLocked)?;
    keys.integrations
        .iter()
        .find(|i| i.id == integration_id)
        .cloned()
        .ok_or_else(|| AppError::Config(format!("no such destination: {integration_id}")))
}

/// When `log_history` is false the push is not recorded in the
/// integration history store.
pub fn integration_push_impl(
    app: &AppState,
    vs: &VaultState,
    session_id: &str,
    integration_id: &str,
    log_history: bool,
) -> Result<()> {
    let integration: Integration = get_integration(vs, integration_id)?;
    let session_dir = app.profile.session_path(session_id);
    if !session_dir.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    let (payload, included) = build_payload(&session_dir, &integration.payloads)?;
    let (kind_str, push_result): (&str, Result<()>) = match &integration.kind {
        IntegrationKind::Webhook { url, auth } => ("webhook", push_webhook(url, auth, &payload)),
    };

    let meeting = payload.get("meeting");
    let meeting_id = meeting
        .and_then(|m| m.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let meeting_title = meeting
        .and_then(|m| m.get("title"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let status = match &push_result {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("error: {e}"),
    };
    if push_result.is_ok() {
        let _ = crate::commands::meeting::mark_sent_to_integration(app, session_id, &integration.id);
    }
    if log_history {
        let _ = append_history(
            app,
            HistoryEntry {
                at_unix_seconds: now_unix(),
                session_id: session_id.to_string(),
                meeting_id,
                meeting_title,
                integration_id: integration.id.clone(),
                integration_name: integration.name.clone(),
                kind: kind_str.to_string(),
                payloads_sent: included,
                status,
            },
        );
    }
    push_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{PayloadSelection, WebhookAuth};

    const SESSION: &str = "daisy-1788200000";

    /// A finished session on disk: a manifest the builder reads every field of,
    /// and all three optional bodies present so a selection is what decides
    /// which of them travel.
    fn session_dir(td: &tempfile::TempDir) -> std::path::PathBuf {
        let dir = td.path().join(SESSION);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = serde_json::json!({
            "schema_version": 4,
            "session_id": SESSION,
            "created_at_unix_seconds": 1_788_200_000i64,
            "title": "Harbour survey review",
            "meeting_id": "mtg-4417",
            "sample_rate": 16_000,
            "channels": 1,
            "tag_ids": ["tag-harbour", "tag-q3"],
            "attendees": [
                { "display_name": "Rowan Vance", "role": "organizer" },
                { "display_name": "Zoe Okonkwo" },
            ],
            "chunks": [
                { "index": 0, "duration_seconds": 90 },
                { "index": 1, "duration_seconds": 30 },
            ],
        });
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::write(dir.join("summary.md"), "Two soundings are missing.").unwrap();
        std::fs::write(dir.join("notes.md"), "Ask the surveyor.").unwrap();
        std::fs::write(dir.join("transcript.md"), "[00:00:00] Rowan: the survey\n").unwrap();
        dir
    }

    fn sel(summary: bool, notes: bool, transcript: bool) -> PayloadSelection {
        PayloadSelection { summary, notes, transcript }
    }

    // ---- build_payload -----------------------------------------------------

    #[test]
    fn the_meeting_block_carries_the_manifest_and_the_summed_duration() {
        let td = tempfile::tempdir().unwrap();
        let (p, _) = build_payload(&session_dir(&td), &sel(false, false, false)).unwrap();

        assert_eq!(p["source"], "daisy");
        let m = &p["meeting"];
        assert_eq!(m["id"], "mtg-4417");
        assert_eq!(m["session_id"], SESSION);
        assert_eq!(m["title"], "Harbour survey review");
        assert_eq!(m["started_at_unix_seconds"], 1_788_200_000i64);
        // Every chunk's duration, not the first one's.
        assert_eq!(m["duration_seconds"], 120);
        assert_eq!(m["tag_ids"], serde_json::json!(["tag-harbour", "tag-q3"]));
        assert_eq!(m["attendees"][0]["display_name"], "Rowan Vance");
        assert_eq!(m["attendees"][0]["role"], "organizer");
        // An attendee with no role keeps the key, carrying null.
        assert_eq!(m["attendees"][1]["display_name"], "Zoe Okonkwo");
        assert!(m["attendees"][1]["role"].is_null());
    }

    #[test]
    fn a_selection_decides_which_bodies_travel() {
        let td = tempfile::tempdir().unwrap();
        let dir = session_dir(&td);

        for (s, n, t) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (false, false, true),
            (true, true, true),
        ] {
            let (p, included) = build_payload(&dir, &sel(s, n, t)).unwrap();
            assert_eq!(p["summary"].is_string(), s, "summary for {s}/{n}/{t}");
            assert_eq!(p["notes"].is_string(), n, "notes for {s}/{n}/{t}");
            assert_eq!(p["transcript"].is_string(), t, "transcript for {s}/{n}/{t}");

            let mut want: Vec<&str> = Vec::new();
            if s {
                want.push("summary");
            }
            if n {
                want.push("notes");
            }
            if t {
                want.push("transcript");
            }
            assert_eq!(included, want, "included for {s}/{n}/{t}");
        }
    }

    #[test]
    fn a_body_that_is_selected_but_missing_or_blank_is_not_reported_as_included() {
        let td = tempfile::tempdir().unwrap();
        let dir = session_dir(&td);
        std::fs::remove_file(dir.join("notes.md")).unwrap();
        std::fs::write(dir.join("summary.md"), "   \n\t\n").unwrap();

        let (p, included) = build_payload(&dir, &sel(true, true, true)).unwrap();
        assert!(p["summary"].is_null(), "whitespace is not a summary");
        assert!(p["notes"].is_null());
        // `included` is what the history row reports, so a blank file must not
        // make it look as though a summary was delivered.
        assert_eq!(included, vec!["transcript".to_string()]);
    }

    #[test]
    fn a_session_with_no_chunks_reports_no_duration_rather_than_zero() {
        let td = tempfile::tempdir().unwrap();
        let dir = session_dir(&td);
        let mut m: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        m["chunks"] = serde_json::json!([]);
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();

        let (p, _) = build_payload(&dir, &sel(false, false, false)).unwrap();
        assert!(p["meeting"]["duration_seconds"].is_null());
    }

    #[test]
    fn a_missing_manifest_is_an_error_rather_than_an_empty_push() {
        let td = tempfile::tempdir().unwrap();
        assert!(build_payload(td.path(), &sel(true, true, true)).is_err());
    }

    // ---- push_webhook ------------------------------------------------------

    fn body() -> serde_json::Value {
        serde_json::json!({ "source": "daisy" })
    }

    #[test]
    fn no_auth_sends_the_body_and_no_credential_header() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/hook")
            .match_header("authorization", mockito::Matcher::Missing)
            .match_body(mockito::Matcher::Json(body()))
            .with_status(200)
            .expect(1)
            .create();
        push_webhook(&format!("{}/hook", server.url()), &WebhookAuth::None, &body()).unwrap();
        mock.assert();
    }

    #[test]
    fn a_header_auth_names_its_own_header() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/hook")
            .match_header("x-api-key", "salt-marsh-19")
            .with_status(204)
            .expect(1)
            .create();
        push_webhook(
            &format!("{}/hook", server.url()),
            &WebhookAuth::Header { name: "X-API-Key".into(), value: "salt-marsh-19".into() },
            &body(),
        )
        .unwrap();
        mock.assert();
    }

    #[test]
    fn a_bearer_auth_becomes_an_authorization_header() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/hook")
            .match_header("authorization", "Bearer salt-marsh-19")
            .with_status(200)
            .expect(1)
            .create();
        push_webhook(
            &format!("{}/hook", server.url()),
            &WebhookAuth::Bearer { token: "salt-marsh-19".into() },
            &body(),
        )
        .unwrap();
        mock.assert();
    }

    #[test]
    fn a_client_error_fails_once_and_carries_the_response_back() {
        let mut server = mockito::Server::new();
        // Exactly one request.
        let mock = server
            .mock("POST", "/hook")
            .with_status(422)
            .with_body("unknown meeting id")
            .expect(1)
            .create();
        let e = push_webhook(&format!("{}/hook", server.url()), &WebhookAuth::None, &body())
            .unwrap_err();
        mock.assert();
        let msg = format!("{e:?}");
        assert!(msg.contains("422"), "the status has to reach the user: {msg}");
        assert!(msg.contains("unknown meeting id"), "and so does the reason: {msg}");
    }

    #[test]
    fn a_server_error_is_retried_once_and_succeeds_on_the_second_try() {
        let mut server = mockito::Server::new();
        let fail = server.mock("POST", "/hook").with_status(503).expect(1).create();
        let ok = server.mock("POST", "/hook").with_status(200).expect(1).create();
        push_webhook(&format!("{}/hook", server.url()), &WebhookAuth::None, &body()).unwrap();
        fail.assert();
        ok.assert();
    }

    #[test]
    fn a_second_failure_gives_up_rather_than_retrying_forever() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/hook")
            .with_status(429)
            .with_body("slow down")
            .expect(2)
            .create();
        let e = push_webhook(&format!("{}/hook", server.url()), &WebhookAuth::None, &body())
            .unwrap_err();
        mock.assert();
        assert!(format!("{e:?}").contains("429"));
    }
}
