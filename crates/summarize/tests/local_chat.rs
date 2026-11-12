// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use summarize::chat::ChatCompleter;
use summarize::local::{LocalChat, LocalCompleter};
use summarize::SummarizeError;

fn chat() -> LocalChat {
    LocalChat::new(PathBuf::from(env!("CARGO_BIN_EXE_fake-llm-worker")), PathBuf::from("model.gguf"), 4096)
        .with_timeout(Duration::from_secs(2))
}

fn completer(chat: &Arc<LocalChat>) -> LocalCompleter {
    LocalCompleter::new(chat.clone(), 512)
}

#[test]
fn a_schema_request_returns_the_parsed_object() {
    let c = chat();
    let v = c.complete_schema("sys", "meeting text", &serde_json::json!({"type":"object"}), 512).unwrap();
    assert_eq!(v["tldr"], "fake");
}

#[test]
fn two_requests_share_one_worker() {
    let c = chat();
    c.complete_schema("sys", "one", &serde_json::json!({}), 512).unwrap();
    c.complete_schema("sys", "two", &serde_json::json!({}), 512).unwrap();
    // The fake worker's "free" reply carries its own pid, so a repeat
    // makes worker reuse observable: same pid means the same process
    // answered both calls, not a fresh spawn each time.
    let t1 = c.complete_text("", &[("user".into(), "free".into())], 16).unwrap();
    let t2 = c.complete_text("", &[("user".into(), "free".into())], 16).unwrap();
    assert!(t1.starts_with("hello from fake "), "{t1}");
    assert_eq!(t1, t2, "two calls on one LocalChat should hit the same worker process");

    // Killing the worker and calling again proves the opposite: a fresh
    // process, so a different pid.
    c.complete_schema("sys", "die", &serde_json::json!({}), 512).unwrap_err();
    let t3 = c.complete_text("", &[("user".into(), "free".into())], 16).unwrap();
    assert_ne!(t1, t3, "a respawned worker is a different process");
}

#[test]
fn two_completers_over_one_shared_instance_see_one_worker() {
    let shared = Arc::new(chat());
    let chapters = completer(&shared);
    let analysis = LocalCompleter::new(shared.clone(), 256);
    chapters.complete_json("sys", "one", "t", &serde_json::json!({})).unwrap();
    analysis.complete_json("sys", "two", "t", &serde_json::json!({})).unwrap();
    // Both completers hold the same Arc, so the pid the fake worker echoes is
    // the same process for the free-text call that follows either of them.
    let a = shared.complete_text("", &[("user".into(), "free".into())], 16).unwrap();
    let b = shared.complete_text("", &[("user".into(), "free".into())], 16).unwrap();
    assert!(a.starts_with("hello from fake "), "{a}");
    assert_eq!(a, b, "one shared instance is one worker process");
}

#[test]
fn an_idle_worker_is_released_and_the_next_call_respawns() {
    let c = chat().with_idle(Duration::from_millis(200));
    let first = c.complete_text("", &[("user".into(), "free".into())], 16).unwrap();
    assert!(c.has_worker());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while c.has_worker() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!c.has_worker(), "the idle watcher should have killed the worker");
    let second = c.complete_text("", &[("user".into(), "free".into())], 16).unwrap();
    assert_ne!(first, second, "the respawned worker is a different process");
}

#[test]
fn close_ends_the_worker_and_refuses_every_later_call() {
    let c = chat();
    let first = c.complete_text("", &[("user".into(), "free".into())], 16).unwrap();
    let pid: u32 = first.rsplit(' ').next().unwrap().parse().unwrap();
    assert!(c.has_worker());
    c.close();
    assert!(!c.has_worker());
    #[cfg(unix)]
    assert!(!process_is_running(pid), "close returns once the process has exited");
    let _ = pid;
    let e = c.complete_text("", &[("user".into(), "free".into())], 16).unwrap_err();
    assert!(matches!(e, SummarizeError::Worker(_)), "{e}");
    assert!(!c.has_worker(), "a closed chat spawns no worker");
}

#[test]
fn close_ends_a_request_in_flight() {
    let c = Arc::new(chat().with_timeout(Duration::from_secs(60)));
    c.complete_text("", &[("user".into(), "free".into())], 16).unwrap();
    let caller = c.clone();
    let request = std::thread::spawn(move || caller.complete_text("", &[("user".into(), "hang".into())], 16));
    std::thread::sleep(Duration::from_millis(500));
    let started = std::time::Instant::now();
    c.close();
    let e = request.join().unwrap().unwrap_err();
    assert!(matches!(e, SummarizeError::Worker(_)), "{e}");
    assert!(started.elapsed() < Duration::from_secs(10), "close waited for the request: {:?}", started.elapsed());
    assert!(!c.has_worker());
}

#[test]
fn an_error_reply_surfaces_as_worker_error() {
    let e = chat().complete_schema("sys", "error", &serde_json::json!({}), 512).unwrap_err();
    assert!(matches!(e, SummarizeError::Worker(ref m) if m == "fake failure"), "{e}");
    assert!(e.to_string().starts_with("Built-in model failed:"));
}

#[test]
fn a_dead_worker_errors_and_the_next_call_respawns() {
    let c = chat();
    let e = c.complete_schema("sys", "die", &serde_json::json!({}), 512).unwrap_err();
    assert!(matches!(e, SummarizeError::Worker(_)), "{e}");
    let v = c.complete_schema("sys", "again", &serde_json::json!({}), 512).unwrap();
    assert_eq!(v["tldr"], "fake");
}

#[test]
fn a_hung_worker_times_out() {
    let e = chat().complete_schema("sys", "hang", &serde_json::json!({}), 512).unwrap_err();
    assert!(e.to_string().contains("no reply within 2 s"), "{e}");
}

/// A process is alive when `ps` reports a state that is not a reaped zombie.
#[cfg(unix)]
fn process_is_running(pid: u32) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
    !state.is_empty() && !state.starts_with('Z')
}

/// A request that times out ends its worker process, even while a second
/// caller queued behind it still holds a handle. The slot is empty after the
/// timeout, so the next call spawns a second process; the timed-out one must
/// not still be holding a copy of the weights.
#[cfg(unix)]
#[test]
fn a_timed_out_request_leaves_no_second_worker_holding_the_weights() {
    let c = Arc::new(chat());
    let greeting = c.complete_text("", &[("user".into(), "free".into())], 16).unwrap();
    let first: u32 = greeting.rsplit(' ').next().unwrap().parse().unwrap();
    assert!(process_is_running(first), "the fake worker should be running");

    let hung = c.clone();
    let timing_out = std::thread::spawn(move || hung.complete_text("", &[("user".into(), "hang".into())], 16));
    std::thread::sleep(Duration::from_millis(300));
    let queued = c.clone();
    let behind = std::thread::spawn(move || queued.complete_text("", &[("user".into(), "hang".into())], 16));

    assert!(timing_out.join().unwrap().is_err(), "the first request times out");
    let second: u32 = c
        .complete_text("", &[("user".into(), "free".into())], 16)
        .unwrap()
        .rsplit(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(first, second, "the timed-out worker was retired and a fresh one spawned");
    assert!(
        !process_is_running(first),
        "the timed-out worker is still alive next to its replacement: two copies of the weights"
    );
    let _ = behind.join().unwrap();
}

#[test]
fn a_missing_worker_binary_is_a_worker_error() {
    let c = LocalChat::new(PathBuf::from("/nonexistent/daisy-llm"), PathBuf::from("m"), 4096);
    let e = c.complete_schema("s", "u", &serde_json::json!({}), 64).unwrap_err();
    assert!(matches!(e, SummarizeError::Worker(_)), "{e}");
}
