use recording::compress::{encode_mono_pcm16, CompressParams};

fn looks_like_ogg(bytes: &[u8]) -> bool {
    bytes.starts_with(b"OggS")
}

fn tone(samples: usize, rate: u32, hz: f32) -> Vec<i16> {
    (0..samples)
        .map(|i| {
            let t = i as f32 / rate as f32;
            ((2.0 * std::f32::consts::PI * hz * t).sin() * 12_000.0) as i16
        })
        .collect()
}

#[test]
fn encodes_mono_16k_pcm_to_opus() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("a.opus");
    let pcm = tone(16_000, 16_000, 440.0); // 1 second
    let n = encode_mono_pcm16(&pcm, 16_000, &CompressParams::default(), &out).unwrap();
    assert!(n > 100, "opus suspiciously small: {n}");
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(bytes.len() as u64, n);
    assert!(
        looks_like_ogg(&bytes),
        "no OggS magic: {:02X?}",
        &bytes[..8.min(bytes.len())]
    );
    // 1 s of 16-bit 16k mono PCM = 32_000 bytes; voice Opus must be much smaller.
    assert!(n < 16_000, "opus not smaller than raw PCM/2: {n}");
}

#[test]
fn accepts_various_bitrates() {
    let dir = tempfile::tempdir().unwrap();
    let pcm = tone(8_000, 16_000, 330.0);
    for kbps in [16u32, 20, 24, 32, 48] {
        let out = dir.path().join(format!("b{kbps}.opus"));
        let n = encode_mono_pcm16(&pcm, 16_000, &CompressParams { bitrate_kbps: kbps }, &out).unwrap();
        assert!(n > 0);
        assert!(looks_like_ogg(&std::fs::read(&out).unwrap()));
    }
}

#[test]
fn rejects_empty_input() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("e.opus");
    assert!(encode_mono_pcm16(&[], 16_000, &CompressParams::default(), &out).is_err());
}

#[test]
fn rejects_non_16khz_input() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("badrate.opus");
    let pcm = tone(8_000, 8_000, 200.0);
    let err = encode_mono_pcm16(&pcm, 8_000, &CompressParams::default(), &out).unwrap_err();
    assert!(format!("{err}").contains("16000 Hz"), "wrong error: {err}");
}
