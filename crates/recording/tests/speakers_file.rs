// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! `speakers.json`: the per-cluster table a phone writes beside its
//! transcript. Names in it are canonical; the manifest's `speaker_map` is the
//! view the desktop derives from it.
use recording::manifest::SpeakerLabel;
use recording::speakers::{self, SpeakersFile};

const PHONE_SPEAKERS: &str = r#"{
  "schema_version": 2,
  "session_id": "daisy-1787249858",
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

fn label(cluster_id: u32, name: &str) -> SpeakerLabel {
    SpeakerLabel {
        cluster_id,
        display_name: name.into(),
        email: Some(format!("{}@example.test", name.to_lowercase())),
        voiceprint_id: None,
        match_confidence: None,
        contact_id: Some(format!("c-{cluster_id}")),
        extra: Default::default(),
    }
}

#[test]
fn absent_file_reads_as_none() {
    let dir = tempfile::tempdir().unwrap();
    assert!(speakers::read(dir.path()).unwrap().is_none());
}

#[test]
fn malformed_file_reads_as_none_so_the_manifest_labels_stand() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("speakers.json"), b"{").unwrap();
    assert!(speakers::read(dir.path()).unwrap().is_none());
    assert!(!speakers::set_name(dir.path(), 1, Some("Rowan")).unwrap(), "nothing to rename in it");
}

#[test]
fn set_match_records_the_source_and_score_and_a_typed_name_clears_them() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("speakers.json"), PHONE_SPEAKERS).unwrap();
    assert!(speakers::set_match(dir.path(), 3, "Sable", 0.62).unwrap());
    let file = speakers::read(dir.path()).unwrap().unwrap();
    let three = file.speakers.iter().find(|s| s.id == 3).unwrap();
    assert_eq!(three.name.as_deref(), Some("Sable"));
    assert_eq!(three.name_source.as_deref(), Some("match"));
    assert_eq!(three.match_cosine, Some(0.62));
    let mut map = vec![];
    speakers::derive_speaker_map(&file, &mut map);
    assert_eq!(map.iter().find(|l| l.cluster_id == 3).unwrap().match_confidence, Some(0.62));

    assert!(speakers::set_name(dir.path(), 3, Some("Sable")).unwrap());
    let file = speakers::read(dir.path()).unwrap().unwrap();
    let three = file.speakers.iter().find(|s| s.id == 3).unwrap();
    assert_eq!(three.name_source.as_deref(), Some("user"));
    assert_eq!(three.match_cosine, None);
}

#[test]
fn retain_drops_the_clusters_a_rediarization_replaced() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("speakers.json"), PHONE_SPEAKERS).unwrap();
    assert!(speakers::retain(dir.path(), &[2]).unwrap());
    let file = speakers::read(dir.path()).unwrap().unwrap();
    assert_eq!(file.speakers.iter().map(|s| s.id).collect::<Vec<_>>(), vec![2]);
    assert_eq!(file.extra.get("session_id").and_then(|v| v.as_str()), Some("daisy-1787249858"), "other keys ride through");
    assert!(!speakers::retain(dir.path(), &[2]).unwrap(), "nothing more to drop");
    let mut map = vec![label(1, "Ro")];
    speakers::derive_speaker_map(&file, &mut map);
    assert!(map.iter().any(|l| l.cluster_id == 1), "a cluster the table no longer lists keeps its manifest label");
    assert_eq!(map.len(), 2, "the named survivor is added");
}

#[test]
fn names_derive_the_speaker_map() {
    let file: SpeakersFile = serde_json::from_str(PHONE_SPEAKERS).unwrap();
    // 1 renamed on the phone since the map was written; 3 lost its name; 9 is
    // a cluster the table does not know.
    let mut map = vec![label(1, "Ro"), label(3, "Ghost"), label(9, "Desk")];
    speakers::derive_speaker_map(&file, &mut map);

    let by_id = |id: u32| map.iter().find(|l| l.cluster_id == id);
    let one = by_id(1).expect("named speaker kept");
    assert_eq!(one.display_name, "Rowan");
    assert_eq!(one.email.as_deref(), Some("ro@example.test"), "other fields survive a rename");
    assert_eq!(one.contact_id.as_deref(), Some("c-1"));
    assert_eq!(one.match_confidence, None, "a typed name carries no score");

    let two = by_id(2).expect("a named speaker missing from the map is added");
    assert_eq!(two.display_name, "Sable");
    assert_eq!(two.match_confidence, Some(0.74));

    assert!(by_id(3).is_none(), "an unnamed speaker has no label");
    assert_eq!(by_id(9).map(|l| l.display_name.as_str()), Some("Desk"), "unknown clusters are left alone");
}

#[test]
fn set_name_rewrites_the_file_and_keeps_every_other_key() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("speakers.json"), PHONE_SPEAKERS).unwrap();

    assert!(speakers::set_name(dir.path(), 2, Some("Sable Quill")).unwrap());
    assert!(speakers::set_name(dir.path(), 1, None).unwrap());

    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("speakers.json")).unwrap()).unwrap();
    let sp = v["speakers"].as_array().unwrap();
    assert_eq!(sp[1]["name"], serde_json::json!("Sable Quill"));
    assert_eq!(sp[1]["name_source"], serde_json::json!("user"));
    assert!(sp[1].get("match_cosine").is_none(), "a typed name replaces the match");
    assert_eq!(sp[1]["embedding"], serde_json::json!([0.4, 0.5, 0.6]));
    assert!(sp[0].get("name").is_none());
    assert!(sp[0].get("name_source").is_none());
    assert_eq!(sp[0]["is_me"], serde_json::json!(true), "clearing a name touches nothing else");
    assert_eq!(v["enrolled_speaker_id"], serde_json::json!(1));
    assert_eq!(v["schema_version"], serde_json::json!(2));
}

#[test]
fn set_name_without_a_file_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!speakers::set_name(dir.path(), 1, Some("Rowan")).unwrap());
    assert!(!dir.path().join("speakers.json").exists());
}

#[test]
fn set_name_for_an_unknown_cluster_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("speakers.json"), PHONE_SPEAKERS).unwrap();
    assert!(!speakers::set_name(dir.path(), 42, Some("Nobody")).unwrap());
    let after = std::fs::read_to_string(dir.path().join("speakers.json")).unwrap();
    assert_eq!(after, PHONE_SPEAKERS);
}
