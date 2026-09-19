// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! What the on-device models cost in memory while the app runs: how many
//! times each one is loaded, how many are resident at once, and whether a
//! model is freed when the thing holding it goes away.
//!
//! Three model runtimes live in this process — the text encoder
//! (`embeddings`, Granite 97M over ONNX), the speaker encoder
//! (`voiceprints`, WeSpeaker ResNet34 over ONNX) and whisper.cpp. Each
//! carries a `loads()` / `resident()` pair; these drive the paths that build
//! them and read the counters back.
//!
//! The local *language* model is not here: on the desktop it always runs out
//! of process (LM Studio, Ollama, or a cloud endpoint), so nothing this
//! process does can put two of its weights in memory.
//!
//! Models come from `<repo>/models/`; whichever is absent skips its own tests
//! out loud.
use std::sync::Arc;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::AppState;

/// The residency counters are process-wide, so these tests take turns.
fn alone() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A second of quiet speech-shaped noise: the speaker front end needs
/// something with variance in it, and digital silence is not that.
fn quiet_pcm(samples: usize) -> Vec<i16> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    (0..samples)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 48) as i16) / 8
        })
        .collect()
}

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// Point the encoders at the in-tree models once, for every test in this
/// binary. Returns false when the model is not staged, so a test can say so
/// and stop rather than pass having measured nothing.
fn embed_model_ready() -> bool {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if std::env::var_os("DAISY_EMBED_DIR").is_none() {
            std::env::set_var("DAISY_EMBED_DIR", repo_root().join("models/embeddings"));
        }
    });
    let ready = embeddings::model_dir().join("model.onnx").is_file();
    if !ready {
        eprintln!(
            "skipping: no text-encoder model at {} (run scripts/download-embeddings.sh)",
            embeddings::model_dir().display()
        );
    }
    ready
}

fn speaker_model_ready() -> bool {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if std::env::var_os("DAISY_VOICEPRINT_DIR").is_none() {
            std::env::set_var("DAISY_VOICEPRINT_DIR", repo_root().join("models/voiceprints"));
        }
    });
    let ready = voiceprints::model_dir().join("model.onnx").is_file();
    if !ready {
        eprintln!(
            "skipping: no speaker-encoder model at {} (run scripts/download-voiceprints.sh)",
            voiceprints::model_dir().display()
        );
    }
    ready
}

fn whisper_model() -> Option<std::path::PathBuf> {
    let p = std::env::var_os("DAISY_WHISPER_MODEL")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repo_root().join("models/whisper/ggml-base.en.bin"));
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("skipping: no whisper model at {} (set DAISY_WHISPER_MODEL)", p.display());
        None
    }
}

fn temp_app() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    (Arc::new(AppState::new(profile)), dir)
}

// ---- the text encoder ------------------------------------------------------

#[test]
fn twenty_four_callers_at_once_load_the_text_encoder_once() {
    let _alone = alone();
    if !embed_model_ready() {
        return;
    }
    let (app, _d) = temp_app();
    let loads_before = embeddings::loads();
    let resident_before = embeddings::resident();

    // Every thread blocks on the same gate so they contend for the slot
    // rather than arriving one after another.
    let gate = Arc::new(std::sync::Barrier::new(24));
    let handles: Vec<_> = (0..24)
        .map(|i| {
            let app = Arc::clone(&app);
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.wait();
                app.with_embedder(|e| Ok(e.encode(&format!("caller {i} says hello")).unwrap()))
                    .expect("encode")
            })
        })
        .collect();
    let vectors: Vec<Vec<f32>> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    assert_eq!(vectors.len(), 24);
    for v in &vectors {
        assert_eq!(v.len(), embeddings::EMBED_DIM, "every caller got a real vector");
    }
    assert_eq!(
        embeddings::loads() - loads_before,
        1,
        "twenty-four concurrent callers must build one session, not twenty-four"
    );
    assert_eq!(
        embeddings::resident(),
        resident_before + 1,
        "exactly one text encoder is resident"
    );
}

#[test]
fn a_second_call_reuses_the_loaded_text_encoder() {
    let _alone = alone();
    if !embed_model_ready() {
        return;
    }
    let (app, _d) = temp_app();
    app.with_embedder(|e| Ok(e.encode("first").unwrap())).unwrap();
    let after_first = embeddings::loads();
    for _ in 0..5 {
        app.with_embedder(|e| Ok(e.encode("again").unwrap())).unwrap();
    }
    assert_eq!(
        embeddings::loads(),
        after_first,
        "the encoder is held for the process, not rebuilt per call"
    );
}

#[test]
fn dropping_the_state_frees_the_text_encoder() {
    let _alone = alone();
    if !embed_model_ready() {
        return;
    }
    let before = embeddings::resident();
    {
        let (app, _d) = temp_app();
        app.with_embedder(|e| Ok(e.encode("held").unwrap())).unwrap();
        assert_eq!(embeddings::resident(), before + 1);
    }
    assert_eq!(
        embeddings::resident(),
        before,
        "the model's weights go with the state that held them"
    );
}

#[test]
fn two_app_states_hold_two_encoders() {
    let _alone = alone();
    // The app builds exactly one `AppState`, so this is the guard on that:
    // a refactor that makes state per-window or per-command would double the
    // resident weights, and this is what would say so.
    if !embed_model_ready() {
        return;
    }
    let before = embeddings::resident();
    let (a, _da) = temp_app();
    let (b, _db) = temp_app();
    a.with_embedder(|e| Ok(e.encode("a").unwrap())).unwrap();
    assert_eq!(embeddings::resident(), before + 1);
    b.with_embedder(|e| Ok(e.encode("b").unwrap())).unwrap();
    assert_eq!(
        embeddings::resident(),
        before + 2,
        "each state carries its own encoder — there must only ever be one state"
    );
}

#[test]
fn a_panic_inside_the_embedder_does_not_wedge_the_next_caller() {
    let _alone = alone();
    if !embed_model_ready() {
        return;
    }
    let (app, _d) = temp_app();
    app.with_embedder(|e| Ok(e.encode("warm").unwrap())).unwrap();
    let loads_after_warm = embeddings::loads();

    let poisoner = {
        let app = Arc::clone(&app);
        std::thread::spawn(move || {
            app.with_embedder(|_| -> tauri_app_core::error::Result<()> {
                panic!("a caller blew up holding the encoder")
            })
        })
    };
    assert!(poisoner.join().is_err(), "the panic reached the thread boundary");

    // The slot's mutex is poisoned now. The next caller must still get the
    // model back rather than an error or a second load.
    let v = app
        .with_embedder(|e| Ok(e.encode("after the panic").unwrap()))
        .expect("the encoder survives a poisoned lock");
    assert_eq!(v.len(), embeddings::EMBED_DIM);
    assert_eq!(
        embeddings::loads(),
        loads_after_warm,
        "recovery reuses the loaded model rather than building another"
    );
}

// ---- the speaker encoder ---------------------------------------------------

#[test]
fn eight_speaker_encoders_can_be_built_at_once_and_all_are_freed() {
    let _alone = alone();
    // whisper.cpp needs its own lock around context creation because ggml's
    // backend registration is not safe from two threads. This is the same
    // question asked of ort: eight threads building sessions at once. It
    // fails by crashing, not by asserting.
    if !speaker_model_ready() {
        return;
    }
    let before = voiceprints::resident();
    let gate = Arc::new(std::sync::Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.wait();
                voiceprints::Encoder::load().expect("speaker encoder")
            })
        })
        .collect();
    let encoders: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        voiceprints::resident(),
        before + 8,
        "eight sessions built concurrently are eight sessions resident"
    );
    drop(encoders);
    assert_eq!(
        voiceprints::resident(),
        before,
        "and all eight are freed when they go out of scope"
    );
}

#[test]
fn a_speaker_encoder_is_freed_when_the_run_that_built_it_ends() {
    let _alone = alone();
    if !speaker_model_ready() {
        return;
    }
    let before = voiceprints::resident();
    {
        let mut e = voiceprints::Encoder::load().unwrap();
        // A second of silence is enough to drive the front end.
        let v = e.encode_pcm(&quiet_pcm(16_000)).expect("embed");
        assert!(!v.is_empty());
        assert_eq!(voiceprints::resident(), before + 1);
    }
    assert_eq!(voiceprints::resident(), before);
}

// ---- whisper ---------------------------------------------------------------

#[test]
fn four_whisper_contexts_can_be_built_at_once_and_all_are_freed() {
    let _alone = alone();
    let Some(model) = whisper_model() else {
        return;
    };
    let before = providers_local::whisper_resident();
    let gate = Arc::new(std::sync::Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let model = model.clone();
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.wait();
                providers_local::WhisperLocalTranscriber::new(&model).expect("whisper context")
            })
        })
        .collect();
    let ctxs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        providers_local::whisper_resident(),
        before + 4,
        "nothing caps how many whisper contexts a process holds"
    );
    drop(ctxs);
    assert_eq!(providers_local::whisper_resident(), before);
}

#[test]
fn a_missing_whisper_model_costs_nothing() {
    let _alone = alone();
    let before = providers_local::whisper_resident();
    let loads = providers_local::whisper_loads();
    let e = match providers_local::WhisperLocalTranscriber::new(
        "/definitely/not/here/ggml-base.en.bin",
    ) {
        Ok(_) => panic!("a model path that is not there must not load"),
        Err(e) => e,
    };
    assert!(format!("{e}").contains("not found"), "{e}");
    assert_eq!(providers_local::whisper_resident(), before, "a failed load leaves nothing behind");
    assert_eq!(providers_local::whisper_loads(), loads, "and is not counted as a load");
}

// ---- the three together ----------------------------------------------------

#[test]
fn the_three_runtimes_can_be_resident_at_once() {
    let _alone = alone();
    // The real shape of a busy moment: a recording finalizing (whisper), its
    // speakers being clustered (the speaker encoder) and a search running
    // (the text encoder). They must coexist.
    if !embed_model_ready() || !speaker_model_ready() {
        return;
    }
    let Some(model) = whisper_model() else {
        return;
    };
    let (app, _d) = temp_app();

    let whisper = providers_local::WhisperLocalTranscriber::new(&model).unwrap();
    let mut speaker = voiceprints::Encoder::load().unwrap();
    app.with_embedder(|e| Ok(e.encode("a search while the meeting finalizes").unwrap()))
        .unwrap();

    assert_eq!(providers_local::whisper_resident(), 1);
    assert_eq!(voiceprints::resident(), 1);
    assert_eq!(embeddings::resident(), 1);

    // And each still answers with the others loaded.
    assert!(!speaker.encode_pcm(&quiet_pcm(16_000)).unwrap().is_empty());
    let v = app
        .with_embedder(|e| Ok(e.encode("still working").unwrap()))
        .unwrap();
    assert_eq!(v.len(), embeddings::EMBED_DIM);
    drop(whisper);
    drop(speaker);
    assert_eq!(providers_local::whisper_resident(), 0);
    assert_eq!(voiceprints::resident(), 0);
}

// ---- the live path: one model for both tracks ------------------------------

/// A live recording runs two tracks — the microphone and the system mix —
/// against one transcriber instance. Both call `run()` on the same `Arc`, and
/// the model has to be built once for the pair: two of these weights is twice
/// the memory, and on the GPU backends a second concurrent init is a crash
/// rather than a cost.
fn drain<T: Send + 'static>(mut rx: tokio::sync::mpsc::Receiver<T>) {
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
}

/// Eight seconds of speech-shaped noise per track, sent as one-second pushes,
/// then both inputs close.
fn feed_two_tracks(
    mic: &tokio::sync::mpsc::UnboundedSender<Vec<i16>>,
    sys: &tokio::sync::mpsc::UnboundedSender<Vec<i16>>,
) {
    for _ in 0..8 {
        mic.send(quiet_pcm(16_000)).unwrap();
        sys.send(quiet_pcm(16_000)).unwrap();
    }
}

#[test]
fn both_live_tracks_share_one_whisper() {
    use providers_realtime::RealtimeTranscriber as _;
    let _alone = alone();
    let Some(model) = whisper_model() else {
        return;
    };
    let loads_before = providers_local::whisper_loads();
    let resident_before = providers_local::whisper_resident();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let live = Arc::new(providers_local::streaming::transcriber::LocalWhisperRealtime::new(
        model,
        2,
        "ggml-base.en",
    ));
    rt.block_on(async {
        let (mic_tx, mic_rx) = tokio::sync::mpsc::unbounded_channel();
        let (sys_tx, sys_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = tokio::sync::mpsc::channel(256);
        drain(ev_rx);
        feed_two_tracks(&mic_tx, &sys_tx);
        drop(mic_tx);
        drop(sys_tx);

        let mic = {
            let live = Arc::clone(&live);
            let ev = ev_tx.clone();
            tokio::spawn(async move { live.run(16_000, mic_rx, ev).await })
        };
        let sys = {
            let live = Arc::clone(&live);
            let ev = ev_tx.clone();
            tokio::spawn(async move { live.run(16_000, sys_rx, ev).await })
        };
        mic.await.unwrap().expect("the mic track ran");
        sys.await.unwrap().expect("the system track ran");
    });

    assert_eq!(
        providers_local::whisper_loads() - loads_before,
        1,
        "both live tracks must build the model once between them"
    );
    assert_eq!(
        providers_local::whisper_resident(),
        resident_before + 1,
        "and only one set of weights is resident while they run"
    );

    drop(live);
    assert_eq!(
        providers_local::whisper_resident(),
        resident_before,
        "ending the recording frees the model"
    );
}

#[test]
fn a_second_recording_builds_its_own_model_and_the_first_is_gone() {
    use providers_realtime::RealtimeTranscriber as _;
    let _alone = alone();
    let Some(model) = whisper_model() else {
        return;
    };
    let resident_before = providers_local::whisper_resident();
    let rt = tokio::runtime::Runtime::new().unwrap();

    // One recording, start to finish, twice over — what pressing record
    // twice in a session does.
    for _ in 0..2 {
        let live = Arc::new(providers_local::streaming::transcriber::LocalWhisperRealtime::new(
            model.clone(),
            2,
            "ggml-base.en",
        ));
        rt.block_on(async {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let (ev_tx, ev_rx) = tokio::sync::mpsc::channel(256);
            drain(ev_rx);
            for _ in 0..4 {
                tx.send(quiet_pcm(16_000)).unwrap();
            }
            drop(tx);
            live.run(16_000, rx, ev_tx).await.expect("the track ran");
        });
        assert_eq!(
            providers_local::whisper_resident(),
            resident_before + 1,
            "one recording holds one model"
        );
        drop(live);
        assert_eq!(
            providers_local::whisper_resident(),
            resident_before,
            "and the next one starts from nothing resident"
        );
    }
}

// ---- parakeet --------------------------------------------------------------

/// The streaming ASR the live pipeline uses when it is the chosen engine. One
/// model is six ONNX sessions and around a gigabyte, so the two tracks
/// sharing it matters more here than anywhere else.
fn parakeet_dir() -> Option<std::path::PathBuf> {
    let dir = repo_root().join("models/parakeet/parakeet-tdt-0.6b-v2-int8");
    dir.join("encoder.int8.onnx").is_file().then_some(dir)
}

#[test]
fn both_live_tracks_share_one_parakeet() {
    use providers_realtime::RealtimeTranscriber as _;
    let _alone = alone();
    let Some(dir) = parakeet_dir() else {
        return;
    };
    let loads_before = providers_local::parakeet::model::loads();
    let resident_before = providers_local::parakeet::model::resident();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let live = Arc::new(providers_local::parakeet::realtime::ParakeetRealtime::new(
        dir,
        providers_local::parakeet::realtime::ParakeetTune::default(),
        "parakeet-tdt-0.6b-v2-int8",
    ));
    rt.block_on(async {
        let (mic_tx, mic_rx) = tokio::sync::mpsc::unbounded_channel();
        let (sys_tx, sys_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = tokio::sync::mpsc::channel(256);
        drain(ev_rx);
        feed_two_tracks(&mic_tx, &sys_tx);
        drop(mic_tx);
        drop(sys_tx);

        let mic = {
            let live = Arc::clone(&live);
            let ev = ev_tx.clone();
            tokio::spawn(async move { live.run(16_000, mic_rx, ev).await })
        };
        let sys = {
            let live = Arc::clone(&live);
            let ev = ev_tx.clone();
            tokio::spawn(async move { live.run(16_000, sys_rx, ev).await })
        };
        mic.await.unwrap().expect("the mic track ran");
        sys.await.unwrap().expect("the system track ran");
    });

    assert_eq!(
        providers_local::parakeet::model::loads() - loads_before,
        1,
        "both live tracks must build the parakeet model once between them"
    );
    assert_eq!(
        providers_local::parakeet::model::resident(),
        resident_before + 1,
    );

    drop(live);
    assert_eq!(
        providers_local::parakeet::model::resident(),
        resident_before,
        "ending the recording frees the six sessions"
    );
}

// ---- the faint-audio marker (Silero) ---------------------------------------

/// The marker detector is built once per recording and shared by both track
/// loops; it loads its model on the first call that needs it. Concurrent
/// first calls must not each build one.
#[test]
fn concurrent_first_calls_build_one_silero() {
    use providers_local::streaming::vad;
    let _alone = alone();
    let model = repo_root().join("models/vad/silero_vad_v5.onnx");
    if !model.is_file() {
        return;
    }
    let loads_before = vad::silero_loads();
    let resident_before = vad::silero_resident();

    let marker = Arc::new(vad::MarkerDetector::new(model));
    let gate = Arc::new(std::sync::Barrier::new(6));
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let marker = Arc::clone(&marker);
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                let pcm: Vec<f32> = quiet_pcm(32_000)
                    .into_iter()
                    .map(|s| s as f32 / 32768.0)
                    .collect();
                gate.wait();
                marker.confirms(&pcm)
            })
        })
        .collect();
    for h in handles {
        assert!(h.join().unwrap().is_some(), "the marker answered");
    }
    assert_eq!(
        vad::silero_loads() - loads_before,
        1,
        "six callers at once must build the detector once"
    );
    assert_eq!(vad::silero_resident(), resident_before + 1);

    drop(marker);
    assert_eq!(
        vad::silero_resident(),
        resident_before,
        "the recording ending frees it"
    );
}

#[test]
fn a_missing_marker_model_turns_markers_off_and_costs_nothing() {
    use providers_local::streaming::vad;
    let _alone = alone();
    let loads_before = vad::silero_loads();
    let resident_before = vad::silero_resident();
    let marker = vad::MarkerDetector::new("/definitely/not/here/silero.onnx".into());
    let pcm = vec![0.0f32; 32_000];
    assert_eq!(marker.confirms(&pcm), None, "markers are off, not fatal");
    assert_eq!(marker.confirms(&pcm), None, "and stay off");
    assert_eq!(vad::silero_loads(), loads_before, "nothing was built");
    assert_eq!(vad::silero_resident(), resident_before);
}
