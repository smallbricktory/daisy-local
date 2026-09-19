// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use std::fs;
use tauri_app_core::commands::tag_suggest::{
    session_apply_tag_suggestions_impl, session_dismiss_tag_suggestions_impl,
    session_undismiss_tag_suggestions_impl, tag_suggest_for_session_impl,
    tag_suggest_prerecord_impl,
};
use tauri_app_core::commands::tags::TagsFile;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, Tag};
use recording::manifest::CalendarLink;
use summarize::hash::source_inputs_hash;
use summarize::prompts::Envelope;
use summarize::{SummaryInput, TagPromptRef};

fn temp_app() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    (AppState::new(profile), dir)
}

fn tag(id: &str, name: &str, vocab: Option<&str>) -> Tag {
    Tag {
        id: id.into(),
        name: name.into(),
        color_hex: "#000000".into(),
        prompt_md: None,
        vocab_md: vocab.map(Into::into),
        created_at_unix_seconds: 0,
        use_count: 0,
        extra: Default::default(), updated_at_unix_seconds: None,
    }
}

fn write_tags(app: &AppState, tags: Vec<Tag>) {
    let file = TagsFile { schema_version: 1, tags };
    let path = app.profile.root().join("tags.json");
    fs::write(&path, serde_json::to_vec_pretty(&file).unwrap()).unwrap();
}

#[allow(clippy::too_many_arguments)]
fn write_session(
    app: &AppState,
    sid: &str,
    created: i64,
    finalized: Option<i64>,
    title: Option<&str>,
    tag_ids: &[&str],
    dismissed: &[&str],
    calendar: Option<(&str, &str)>,
) {
    let dir = app.profile.session_path(sid);
    fs::create_dir_all(&dir).unwrap();
    let manifest = serde_json::json!({
        "schema_version": 4,
        "session_id": sid,
        "created_at_unix_seconds": created,
        "sample_rate": 16000,
        "channels": 1,
        "mic_source_id": 1,
        "mic_source_node_name": "m",
        "mic_source_description": "m",
        "system_source_id": 2,
        "system_source_node_name": "s",
        "system_source_description": "s",
        "aec_mode": "disabled",
        "chunks": [],
        "finalized_at_unix_seconds": finalized,
        "title": title,
        "meeting_id": "mid",
        "tag_ids": tag_ids,
        "notes_md_relative": null,
        "attendees": [],
        "calendar": calendar.map(|(p, e)| serde_json::json!({
            "provider": p,
            "event_id": e,
            "recurrence_id": null,
            "planned_start_unix_seconds": 0,
            "planned_end_unix_seconds": 0,
        })),
        "recording_segments": [],
        "dismissed_suggested_tag_ids": dismissed,
    });
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn write_transcript(app: &AppState, sid: &str, md: &str) {
    let dir = app.profile.session_path(sid);
    fs::write(dir.join("transcript.md"), md).unwrap();
}

fn read_manifest_json(app: &AppState, sid: &str) -> serde_json::Value {
    let dir = app.profile.session_path(sid);
    let bytes = fs::read(dir.join("manifest.json")).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn prerecord_matches_by_calendar() {
    let (app, _d) = temp_app();
    write_tags(&app, vec![tag("t1", "Ops", None)]);
    write_session(
        &app,
        "prior",
        1_000,
        Some(1_100),
        Some("Weekly Ops"),
        &["t1"],
        &[],
        Some(("s", "uid")),
    );
    write_session(&app, "other", 2_000, Some(2_100), Some("Unrelated"), &[], &[], None);

    let link = CalendarLink {
        provider: "s".into(),
        event_id: "uid".into(),
        recurrence_id: None,
        planned_start_unix_seconds: 0,
        planned_end_unix_seconds: 0,
    };
    let got = tag_suggest_prerecord_impl(&app, None, Some(link)).unwrap().unwrap();
    assert_eq!(got.source_session_id, "prior");
    assert_eq!(got.tag_ids, vec!["t1".to_string()]);
}

#[test]
fn prerecord_skips_deleted_tags() {
    let (app, _d) = temp_app();
    // Catalog has no tags at all — "t1" referenced by the prior session no
    // longer exists.
    write_tags(&app, vec![]);
    write_session(
        &app,
        "prior",
        1_000,
        Some(1_100),
        Some("Weekly Ops"),
        &["t1"],
        &[],
        Some(("s", "uid")),
    );

    let link = CalendarLink {
        provider: "s".into(),
        event_id: "uid".into(),
        recurrence_id: None,
        planned_start_unix_seconds: 0,
        planned_end_unix_seconds: 0,
    };
    let got = tag_suggest_prerecord_impl(&app, None, Some(link)).unwrap();
    assert!(got.is_none());
}

#[test]
fn for_session_subtracts_applied_and_dismissed() {
    let (app, _d) = temp_app();
    write_tags(
        &app,
        vec![tag("t1", "Alpha", None), tag("t2", "Beta", None), tag("t3", "Gamma", None)],
    );
    write_session(
        &app,
        "prior",
        1_000,
        Some(1_100),
        Some("Weekly Ops"),
        &["t1", "t2", "t3"],
        &[],
        None,
    );
    write_session(
        &app,
        "cur",
        2_000,
        Some(2_100),
        Some("Weekly Ops"),
        &["t1"],
        &["t2"],
        None,
    );

    let out = tag_suggest_for_session_impl(&app, "cur").unwrap();
    let lt = out.last_time.unwrap();
    assert_eq!(lt.source_session_id, "prior");
    assert_eq!(lt.tag_ids, vec!["t3".to_string()]);
}

#[test]
fn for_session_vocab_needs_transcript() {
    let (app, _d) = temp_app();
    write_tags(&app, vec![tag("t1", "Brewhaven", Some("BH-Cloud\nbrew kettle"))]);
    write_session(&app, "cur", 1_000, Some(1_100), None, &[], &[], None);

    // No transcript.md at all: vocab is empty.
    let out = tag_suggest_for_session_impl(&app, "cur").unwrap();
    assert!(out.vocab.is_empty());

    // Transcript with 2 vocab-term hits: one suggestion.
    write_transcript(&app, "cur", "The BH-Cloud rollout needs a brew kettle check.");
    let out2 = tag_suggest_for_session_impl(&app, "cur").unwrap();
    assert_eq!(out2.vocab.len(), 1);
    assert_eq!(out2.vocab[0].tag_id, "t1");
}

#[test]
fn for_session_vocab_caps_at_three() {
    let (app, _d) = temp_app();
    // 5 tags, each qualifying via >=2 hits of a distinct single vocab term
    // repeated twice, so all 5 qualify; ordering by total_hits desc breaks
    // ties by more repeats.
    let tags = vec![
        tag("a", "TagA", Some("alpha")),
        tag("b", "TagB", Some("bravo")),
        tag("c", "TagC", Some("charlie")),
        tag("d", "TagD", Some("delta")),
        tag("e", "TagE", Some("echo")),
    ];
    write_tags(&app, tags);
    write_session(&app, "cur", 1_000, Some(1_100), None, &[], &[], None);
    // Distinct hit counts so ordering is deterministic: echo x6, delta x5,
    // charlie x4, bravo x3, alpha x2.
    let transcript = "alpha alpha \
         bravo bravo bravo \
         charlie charlie charlie charlie \
         delta delta delta delta delta \
         echo echo echo echo echo echo";
    write_transcript(&app, "cur", transcript);

    let out = tag_suggest_for_session_impl(&app, "cur").unwrap();
    assert_eq!(out.vocab.len(), 3, "capped at MAX_VOCAB_SUGGESTIONS");
    assert_eq!(out.vocab[0].tag_id, "e");
    assert_eq!(out.vocab[1].tag_id, "d");
    assert_eq!(out.vocab[2].tag_id, "c");
}

#[test]
fn dismiss_then_undismiss_round_trip() {
    let (app, _d) = temp_app();
    write_tags(&app, vec![tag("t9", "Nine", None)]);
    write_session(&app, "cur", 1_000, Some(1_100), None, &[], &[], None);

    session_dismiss_tag_suggestions_impl(&app, "cur", vec!["t9".to_string()]).unwrap();
    let m = read_manifest_json(&app, "cur");
    assert_eq!(
        m["dismissed_suggested_tag_ids"],
        serde_json::json!(["t9"])
    );

    // Set up a prior session whose last-time suggestion would include t9, to
    // confirm the dismissed id is subtracted from for_session output.
    write_session(
        &app,
        "prior",
        500,
        Some(600),
        Some("Nine Sync"),
        &["t9"],
        &[],
        None,
    );
    let cur_dir = app.profile.session_path("cur");
    let mut cur_manifest: serde_json::Value = read_manifest_json(&app, "cur");
    cur_manifest["title"] = serde_json::json!("Nine Sync");
    fs::write(
        cur_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&cur_manifest).unwrap(),
    )
    .unwrap();

    let out = tag_suggest_for_session_impl(&app, "cur").unwrap();
    assert!(
        out.last_time.is_none(),
        "t9 is dismissed, and it's the only candidate tag"
    );

    session_undismiss_tag_suggestions_impl(&app, "cur", vec!["t9".to_string()]).unwrap();
    let m2 = read_manifest_json(&app, "cur");
    assert_eq!(
        m2["dismissed_suggested_tag_ids"],
        serde_json::json!([])
    );
    let out2 = tag_suggest_for_session_impl(&app, "cur").unwrap();
    let lt = out2.last_time.unwrap();
    assert_eq!(lt.tag_ids, vec!["t9".to_string()]);
}

#[test]
fn memo_reuses_scan_and_invalidates_on_content_change() {
    let (app, _d) = temp_app();
    write_tags(&app, vec![tag("t1", "Ops", Some("runbook"))]);
    write_session(&app, "cur", 1_000, Some(1_100), None, &[], &[], None);
    write_transcript(&app, "cur", "runbook here, runbook there");

    let first = tag_suggest_for_session_impl(&app, "cur").unwrap();
    assert_eq!(first.vocab.len(), 1);
    let second = tag_suggest_for_session_impl(&app, "cur").unwrap();
    assert_eq!(
        first.vocab.iter().map(|v| (v.tag_id.clone(), v.total_hits)).collect::<Vec<_>>(),
        second.vocab.iter().map(|v| (v.tag_id.clone(), v.total_hits)).collect::<Vec<_>>(),
        "memo hit reproduces the same result"
    );

    // Content changes: the transcript no longer has any vocab hits. A stale
    // memo would still report the old suggestion.
    write_transcript(&app, "cur", "nothing relevant here at all");
    let third = tag_suggest_for_session_impl(&app, "cur").unwrap();
    assert!(third.vocab.is_empty(), "memo must invalidate on transcript change");
}

// -- End-to-end parity fixtures ---------------------------------------------
//
// tests/fixtures/tag_suggest/end_to_end.json is the cross-platform contract:
// iOS copies this file and runs the same cases against its own port of
// tag_suggest_for_session. Each case synthesizes a temp profile (tags.json +
// session manifests + optional transcript.md) and asserts the exact
// last_time / vocab shape tag_suggest_for_session_impl returns.

#[derive(serde::Deserialize)]
struct EndToEndFixtures {
    cases: Vec<EndToEndCase>,
}

#[derive(serde::Deserialize)]
struct EndToEndCase {
    name: String,
    tags: Vec<FixtureTag>,
    sessions: Vec<FixtureSession>,
    current_session: String,
    expect: FixtureExpect,
}

#[derive(serde::Deserialize)]
struct FixtureTag {
    id: String,
    name: String,
    vocab_md: Option<String>,
}

#[derive(serde::Deserialize)]
struct FixtureCalendar {
    provider: String,
    event_id: String,
}

#[derive(serde::Deserialize)]
struct FixtureSession {
    session_id: String,
    finalized_at_unix_seconds: i64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    tag_ids: Vec<String>,
    #[serde(default)]
    dismissed_suggested_tag_ids: Vec<String>,
    #[serde(default)]
    calendar: Option<FixtureCalendar>,
    #[serde(default)]
    transcript_md: Option<String>,
}

#[derive(serde::Deserialize)]
struct FixtureExpect {
    last_time: Option<FixtureLastTime>,
    vocab: Vec<FixtureVocab>,
}

#[derive(serde::Deserialize)]
struct FixtureLastTime {
    source_session_id: String,
    tag_ids: Vec<String>,
}

#[derive(serde::Deserialize)]
struct FixtureVocab {
    tag_id: String,
    total_hits: u32,
}

#[test]
fn end_to_end_parity_fixtures() {
    let f: EndToEndFixtures = serde_json::from_str(include_str!(
        "fixtures/tag_suggest/end_to_end.json"
    ))
    .unwrap();

    for c in f.cases {
        let (app, _d) = temp_app();

        let tags: Vec<_> = c
            .tags
            .iter()
            .map(|t| tag(&t.id, &t.name, t.vocab_md.as_deref()))
            .collect();
        write_tags(&app, tags);

        for s in &c.sessions {
            let created = s.finalized_at_unix_seconds - 100;
            let tag_ids: Vec<&str> = s.tag_ids.iter().map(String::as_str).collect();
            let dismissed: Vec<&str> =
                s.dismissed_suggested_tag_ids.iter().map(String::as_str).collect();
            let calendar = s
                .calendar
                .as_ref()
                .map(|cal| (cal.provider.as_str(), cal.event_id.as_str()));
            write_session(
                &app,
                &s.session_id,
                created,
                Some(s.finalized_at_unix_seconds),
                s.title.as_deref(),
                &tag_ids,
                &dismissed,
                calendar,
            );
            if let Some(md) = &s.transcript_md {
                write_transcript(&app, &s.session_id, md);
            }
        }

        let out = tag_suggest_for_session_impl(&app, &c.current_session).unwrap();

        match &c.expect.last_time {
            None => assert!(
                out.last_time.is_none(),
                "case '{}': expected no last_time suggestion, got {:?}",
                c.name,
                out.last_time.map(|l| l.source_session_id)
            ),
            Some(exp) => {
                let lt = out
                    .last_time
                    .unwrap_or_else(|| panic!("case '{}': expected a last_time suggestion", c.name));
                assert_eq!(
                    lt.source_session_id, exp.source_session_id,
                    "case '{}': last_time.source_session_id",
                    c.name
                );
                assert_eq!(
                    lt.tag_ids, exp.tag_ids,
                    "case '{}': last_time.tag_ids",
                    c.name
                );
            }
        }

        let got_vocab: Vec<(String, u32)> =
            out.vocab.iter().map(|v| (v.tag_id.clone(), v.total_hits)).collect();
        let exp_vocab: Vec<(String, u32)> =
            c.expect.vocab.iter().map(|v| (v.tag_id.clone(), v.total_hits)).collect();
        assert_eq!(got_vocab, exp_vocab, "case '{}': vocab", c.name);
    }
}

// -- Atomic apply (last-time chip accept) -----------------------------------

#[test]
fn apply_writes_tags_and_dismissals_in_one_manifest_state() {
    let (app, _d) = temp_app();
    write_tags(&app, vec![tag("t1", "Alpha", None), tag("t2", "Beta", None)]);
    write_session(&app, "cur", 1_000, Some(1_100), None, &[], &[], None);

    // Accept t1, deselect (dismiss) t2 — a single call, one manifest write.
    session_apply_tag_suggestions_impl(
        &app,
        "cur",
        vec!["t1".to_string()],
        vec!["t2".to_string()],
    )
    .unwrap();

    // Both effects are visible in the manifest reloaded from disk: there is
    // no intermediate state where one landed without the other.
    let m = read_manifest_json(&app, "cur");
    assert_eq!(m["tag_ids"], serde_json::json!(["t1"]), "applied tag written");
    assert_eq!(
        m["dismissed_suggested_tag_ids"],
        serde_json::json!(["t2"]),
        "deselected tag dismissed in the same write"
    );

    let tags_path = app.profile.root().join("tags.json");
    let tags_file: TagsFile = serde_json::from_slice(&fs::read(tags_path).unwrap()).unwrap();
    let t1 = tags_file.tags.iter().find(|t| t.id == "t1").unwrap();
    assert_eq!(t1.use_count, 1, "use_count bumped for the applied tag");
    let t2 = tags_file.tags.iter().find(|t| t.id == "t2").unwrap();
    assert_eq!(t2.use_count, 0, "dismissed (not applied) tag's use_count untouched");
}

#[test]
fn apply_prunes_dismiss_ids_for_deleted_tags() {
    let (app, _d) = temp_app();
    // "ghost" is not in the catalog at apply time.
    write_tags(&app, vec![tag("t1", "Alpha", None)]);
    write_session(&app, "cur", 1_000, Some(1_100), None, &[], &[], None);

    session_apply_tag_suggestions_impl(
        &app,
        "cur",
        vec!["t1".to_string()],
        vec!["ghost".to_string()],
    )
    .unwrap();

    let m = read_manifest_json(&app, "cur");
    assert_eq!(
        m["dismissed_suggested_tag_ids"],
        serde_json::json!([]),
        "dead tag id pruned from the dismissed list on write"
    );
}

/// Mirrors, minimally, the input-building `summary_regenerate_impl` /
/// `summary_load_impl` do at crates/tauri-app/src/commands/summary.rs:349:
/// title/transcript/notes straight from the session, `tag_prompts` built by
/// joining the manifest's `tag_ids` against the tag catalog (name, prompt_md,
/// comma-joined vocab terms). No provider/gateway machinery is involved:
/// `source_inputs_hash` takes a `SummaryInput` value, not a provider.
fn compute_source_inputs_hash(app: &AppState, sid: &str, tags: &[Tag]) -> String {
    let m = read_manifest_json(app, sid);
    let tag_ids: Vec<String> = m["tag_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let transcript_path = app.profile.session_path(sid).join("transcript.md");
    let transcript = fs::read_to_string(&transcript_path).unwrap_or_default();

    let tag_prompts_owned: Vec<(String, String, String)> = tag_ids
        .iter()
        .filter_map(|id| tags.iter().find(|t| &t.id == id))
        .filter_map(|t| {
            let prompt = t.prompt_md.clone().unwrap_or_default();
            let terms = t.vocab_md.clone().unwrap_or_default();
            if prompt.is_empty() && terms.is_empty() {
                None
            } else {
                Some((t.name.clone(), prompt, terms))
            }
        })
        .collect();
    let tag_refs: Vec<TagPromptRef> = tag_prompts_owned
        .iter()
        .map(|(n, p, terms)| TagPromptRef { name: n, prompt_md: p, terms })
        .collect();

    let input = SummaryInput {
        session_id: sid,
        title: None,
        attendees: &[],
        user_notes_md: "",
        transcript_md: &transcript,
        tag_prompts: &tag_refs,
        style_envelope: Envelope::Classic,
        style_directive: "",
    };
    source_inputs_hash(&input, "test-provider", "test-model")
}

#[test]
fn accept_with_tag_prompt_changes_source_inputs_hash() {
    // Applying a tag whose prompt_md is non-empty changes source_inputs_hash,
    // so a summary generated before the accept is stale after it.
    let (app, _d) = temp_app();
    let mut t1 = tag("t1", "Ops", None);
    t1.prompt_md = Some("Focus on infrastructure changes.".to_string());
    write_tags(&app, vec![t1.clone()]);
    write_session(&app, "cur", 1_000, Some(1_100), None, &[], &[], None);
    write_transcript(&app, "cur", "some transcript text");

    let hash_before = compute_source_inputs_hash(&app, "cur", &[t1.clone()]);

    session_apply_tag_suggestions_impl(&app, "cur", vec!["t1".to_string()], vec![]).unwrap();

    let hash_after = compute_source_inputs_hash(&app, "cur", &[t1]);

    assert_ne!(
        hash_before, hash_after,
        "applying a tag with a prompt must change source_inputs_hash"
    );
}
