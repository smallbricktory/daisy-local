//! Whole-file WAV decode/encode: readers return mono samples, downmixing
//! multi-channel input by averaging. Accepts 16/24/32-bit int and f32
//! sample formats; the writer emits mono 16-bit PCM.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
#[error("wav {path}: {source}")]
pub struct WavIoError {
    pub path: PathBuf,
    #[source]
    pub source: hound::Error,
}

impl From<WavIoError> for crate::error::TranscriptError {
    fn from(e: WavIoError) -> Self {
        crate::error::TranscriptError::Io {
            path: e.path,
            source: std::io::Error::other(e.source.to_string()),
        }
    }
}

fn wav_err(path: &Path, source: hound::Error) -> WavIoError {
    WavIoError { path: path.to_path_buf(), source }
}

/// Collect decoded samples, salvaging the valid prefix of a torn file: a
/// mid-stream decode error after at least one good sample ends the read
/// (crash-truncated tails), while an error before any sample propagates.
fn collect_salvage<T>(
    iter: impl Iterator<Item = Result<T, hound::Error>>,
    path: &Path,
) -> Result<Vec<T>, WavIoError> {
    let mut out = Vec::new();
    for s in iter {
        match s {
            Ok(v) => out.push(v),
            Err(e) if out.is_empty() => return Err(wav_err(path, e)),
            Err(e) => {
                log::warn!("wav {}: truncated tail ({e}); kept {} samples", path.display(), out.len());
                break;
            }
        }
    }
    Ok(out)
}

/// Read only the WAV header: `(sample_rate, channels)`. No samples are
/// decoded.
pub fn wav_spec(path: &Path) -> Result<(u32, u16), WavIoError> {
    let r = hound::WavReader::open(path).map_err(|e| wav_err(path, e))?;
    let spec = r.spec();
    Ok((spec.sample_rate, spec.channels))
}

/// Read a WAV whole as mono `f32` in `[-1, 1]`. Returns `(samples, sample_rate)`.
pub fn read_wav_mono_f32(path: &Path) -> Result<(Vec<f32>, u32), WavIoError> {
    let mut r = hound::WavReader::open(path).map_err(|e| wav_err(path, e))?;
    let spec = r.spec();
    let interleaved = decode_f32(&mut r, path)?;
    Ok((downmix_f32(interleaved, spec.channels), spec.sample_rate))
}

/// Read a WAV whole as mono `i16`. Returns `(samples, sample_rate)`.
/// Mono 16-bit int input passes through bit-exactly; other formats are
/// scaled to the i16 range.
pub fn read_wav_mono_i16(path: &Path) -> Result<(Vec<i16>, u32), WavIoError> {
    let mut r = hound::WavReader::open(path).map_err(|e| wav_err(path, e))?;
    let spec = r.spec();
    if spec.sample_format == hound::SampleFormat::Int && spec.bits_per_sample == 16 {
        let raw: Vec<i16> = collect_salvage(r.samples::<i16>(), path)?;
        return Ok((downmix_i16(raw, spec.channels), spec.sample_rate));
    }
    let mono = downmix_f32(decode_f32(&mut r, path)?, spec.channels);
    let pcm = mono
        .into_iter()
        .map(|v| (v * 32_768.0).round().clamp(i16::MIN as f32, i16::MAX as f32) as i16)
        .collect();
    Ok((pcm, spec.sample_rate))
}

/// Write a mono 16-bit PCM WAV, replacing any existing file.
pub fn write_wav_mono_i16(
    path: &Path,
    samples: &[i16],
    sample_rate: u32,
) -> Result<(), WavIoError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| wav_err(path, e))?;
    for &s in samples {
        w.write_sample(s).map_err(|e| wav_err(path, e))?;
    }
    w.finalize().map_err(|e| wav_err(path, e))
}

/// Decode all samples as interleaved `f32` in `[-1, 1]`, whatever the file's
/// sample format.
fn decode_f32<R: std::io::Read>(
    r: &mut hound::WavReader<R>,
    path: &Path,
) -> Result<Vec<f32>, WavIoError> {
    let spec = r.spec();
    match spec.sample_format {
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            collect_salvage(r.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)), path)
        }
        hound::SampleFormat::Float => collect_salvage(r.samples::<f32>(), path),
    }
}

fn downmix_f32(interleaved: Vec<f32>, channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return interleaved;
    }
    let ch = channels as usize;
    // Each frame averages over its actual sample count, so a truncated
    // trailing frame keeps full amplitude.
    interleaved
        .chunks(ch)
        .map(|f| f.iter().sum::<f32>() / f.len() as f32)
        .collect()
}

fn downmix_i16(interleaved: Vec<i16>, channels: u16) -> Vec<i16> {
    if channels <= 1 {
        return interleaved;
    }
    let ch = channels as usize;
    interleaved
        .chunks(ch)
        .map(|f| (f.iter().map(|&s| i32::from(s)).sum::<i32>() / f.len() as i32) as i16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_raw(path: &Path, channels: u16, rate: u32, bits: u16, samples: &[i32]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: bits,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &s in samples {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    }

    #[test]
    fn truncated_file_salvages_valid_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("torn.wav");
        let samples: Vec<i16> = (0..1000).map(|i| (i % 313) as i16).collect();
        write_wav_mono_i16(&path, &samples, 16_000).unwrap();
        // Cut mid-data (and mid-sample): the header now promises more bytes
        // than the file holds — the crash-torn chunk shape.
        let full = std::fs::read(&path).unwrap();
        std::fs::write(&path, &full[..full.len() - 501]).unwrap();
        let (out, rate) = read_wav_mono_i16(&path).unwrap();
        assert_eq!(rate, 16_000);
        assert!(!out.is_empty() && out.len() < samples.len());
        assert_eq!(out[..], samples[..out.len()]);
        // f32 reader salvages the same prefix.
        let (f, _) = read_wav_mono_f32(&path).unwrap();
        assert_eq!(f.len(), out.len());
    }

    #[test]
    fn i16_mono_roundtrip_is_bit_exact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.wav");
        let samples: Vec<i16> = vec![0, 1, -1, i16::MAX, i16::MIN, 1234, -4321];
        write_wav_mono_i16(&path, &samples, 16_000).unwrap();
        let (out, rate) = read_wav_mono_i16(&path).unwrap();
        assert_eq!(out, samples);
        assert_eq!(rate, 16_000);
    }

    #[test]
    fn stereo_i16_downmixes_by_average() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.wav");
        write_raw(&path, 2, 16_000, 16, &[1000, -1000, 2000, 2000, 10, 30]);
        let (out, _) = read_wav_mono_i16(&path).unwrap();
        assert_eq!(out, vec![0, 2000, 20]);
        let (f, _) = read_wav_mono_f32(&path).unwrap();
        assert!((f[1] - 2000.0 / 32_768.0).abs() < 1e-6);
        assert!((f[2] - 20.0 / 32_768.0).abs() < 1e-6);
    }

    #[test]
    fn int24_reads_scaled_on_both_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d24.wav");
        // Half scale in 24-bit: 2^22.
        write_raw(&path, 1, 48_000, 24, &[1 << 22, -(1 << 22), 0]);
        let (f, rate) = read_wav_mono_f32(&path).unwrap();
        assert_eq!(rate, 48_000);
        assert!((f[0] - 0.5).abs() < 1e-6);
        assert!((f[1] + 0.5).abs() < 1e-6);
        let (i, _) = read_wav_mono_i16(&path).unwrap();
        assert_eq!(i, vec![16_384, -16_384, 0]);
    }

    #[test]
    fn f32_file_reads_on_both_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for s in [0.5f32, -0.25, 2.0] {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
        let (f, _) = read_wav_mono_f32(&path).unwrap();
        assert_eq!(f, vec![0.5, -0.25, 2.0]);
        let (i, _) = read_wav_mono_i16(&path).unwrap();
        assert_eq!(i, vec![16_384, -8_192, i16::MAX]); // out-of-range clamps
    }

    #[test]
    fn missing_file_error_names_the_path() {
        let err = read_wav_mono_i16(Path::new("/nonexistent/x.wav")).unwrap_err();
        assert!(err.to_string().contains("/nonexistent/x.wav"));
    }
}
