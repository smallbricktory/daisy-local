//! DeepFilterNet3 noise suppression for the finalize mic track: batch WAV in →
//! WAV out. Output feeds the saved `meeting.opus` and mic-scope diarization;
//! it is not fed to ASR.
//!
//! The DFN3 model is embedded in the binary via the `deep_filter` crate's
//! `default-model` feature; inference runs on tract.

use std::path::Path;

use df::tract::{DfParams, DfTract, RuntimeParams};
use df::transforms::resample;
use ndarray::{Array2, Axis};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("WAV: {0}")]
    Wav(#[from] hound::Error),
    #[error("denoise: {0}")]
    Df(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Denoise a 16 kHz mono i16 WAV. Reads `input`, writes `output` with the
/// same length, rate, and format; head delay is compensated and the output
/// stays time-aligned with the input.
///
/// Pipeline: read → resample to the model's 48 kHz → DfTract::process per
/// hop-size frame → trim STFT+lookahead delay head → resample back →
/// restore exact input length.
pub fn denoise_wav(input: &Path, output: &Path) -> Result<()> {
    let mut reader = hound::WavReader::open(input)?;
    let spec = reader.spec();
    let in_sr = spec.sample_rate as usize;
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<std::result::Result<_, _>>()?;
    let in_len = samples.len();
    if in_len == 0 {
        return Err(Error::Df("empty input".into()));
    }
    let noisy = Array2::from_shape_vec((1, in_len), samples)
        .map_err(|e| Error::Df(format!("shape: {e}")))?;

    let r_params = RuntimeParams::default(); // mono
    let mut model = DfTract::new(DfParams::default(), &r_params)
        .map_err(|e| Error::Df(format!("model init: {e:#}")))?;
    let model_sr = model.sr;

    let mut noisy = if in_sr != model_sr {
        resample(noisy.view(), in_sr, model_sr, None)
            .map_err(|e| Error::Df(format!("resample up: {e}")))?
    } else {
        noisy
    };
    // Pad to a hop-size multiple.
    let hop = model.hop_size;
    let n = noisy.len_of(Axis(1));
    let rem = n % hop;
    if rem != 0 {
        let mut padded = Array2::zeros((1, n + (hop - rem)));
        padded.slice_mut(ndarray::s![.., ..n]).assign(&noisy);
        noisy = padded;
    }
    let noisy = noisy.as_standard_layout();

    let mut enh: Array2<f32> = Array2::zeros(noisy.raw_dim());
    for (ns, eh) in noisy
        .view()
        .axis_chunks_iter(Axis(1), hop)
        .zip(enh.view_mut().axis_chunks_iter_mut(Axis(1), hop))
    {
        model
            .process(ns, eh)
            .map_err(|e| Error::Df(format!("process: {e}")))?;
    }

    // Trim the STFT + lookahead delay head.
    let delay = model.fft_size - hop + model.lookahead * hop;
    enh.slice_axis_inplace(Axis(1), ndarray::Slice::from(delay as isize..));

    let mut enh = if in_sr != model_sr {
        resample(enh.view(), model_sr, in_sr, None)
            .map_err(|e| Error::Df(format!("resample down: {e}")))?
    } else {
        enh
    };
    // Restore exact input length: trim overshoot or zero-pad tail.
    let cur = enh.len_of(Axis(1));
    if cur > in_len {
        enh.slice_axis_inplace(Axis(1), ndarray::Slice::from(..in_len as isize));
    } else if cur < in_len {
        let mut padded = Array2::zeros((1, in_len));
        padded.slice_mut(ndarray::s![.., ..cur]).assign(&enh);
        enh = padded;
    }

    let out_spec = hound::WavSpec {
        channels: 1,
        sample_rate: in_sr as u32,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(output, out_spec)?;
    for &v in enh.row(0).iter() {
        w.write_sample((v.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    w.finalize()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn write_test_wav(path: &Path, samples: &[i16], sr: u32) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: sr,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &s in samples {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    }

    /// 3 s of 300 Hz tone + white noise at 16 kHz.
    fn noisy_tone(len: usize) -> Vec<i16> {
        let mut seed: u32 = 0x1234_5678;
        (0..len)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = ((seed >> 16) as f32 / 32768.0 - 1.0) * 0.25;
                let tone = (2.0 * PI * 300.0 * i as f32 / 16000.0).sin() * 0.4;
                ((tone + noise).clamp(-1.0, 1.0) * 32767.0) as i16
            })
            .collect()
    }

    fn rms(s: &[i16]) -> f64 {
        (s.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / s.len().max(1) as f64).sqrt()
    }

    #[test]
    fn denoise_reduces_noise_preserves_length_and_rate() {
        let dir = tempfile::tempdir().unwrap();
        let inp = dir.path().join("in.wav");
        let out = dir.path().join("out.wav");
        let samples = noisy_tone(16000 * 3);
        write_test_wav(&inp, &samples, 16000);

        denoise_wav(&inp, &out).unwrap();

        let mut r = hound::WavReader::open(&out).unwrap();
        assert_eq!(r.spec().sample_rate, 16000);
        assert_eq!(r.spec().channels, 1);
        let got: Vec<i16> = r.samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(got.len(), samples.len());
        let out_rms = rms(&got);
        let in_rms = rms(&samples);
        assert!(out_rms > 32.767, "output near-silent: {out_rms}");
        assert!(out_rms < in_rms, "denoise did not reduce energy: {out_rms} >= {in_rms}");
    }

    #[test]
    fn missing_input_errors_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.wav");
        assert!(denoise_wav(&dir.path().join("nope.wav"), &out).is_err());
        assert!(!out.exists());
    }
}
