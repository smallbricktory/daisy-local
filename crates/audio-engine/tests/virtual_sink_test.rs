//! Virtual-sink lifecycle test. Requires running PipeWire with PA shim.

use audio_engine::virtual_sink::VirtualSink;
use std::process::Command;

fn pulse_available() -> bool {
    Command::new("pactl")
        .arg("info")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn sink_exists(name: &str) -> bool {
    let output = Command::new("pactl")
        .args(["list", "sinks", "short"])
        .output()
        .expect("pactl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().any(|l| l.split_whitespace().nth(1) == Some(name))
}

#[test]
fn virtual_sink_creates_and_destroys() {
    if !pulse_available() {
        eprintln!("skipping: pactl not available");
        return;
    }
    let name = "daisy-capture-test-create-destroy";

    {
        let sink = VirtualSink::create(name).expect("create");
        assert!(sink_exists(name), "sink should exist while VirtualSink is alive");
        assert!(!sink.monitor_source_name().is_empty());
    }

    // Give pactl a beat to settle
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!sink_exists(name), "sink must be destroyed after Drop");
}

#[test]
fn virtual_sink_dispose_is_idempotent() {
    if !pulse_available() {
        eprintln!("skipping: pactl not available");
        return;
    }
    let name = "daisy-capture-test-dispose";
    let mut sink = VirtualSink::create(name).expect("create");
    sink.dispose().expect("dispose 1");
    sink.dispose().expect("dispose 2 should be a no-op");
}

#[test]
fn virtual_sink_monitor_source_appears_in_list_sources() {
    if !pulse_available() {
        eprintln!("skipping: pactl not available");
        return;
    }
    let name = "daisy-capture-test-listed";
    let _sink = VirtualSink::create(name).expect("create");
    // Give PW a beat to register the new sink globally
    std::thread::sleep(std::time::Duration::from_millis(300));

    let sources = audio_engine::list_sources().expect("list sources");
    let monitor_name = format!("{}.monitor", name);
    let found = sources.iter().any(|s| s.node_name == monitor_name);
    assert!(
        found,
        "expected to find monitor source {monitor_name} in {} sources: {:?}",
        sources.len(),
        sources.iter().map(|s| &s.node_name).collect::<Vec<_>>()
    );
}
