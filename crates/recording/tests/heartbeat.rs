// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use recording::heartbeat::Heartbeat;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn write_then_read_returns_pid_and_recent_age() {
    let td = TempDir::new().unwrap();
    let path = td.path().join("hb");

    let hb = Heartbeat::create(&path).unwrap();
    let snap = Heartbeat::read(&path).unwrap();
    assert_eq!(snap.pid, std::process::id());
    assert!(snap.age_seconds() < 5);
    drop(hb);
}

#[test]
fn heartbeat_records_the_writing_host() {
    let td = TempDir::new().unwrap();
    let path = td.path().join("hb");
    let _hb = Heartbeat::create(&path).unwrap();
    let snap = Heartbeat::read(&path).unwrap();
    assert_eq!(snap.host.as_deref(), Some(recording::heartbeat::this_host()));
    assert!(snap.written_here() || recording::heartbeat::this_host().is_empty());
}

#[test]
fn two_line_heartbeat_reads_with_no_host() {
    let td = TempDir::new().unwrap();
    let path = td.path().join("hb");
    std::fs::write(&path, "4242\n1790887989\n").unwrap();
    let snap = Heartbeat::read(&path).unwrap();
    assert_eq!((snap.pid, snap.last_update_unix), (4242, 1_790_887_989));
    assert_eq!(snap.host, None);
    assert!(!snap.written_here());
}

#[test]
fn touch_advances_mtime() {
    let td = TempDir::new().unwrap();
    let path = td.path().join("hb");
    let hb = Heartbeat::create(&path).unwrap();
    let before = Heartbeat::read(&path).unwrap().last_update_unix;

    thread::sleep(Duration::from_millis(1100));
    hb.touch().unwrap();

    let after = Heartbeat::read(&path).unwrap().last_update_unix;
    assert!(after > before, "expected mtime to advance: before={before} after={after}");
}

#[test]
fn missing_file_reads_as_dead() {
    let td = TempDir::new().unwrap();
    let path = td.path().join("nope");
    assert!(Heartbeat::read(&path).is_err());
    assert!(!Heartbeat::is_alive(&path, 5));
}

#[test]
fn stale_heartbeat_not_alive() {
    let td = TempDir::new().unwrap();
    let path = td.path().join("hb");
    let _hb = Heartbeat::create(&path).unwrap();
    // Threshold of 0 means "any age is stale".
    assert!(!Heartbeat::is_alive(&path, 0));
}
