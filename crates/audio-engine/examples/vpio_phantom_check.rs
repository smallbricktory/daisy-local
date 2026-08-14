//! Lists mic sources while a VoiceProcessingIO capture runs, and before and
//! after it. VPIO grafts an input scope onto the output device it pins, so the
//! speakers must not turn into a microphone for the duration.
//!
//! Needs real audio devices — run it on the machine, not in CI:
//!   cargo run -p audio-engine --example vpio_phantom_check

use audio_engine::{capture_one, list_sources, SourceKind};
use std::time::Duration;

fn mics(label: &str) -> Vec<String> {
    let names: Vec<String> = list_sources()
        .expect("list sources")
        .into_iter()
        .filter(|s| s.kind == SourceKind::Mic)
        .map(|s| format!("{} (id {})", s.description, s.id))
        .collect();
    println!("── mics, {label} ──");
    for n in &names {
        println!("   {n}");
    }
    println!();
    names
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let before = mics("no capture running");
    let mic = list_sources()
        .expect("list sources")
        .into_iter()
        .find(|s| s.kind == SourceKind::Mic)
        .expect("no microphone on this machine");
    println!("capturing 4s from {} (id {})\n", mic.description, mic.id);

    let out = std::env::temp_dir().join("vpio-phantom-check.wav");
    let handle = std::thread::spawn(move || capture_one(&mic, Duration::from_secs(4), &out));

    std::thread::sleep(Duration::from_millis(1500));
    let during = mics("capture RUNNING");

    let voice_processed = handle.join().expect("capture thread").expect("capture");
    println!("capture was voice-processed (VPIO): {voice_processed}\n");
    std::thread::sleep(Duration::from_millis(1500));
    let after = mics("capture finished");

    let appeared: Vec<&String> = during.iter().filter(|n| !before.contains(n)).collect();
    if !voice_processed {
        println!("INCONCLUSIVE: capture fell back to AUHAL, so no VPIO output was pinned.");
        std::process::exit(2);
    }
    if appeared.is_empty() && before == after {
        println!("PASS: the mic list is unchanged while VPIO runs.");
    } else {
        for n in &appeared {
            println!("FAIL: appeared only while VPIO ran: {n}");
        }
        if before != after {
            println!("FAIL: the list did not return to its starting state");
        }
        std::process::exit(1);
    }
}
