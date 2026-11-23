// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Offline repro harness for diarization. Ignored by default; run explicitly with a session copy staged at DAISY_REPRO_PROFILE (a profile root containing sessions/<id>).
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
    let r = tauri_app_core::commands::voiceprints::diarize_session_impl(&app, &sid, None, None, &[]);
    eprintln!("diarize_repro: finished in {:.1}s -> {r:?}", t.elapsed().as_secs_f32());
    r.unwrap();
}

/// Sequence repro: whisper full pass → dedup → diarize in one process.
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
    let st = providers_http::transcribe_session(&provider, &session_dir, &manifest_bytes, Some("en"), false, &|_, _| {})
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
    let r = tauri_app_core::commands::voiceprints::diarize_session_impl(&app, &sid, None, None, &[]);
    eprintln!("repro: diarize finished in {:.1}s -> {r:?}", t3.elapsed().as_secs_f32());
    r.unwrap();
}

/// What enrollment says about each cluster of a staged session, after the run that produced them. The guard reads `manifest.speaker_separation`.
///
///   DAISY_REPRO_PROFILE=/path/to/profile DAISY_REPRO_SESSION=daisy-… \
///     cargo test -p tauri-app --test diarize_repro -- --ignored \
///       --exact enroll_every_cluster_repro --nocapture
#[test]
#[ignore]
fn enroll_every_cluster_repro() {
    use recording::manifest::SessionManifest;
    use tauri_app_core::commands::lifecycle::init_vault_impl;
    use tauri_app_core::commands::voiceprints::{
        enroll_voiceprint_from_speaker_impl, EnrollFromSpeakerRequest,
    };
    use tauri_app_core::state::VaultState;

    let root = std::env::var("DAISY_REPRO_PROFILE").expect("DAISY_REPRO_PROFILE");
    let sid = std::env::var("DAISY_REPRO_SESSION").expect("DAISY_REPRO_SESSION");
    let app = AppState::new(ProfileDir::at(&root).unwrap());
    let vs = VaultState::default();
    init_vault_impl(&app, &vs, "correct-horse-battery-staple-extended-22").unwrap();

    let m: SessionManifest = serde_json::from_slice(
        &std::fs::read(app.profile.session_path(&sid).join("manifest.json")).unwrap(),
    )
    .unwrap();
    eprintln!(
        "enroll_repro: {sid} separation={:?} mixed={:?}",
        m.speaker_separation, m.mixed_clusters
    );
    for c in &m.cluster_sides {
        let t = std::time::Instant::now();
        let r = enroll_voiceprint_from_speaker_impl(
            &app,
            &vs,
            EnrollFromSpeakerRequest {
                session_id: sid.clone(),
                cluster_id: c.cluster_id,
                display_name: format!("Cluster {}", c.cluster_id),
                email: None,
                allow_mixed: false,
            },
        );
        eprintln!(
            "enroll_repro: cluster {} in {:.1}s -> {}",
            c.cluster_id,
            t.elapsed().as_secs_f32(),
            match &r {
                Ok(_) => "ACCEPTED".to_string(),
                Err(e) => format!("REFUSED {e}"),
            }
        );
    }
}
