use std::f32::consts::PI;
use transcript::rms::{is_silent_wav, rms_dbfs_window, WavSamples};

fn write_wav_at(path: &std::path::Path, samples: &[i16], sample_rate: u32) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for &s in samples {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
}

#[test]
fn silence_is_very_negative_dbfs() {
    let td = tempfile::TempDir::new().unwrap();
    let path = td.path().join("silence.wav");
    let samples = vec![0_i16; 16_000]; // 1 second of zeros
    write_wav_at(&path, &samples, 16_000);

    let wav = WavSamples::load(&path).unwrap();
    let db = rms_dbfs_window(&wav, 0, 1000);
    assert!(db < -90.0, "silence should be near -inf dBFS, got {db}");
}

#[test]
fn full_scale_sine_is_near_minus_three_dbfs() {
    // A sine wave at amplitude i16::MAX has RMS = max/sqrt(2),
    // which is about -3 dBFS.
    let td = tempfile::TempDir::new().unwrap();
    let path = td.path().join("sine.wav");
    let samples: Vec<i16> = (0..16_000)
        .map(|i| {
            let phase = (i as f32 / 16_000.0) * 2.0 * PI * 440.0;
            (i16::MAX as f32 * phase.sin()) as i16
        })
        .collect();
    write_wav_at(&path, &samples, 16_000);

    let wav = WavSamples::load(&path).unwrap();
    let db = rms_dbfs_window(&wav, 0, 1000);
    assert!(db > -4.0 && db < -2.0, "expected ~-3 dBFS, got {db}");
}

#[test]
fn out_of_range_window_clamped() {
    let td = tempfile::TempDir::new().unwrap();
    let path = td.path().join("short.wav");
    write_wav_at(&path, &vec![100_i16; 100], 16_000); // only 100 samples
    let wav = WavSamples::load(&path).unwrap();
    // Request a 1-second window; only 100 samples available — function clamps.
    let db = rms_dbfs_window(&wav, 0, 1000);
    assert!(db.is_finite(), "should not return NaN/inf for short windows");
}

#[test]
fn empty_window_returns_minus_inf() {
    let td = tempfile::TempDir::new().unwrap();
    let path = td.path().join("ok.wav");
    write_wav_at(&path, &vec![100_i16; 1000], 16_000);
    let wav = WavSamples::load(&path).unwrap();
    let db = rms_dbfs_window(&wav, 500, 500); // start == end
    assert!(db <= -120.0, "empty window should be near -inf, got {db}");
}

#[test]
fn is_silent_wav_returns_true_for_silence() {
    let td = tempfile::TempDir::new().unwrap();
    let path = td.path().join("silent.wav");
    write_wav_at(&path, &vec![0_i16; 16_000], 16_000);
    let result = is_silent_wav(&path).unwrap();
    assert!(result, "all-zero WAV should be detected as silent");
}

#[test]
fn is_silent_wav_returns_false_for_loud_sine() {
    let td = tempfile::TempDir::new().unwrap();
    let path = td.path().join("loud.wav");
    let samples: Vec<i16> = (0..16_000)
        .map(|i| {
            let phase = (i as f32 / 16_000.0) * 2.0 * PI * 440.0;
            (i16::MAX as f32 * phase.sin()) as i16
        })
        .collect();
    write_wav_at(&path, &samples, 16_000);
    let result = is_silent_wav(&path).unwrap();
    assert!(!result, "full-scale sine WAV should not be detected as silent");
}
