// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Decode a user-supplied audio file (mp3 / m4a-aac / flac / wav / ogg /
//! alac) down to the mono 16 kHz i16 PCM the transcription pipeline expects,
//! plus a best-effort quality read.
//!
//! Decode-only via Symphonia (MPL-2.0); the archival format is Opus.

use std::path::Path;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, Value};
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

/// Samples an AAC encoder writes before the first real one, for a file with
/// no iTunSMPB tag. The mp4 edit list hides them from players; symphonia 0.5
/// decodes them.
const AAC_PRIMING_SAMPLES: usize = 2112;

/// The iTunSMPB gapless tag an AAC encoder writes: the priming count, and the
/// count of real samples after it. The real count is `None` when the tag holds
/// 0, as it does in a file whose writer never closed it.
fn itunes_gapless(format: &mut Box<dyn FormatReader>) -> Option<(usize, Option<usize>)> {
    let meta = format.metadata();
    let tag = meta.current()?.tags().iter().find(|t| t.key.ends_with(":iTunSMPB"))?;
    let Value::String(s) = &tag.value else { return None };
    let fields: Vec<u64> = s
        .split_whitespace()
        .take(4)
        .map(|w| u64::from_str_radix(w, 16))
        .collect::<Result<_, _>>()
        .ok()?;
    let priming = *fields.get(1)? as usize;
    let valid = *fields.get(3)? as usize;
    Some((priming, (valid > 0).then_some(valid)))
}

/// Decode any supported file → mono f32 at the file's own rate.
pub fn decode_mono(path: &Path) -> Result<(Vec<f32>, u32), ImportError> {
    let file = syncsafe::open(path)?;
    let ext = path.extension().and_then(|e| e.to_str());
    decode_mono_source(Box::new(file), ext)
}

/// Decode audio held in memory → mono f32 at its own rate. `ext` is the
/// file name's extension, a hint for the container probe.
pub fn decode_mono_bytes(bytes: Vec<u8>, ext: Option<&str>) -> Result<(Vec<f32>, u32), ImportError> {
    decode_mono_source(Box::new(std::io::Cursor::new(bytes)), ext)
}

fn decode_mono_source(
    source: Box<dyn symphonia::core::io::MediaSource>,
    ext: Option<&str>,
) -> Result<(Vec<f32>, u32), ImportError> {
    let mss = MediaSourceStream::new(source, Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = ext {
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
    let aac = track.codec_params.codec == symphonia::core::codecs::CODEC_TYPE_AAC;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| ImportError::Decode(e.to_string()))?;
    let (priming, valid) = if aac {
        itunes_gapless(&mut format).unwrap_or((AAC_PRIMING_SAMPLES, None))
    } else {
        (0, None)
    };

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

    if mono_src.len() > priming {
        mono_src.drain(..priming);
    } else {
        mono_src.clear();
    }
    if let Some(valid) = valid {
        mono_src.truncate(valid);
    }
    if mono_src.is_empty() {
        return Err(ImportError::Empty);
    }
    Ok((mono_src, src_rate))
}

/// Decode any supported file → mono 16 kHz i16 + a quality report.
pub fn decode_to_mono_16k(path: &Path) -> Result<ImportedAudio, ImportError> {
    let (mono_src, src_rate) = decode_mono(path)?;
    Ok(to_16k(mono_src, src_rate))
}

/// Decode audio held in memory → mono 16 kHz i16 + a quality report.
pub fn decode_bytes_to_mono_16k(bytes: Vec<u8>, ext: Option<&str>) -> Result<ImportedAudio, ImportError> {
    let (mono_src, src_rate) = decode_mono_bytes(bytes, ext)?;
    Ok(to_16k(mono_src, src_rate))
}

fn to_16k(mono_src: Vec<f32>, src_rate: u32) -> ImportedAudio {
    // Resample with the capture path's band-limited resampler.
    let mut rs = audio_engine::resample::FrameResampler::new(src_rate, 1);
    let mut pcm: Vec<i16> = Vec::with_capacity(mono_src.len());
    for frame in rs.push(&mono_src).into_iter().chain(rs.flush()) {
        pcm.extend(frame);
    }
    // The resampler pads its last frame with silence to 320 samples; 16 kHz
    // audio passes through sample for sample.
    if src_rate == TARGET_RATE {
        pcm.truncate(mono_src.len());
    }

    let (quality_ok, quality_note) = assess(&pcm, src_rate);
    let duration_secs = pcm.len() as f32 / TARGET_RATE as f32;

    ImportedAudio {
        pcm,
        duration_secs,
        quality_ok,
        quality_note,
    }
}

/// Heuristic quality read. Flags near-silence, heavy clipping, or audio that
/// was recorded below 16 kHz.
fn assess(samples: &[i16], src_rate: u32) -> (bool, String) {
    if samples.is_empty() {
        return (false, "empty audio".into());
    }
    let n = samples.len() as f32;
    let norm = |s: i16| s as f32 / i16::MAX as f32;
    let rms = (samples.iter().map(|&s| norm(s) * norm(s)).sum::<f32>() / n).sqrt();
    let clipped = samples.iter().filter(|&&s| norm(s).abs() >= 0.99).count() as f32 / n;
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
