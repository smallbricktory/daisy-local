// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Prints the processes holding the microphone open, once a second.
//!
//! Needs real audio devices and another app on the mic — run it on the
//! machine, not in CI:
//!   cargo run -p audio-engine --example mic_clients

use audio_engine::mic_clients::{mic_clients, mic_clients_support, MicClientsSupport};
use std::time::Duration;

fn main() {
    match mic_clients_support() {
        MicClientsSupport::Yes => println!("support: yes"),
        MicClientsSupport::No(reason) => {
            println!("support: no ({reason})");
            return;
        }
    }
    for _ in 0..30 {
        let clients = mic_clients();
        if clients.is_empty() {
            println!("— nothing on the mic —");
        } else {
            for c in &clients {
                println!("{} (pid {})", c.name, c.pid);
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
