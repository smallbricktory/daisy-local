// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Logs who takes and releases the microphone, with a local timestamp on
//! every change. Runs until Ctrl-C, or for the given number of seconds.
//!
//! Needs real audio devices and another app on the mic — run it on the
//! machine, not in CI:
//!   cargo run -p audio-engine --example mic_clients [-- <seconds>]

use audio_engine::mic_clients::{mic_clients, mic_clients_support, MicClientsSupport};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

fn stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn main() {
    match mic_clients_support() {
        MicClientsSupport::Yes => println!("{} support: yes", stamp()),
        MicClientsSupport::No(reason) => {
            println!("{} support: no ({reason})", stamp());
            return;
        }
    }
    let limit = std::env::args()
        .nth(1)
        .and_then(|a| a.parse::<u64>().ok())
        .map(Duration::from_secs);
    let start = Instant::now();
    let mut before: Option<BTreeSet<String>> = None;
    while limit.is_none_or(|l| start.elapsed() < l) {
        let now: BTreeSet<String> = mic_clients()
            .iter()
            .map(|c| format!("{} (pid {})", c.name, c.pid))
            .collect();
        match &before {
            None => {
                if now.is_empty() {
                    println!("{} nothing on the mic", stamp());
                }
                for c in &now {
                    println!("{} on the mic: {c}", stamp());
                }
            }
            Some(prev) => {
                for c in now.difference(prev) {
                    println!("{} joined: {c}", stamp());
                }
                for c in prev.difference(&now) {
                    println!("{} left: {c}", stamp());
                }
                if now.is_empty() && !prev.is_empty() {
                    println!("{} nothing on the mic", stamp());
                }
            }
        }
        before = Some(now);
        std::thread::sleep(Duration::from_secs(1));
    }
}
