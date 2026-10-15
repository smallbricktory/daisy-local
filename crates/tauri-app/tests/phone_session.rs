// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! A session recorded on a phone: schema 4, one mic track, no system source,
//! a transcript already on disk, and a `speakers.json` beside it.
use std::fs;
use tauri_app_core::commands::library::list_sessions_impl;
use tauri_app_core::commands::session::{read_session_impl, set_session_speaker_label_impl};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::AppState;

const SID: &str = "daisy-1700000000";

const MANIFEST: &str = r#"{
  "audio_purged": false,
  "channels": 1,
  "chunks": [{
    "duration_seconds": 9.3995625,
    "ended_at_unix_seconds": 1700000010,
    "index": 1,
    "mic_host_ns": 327071631296500,
    "mic_samples": 150393,
    "mic_wav_relative": "chunks/0001/mic.wav",
    "started_at_unix_seconds": 1700000000
  }],
  "created_at_unix_seconds": 1700000000,
  "finalized_at_unix_seconds": 1700000010,
  "interrupted": false,
  "recording_segments": [{
    "first_chunk_index": 1,
    "last_chunk_index": 1,
    "started_at_unix_seconds": 1700000000,
    "stopped_at_unix_seconds": 1700000010,
    "reason": "user"
  }],
  "dismissed_suggested_tag_ids": [],
  "sample_rate": 16000,
  "schema_version": 4,
  "session_id": "daisy-1700000000",
  "recording_kind": "phone_room",
  "origin_device": "dev-7f3a",
  "speaker_map": [
    { "cluster_id": 1, "display_name": "Ro", "email": null, "voiceprint_id": null, "match_confidence": null, "contact_id": null }
  ]
}"#;

const TRANSCRIPT: &str = r#"{
  "schema_version": 1,
  "session_id": "daisy-1700000000",
  "provider": "speechanalyzer_local",
  "model": "Apple SpeechAnalyzer",
  "transcribed_at_unix_seconds": 1700000010,
  "chunks": [{
    "chunk_index": 1,
    "tracks": [{
      "track": "mic",
      "source_wav_relative": "chunks/0001/mic.wav",
      "segments": [
        { "start_ms": 85, "end_ms": 4200, "text": "Quarterly numbers first, then the roadmap.", "speaker_id": 1, "confidence": null },
        { "start_ms": 4300, "end_ms": 9399, "text": "Roadmap slides are on the shared drive.", "speaker_id": 2, "confidence": null }
      ]
    }]
  }]
}"#;

const SPEAKERS: &str = r#"{
  "schema_version": 2,
  "session_id": "daisy-1700000000",
  "speakers": [
    { "id": 1, "seconds": 4.1, "segments": 1, "embedding": [0.1, 0.2], "name": "Rowan", "name_source": "user" },
    { "id": 2, "seconds": 5.1, "segments": 1, "embedding": [0.3, 0.4] }
  ]
}"#;

fn phone_profile() -> (AppState, tempfile::TempDir) {
    let td = tempfile::tempdir().unwrap();
    let p = ProfileDir::at(td.path().join("daisy")).unwrap();
    let sdir = p.session_path(SID);
    fs::create_dir_all(sdir.join("chunks/0001")).unwrap();
    fs::write(sdir.join("manifest.json"), MANIFEST).unwrap();
    fs::write(sdir.join("transcript.json"), TRANSCRIPT).unwrap();
    fs::write(sdir.join("speakers.json"), SPEAKERS).unwrap();
    (AppState::new(p), td)
}

#[test]
fn lists_and_opens_with_no_edits() {
    let (app, _td) = phone_profile();
    let list = list_sessions_impl(&app).unwrap();
    assert_eq!(list.len(), 1, "the phone session is listed");
    assert_eq!(list[0].session_id, SID);

    let v = read_session_impl(&app, SID).unwrap();
    assert_eq!(v.session_id, SID);
    assert!(v.has_transcript, "transcript.json is on disk, so finalize skips transcription");
    assert_eq!(v.manifest_json["recording_kind"], serde_json::json!("phone_room"));
    assert_eq!(v.manifest_json["origin_device"], serde_json::json!("dev-7f3a"));
    assert_eq!(v.manifest_json["audio_purged"], serde_json::json!(false), "phone keys reach the view");
}

#[test]
fn transcript_on_disk_parses_so_finalize_skips_transcription() {
    let t: transcript::SessionTranscript = serde_json::from_str(TRANSCRIPT).unwrap();
    assert_eq!(t.chunks.len(), 1);
}

#[test]
fn the_manifests_own_speaker_map_is_what_reading_shows() {
    // No meta.json has projected speakers.json's names onto this manifest,
    // so the manifest's own field is what a reader sees: speakers.json plays
    // no part in it.
    let (app, _td) = phone_profile();
    let names = display_names(&read_session_impl(&app, SID).unwrap().manifest_json);
    assert_eq!(names.get(&1).map(String::as_str), Some("Ro"), "the manifest's own name, not speakers.json's");
    assert!(names.get(&2).is_none());
}

#[test]
fn a_desktop_rename_reaches_the_manifest_through_meta_and_leaves_the_table_untouched() {
    let (app, _td) = phone_profile();
    set_session_speaker_label_impl(&app, SID, 2, "Sable".into(), None).unwrap();

    let root = app.profile.session_path(SID);
    assert_eq!(
        fs::read(root.join("speakers.json")).unwrap(),
        SPEAKERS.as_bytes(),
        "the desktop never rewrites the phone's table"
    );

    let m: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let labels = m["speaker_map"].as_array().unwrap();
    assert!(labels.iter().any(|l| l["cluster_id"] == 2 && l["display_name"] == "Sable"));
    assert_eq!(m["audio_purged"], serde_json::json!(false), "phone keys survive the rewrite");
    assert_eq!(m["recording_segments"][0]["reason"], serde_json::json!("user"));
    assert_eq!(m["schema_version"], serde_json::json!(4));

    // Clearing the name clears it on the manifest too, and still never
    // touches the table.
    set_session_speaker_label_impl(&app, SID, 1, "  ".into(), None).unwrap();
    assert_eq!(
        fs::read(root.join("speakers.json")).unwrap(),
        SPEAKERS.as_bytes(),
        "clearing a name never touches the table either"
    );
    let names = display_names(&read_session_impl(&app, SID).unwrap().manifest_json);
    assert!(names.get(&1).is_none());
}

fn display_names(manifest_json: &serde_json::Value) -> std::collections::HashMap<u32, String> {
    manifest_json["speaker_map"]
        .as_array()
        .map(|labels| {
            labels
                .iter()
                .map(|l| (l["cluster_id"].as_u64().unwrap() as u32, l["display_name"].as_str().unwrap().to_string()))
                .collect()
        })
        .unwrap_or_default()
}
