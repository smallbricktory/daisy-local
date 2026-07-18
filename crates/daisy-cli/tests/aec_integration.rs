//! Integration test: AEC reduces mic-vs-system correlation on the meeting
//! fixture.

use audio_engine::correlation::best_correlation;
use std::path::Path;

fn read_wav_f32(path: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).expect("open");
    let max = i16::MAX as f32;
    r.samples::<i16>().map(|s| s.unwrap() as f32 / max).collect()
}

#[test]
fn aec_reduces_mic_vs_system_correlation() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mic = fixtures.join("meeting_2min.mic.wav");
    let sys = fixtures.join("meeting_2min.sys.wav");
    if !mic.exists() || !sys.exists() {
        eprintln!("skipping: meeting_2min fixtures absent");
        return;
    }
    if !aec::model_dir().join("model_256_1.onnx").exists() {
        eprintln!("skipping: AEC models absent");
        return;
    }
    // Run daisy aec to produce mic_aec.wav
    let temp = tempfile::tempdir().unwrap();
    let mic_aec = temp.path().join("mic_aec.wav");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_daisy"))
        .args([
            "aec",
            "--mic",
            mic.to_str().unwrap(),
            "--far",
            sys.to_str().unwrap(),
            "--out",
            mic_aec.to_str().unwrap(),
        ])
        .status()
        .expect("run daisy aec");
    assert!(status.success());

    let mic_samples = read_wav_f32(&mic);
    let sys_samples = read_wav_f32(&sys);
    let aec_samples = read_wav_f32(&mic_aec);
    let n = mic_samples.len().min(aec_samples.len()).min(sys_samples.len());

    // Best-lag correlation in ±500 ms = ±8000 samples
    let (_, raw_corr) = best_correlation(&mic_samples[..n], &sys_samples[..n], 8000);
    let (_, aec_corr) = best_correlation(&aec_samples[..n], &sys_samples[..n], 8000);

    eprintln!("raw mic vs sys best-lag corr:  {raw_corr:.3}");
    eprintln!("AEC mic vs sys best-lag corr:  {aec_corr:.3}");

    // AEC must reduce best-lag correlation; no specific dB target is enforced.
    assert!(
        aec_corr.abs() < raw_corr.abs(),
        "AEC did NOT reduce mic-vs-system correlation: raw={raw_corr:.3} AEC={aec_corr:.3}"
    );
}
