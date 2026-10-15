// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Chapters and polish: the two commands that hand a finished transcript to
//! an AI provider and write the answer back into the session.
//!
//! The provider is a local HTTP server speaking the OpenAI-compatible shape
//! LM Studio does, so the request the app builds and the reply it parses are
//! both exercised. What is asserted is what the session holds afterwards.
use std::fs;

use tauri_app_core::commands::chapters::{extract_chapters_impl, ChaptersRequest};
use tauri_app_core::commands::pipeline::{polish_impl, PolishRequest};
use tauri_app_core::error::AppError;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, DecryptedKeys, ProviderConfig, ProviderId, VaultState};

const SESSION: &str = "daisy-1788100000";

/// A finished session: a deduped transcript of four turns, and the markdown
/// rendered from it with the `[HH:MM:SS]` stamps chapters must echo.
fn session(app: &AppState) {
    let root = app.profile.session_path(SESSION);
    fs::create_dir_all(&root).unwrap();
    let seg = |start: u32, text: &str| {
        serde_json::json!({
            "start_ms": start, "end_ms": start + 4_000, "text": text,
            "confidence": null, "speaker_id": 0,
        })
    };
    let transcript = serde_json::json!({
        "schema_version": 1,
        "session_id": SESSION,
        "provider": "whisper",
        "model": "base.en",
        "transcribed_at_unix_seconds": 1_788_100_100i64,
        "chunks": [{
            "chunk_index": 0,
            "tracks": [{
                "track": "mic_aec",
                "source_wav_relative": "chunks/0001/mic.wav",
                "segments": [
                    seg(0, "so um the rebate import is the first thing"),
                    seg(60_000, "and the portal certificate is the second"),
                ],
            }, {
                "track": "system",
                "source_wav_relative": "chunks/0001/system.wav",
                "segments": [
                    seg(30_000, "right the bank sends a fixed width file"),
                    seg(90_000, "the rotation takes about a day"),
                ],
            }],
        }],
    });
    fs::write(
        root.join("transcript.dedup.json"),
        serde_json::to_vec_pretty(&transcript).unwrap(),
    )
    .unwrap();
    fs::write(
        root.join("transcript.md"),
        "[00:00:00] Me: so um the rebate import is the first thing\n\n\
         [00:00:30] Them: right the bank sends a fixed width file\n\n\
         [00:01:00] Me: and the portal certificate is the second\n\n\
         [00:01:30] Them: the rotation takes about a day\n",
    )
    .unwrap();
}

fn app() -> (AppState, VaultState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(dir.path().join("daisy")).unwrap());
    let vault = VaultState::default();
    *vault.keys.lock().unwrap() = Some(DecryptedKeys::default());
    session(&app);
    (app, vault, dir)
}

/// Points the vault's LM Studio entry at `url`. Nothing else about the
/// provider is set, which is the shape a person gets from the Providers
/// screen for a local server.
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

/// A provider that answers every call with `content`, and counts the calls.
fn provider_saying(content: serde_json::Value) -> (mockito::ServerGuard, mockito::Mock) {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "choices": [{ "message": { "content": content.to_string() } }]
            })
            .to_string(),
        )
        .expect_at_least(1)
        .create();
    (server, mock)
}

fn chapters_req() -> ChaptersRequest {
    ChaptersRequest {
        session_id: SESSION.into(),
        provider: Some(ProviderId::LmStudio),
        model: None,
    }
}

fn polish_req() -> PolishRequest {
    PolishRequest {
        session_id: SESSION.into(),
        provider: Some(ProviderId::LmStudio),
        model: None,
    }
}

fn dedup(app: &AppState) -> serde_json::Value {
    serde_json::from_slice(
        &fs::read(app.profile.session_path(SESSION).join("transcript.dedup.json")).unwrap(),
    )
    .unwrap()
}

/// Every segment of the deduped transcript, in the order the renderer walks.
fn texts(v: &serde_json::Value) -> Vec<String> {
    v["chunks"][0]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|t| t["segments"].as_array().unwrap())
        .map(|s| s["text"].as_str().unwrap().to_string())
        .collect()
}

// ---- chapters --------------------------------------------------------------

#[test]
fn a_session_that_is_not_here_is_named() {
    let (app, vault, _d) = app();
    let e = extract_chapters_impl(
        &app,
        &vault,
        ChaptersRequest { session_id: "daisy-nothing".into(), provider: None, model: None },
    )
    .unwrap_err();
    assert!(matches!(e, AppError::SessionNotFound(s) if s == "daisy-nothing"));
}

#[test]
fn with_no_provider_configured_chapters_are_skipped_and_say_why() {
    let (app, vault, _d) = app();
    let r = extract_chapters_impl(
        &app,
        &vault,
        ChaptersRequest { session_id: SESSION.into(), provider: None, model: None },
    )
    .unwrap();
    assert!(r.skipped);
    assert!(r.chapters.is_empty());
    assert_eq!(r.reason.as_deref(), Some("no AI provider configured"));
    assert!(
        !app.profile.session_path(SESSION).join("chapters.json").exists(),
        "a skipped run writes nothing"
    );
}

#[test]
fn an_empty_transcript_is_skipped_and_says_why() {
    let (app, vault, _d) = app();
    fs::write(app.profile.session_path(SESSION).join("transcript.md"), "   \n").unwrap();
    let r = extract_chapters_impl(&app, &vault, chapters_req()).unwrap();
    assert!(r.skipped);
    assert_eq!(r.reason.as_deref(), Some("transcript is empty"));
}

#[test]
fn a_session_that_was_never_transcribed_is_skipped_and_says_why() {
    let (app, vault, _d) = app();
    fs::remove_file(app.profile.session_path(SESSION).join("transcript.md")).unwrap();
    let r = extract_chapters_impl(&app, &vault, chapters_req()).unwrap();
    assert!(r.skipped);
    assert_eq!(r.reason.as_deref(), Some("transcript is empty"));
}

#[test]
fn a_locked_vault_stops_chapters_rather_than_asking_the_provider() {
    let (app, vault, _d) = app();
    *vault.keys.lock().unwrap() = None;
    let e = extract_chapters_impl(&app, &vault, chapters_req()).unwrap_err();
    assert!(matches!(e, AppError::VaultLocked));
}

#[test]
fn chapters_are_written_to_the_session_with_the_model_that_made_them() {
    let (app, vault, _d) = app();
    let (server, mock) = provider_saying(serde_json::json!({
        "chapters": [
            { "title": "Rebate import", "start_hms": "00:00:00", "summary": "The bank's file." },
            { "title": "Portal certificate", "start_hms": "00:01:00", "summary": null },
        ]
    }));
    provider_at(&vault, &server.url());

    let r = extract_chapters_impl(&app, &vault, chapters_req()).unwrap();
    mock.assert();
    assert!(!r.skipped);
    assert_eq!(r.chapters.len(), 2);
    assert_eq!(r.chapters[0].title, "Rebate import");

    let on_disk: serde_json::Value = serde_json::from_slice(
        &fs::read(app.profile.session_path(SESSION).join("chapters.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(on_disk["session_id"], SESSION);
    assert_eq!(on_disk["schema_version"], 2, "chapters on the audio timeline");
    assert_eq!(
        on_disk["model"], "qwen3.5-4b",
        "the file records which model chaptered it"
    );
    assert_eq!(on_disk["chapters"][1]["start_hms"], "00:01:00");
    assert!(
        on_disk["generated_at_unix_seconds"].as_i64().unwrap() > 1_700_000_000,
        "and when"
    );
}

#[test]
fn a_chapter_at_a_time_the_transcript_does_not_carry_is_dropped() {
    let (app, vault, _d) = app();
    let (server, _mock) = provider_saying(serde_json::json!({
        "chapters": [
            { "title": "Rebate import", "start_hms": "00:00:00" },
            // A minute that no turn starts at: the model invented it.
            { "title": "Invented", "start_hms": "00:02:30" },
        ]
    }));
    provider_at(&vault, &server.url());

    let r = extract_chapters_impl(&app, &vault, chapters_req()).unwrap();
    assert_eq!(
        r.chapters.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        ["Rebate import"],
        "a chapter that cannot be seeked to is not offered"
    );
}

#[test]
fn chapters_come_back_in_order_however_the_model_sent_them() {
    let (app, vault, _d) = app();
    let (server, _mock) = provider_saying(serde_json::json!({
        "chapters": [
            { "title": "Third", "start_hms": "00:01:30" },
            { "title": "First", "start_hms": "00:00:00" },
            { "title": "Second", "start_hms": "00:01:00" },
        ]
    }));
    provider_at(&vault, &server.url());
    let r = extract_chapters_impl(&app, &vault, chapters_req()).unwrap();
    assert_eq!(
        r.chapters.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        ["First", "Second", "Third"]
    );
}

#[test]
fn running_chapters_again_replaces_the_file_rather_than_adding_to_it() {
    let (app, vault, _d) = app();
    let (first, _m1) = provider_saying(serde_json::json!({
        "chapters": [{ "title": "One pass", "start_hms": "00:00:00" }]
    }));
    provider_at(&vault, &first.url());
    extract_chapters_impl(&app, &vault, chapters_req()).unwrap();

    let (second, _m2) = provider_saying(serde_json::json!({
        "chapters": [
            { "title": "Rebate import", "start_hms": "00:00:00" },
            { "title": "Portal certificate", "start_hms": "00:01:00" },
        ]
    }));
    provider_at(&vault, &second.url());
    let r = extract_chapters_impl(&app, &vault, chapters_req()).unwrap();

    assert_eq!(r.chapters.len(), 2);
    let on_disk: serde_json::Value = serde_json::from_slice(
        &fs::read(app.profile.session_path(SESSION).join("chapters.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(on_disk["chapters"].as_array().unwrap().len(), 2);
    assert_eq!(on_disk["chapters"][0]["title"], "Rebate import");
}

#[test]
fn a_provider_that_answers_with_an_error_is_reported_and_writes_nothing() {
    let (app, vault, _d) = app();
    let mut server = mockito::Server::new();
    let _mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(500)
        .with_body("upstream is down")
        .expect_at_least(1)
        .create();
    provider_at(&vault, &server.url());

    let e = extract_chapters_impl(&app, &vault, chapters_req()).unwrap_err();
    assert!(
        format!("{e}").contains("chapter extraction"),
        "the message names what failed: {e}"
    );
    assert!(!app.profile.session_path(SESSION).join("chapters.json").exists());
}

// ---- polish ----------------------------------------------------------------

#[test]
fn polish_on_a_session_that_is_not_here_is_named() {
    let (app, vault, _d) = app();
    let e = polish_impl(
        &app,
        &vault,
        PolishRequest { session_id: "daisy-nothing".into(), provider: None, model: None },
    )
    .unwrap_err();
    assert!(matches!(e, AppError::SessionNotFound(s) if s == "daisy-nothing"));
}

#[test]
fn with_no_provider_configured_polish_leaves_the_transcript_alone() {
    let (app, vault, _d) = app();
    let before = dedup(&app);
    let r = polish_impl(
        &app,
        &vault,
        PolishRequest { session_id: SESSION.into(), provider: None, model: None },
    )
    .unwrap();
    assert_eq!(r.batches, 0);
    assert_eq!(r.segments_polished, 0);
    assert_eq!(dedup(&app), before, "nothing was rewritten");
    assert!(
        fs::read_to_string(app.profile.session_path(SESSION).join("transcript.md"))
            .unwrap()
            .contains("fixed width file"),
        "and the markdown is the one that was there"
    );
}

#[test]
fn polish_rewrites_the_segments_and_re_renders_the_markdown() {
    let (app, vault, _d) = app();
    let (server, mock) = provider_saying(serde_json::json!({
        "cleaned": [
            "So the rebate import is the first thing.",
            "And the portal certificate is the second.",
            "Right, the bank sends a fixed-width file.",
            "The rotation takes about a day.",
        ]
    }));
    provider_at(&vault, &server.url());

    let r = polish_impl(&app, &vault, polish_req()).unwrap();
    mock.assert();
    assert_eq!(r.batches, 1, "one chunk is one round-trip");
    assert_eq!(r.failed_batches, 0);
    assert_eq!(r.segments_polished, 4);
    assert_eq!(r.segments_unchanged, 0);

    // The order the polisher sends is mic first, then system, and the
    // cleaned text has to come back to the segment it came from.
    assert_eq!(
        texts(&dedup(&app)),
        [
            "So the rebate import is the first thing.",
            "And the portal certificate is the second.",
            "Right, the bank sends a fixed-width file.",
            "The rotation takes about a day.",
        ]
    );
    let md = fs::read_to_string(app.profile.session_path(SESSION).join("transcript.md")).unwrap();
    assert!(md.contains("fixed-width file"), "the markdown was re-rendered: {md}");
    assert!(!md.contains("fixed width file"), "and the raw text is gone");
}

#[test]
fn text_the_provider_returns_unchanged_is_counted_as_unchanged() {
    let (app, vault, _d) = app();
    let (server, _mock) = provider_saying(serde_json::json!({
        "cleaned": [
            "so um the rebate import is the first thing",
            "and the portal certificate is the second",
            "right the bank sends a fixed width file",
            "the rotation takes about a day",
        ]
    }));
    provider_at(&vault, &server.url());
    let r = polish_impl(&app, &vault, polish_req()).unwrap();
    assert_eq!(r.segments_unchanged, 4);
    assert_eq!(r.segments_polished, 0);
}

#[test]
fn a_batch_the_provider_fails_keeps_the_raw_text_rather_than_losing_it() {
    let (app, vault, _d) = app();
    let before = texts(&dedup(&app));
    let mut server = mockito::Server::new();
    let _mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(500)
        .with_body("upstream is down")
        .expect_at_least(1)
        .create();
    provider_at(&vault, &server.url());

    let r = polish_impl(&app, &vault, polish_req()).unwrap();
    assert_eq!(r.batches, 1);
    assert_eq!(r.failed_batches, 1);
    assert_eq!(r.segments_polished, 0);
    assert_eq!(texts(&dedup(&app)), before, "the words a person said survive");
}

#[test]
fn a_reply_that_covers_only_some_segments_leaves_the_rest_as_they_were() {
    let (app, vault, _d) = app();
    let before = texts(&dedup(&app));
    let (server, _mock) = provider_saying(serde_json::json!({
        // Four went out; two came back. Position is the only thing tying a
        // cleaned line to the segment it came from, so the two that were
        // answered take theirs and the two that were not keep their words.
        "cleaned": ["So the rebate import is first.", "And the portal certificate is second."]
    }));
    provider_at(&vault, &server.url());

    let r = polish_impl(&app, &vault, polish_req()).unwrap();
    assert_eq!(r.failed_batches, 0);
    assert_eq!(r.segments_polished, 2);
    let after = texts(&dedup(&app));
    assert_eq!(after[0], "So the rebate import is first.");
    assert_eq!(after[1], "And the portal certificate is second.");
    assert_eq!(after[2..], before[2..], "nothing was shifted onto the other track");
}

#[test]
fn polish_masks_profanity_the_provider_put_back() {
    let (app, vault, _d) = app();
    let (server, _mock) = provider_saying(serde_json::json!({
        "cleaned": [
            "So the fucking rebate import is first.",
            "And the portal certificate is the second.",
            "Right, the bank sends a fixed-width file.",
            "The rotation takes about a day.",
        ]
    }));
    provider_at(&vault, &server.url());
    polish_impl(&app, &vault, polish_req()).unwrap();

    let first = texts(&dedup(&app))[0].clone();
    assert!(
        !first.contains("fucking"),
        "persisted text is masked whatever the provider sends: {first}"
    );
}

#[test]
fn a_locked_vault_stops_polish_rather_than_asking_the_provider() {
    let (app, vault, _d) = app();
    *vault.keys.lock().unwrap() = None;
    let e = polish_impl(&app, &vault, polish_req()).unwrap_err();
    assert!(matches!(e, AppError::VaultLocked));
}
