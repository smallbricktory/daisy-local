//! WAV decoding into the 16 kHz mono `f32` PCM that whisper.cpp wants.

use std::path::Path;

/// Decode a WAV file to mono `f32` samples in `[-1.0, 1.0]`.
///
/// The sample rate must be exactly 16 kHz (whisper.cpp expects 16 kHz input)
/// and is checked from the header before any samples are decoded; any format
/// `transcript::wav_io` decodes is accepted, multi-channel input is downmixed
/// by averaging.
pub fn decode_wav_16k_mono_f32(wav_path: &Path) -> Result<Vec<f32>, String> {
    let (sample_rate, _channels) =
        transcript::wav_io::wav_spec(wav_path).map_err(|e| e.to_string())?;
    if sample_rate != 16_000 {
        return Err(format!(
            "whisper_local expects 16 kHz audio; got {} Hz ({})",
            sample_rate,
            wav_path.display()
        ));
    }
    let (samples, _) =
        transcript::wav_io::read_wav_mono_f32(wav_path).map_err(|e| e.to_string())?;
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav_stereo_i16(path: &Path, samples: &[i16]) {
        let spec = hound::WavSpec {
            channels: 2,
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
        transcript::wav_io::write_wav_mono_i16(&path, &samples, 16_000).unwrap();

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
        write_wav_stereo_i16(&path, &samples);

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
        transcript::wav_io::write_wav_mono_i16(&path, &[0], 44_100).unwrap();

        let err = decode_wav_16k_mono_f32(&path).unwrap_err();
        assert!(err.contains("16 kHz"), "unexpected error: {err}");
    }
}
