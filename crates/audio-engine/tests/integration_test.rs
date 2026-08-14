//! End-to-end distinctness gate against the meeting fixture:
//!   - zero-lag correlation < 0.5
//!   - best-lag correlation in ±500 ms < 0.7
//!
//! over a ~2-minute meeting recorded with --virtual-sink.

use audio_engine::correlation::{best_correlation, cross_correlation_at_lag};
use std::path::Path;

const FIXTURES_DIR: &str = "tests/fixtures";

fn read_wav_as_f32(path: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).expect("open fixture");
    let max = i16::MAX as f32;
    r.samples::<i16>().map(|s| s.unwrap() as f32 / max).collect()
}

#[test]
fn meeting_fixture_passes_distinctness_gate() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES_DIR);
    let m = dir.join("meeting_2min.mic.wav");
    let s = dir.join("meeting_2min.sys.wav");
    if !m.exists() || !s.exists() {
        eprintln!(
            "skipping: meeting_2min fixtures absent at {} — record per fixtures/README.md",
            dir.display()
        );
        return;
    }
    let m_samples = read_wav_as_f32(&m);
    let s_samples = read_wav_as_f32(&s);

    // Gate:
    //   zero-lag correlation < 0.5
    //   best-lag correlation in ±500 ms < 0.7
    let zero = cross_correlation_at_lag(&m_samples, &s_samples, 0);
    assert!(
        zero.abs() < 0.5,
        "MVP gate violated: zero-lag corr = {zero:.3} (must be < 0.5)"
    );

    let (lag, peak) = best_correlation(&m_samples, &s_samples, 8000);
    assert!(
        peak.abs() < 0.7,
        "MVP gate violated: best-lag corr = {peak:.3} at lag {lag} samples \
         = {} ms (must be < 0.7)",
        lag * 1000 / 16_000
    );
}

#[test]
fn meeting_fixture_has_meaningful_signal() {
    // Both channels have non-trivial RMS; the gate is not measured against
    // silence.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES_DIR);
    let m = dir.join("meeting_2min.mic.wav");
    let s = dir.join("meeting_2min.sys.wav");
    if !m.exists() || !s.exists() {
        return;
    }
    for path in [&m, &s] {
        let samples = read_wav_as_f32(path);
        let rms = (samples.iter().map(|x| x * x).sum::<f32>() / samples.len() as f32).sqrt();
        assert!(
            rms > 0.001,
            "fixture {} has near-zero RMS ({rms:.4}) — recording was silent",
            path.display()
        );
    }
}
