//! Offline repro harness for a diarization hang on a real session.
//! Ignored by default — run explicitly with a session copy staged at
//! DAISY_REPRO_PROFILE (a profile root containing sessions/<id>).
//!
//!   DAISY_REPRO_PROFILE=/path/to/profile DAISY_REPRO_SESSION=daisy-… \
//!     cargo test -p tauri-app --test diarize_repro -- --ignored --nocapture

use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::AppState;

#[test]
#[ignore]
fn diarize_repro() {
    let root = std::env::var("DAISY_REPRO_PROFILE").expect("DAISY_REPRO_PROFILE");
    let sid = std::env::var("DAISY_REPRO_SESSION").expect("DAISY_REPRO_SESSION");
    let app = AppState::new(ProfileDir::at(&root).unwrap());
    let t = std::time::Instant::now();
    eprintln!("diarize_repro: starting {sid}");
    let r = tauri_app_core::commands::voiceprints::diarize_session_impl(&app, &sid, None, None);
    eprintln!("diarize_repro: finished in {:.1}s -> {r:?}", t.elapsed().as_secs_f32());
    r.unwrap();
}

/// Faithful sequence repro: whisper full pass → dedup → diarize in one
/// process, mirroring the finalize pipeline's thread history.
#[test]
#[ignore]
fn diarize_after_whisper_repro() {
    let root = std::env::var("DAISY_REPRO_PROFILE").expect("DAISY_REPRO_PROFILE");
    let sid = std::env::var("DAISY_REPRO_SESSION").expect("DAISY_REPRO_SESSION");
    let model = std::env::var("DAISY_REPRO_WHISPER").expect("DAISY_REPRO_WHISPER");
    let session_dir = std::path::Path::new(&root).join("sessions").join(&sid);
    // Force the full pass: drop transcripts from the copy.
    let _ = std::fs::remove_file(session_dir.join("transcript.json"));
    let _ = std::fs::remove_file(session_dir.join("transcript.dedup.json"));

    let app = AppState::new(ProfileDir::at(&root).unwrap());

    let t = std::time::Instant::now();
    eprintln!("repro: whisper full pass starting");
    let manifest_bytes = std::fs::read(session_dir.join("manifest.json")).unwrap();
    let provider = providers_local::WhisperLocalTranscriber::new(std::path::Path::new(&model)).unwrap();
    let st = providers_http::transcribe_session(&provider, &session_dir, &manifest_bytes, Some("en"), &|_, _| {})
        .unwrap();
    std::fs::write(session_dir.join("transcript.json"), serde_json::to_vec_pretty(&st).unwrap()).unwrap();
    eprintln!("repro: whisper done in {:.1}s", t.elapsed().as_secs_f32());

    let t2 = std::time::Instant::now();
    tauri_app_core::commands::pipeline::dedup_impl(
        &app,
        tauri_app_core::commands::pipeline::DedupRequest { session_id: sid.clone() },
    )
    .unwrap();
    eprintln!("repro: dedup done in {:.1}s", t2.elapsed().as_secs_f32());

    let t3 = std::time::Instant::now();
    let r = tauri_app_core::commands::voiceprints::diarize_session_impl(&app, &sid, None, None);
    eprintln!("repro: diarize finished in {:.1}s -> {r:?}", t3.elapsed().as_secs_f32());
    r.unwrap();
}
