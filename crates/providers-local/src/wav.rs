//! WAV decoding into the 16 kHz mono `f32` PCM that whisper.cpp wants.

use std::path::Path;

/// Decode a WAV file to mono `f32` samples in `[-1.0, 1.0]`.
///
/// Requirements (errors otherwise):
/// - sample rate must be exactly 16 kHz (whisper.cpp expects 16 kHz input)
/// - format must be 16-bit signed int PCM or 32-bit float PCM
/// - 1 or 2 channels (stereo is downmixed by averaging L/R)
pub fn decode_wav_16k_mono_f32(wav_path: &Path) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(wav_path)
        .map_err(|e| format!("open WAV {}: {e}", wav_path.display()))?;
    let spec = reader.spec();

    if spec.sample_rate != 16_000 {
        return Err(format!(
            "whisper_local expects 16 kHz audio; got {} Hz ({})",
            spec.sample_rate,
            wav_path.display()
        ));
    }
    if spec.channels == 0 || spec.channels > 2 {
        return Err(format!(
            "whisper_local expects mono or stereo audio; got {} channels ({})",
            spec.channels,
            wav_path.display()
        ));
    }

    // Read interleaved samples as f32 in [-1.0, 1.0].
    let interleaved: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32_768.0))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("decode i16 samples from {}: {e}", wav_path.display()))?,
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("decode f32 samples from {}: {e}", wav_path.display()))?,
        (fmt, bits) => {
            return Err(format!(
                "whisper_local expects 16-bit int or 32-bit float WAV; got {fmt:?}/{bits}-bit ({})",
                wav_path.display()
            ))
        }
    };

    if spec.channels == 1 {
        return Ok(interleaved);
    }

    // Stereo -> mono by averaging each L/R pair; an odd trailing sample is
    // dropped.
    let mono = interleaved
        .chunks_exact(2)
        .map(|lr| (lr[0] + lr[1]) * 0.5)
        .collect();
    Ok(mono)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav_i16(path: &Path, channels: u16, samples: &[i16]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: 16_000,
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
    fn decodes_mono_i16_to_f32_in_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mono.wav");
        let samples: [i16; 6] = [0, i16::MAX, i16::MIN, 16_384, -16_384, 100];
        write_wav_i16(&path, 1, &samples);

        let out = decode_wav_16k_mono_f32(&path).unwrap();
        assert_eq!(out.len(), samples.len());
        for v in &out {
            assert!((-1.0..=1.0).contains(v), "sample {v} out of range");
        }
        assert!((out[0]).abs() < 1e-9);
        assert!((out[1] - (i16::MAX as f32 / 32_768.0)).abs() < 1e-6);
        assert!((out[2] - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn downmixes_stereo_to_mono() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stereo.wav");
        // Interleaved L/R pairs: (1000,-1000)->0, (2000,2000)->2000, (10,30)->20
        let samples: [i16; 6] = [1000, -1000, 2000, 2000, 10, 30];
        write_wav_i16(&path, 2, &samples);

        let out = decode_wav_16k_mono_f32(&path).unwrap();
        assert_eq!(out.len(), 3);
        assert!(out[0].abs() < 1e-6);
        assert!((out[1] - (2000.0 / 32_768.0)).abs() < 1e-6);
        assert!((out[2] - (20.0 / 32_768.0)).abs() < 1e-6);
    }

    #[test]
    fn rejects_wrong_sample_rate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("44k.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        w.write_sample(0i16).unwrap();
        w.finalize().unwrap();

        let err = decode_wav_16k_mono_f32(&path).unwrap_err();
        assert!(err.contains("16 kHz"), "unexpected error: {err}");
    }
}
