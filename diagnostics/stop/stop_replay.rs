// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Replays the app's Stop-time finalize cascade over a recorded session and
//! prints a wall-clock table per stage. It calls the shipped
//! `finalize_and_summarize_impl` (then `resume_finalize_impl` when the cascade
//! pauses at the speaker-label gate), so AEC, live-transcript promotion, gap
//! patch, dedup, D diarization and voiceprint matching run exactly as the app
//! runs them.
//!
//!   stop_replay <pristine-session-dir> <work-dir>
//!
//! The session is copied into `<work-dir>/profile/sessions/`, its finalize
//! products are removed and its manifest is reset to the just-stopped state
//! (raw chunks and live_transcript.jsonl stay). The original is never touched.
//!
//! Env: REPLAY_NO_OPTOUT=1 skips the EcoQoS opt-out; REPLAY_MODELS models dir
//! (default: this checkout's `models/`); REPLAY_NO_TAIL=1 stops at the label
//! gate; REPLAY_KEEP=1 keeps the work dir; REPLAY_DROP_LIVE=1 removes the live
//! transcript so finalize runs the batch full pass; REPLAY_LIVE_DIAR=1 runs the
//! live diarization pass over the chunks before the replay clock starts.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use tauri_app_core::commands::finalize::{
    finalize_and_summarize_impl, resume_finalize_impl, FinalizeOutcome, FinalizeRequest,
    ProgressEvent,
};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, VaultState};

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}

/// Back to the state Stop leaves: raw chunk audio, live transcript, manifest
/// without any finalize output.
fn reset_to_stopped(root: &Path) {
    for f in [
        "transcript.json",
        "transcript.dedup.json",
        "transcript.dedup.report.json",
        "transcript.md",
        "summary.json",
        "summary.md",
        "chapters.json",
        "meeting.opus",
        "finalize.status.json",
        "finalize.recovery.json",
        "chunks.json",
        "embeddings.bin",
        "speakers.json",
    ] {
        let _ = std::fs::remove_file(root.join(f));
    }
    if let Ok(rd) = std::fs::read_dir(root.join("chunks")) {
        for c in rd.flatten() {
            for f in ["mic_aec.wav", "mic_dn.wav"] {
                let _ = std::fs::remove_file(c.path().join(f));
            }
        }
    }
    let mp = root.join("manifest.json");
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(&mp).unwrap()).unwrap();
    let o = m.as_object_mut().unwrap();
    for k in ["finalized_at_unix_seconds", "denoise_applied", "speaker_separation", "diarization_id"] {
        o.remove(k);
    }
    for k in ["speaker_map", "cluster_sides", "mixed_clusters", "cluster_sample_spans"] {
        o.insert(k.into(), serde_json::json!([]));
    }
    o.insert("diarization_unavailable".into(), false.into());
    for c in o.get_mut("chunks").and_then(|c| c.as_array_mut()).unwrap() {
        let c = c.as_object_mut().unwrap();
        c.insert("mic_aec_wav_relative".into(), serde_json::Value::Null);
        c.insert("mic_dn_wav_relative".into(), serde_json::Value::Null);
    }
    std::fs::write(&mp, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
}

#[cfg(windows)]
fn power_state() -> String {
    use std::ffi::c_void;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetProcessInformation, ProcessPowerThrottling,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE,
    };
    let mut got = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: 0,
        StateMask: 0,
    };
    let r = unsafe {
        GetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &mut got as *mut _ as *mut c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    match r {
        Ok(()) => format!(
            "control_mask={:#x} state_mask={:#x} execution_speed_throttled={}",
            got.ControlMask,
            got.StateMask,
            got.ControlMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
                && got.StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
        ),
        Err(e) => format!("readback failed: {e}"),
    }
}
#[cfg(not(windows))]
fn power_state() -> String {
    "n/a (not windows)".into()
}

struct Timeline(Mutex<Vec<(String, Instant)>>);

fn table(title: &str, t: &Timeline, end: Instant) {
    let v = t.0.lock().unwrap();
    println!("--- {title}");
    let mut total = 0.0;
    for (i, (stage, at)) in v.iter().enumerate() {
        let until = v.get(i + 1).map(|n| n.1).unwrap_or(end);
        let s = until.duration_since(*at).as_secs_f64();
        total += s;
        if s >= 0.05 || i + 1 < v.len() {
            println!("  {stage:<16}{s:>9.1} s");
        }
    }
    println!("  {:<16}{total:>9.1} s", "TOTAL");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(src), Some(work)) = (args.next(), args.next()) else {
        eprintln!("usage: stop_replay <pristine-session-dir> <work-dir>");
        std::process::exit(2);
    };
    let (src, work) = (PathBuf::from(src), PathBuf::from(work));
    let t0 = Instant::now();
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .format_timestamp_millis()
        .init();

    let no_optout = std::env::var_os("REPLAY_NO_OPTOUT").is_some_and(|v| v == "1");
    println!("build sha: {}", env!("DAISY_BUILD_SHA"));
    println!(
        "logical cpus: {}; has window: false (console process)",
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0)
    );
    if no_optout {
        println!("opt-out: SKIPPED (REPLAY_NO_OPTOUT=1)");
    } else {
        match daisy_llm_protocol::process_power::opt_out_of_execution_throttling() {
            Some(line) => println!("opt-out: {line}"),
            None => println!("opt-out: n/a (not windows)"),
        }
    }
    println!("power state at start: {}", power_state());

    // The app's startup environment.
    std::env::set_var("OPENBLAS_NUM_THREADS", "1");
    std::env::set_var("OMP_NUM_THREADS", "1");
    transcript::cpu::start_normal_priority_runner();
    let models = std::env::var_os("REPLAY_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models"));
    for (sub, var) in [
        ("voiceprints", "DAISY_VOICEPRINT_DIR"),
        ("whisper", "DAISY_WHISPER_MODEL_DIR"),
        ("nemotron", "DAISY_NEMOTRON_MODEL_DIR"),
        ("embeddings", "DAISY_EMBED_DIR"),
    ] {
        let d = models.join(sub);
        if d.is_dir() {
            std::env::set_var(var, &d);
        }
    }

    // Only a directory this probe made (marker present) or an empty one is
    // cleared; anything else is the caller's data.
    let marker = work.join(".stop_replay_work");
    let non_empty = std::fs::read_dir(&work).is_ok_and(|mut d| d.next().is_some());
    if non_empty && !marker.is_file() {
        eprintln!("refusing to clear {}: not empty and not made by stop_replay", work.display());
        std::process::exit(2);
    }
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(&marker, b"stop_replay work dir").unwrap();
    let id = src.file_name().unwrap().to_string_lossy().into_owned();
    let profile = ProfileDir::at(work.join("profile")).unwrap();
    let dst = profile.session_path(&id);
    copy_dir(&src, &dst);
    reset_to_stopped(&dst);
    if std::env::var_os("REPLAY_DROP_LIVE").is_some_and(|v| v == "1") {
        // No live transcript: finalize falls back to the batch full pass.
        let _ = std::fs::remove_file(dst.join("live_transcript.jsonl"));
        println!("live_transcript.jsonl removed (REPLAY_DROP_LIVE=1): full-pass path");
    }
    let app = AppState::new(profile);
    let vs = VaultState::new();

    // The recording's live diarization pass, run to its end before the
    // replay clock starts, as the recording would have run it.
    if std::env::var_os("REPLAY_LIVE_DIAR").is_some_and(|v| v == "1") {
        let t = Instant::now();
        tauri_app_core::live_diarize::start(&id, dst.clone());
        tauri_app_core::live_diarize::recording_stopped(&id);
        while !tauri_app_core::live_diarize::is_finished(&id) {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        println!("live diarize pass before the replay (REPLAY_LIVE_DIAR=1): {:.1} s", t.elapsed().as_secs_f64());
    }

    // Once a second: unix time and the Nemotron models alive in this process.
    std::thread::spawn(|| loop {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        println!(
            "REPLAY_MEM t={t} resident_models={} loads={}",
            providers_local::nemotron::model::resident(),
            providers_local::nemotron::model::loads()
        );
        std::thread::sleep(std::time::Duration::from_secs(1));
    });

    let tl = Timeline(Mutex::new(Vec::new()));
    let emit = |ev: ProgressEvent| tl.0.lock().unwrap().push((ev.stage, Instant::now()));
    let req = |skip| FinalizeRequest {
        session_id: id.clone(),
        summary_provider: None,
        model: None,
        skip_label_gate: skip,
    };
    println!("replay start (setup {:.1} s)", t0.elapsed().as_secs_f64());
    let r = finalize_and_summarize_impl(&app, &vs, req(false), emit);
    let end = Instant::now();
    match &r {
        Ok(FinalizeOutcome::Completed { .. }) => println!("outcome: completed"),
        Ok(FinalizeOutcome::NeedsLabels { clusters }) => {
            println!("outcome: needs labels ({} cluster(s))", clusters.len())
        }
        Err(e) => println!("outcome: ERROR {e}"),
    }
    table("Stop cascade (to the label gate / done)", &tl, end);
    println!("power state after cascade: {}", power_state());

    if matches!(r, Ok(FinalizeOutcome::NeedsLabels { .. }))
        && std::env::var_os("REPLAY_NO_TAIL").is_none()
    {
        let tl2 = Timeline(Mutex::new(Vec::new()));
        let emit2 = |ev: ProgressEvent| tl2.0.lock().unwrap().push((ev.stage, Instant::now()));
        let r2 = resume_finalize_impl(&app, &vs, req(true), emit2);
        let end2 = Instant::now();
        println!("tail outcome: {}", if r2.is_ok() { "ok" } else { "ERROR" });
        table("tail after labelling (summary/chapters/opus)", &tl2, end2);
    }
    println!("wall total incl. setup: {:.1} s", t0.elapsed().as_secs_f64());
    if std::env::var_os("REPLAY_KEEP").is_none() {
        drop(app);
        let _ = std::fs::remove_dir_all(&work);
    }
}
