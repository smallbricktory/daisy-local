//! Ogg-Opus encoding for a small archival voice reference of a recording:
//! phone-grade, low-bitrate VBR. Stack: `opus` (MIT/Apache-2.0, bindings to
//! libopus BSD-3-Clause) + `ogg` (BSD-3-Clause) as the Ogg-page muxer.
//!
//! Output is an Ogg-Opus file (.opus). WebKitGTK / Safari 17+ /
//! Chromium-based webviews play this natively in `<audio>` via the
//! `audio/ogg; codecs=opus` MIME.
//!
//! ## Encoding contract
//!
//! Daisy records at 16 kHz mono i16 throughout the pipeline. This function
//! requires `sample_rate == 16_000` and rejects anything else.
//!
//! Frames are 20 ms (320 samples). Trailing partial frames are zero-padded
//! to 320 samples; the granule position on the final page still reflects
//! only the original sample count.

use crate::error::{RecordingError, Result};
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use opus::{Application, Bitrate, Channels, Encoder};
use std::path::Path;

/// Compression settings. The default targets phone-quality voice at 24 kbps.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct CompressParams {
    /// VBR target bitrate in kbps; snapped to the nearest exposed step.
    pub bitrate_kbps: u32,
}

impl Default for CompressParams {
    fn default() -> Self {
        Self { bitrate_kbps: 24 }
    }
}

/// Bitrate ladder exposed to the UI.
const BITRATE_STEPS: [u32; 5] = [16, 20, 24, 32, 48];

/// Daisy's pipeline-wide sample rate.
const SAMPLE_RATE: u32 = 16_000;
/// Opus frame length in milliseconds. 20 ms is the standard default.
const FRAME_MS: u32 = 20;
/// Samples per channel per frame at `SAMPLE_RATE`. (16_000 * 20 / 1000 = 320.)
const FRAME_SAMPLES: usize = (SAMPLE_RATE as usize * FRAME_MS as usize) / 1000;
/// Granule position increment per encoded packet. Opus reports granule at
/// 48 kHz regardless of input rate; each 20 ms packet advances by 960.
const GRANULE_PER_FRAME: u64 = 48_000 * (FRAME_MS as u64) / 1000;
/// Max bytes per encoded Opus packet at any reasonable bitrate. 4000 is the
/// libopus-documented upper bound.
const MAX_PACKET_BYTES: usize = 4000;

fn snap_bitrate(kbps: u32) -> u32 {
    *BITRATE_STEPS
        .iter()
        .min_by_key(|&&b| b.abs_diff(kbps))
        .unwrap_or(&24)
}

fn io_err(out: &Path, msg: impl Into<String>) -> RecordingError {
    RecordingError::Io {
        path: out.to_path_buf(),
        source: std::io::Error::other(msg.into()),
    }
}

/// Encode mono 16-bit PCM `samples` at 16 kHz to an Ogg-Opus file. Wrapper
/// around the channel-generic encoder. Returns bytes written.
pub fn encode_mono_pcm16(
    samples: &[i16],
    sample_rate: u32,
    params: &CompressParams,
    out_path: &Path,
) -> Result<u64> {
    encode_pcm16(samples, 1, sample_rate, params, out_path)
}

/// Encode interleaved stereo 16-bit PCM (L0,R0,L1,R1,…) at 16 kHz to an
/// Ogg-Opus file. Used for the meeting archive where L=mic and R=system;
/// the two tracks are recoverable via `decode_opus`.
pub fn encode_stereo_pcm16(
    interleaved: &[i16],
    sample_rate: u32,
    params: &CompressParams,
    out_path: &Path,
) -> Result<u64> {
    encode_pcm16(interleaved, 2, sample_rate, params, out_path)
}

/// Channel-generic Ogg-Opus encoder. `samples` is interleaved if `channels==2`.
fn encode_pcm16(
    samples: &[i16],
    channels: u8,
    sample_rate: u32,
    params: &CompressParams,
    out_path: &Path,
) -> Result<u64> {
    if samples.is_empty() {
        return Err(io_err(out_path, "no audio samples to encode"));
    }
    if sample_rate != SAMPLE_RATE {
        return Err(io_err(
            out_path,
            format!("Opus encoder requires {SAMPLE_RATE} Hz; got {sample_rate} Hz"),
        ));
    }
    let opus_channels = if channels == 2 { Channels::Stereo } else { Channels::Mono };
    let frame_interleaved = FRAME_SAMPLES * channels as usize;

    let mut encoder = Encoder::new(SAMPLE_RATE, opus_channels, Application::Voip)
        .map_err(|e| io_err(out_path, format!("opus init: {e}")))?;
    encoder
        .set_bitrate(Bitrate::Bits(
            (snap_bitrate(params.bitrate_kbps) as i32) * 1000,
        ))
        .map_err(|e| io_err(out_path, format!("opus bitrate: {e}")))?;
    encoder
        .set_vbr(true)
        .map_err(|e| io_err(out_path, format!("opus vbr: {e}")))?;

    let file = syncsafe::create(out_path).map_err(|e| RecordingError::Io {
        path: out_path.to_path_buf(),
        source: e,
    })?;
    let mut writer = PacketWriter::new(file);

    // Stream serial — Ogg takes a non-zero u32 unique per stream. The value
    // is wall-clock-derived; streams are never multiplexed.
    let serial: u32 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0xDA_15_EF_47);
    let serial = if serial == 0 { 0xDA_15_EF_47 } else { serial };

    // --- OpusHead identification header (must be its own Ogg page) ---
    // Layout (RFC 7845 §5.1):
    //   8 bytes  magic "OpusHead"
    //   1 byte   version (1)
    //   1 byte   channel count
    //   2 bytes  pre-skip (LE)
    //   4 bytes  input sample rate (LE) — informational
    //   2 bytes  output gain Q7.8 (LE)
    //   1 byte   channel mapping family (0 = mono/stereo)
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1);
    head.push(channels);
    head.extend_from_slice(&0u16.to_le_bytes());
    head.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes());
    head.push(0);
    writer
        .write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)
        .map_err(|e| io_err(out_path, format!("ogg head: {e}")))?;

    // --- OpusTags comment header (own page) ---
    let vendor = b"Daisy";
    let mut tags = Vec::with_capacity(8 + 4 + vendor.len() + 4);
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes()); // zero user comments
    writer
        .write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)
        .map_err(|e| io_err(out_path, format!("ogg tags: {e}")))?;

    // --- Audio packets ---
    let mut packet = vec![0u8; MAX_PACKET_BYTES];
    let frames: Vec<&[i16]> = samples.chunks(frame_interleaved).collect();
    let last_idx = frames.len().saturating_sub(1);
    let mut granule: u64 = 0;
    // Granule is in per-channel samples (48 kHz units).
    let per_channel_samples = samples.len() as u64 / channels as u64;
    let final_granule = per_channel_samples
        .saturating_mul(GRANULE_PER_FRAME)
        / FRAME_SAMPLES as u64;

    for (i, chunk) in frames.iter().enumerate() {
        // Pad trailing partial frame with silence to a full interleaved frame.
        let mut frame_buf = vec![0i16; frame_interleaved];
        frame_buf[..chunk.len()].copy_from_slice(chunk);

        let written = encoder
            .encode(&frame_buf, &mut packet)
            .map_err(|e| io_err(out_path, format!("opus encode: {e}")))?;

        let is_last = i == last_idx;
        // Granule on the last packet reports the *actual* sample count
        // (in 48 kHz units), which lets players show a correct duration.
        granule = if is_last {
            final_granule
        } else {
            granule.saturating_add(GRANULE_PER_FRAME)
        };

        let end_info = if is_last {
            PacketWriteEndInfo::EndStream
        } else {
            PacketWriteEndInfo::NormalPacket
        };

        writer
            .write_packet(packet[..written].to_vec(), serial, end_info, granule)
            .map_err(|e| io_err(out_path, format!("ogg packet: {e}")))?;
    }

    let final_size = std::fs::metadata(out_path).map(|m| m.len()).unwrap_or(0);
    Ok(final_size)
}

/// Decode an Ogg-Opus file back to (channels, interleaved i16 @ 16 kHz). Used
/// to recover a session's audio from the `meeting.opus` archive after the raw
/// chunk WAVs have been pruned. Lossy (it's a low-bitrate archive), but enough
/// to re-transcribe / re-diarize.
pub fn decode_opus(path: &Path) -> Result<(u8, Vec<i16>)> {
    use ogg::reading::PacketReader;
    let file = syncsafe::open(path).map_err(|e| RecordingError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut reader = PacketReader::new(file);

    // Packet 1: OpusHead — channel count is byte 9 (RFC 7845 §5.1).
    let head = reader
        .read_packet()
        .map_err(|e| io_err(path, format!("ogg read: {e}")))?
        .ok_or_else(|| io_err(path, "empty opus stream"))?;
    if head.data.len() < 10 || &head.data[0..8] != b"OpusHead" {
        return Err(io_err(path, "not an Ogg-Opus stream"));
    }
    let channels = head.data[9].max(1);
    // Packet 2: OpusTags — skip.
    let _ = reader.read_packet().map_err(|e| io_err(path, format!("ogg read: {e}")))?;

    let opus_ch = if channels == 2 { Channels::Stereo } else { Channels::Mono };
    let mut decoder = opus::Decoder::new(SAMPLE_RATE, opus_ch)
        .map_err(|e| io_err(path, format!("opus decoder: {e}")))?;

    let mut out: Vec<i16> = Vec::new();
    // Max Opus frame is 120 ms = 1920 samples/channel at 16 kHz.
    let mut buf = vec![0i16; 1920 * channels as usize];
    while let Some(pkt) = reader
        .read_packet()
        .map_err(|e| io_err(path, format!("ogg read: {e}")))?
    {
        if pkt.data.is_empty() {
            continue;
        }
        let per_ch = decoder
            .decode(&pkt.data, &mut buf, false)
            .map_err(|e| io_err(path, format!("opus decode: {e}")))?;
        out.extend_from_slice(&buf[..per_ch * channels as usize]);
    }
    Ok((channels, out))
}

/// Decode `meeting.opus` and re-emit it as an in-memory 8 kHz mono µ-law WAV
/// (telephone quality, G.711; `audioFormat` tag 7 in WAV).
///
/// On macOS, WebKit/CoreMedia mis-clocks Ogg-Opus playback (wrong duration
/// and a skewed seek time-base); a WAV's byte-rate header is clocked
/// correctly everywhere. Used on macOS only — other webviews play the opus
/// natively. The caller bounds total size.
pub fn decode_opus_to_ulaw_wav_bytes(path: &Path) -> Result<Vec<u8>> {
    let (channels, interleaved) = decode_opus(path)?;
    // Downmix to mono @ 16 kHz.
    let mono16: Vec<i16> = if channels >= 2 {
        interleaved
            .chunks(channels as usize)
            .map(|f| {
                let sum: i32 = f.iter().map(|&s| s as i32).sum();
                (sum / f.len() as i32) as i16
            })
            .collect()
    } else {
        interleaved
    };

    // Downsample 16 kHz → 8 kHz with a 3-tap triangular low-pass at each output
    // point (cheap anti-alias; voice review doesn't need a sharp filter), then
    // µ-law encode. One output byte per 8 kHz sample.
    let n = mono16.len();
    let mut ulaw: Vec<u8> = Vec::with_capacity(n / 2 + 1);
    let mut i = 0usize;
    while i < n {
        let prev = if i > 0 { mono16[i - 1] as i32 } else { mono16[i] as i32 };
        let cur = mono16[i] as i32;
        let next = if i + 1 < n { mono16[i + 1] as i32 } else { cur };
        let y = ((prev + 2 * cur + next) / 4).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        ulaw.push(linear_to_ulaw(y));
        i += 2;
    }

    Ok(build_ulaw_wav(&ulaw, 8_000))
}

/// Encode one 16-bit PCM sample to 8-bit G.711 µ-law.
fn linear_to_ulaw(pcm: i16) -> u8 {
    const BIAS: i32 = 0x84; // 132
    const CLIP: i32 = 32_635;
    let s = pcm as i32;
    let sign: i32 = if s < 0 { 0x80 } else { 0 };
    let mut mag = if s < 0 { -s } else { s };
    if mag > CLIP {
        mag = CLIP;
    }
    mag += BIAS;
    // exponent = floor(log2(mag >> 7)), 0..7  (the standard G.711 segment).
    let seg = ((mag >> 7) & 0xFF) as u8;
    let exponent: i32 = if seg == 0 { 0 } else { 7 - seg.leading_zeros() as i32 };
    let mantissa = (mag >> (exponent + 3)) & 0x0F;
    !(sign | (exponent << 4) | mantissa) as u8
}

/// Wrap µ-law bytes in a minimal RIFF/WAVE container: `fmt ` (audioFormat 7,
/// 8-bit, mono) + `fact` (sample count, expected for non-PCM) + `data`.
fn build_ulaw_wav(ulaw: &[u8], sample_rate: u32) -> Vec<u8> {
    let data_len = ulaw.len() as u32;
    let pad = (data_len & 1) as u32; // RIFF chunks are word-aligned.
    // RIFF size = 4 ("WAVE") + (8+18 fmt) + (8+4 fact) + (8 + data_len + pad).
    let riff_size = 4 + 26 + 12 + 8 + data_len + pad;
    let mut w = Vec::with_capacity(riff_size as usize + 8);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&riff_size.to_le_bytes());
    w.extend_from_slice(b"WAVE");
    // fmt chunk (18 bytes: includes cbSize for non-PCM).
    w.extend_from_slice(b"fmt ");
    w.extend_from_slice(&18u32.to_le_bytes());
    w.extend_from_slice(&7u16.to_le_bytes()); // WAVE_FORMAT_MULAW
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&sample_rate.to_le_bytes());
    w.extend_from_slice(&sample_rate.to_le_bytes()); // byte rate = sr * 1 * 1
    w.extend_from_slice(&1u16.to_le_bytes()); // block align
    w.extend_from_slice(&8u16.to_le_bytes()); // bits per sample
    w.extend_from_slice(&0u16.to_le_bytes()); // cbSize
    // fact chunk (sample count) — required for non-PCM WAV.
    w.extend_from_slice(b"fact");
    w.extend_from_slice(&4u32.to_le_bytes());
    w.extend_from_slice(&data_len.to_le_bytes());
    // data chunk.
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    w.extend_from_slice(ulaw);
    if pad == 1 {
        w.push(0xFF); // µ-law silence; not counted in data_len.
    }
    w
}

/// Split interleaved stereo into (left, right). For a mono buffer, both
/// returned vecs are the same samples.
pub fn deinterleave_stereo(channels: u8, interleaved: &[i16]) -> (Vec<i16>, Vec<i16>) {
    if channels < 2 {
        return (interleaved.to_vec(), interleaved.to_vec());
    }
    let mut left = Vec::with_capacity(interleaved.len() / 2);
    let mut right = Vec::with_capacity(interleaved.len() / 2);
    for f in interleaved.chunks(2) {
        left.push(f[0]);
        right.push(*f.get(1).unwrap_or(&f[0]));
    }
    (left, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_roundtrip_preserves_channels() {
        // L = 220 Hz, R = 660 Hz, 0.5 s @ 16 kHz, interleaved.
        let n = 8000;
        let mut inter = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f32 / SAMPLE_RATE as f32;
            let l = ((2.0 * std::f32::consts::PI * 220.0 * t).sin() * 12000.0) as i16;
            let r = ((2.0 * std::f32::consts::PI * 660.0 * t).sin() * 12000.0) as i16;
            inter.push(l);
            inter.push(r);
        }
        let tmp = std::env::temp_dir().join("daisy_stereo_rt.opus");
        encode_stereo_pcm16(&inter, SAMPLE_RATE, &CompressParams::default(), &tmp).unwrap();
        let (ch, decoded) = decode_opus(&tmp).unwrap();
        let _ = syncsafe::remove_file(&tmp);
        assert_eq!(ch, 2, "channel count preserved");
        let (left, right) = deinterleave_stereo(ch, &decoded);
        // Lossy + pre-skip: checks that both channels carry energy and
        // lengths are within ~15%.
        assert!((left.len() as f32 - n as f32).abs() / (n as f32) < 0.15);
        let rms = |v: &[i16]| (v.iter().map(|s| (*s as f32).powi(2)).sum::<f32>() / v.len() as f32).sqrt();
        assert!(rms(&left) > 1000.0 && rms(&right) > 1000.0, "both channels decoded");
    }

    #[test]
    fn ulaw_encodes_known_anchors() {
        // G.711 µ-law: 0 → 0xFF; full-scale + → 0x80; full-scale − → 0x00.
        assert_eq!(linear_to_ulaw(0), 0xFF);
        assert_eq!(linear_to_ulaw(i16::MAX), 0x80);
        assert_eq!(linear_to_ulaw(i16::MIN), 0x00);
    }

    #[test]
    fn opus_to_ulaw_wav_has_correct_header_and_length() {
        // 1 s stereo @ 16 kHz → encode opus → decode to 8 kHz µ-law WAV.
        let n = 16_000;
        let mut inter = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f32 / SAMPLE_RATE as f32;
            let s = ((2.0 * std::f32::consts::PI * 300.0 * t).sin() * 10000.0) as i16;
            inter.push(s);
            inter.push(s);
        }
        let tmp = std::env::temp_dir().join("daisy_ulaw_conv.opus");
        encode_stereo_pcm16(&inter, SAMPLE_RATE, &CompressParams::default(), &tmp).unwrap();
        let wav = decode_opus_to_ulaw_wav_bytes(&tmp).unwrap();
        let _ = syncsafe::remove_file(&tmp);

        // Parse the hand-written header.
        let u16le = |o: usize| u16::from_le_bytes([wav[o], wav[o + 1]]);
        let u32le = |o: usize| u32::from_le_bytes([wav[o], wav[o + 1], wav[o + 2], wav[o + 3]]);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u16le(20), 7, "WAVE_FORMAT_MULAW");
        assert_eq!(u16le(22), 1, "mono");
        assert_eq!(u32le(24), 8_000, "8 kHz");
        assert_eq!(u16le(34), 8, "8-bit");
        // fact chunk: magic[38..42], size[42..46]=4, sampleCount[46..50].
        assert_eq!(&wav[38..42], b"fact");
        assert_eq!(u32le(42), 4);
        let samples = u32le(46);
        // ~1 s downsampled to 8 kHz ≈ 8000 samples (within 15%, lossy + pre-skip).
        let expected = (n / 2) as f32;
        assert!(
            (samples as f32 - expected).abs() / expected < 0.15,
            "µ-law sample count {samples} ~ {expected}"
        );
        // data chunk: magic[50..54], size[54..58] == sample count (8-bit mono).
        assert_eq!(&wav[50..54], b"data");
        assert_eq!(u32le(54), samples, "data len == sample count (8-bit mono)");
    }

    #[test]
    fn mono_roundtrip_decodes() {
        let n = 4000;
        let mono: Vec<i16> = (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                ((2.0 * std::f32::consts::PI * 300.0 * t).sin() * 10000.0) as i16
            })
            .collect();
        let tmp = std::env::temp_dir().join("daisy_mono_rt.opus");
        encode_mono_pcm16(&mono, SAMPLE_RATE, &CompressParams::default(), &tmp).unwrap();
        let (ch, decoded) = decode_opus(&tmp).unwrap();
        let _ = syncsafe::remove_file(&tmp);
        assert_eq!(ch, 1);
        assert!(!decoded.is_empty());
    }

    #[test]
    fn snap_bitrate_picks_nearest_step() {
        assert_eq!(snap_bitrate(15), 16);
        assert_eq!(snap_bitrate(22), 20); // 22 is 2 from 20 and 2 from 24 -> picks lower (first min)
        assert_eq!(snap_bitrate(28), 24); // 28 ties 24 and 32 (±4) -> lower wins, same first-min rule
        assert_eq!(snap_bitrate(30), 32); // closer to 32 (2) than 24 (6)
        assert_eq!(snap_bitrate(50), 48);
        assert_eq!(snap_bitrate(1000), 48);
    }

    #[test]
    fn rejects_wrong_sample_rate() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("x.opus");
        let samples = vec![0i16; 16_000];
        let err = encode_mono_pcm16(&samples, 44_100, &CompressParams::default(), &p).unwrap_err();
        assert!(
            format!("{err}").contains("16000 Hz"),
            "expected sample-rate error: {err}"
        );
    }

    #[test]
    fn rejects_empty_samples() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("x.opus");
        let err = encode_mono_pcm16(&[], 16_000, &CompressParams::default(), &p).unwrap_err();
        assert!(format!("{err}").contains("no audio"));
    }

    #[test]
    fn encodes_writes_a_file_with_an_ogg_magic_header() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("voice.opus");
        // 1 s of a 440 Hz sine at 30% amplitude.
        let samples: Vec<i16> = (0..16_000)
            .map(|i| {
                let phase = (i as f32 / 16_000.0) * 2.0 * std::f32::consts::PI * 440.0;
                (i16::MAX as f32 * 0.3 * phase.sin()) as i16
            })
            .collect();
        let bytes_written = encode_mono_pcm16(&samples, 16_000, &CompressParams::default(), &p).unwrap();
        assert!(bytes_written > 0);

        // Sanity: file starts with the Ogg capture pattern "OggS".
        let head = syncsafe::read(&p).unwrap();
        assert!(head.len() >= 4);
        assert_eq!(&head[..4], b"OggS", "expected OggS magic at start");
    }
}
