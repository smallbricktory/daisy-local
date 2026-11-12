// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! `speakers.json`: the per-cluster table a phone writes beside its
//! transcript. The table is the phone's; the desktop reads its run id and
//! rewrites it only via `retain`/`set_run`. Names live in `meta.json`.
use recording::speakers;

const PHONE_SPEAKERS: &str = r#"{
  "schema_version": 2,
  "session_id": "daisy-1700000000",
  "enrolled_speaker_id": 1,
  "enrolled_cosine": 0.81,
  "speakers": [
    { "id": 1, "seconds": 41.2, "segments": 9, "embedding": [0.1, 0.2, 0.3],
      "name": "Rowan", "name_source": "user", "clip_start_ms": 1000, "clip_end_ms": 4000, "is_me": true },
    { "id": 2, "seconds": 12.0, "segments": 3, "embedding": [0.4, 0.5, 0.6],
      "name": "Sable", "name_source": "match", "match_cosine": 0.74 },
    { "id": 3, "seconds": 5.5, "segments": 2, "embedding": [0.7, 0.8, 0.9] }
  ]
}"#;

#[test]
fn absent_file_reads_as_none() {
    let dir = tempfile::tempdir().unwrap();
    assert!(speakers::read(dir.path()).unwrap().is_none());
}

#[test]
fn malformed_file_reads_as_none() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("speakers.json"), b"{").unwrap();
    assert!(speakers::read(dir.path()).unwrap().is_none());
}

#[test]
fn retain_drops_the_clusters_a_rediarization_replaced_and_keeps_unknown_keys() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("speakers.json"), PHONE_SPEAKERS).unwrap();
    assert!(speakers::retain(dir.path(), &[2]).unwrap());
    let file = speakers::read(dir.path()).unwrap().unwrap();
    assert_eq!(file.speakers.iter().map(|s| s.id).collect::<Vec<_>>(), vec![2]);
    assert_eq!(file.extra.get("session_id").and_then(|v| v.as_str()), Some("daisy-1700000000"), "other keys ride through");
    let two = file.speakers.iter().find(|s| s.id == 2).unwrap();
    assert_eq!(two.name.as_deref(), Some("Sable"), "retain does not touch the fields it keeps");
    assert!(!speakers::retain(dir.path(), &[2]).unwrap(), "nothing more to drop");
}

#[test]
fn retain_without_a_file_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!speakers::retain(dir.path(), &[1]).unwrap());
    assert!(!dir.path().join("speakers.json").exists());
}

#[test]
fn set_run_records_the_run_and_a_repeat_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("speakers.json"), PHONE_SPEAKERS).unwrap();
    assert!(speakers::set_run(dir.path(), "run-a").unwrap());
    assert_eq!(speakers::read(dir.path()).unwrap().unwrap().diarization_id.as_deref(), Some("run-a"));
    assert!(!speakers::set_run(dir.path(), "run-a").unwrap(), "already this run");
}
