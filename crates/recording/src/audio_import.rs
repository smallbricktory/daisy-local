//! Decode a user-supplied audio file (mp3 / m4a-aac / flac / wav / ogg /
//! alac) down to the mono 16 kHz i16 PCM the transcription pipeline expects,
//! plus a best-effort quality read.
//!
//! Decode-only via Symphonia (MPL-2.0); the archival format is Opus.

use std::path::Path;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const TARGET_RATE: u32 = 16_000;

#[derive(Debug, Clone)]
pub struct ImportedAudio {
    /// Mono 16 kHz i16 PCM.
    pub pcm: Vec<i16>,
    pub duration_secs: f32,
    /// True if the decode looks clean enough for good transcription.
    pub quality_ok: bool,
    /// Human note when quality is suboptimal (empty when ok).
    pub quality_note: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not decode this audio (unsupported or corrupt file): {0}")]
    Decode(String),
    #[error("the file contains no audio")]
    Empty,
}

/// Decode any supported file → mono 16 kHz i16 + a quality report.
pub fn decode_to_mono_16k(path: &Path) -> Result<ImportedAudio, ImportError> {
    let file = syncsafe::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| ImportError::Decode(e.to_string()))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| ImportError::Decode("no audio track".into()))?;
    let track_id = track.id;
    let src_rate = track.codec_params.sample_rate.unwrap_or(TARGET_RATE);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| ImportError::Decode(e.to_string()))?;

    // Decode every packet to interleaved mono f32 at the source rate.
    let mut mono_src: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(e) => return Err(ImportError::Decode(e.to_string())),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                let ch = spec.channels.count().max(1);
                // Copy into an f32 sample buffer, then downmix to mono.
                let mut sbuf =
                    symphonia::core::audio::SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                sbuf.copy_interleaved_ref(decoded);
                let samples = sbuf.samples();
                for frame in samples.chunks(ch) {
                    let sum: f32 = frame.iter().copied().sum();
                    mono_src.push(sum / ch as f32);
                }
            }
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue, // skip a bad frame
            Err(e) => return Err(ImportError::Decode(e.to_string())),
        }
    }

    if mono_src.is_empty() {
        return Err(ImportError::Empty);
    }

    // Linear resample to 16 kHz (plenty for ASR; cheap, dependency-free).
    let resampled = resample_linear(&mono_src, src_rate, TARGET_RATE);

    // Quality read on the resampled mono signal.
    let (quality_ok, quality_note) = assess(&resampled, src_rate);

    let pcm: Vec<i16> = resampled
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect();
    let duration_secs = pcm.len() as f32 / TARGET_RATE as f32;

    Ok(ImportedAudio {
        pcm,
        duration_secs,
        quality_ok,
        quality_note,
    })
}

fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 / ratio;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

/// Heuristic quality read. Flags near-silence, heavy clipping, or audio that
/// was recorded below 16 kHz (upsampled = muddy for ASR).
fn assess(samples: &[f32], src_rate: u32) -> (bool, String) {
    if samples.is_empty() {
        return (false, "empty audio".into());
    }
    let n = samples.len() as f32;
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / n).sqrt();
    let clipped = samples.iter().filter(|s| s.abs() >= 0.99).count() as f32 / n;
    let mut notes: Vec<&str> = Vec::new();
    if rms < 0.01 {
        notes.push("very quiet");
    }
    if clipped > 0.01 {
        notes.push("clipping/distortion");
    }
    if src_rate < TARGET_RATE {
        notes.push("low source sample rate");
    }
    if notes.is_empty() {
        (true, String::new())
    } else {
        (
            false,
            format!("source audio looks rough ({})", notes.join(", ")),
        )
    }
}
