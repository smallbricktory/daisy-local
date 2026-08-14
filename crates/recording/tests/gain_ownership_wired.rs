//! The auto-gain tap must not write the hardware input gain while the OS
//! voice-processes the capture. No test here runs on voice-processed
//! capture, so this reads the source for the gate instead.

use std::fs;

const PIPELINE: &str = "src/live_pipeline.rs";

/// Body of `autogain_tap`, from its signature to the next top-level item.
fn autogain_tap_body(src: &str) -> String {
    let start = src
        .find("async fn autogain_tap(")
        .expect("autogain_tap has been renamed — update this guardrail");
    let rest = &src[start..];
    let end = rest[1..]
        .find("\nasync fn ")
        .or_else(|| rest[1..].find("\nfn "))
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
fn the_auto_gain_tap_consults_the_capture_backend() {
    let src = fs::read_to_string(PIPELINE).unwrap_or_else(|e| panic!("read {PIPELINE}: {e}"));
    let body = autogain_tap_body(&src);
    // The read, not the parameter: taking the flag and ignoring it is the
    // failure this exists to catch.
    assert!(
        body.contains("mic_voice_processed.load("),
        "autogain_tap no longer reads mic_voice_processed: it will fight \
         the OS AGC and walk the input gain to the floor on a \
         voice-processed capture",
    );
    assert!(
        body.contains("restore_gain"),
        "autogain_tap no longer restores the user's input gain — a stepped \
         gain would outlive the recording",
    );
}
