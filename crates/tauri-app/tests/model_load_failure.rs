// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! What a model that will not load costs. Its own test binary, because it
//! rewrites `DAISY_EMBED_DIR` process-wide and the rest of the model tests
//! read it.
//!
//! The question behind these: after a failed load, does the process end up
//! holding a half-built model, refusing forever, or quietly trying again?
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::AppState;

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn temp_app() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    (AppState::new(profile), dir)
}

/// One test: the whole point is the ordering of a bad env then a good one,
/// and the counters are process-wide.
#[test]
fn a_failed_load_leaves_nothing_resident_and_the_next_call_tries_again() {
    let real = repo_root().join("models/embeddings");
    let missing = repo_root().join("models/definitely-not-here");

    std::env::set_var("DAISY_EMBED_DIR", &missing);
    let (app, _d) = temp_app();
    let loads_before = embeddings::loads();

    let err = app
        .with_embedder(|e| Ok(e.encode("never runs").unwrap()))
        .unwrap_err();
    assert!(
        format!("{err}").contains("embedding model not available"),
        "the caller is told the model is the problem: {err}"
    );
    assert_eq!(
        embeddings::resident(),
        0,
        "a load that failed must not count as a resident model"
    );
    assert_eq!(
        embeddings::loads(),
        loads_before,
        "and must not count as a load"
    );

    // Failing does not latch: a second call with the model still missing
    // fails the same way rather than returning a cached error.
    let again = app
        .with_embedder(|e| Ok(e.encode("still never runs").unwrap()))
        .unwrap_err();
    assert!(format!("{again}").contains("embedding model not available"));

    if !real.join("model.onnx").is_file() {
        eprintln!(
            "stopping early: no text-encoder model at {} to prove the retry succeeds",
            real.display()
        );
        return;
    }

    // The model appears — a download finishing, a profile moving — and the
    // very next call picks it up without a restart.
    std::env::set_var("DAISY_EMBED_DIR", &real);
    let v = app
        .with_embedder(|e| Ok(e.encode("now it works").unwrap()))
        .expect("the retry loads the model that has arrived");
    assert_eq!(v.len(), embeddings::EMBED_DIM);
    assert_eq!(embeddings::loads(), loads_before + 1);
    assert_eq!(embeddings::resident(), 1);
}
