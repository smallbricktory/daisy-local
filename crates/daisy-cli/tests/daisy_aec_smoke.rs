//! Smoke test for `daisy aec` against the meeting_2min fixture.

use std::path::Path;

#[test]
fn daisy_aec_produces_nonsilent_output() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mic = fixtures.join("meeting_2min.mic.wav");
    let far = fixtures.join("meeting_2min.sys.wav");
    if !mic.exists() || !far.exists() {
        eprintln!("skipping: meeting_2min fixtures absent");
        return;
    }
    if !aec::model_dir().join("model_256_1.onnx").exists() {
        eprintln!("skipping: AEC models absent (run models/dtln-aec/download.sh 256)");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("aec.wav");

    let status = std::process::Command::new(env!("CARGO_BIN_EXE_daisy"))
        .args([
            "aec",
            "--mic",
            mic.to_str().unwrap(),
            "--far",
            far.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .status()
        .expect("run daisy aec");
    assert!(status.success(), "daisy aec exited non-zero");
    assert!(out.exists());

    let mut r = hound::WavReader::open(&out).unwrap();
    let samples: Vec<i16> = r.samples::<i16>().map(|s| s.unwrap()).collect();
    let n = samples.len() as f32;
    let rms = (samples
        .iter()
        .map(|&s| (s as f32).powi(2))
        .sum::<f32>()
        / n.max(1.0))
    .sqrt();
    eprintln!(
        "daisy aec produced {} samples, RMS={rms:.2}",
        samples.len()
    );
    assert!(
        rms > 1.0,
        "AEC output near-silent (RMS={rms:.2}); over-suppression likely"
    );
}
