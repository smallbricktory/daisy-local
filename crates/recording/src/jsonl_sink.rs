// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Append-only JSONL file fed through a bounded queue. Emitters hand over
//! finished lines; one writer thread owns the file and writes each line with
//! a single unbuffered `write`, so appends from another handle interleave at
//! line boundaries. A full queue drops the line and counts it. Drop closes
//! the queue and joins the writer, so the file is complete once the last
//! handle is gone.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Mutex;

/// Lines that may wait for the writer before new ones are dropped.
pub const QUEUE_LINES: usize = 8192;

pub struct JsonlSink {
    tx: Option<SyncSender<String>>,
    writer: Mutex<Option<std::thread::JoinHandle<()>>>,
    dropped: AtomicU64,
}

impl Drop for JsonlSink {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(h) = self.writer.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
    }
}

impl JsonlSink {
    /// Opens `path` for appending (created when missing) and starts the
    /// writer thread named `thread_name`.
    pub fn open(path: &Path, thread_name: &str) -> std::io::Result<Self> {
        let file = syncsafe::retry(|| std::fs::OpenOptions::new().create(true).append(true).open(path))?;
        let (tx, rx) = sync_channel::<String>(QUEUE_LINES);
        let writer = std::thread::Builder::new().name(thread_name.into()).spawn(move || {
            let mut file = file;
            while let Ok(line) = rx.recv() {
                if file.write_all(line.as_bytes()).is_err() {
                    break;
                }
            }
        })?;
        Ok(Self { tx: Some(tx), writer: Mutex::new(Some(writer)), dropped: AtomicU64::new(0) })
    }

    /// A sink that discards every line.
    pub fn disabled() -> Self {
        Self { tx: None, writer: Mutex::new(None), dropped: AtomicU64::new(0) }
    }

    pub fn is_enabled(&self) -> bool {
        self.tx.is_some()
    }

    /// Queues one line; a trailing newline is added when missing. Never
    /// blocks.
    pub fn send(&self, mut line: String) {
        let Some(tx) = &self.tx else { return };
        if !line.ends_with('\n') {
            line.push('\n');
        }
        match tx.try_send(line) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// Lines dropped on a full queue so far.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_land_in_order_and_drop_completes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.jsonl");
        let sink = JsonlSink::open(&p, "test-sink").unwrap();
        for i in 0..100 {
            sink.send(format!("{{\"i\":{i}}}"));
        }
        drop(sink);
        let text = std::fs::read_to_string(&p).unwrap();
        let got: Vec<u64> = text
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["i"].as_u64().unwrap())
            .collect();
        assert_eq!(got, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn a_disabled_sink_writes_nothing() {
        let sink = JsonlSink::disabled();
        assert!(!sink.is_enabled());
        sink.send("{}".into());
        assert_eq!(sink.dropped(), 0);
    }
}
