// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
#[path = "../../audio-engine/tests/common/mod.rs"]
mod common;

#[test]
#[ignore = "requires PipeWire + pactl; run with --ignored"]
fn start_pause_resume_stop_via_commands() {
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;
    use tauri_app_core::commands::recording::{
        pause_impl, recording_snapshot_impl, resume_impl, start_recording_impl, stop_impl,
        ActiveRecording, StartRequest,
    };
    use tauri_app_core::profile::ProfileDir;
    use tauri_app_core::state::AppState;
    use tempfile::TempDir;

    let _ = env_logger::builder().is_test(true).try_init();
    let (mic, monitor, _vs) = common::sink_rig("daisy-tauri-test");

    let td = TempDir::new().unwrap();
    let app = AppState::new(ProfileDir::at(td.path().join("daisy")).unwrap());
    let active: Mutex<Option<ActiveRecording>> = Mutex::new(None);

    let (snap, _live_rx) = start_recording_impl(
        &app,
        &active,
        StartRequest {
            mic_source_id: mic.id,
            system_source_id: Some(monitor.id),
            session_id: Some("lc-tauri-1".into()),
            title: Some("Test".into()),
            tag_ids: vec![],
            notes_md: None,
            meeting_id: None,
            calendar_link: None,
            attendees: vec![],
            single_local_speaker: Some(true),
        },
        std::sync::Arc::new(audio_engine::topology::TopologyHub::start()),
        true,
    )
    .unwrap();
    assert_eq!(snap.state, "recording");
    assert!(snap.clock_running_since_unix_ms.is_some(), "the clock runs while recording");

    thread::sleep(Duration::from_millis(600));
    assert_eq!(pause_impl(&app, &active).unwrap(), "paused");
    let paused = recording_snapshot_impl(&active).unwrap();
    assert_eq!(paused.state, "paused");
    assert_eq!(paused.clock_running_since_unix_ms, None, "the clock holds still while paused");
    assert!(paused.recorded_ms > 0, "the first chunk's audio counts");
    thread::sleep(Duration::from_millis(200));
    assert_eq!(resume_impl(&app, &active).unwrap(), "recording");
    let resumed = recording_snapshot_impl(&active).unwrap();
    assert!(
        resumed.recorded_ms.abs_diff(paused.recorded_ms) < 100,
        "the paused span adds nothing: {} vs {}",
        resumed.recorded_ms,
        paused.recorded_ms
    );
    assert!(resumed.clock_running_since_unix_ms.is_some(), "the clock runs again");
    thread::sleep(Duration::from_millis(600));
    let final_root = stop_impl(&active).unwrap();
    assert!(std::path::Path::new(&final_root)
        .join("manifest.json")
        .is_file());
}

#[test]
#[ignore = "requires PipeWire + pactl; run with --ignored"]
fn pauses_and_resumes_from_two_threads_keep_every_segment() {
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;
    use tauri_app_core::commands::recording::{
        pause_impl, resume_impl, start_recording_impl, stop_impl, ActiveRecording, StartRequest,
    };
    use tauri_app_core::profile::ProfileDir;
    use tauri_app_core::state::AppState;
    use tempfile::TempDir;

    let (mic, monitor, _vs) = common::sink_rig("daisy-tauri-race");
    let td = TempDir::new().unwrap();
    let app = AppState::new(ProfileDir::at(td.path().join("daisy")).unwrap());
    let active: Mutex<Option<ActiveRecording>> = Mutex::new(None);
    start_recording_impl(
        &app,
        &active,
        StartRequest {
            mic_source_id: mic.id,
            system_source_id: Some(monitor.id),
            session_id: Some("lc-tauri-race".into()),
            title: None,
            tag_ids: vec![],
            notes_md: None,
            meeting_id: None,
            calendar_link: None,
            attendees: vec![],
            single_local_speaker: Some(true),
        },
        std::sync::Arc::new(audio_engine::topology::TopologyHub::start()),
        true,
    )
    .unwrap();
    // The window and the sleep watcher pause and resume at once.
    thread::scope(|s| {
        for _ in 0..2 {
            s.spawn(|| {
                for _ in 0..10 {
                    let _ = pause_impl(&app, &active);
                    thread::sleep(Duration::from_millis(20));
                    let _ = resume_impl(&app, &active);
                    thread::sleep(Duration::from_millis(20));
                }
            });
        }
    });
    let root = stop_impl(&active).unwrap();
    let m: recording::manifest::SessionManifest =
        serde_json::from_slice(&std::fs::read(std::path::Path::new(&root).join("manifest.json")).unwrap())
            .unwrap();
    for pair in m.recording_segments.windows(2) {
        assert!(pair[0].stopped_at_unix_seconds.is_some(), "{:?}", m.recording_segments);
        assert!(pair[0].started_at_unix_seconds <= pair[1].started_at_unix_seconds);
    }
    assert!(m.recording_segments.len() > 1);
}
