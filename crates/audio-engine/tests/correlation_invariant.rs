//! Cross-correlation invariants over committed recorded fixtures.
//!
//! Gates that catch the "both streams capturing the same source" failure
//! class. Tests skip cleanly when fixtures are absent.

use audio_engine::correlation::{best_correlation, cross_correlation_at_lag};
use std::path::Path;

const FIXTURES_DIR: &str = "tests/fixtures";

fn read_wav_as_f32(path: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).expect("open fixture");
    let max = i16::MAX as f32;
    r.samples::<i16>()
        .map(|s| s.unwrap() as f32 / max)
        .collect()
}

#[test]
fn same_source_pair_is_highly_correlated() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES_DIR);
    let a = dir.join("same_source_30s.a.wav");
    let b = dir.join("same_source_30s.b.wav");
    if !a.exists() || !b.exists() {
        eprintln!("skipping: same_source fixtures absent at {}", dir.display());
        return;
    }
    let a_samples = read_wav_as_f32(&a);
    let b_samples = read_wav_as_f32(&b);
    // ±500 ms search window at 16 kHz = 8000 samples
    let (lag, peak) = best_correlation(&a_samples, &b_samples, 8000);
    assert!(
        peak.abs() > 0.85,
        "same-source pair must be highly correlated (got {peak:.3} at lag {lag})"
    );
}

#[test]
fn different_sources_pair_is_distinct() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES_DIR);
    let m = dir.join("different_sources_30s.mic.wav");
    let s = dir.join("different_sources_30s.sys.wav");
    if !m.exists() || !s.exists() {
        eprintln!("skipping: different_sources fixtures absent at {}", dir.display());
        return;
    }
    let m_samples = read_wav_as_f32(&m);
    let s_samples = read_wav_as_f32(&s);

    let zero_lag = cross_correlation_at_lag(&m_samples, &s_samples, 0);
    assert!(
        zero_lag.abs() < 0.5,
        "different-source pair must have low zero-lag correlation (got {zero_lag:.3})"
    );

    let (lag, peak) = best_correlation(&m_samples, &s_samples, 8000);
    assert!(
        peak.abs() < 0.7,
        "different-source pair must have low best-lag correlation \
         (got {peak:.3} at lag {lag} samples = {} ms; \
          this likely means both streams are capturing the same source)",
        lag * 1000 / 16_000
    );
}
