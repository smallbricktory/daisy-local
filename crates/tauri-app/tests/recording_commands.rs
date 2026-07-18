#[test]
#[ignore = "requires PipeWire + pactl; run with --ignored"]
fn start_pause_resume_stop_via_commands() {
    use audio_engine::source::{list_sources, SourceKind};
    use audio_engine::virtual_sink::VirtualSink;
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;
    use tauri_app_core::commands::recording::{
        pause_impl, resume_impl, start_recording_impl, stop_impl, ActiveRecording, StartRequest,
    };
    use tauri_app_core::profile::ProfileDir;
    use tauri_app_core::state::AppState;
    use tempfile::TempDir;

    let _ = env_logger::builder().is_test(true).try_init();
    let _vs = VirtualSink::create("daisy-tauri-test").unwrap();

    // PipeWire needs time to register the new virtual sink.
    thread::sleep(Duration::from_millis(300));

    let sources = list_sources().unwrap();
    let mic = sources
        .iter()
        .find(|s| s.kind == SourceKind::Mic)
        .unwrap()
        .clone();
    let monitor = sources
        .iter()
        .find(|s| s.node_name == "daisy-tauri-test.monitor")
        .unwrap()
        .clone();

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
    )
    .unwrap();
    assert_eq!(snap.state, "recording");

    thread::sleep(Duration::from_millis(600));
    assert_eq!(pause_impl(&app, &active).unwrap(), "paused");
    thread::sleep(Duration::from_millis(200));
    assert_eq!(resume_impl(&app, &active).unwrap(), "recording");
    thread::sleep(Duration::from_millis(600));
    let final_root = stop_impl(&active).unwrap();
    assert!(std::path::Path::new(&final_root)
        .join("manifest.json")
        .is_file());
}
