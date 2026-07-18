use std::fs;
use tauri_app_core::commands::recording::{resume_open_segment, stop_close_segment};

#[test]
fn stop_then_resume_segments() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("manifest.json"), r#"{"schema_version":2,"session_id":"s","created_at_unix_seconds":1000,"sample_rate":16000,"channels":1,"mic_source_id":1,"mic_source_node_name":"m","mic_source_description":"m","system_source_id":2,"system_source_node_name":"s","system_source_description":"s","aec_mode":"disabled","chunks":[{"index":1,"started_at_unix_seconds":1000,"ended_at_unix_seconds":1360,"duration_seconds":360,"mic_wav_relative":"c/1/mic.wav","system_wav_relative":"c/1/sys.wav","mic_aec_wav_relative":null}],"finalized_at_unix_seconds":null,"title":null,"meeting_id":"id","tag_ids":[],"notes_md_relative":null,"attendees":[],"calendar":null,"recording_segments":[{"started_at_unix_seconds":1000,"stopped_at_unix_seconds":null,"first_chunk_index":1,"last_chunk_index":null}]}"#).unwrap();
    stop_close_segment(d.path(), 2000).unwrap();
    let v: serde_json::Value =
        serde_json::from_slice(&fs::read(d.path().join("manifest.json")).unwrap()).unwrap();
    assert_eq!(v["recording_segments"][0]["stopped_at_unix_seconds"], 2000);
    assert_eq!(v["recording_segments"][0]["last_chunk_index"], 1);
    resume_open_segment(d.path(), 5000).unwrap();
    let v2: serde_json::Value =
        serde_json::from_slice(&fs::read(d.path().join("manifest.json")).unwrap()).unwrap();
    assert_eq!(v2["recording_segments"].as_array().unwrap().len(), 2);
    assert_eq!(v2["recording_segments"][1]["first_chunk_index"], 2);
}
