use audio_engine::wav::WavFrameWriter;
use std::path::PathBuf;
use tempfile::tempdir;

fn synthetic_sine_i16(seconds: f64, hz: f64, sr: u32) -> Vec<i16> {
    let n = (seconds * sr as f64) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / sr as f64;
            (0.3 * (2.0 * std::f64::consts::PI * hz * t).sin() * 32767.0) as i16
        })
        .collect()
}

#[test]
fn writer_persists_audio_to_wav() {
    let dir = tempdir().unwrap();
    let path: PathBuf = dir.path().join("out.wav");
    let pcm = synthetic_sine_i16(2.0, 440.0, 16_000);

    let mut w = WavFrameWriter::create(&path, 16_000).unwrap();
    let frame = 320; // 20 ms
    for chunk in pcm.chunks(frame) {
        if chunk.len() == frame {
            w.write_frame(chunk).unwrap();
        }
    }
    w.close().unwrap();

    let mut r = hound::WavReader::open(&path).unwrap();
    let spec = r.spec();
    assert_eq!(spec.sample_rate, 16_000);
    assert_eq!(spec.channels, 1);
    assert_eq!(spec.bits_per_sample, 16);
    let decoded: Vec<i16> = r.samples::<i16>().map(|s| s.unwrap()).collect();
    assert!(decoded.len() >= pcm.len() - frame);
    assert!(decoded.len() <= pcm.len());
}

#[test]
fn writer_rejects_wrong_frame_size() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bad.wav");
    let mut w = WavFrameWriter::create(&path, 16_000).unwrap();
    let bad = vec![0i16; 100];
    let err = w.write_frame(&bad).unwrap_err();
    assert!(format!("{err}").contains("frame"));
}

#[test]
fn partial_file_remains_decodable_after_close() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("partial.wav");
    let pcm = vec![0i16; 320 * 5];

    let mut w = WavFrameWriter::create(&path, 16_000).unwrap();
    for chunk in pcm.chunks(320) {
        w.write_frame(chunk).unwrap();
    }
    w.close().unwrap();

    let r = hound::WavReader::open(&path);
    assert!(r.is_ok(), "partial WAV should decode after close");
}
