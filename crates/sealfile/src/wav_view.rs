// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! A sealed PCM stream seen as a WAV file: a 44-byte PCM header built from
//! the stream header and plaintext length, then the samples. `Read + Seek`,
//! so WAV readers such as `hound` open it unchanged.

use std::io::{self, Read, Seek, SeekFrom};

use crate::stream::{PcmFormat, StreamReader};

pub(crate) const WAV_HEADER_LEN: u64 = 44;

pub struct WavView<R: Read + Seek> {
    inner: StreamReader<R>,
    /// The synthesized header; `None` when `inner` is a plaintext WAV file
    /// read as it is.
    header: Option<[u8; WAV_HEADER_LEN as usize]>,
    data_len: u64,
    pos: u64,
}

/// A 44-byte PCM WAV header for `data_len` bytes of samples.
pub(crate) fn wav_header(f: PcmFormat, data_len: u64) -> [u8; WAV_HEADER_LEN as usize] {
    let block_align = u64::from(f.channels) * u64::from(f.bits_per_sample).div_ceil(8);
    let mut h = [0u8; WAV_HEADER_LEN as usize];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&((36 + data_len) as u32).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&f.channels.to_le_bytes());
    h[24..28].copy_from_slice(&f.sample_rate.to_le_bytes());
    h[28..32].copy_from_slice(&((u64::from(f.sample_rate) * block_align) as u32).to_le_bytes());
    h[32..34].copy_from_slice(&(block_align as u16).to_le_bytes());
    h[34..36].copy_from_slice(&f.bits_per_sample.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&(data_len as u32).to_le_bytes());
    h
}

/// Where a plaintext WAV file keeps its samples.
pub(crate) struct WavLayout {
    pub format: PcmFormat,
    /// Offset of the sample data.
    pub data_at: u64,
    /// Bytes of sample data: what the header counts, at most what the file
    /// holds.
    pub data_len: u64,
}

/// The layout of a RIFF/WAVE file, or `None` when `r` is not one (no `fmt `
/// before its `data` chunk). Leaves `r` anywhere.
pub(crate) fn parse_wav<R: Read + Seek>(r: &mut R) -> io::Result<Option<WavLayout>> {
    let file_len = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(0))?;
    let mut riff = [0u8; 12];
    if r.read_exact(&mut riff).is_err() || &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Ok(None);
    }
    let mut format = None;
    let mut at = 12u64;
    loop {
        let mut chunk = [0u8; 8];
        r.seek(SeekFrom::Start(at))?;
        if r.read_exact(&mut chunk).is_err() {
            return Ok(None);
        }
        let size = u64::from(u32::from_le_bytes(chunk[4..8].try_into().expect("4-byte slice")));
        let body = at + 8;
        match &chunk[0..4] {
            b"fmt " => {
                let mut fmt = [0u8; 16];
                if size < 16 || r.read_exact(&mut fmt).is_err() {
                    return Ok(None);
                }
                format = Some(PcmFormat {
                    channels: u16::from_le_bytes(fmt[2..4].try_into().expect("2-byte slice")),
                    sample_rate: u32::from_le_bytes(fmt[4..8].try_into().expect("4-byte slice")),
                    bits_per_sample: u16::from_le_bytes(fmt[14..16].try_into().expect("2-byte slice")),
                });
            }
            b"data" => {
                let Some(format) = format else { return Ok(None) };
                return Ok(Some(WavLayout { format, data_at: body, data_len: size.min(file_len.saturating_sub(body)) }));
            }
            _ => {}
        }
        at = body + size + (size & 1);
    }
}

/// The format of a plaintext WAV file that [`wav_view`] presents byte for
/// byte from a PCM stream of its samples: a 44-byte PCM header that
/// [`wav_header`] writes for its format and length, then whole sample
/// frames to the end of the file. `None` for any other file. Leaves `r`
/// anywhere.
pub fn canonical_pcm<R: Read + Seek>(r: &mut R) -> io::Result<Option<PcmFormat>> {
    let Some(l) = parse_wav(r)? else { return Ok(None) };
    let file_len = r.seek(SeekFrom::End(0))?;
    let block_align = u64::from(l.format.channels) * u64::from(l.format.bits_per_sample).div_ceil(8);
    if l.data_at != WAV_HEADER_LEN
        || l.data_at + l.data_len != file_len
        || block_align == 0
        || !l.data_len.is_multiple_of(block_align)
        || l.data_len > u64::from(u32::MAX) - 36
    {
        return Ok(None);
    }
    let mut head = [0u8; WAV_HEADER_LEN as usize];
    r.seek(SeekFrom::Start(0))?;
    r.read_exact(&mut head)?;
    Ok((head == wav_header(l.format, l.data_len)).then_some(l.format))
}

/// The WAV view of `r`. The data length is the plaintext length rounded
/// down to whole sample frames. An opaque stream holds a whole file (a WAV
/// file sealed as it was) and passes its plaintext through as it is, as
/// does a reader of a plaintext file.
pub fn wav_view<R: Read + Seek>(mut r: StreamReader<R>) -> WavView<R> {
    let Some(format) = r.header().pcm else {
        let data_len = parse_wav(&mut r).ok().flatten().map_or(0, |l| l.data_len);
        // A failed rewind surfaces on the first read.
        let _ = r.seek(SeekFrom::Start(0));
        return wav_passthrough(r, data_len);
    };
    let block_align = u64::from(format.channels) * u64::from(format.bits_per_sample).div_ceil(8);
    let mut data_len = r.len();
    if block_align > 0 {
        data_len -= data_len % block_align;
    }
    let cap = u64::from(u32::MAX) - 36;
    if data_len > cap {
        data_len = cap - if block_align > 0 { cap % block_align } else { 0 };
    }
    WavView { inner: r, header: Some(wav_header(format, data_len)), data_len, pos: 0 }
}

/// A plaintext WAV file read as it is; `data_len` is what its header counts.
pub(crate) fn wav_passthrough<R: Read + Seek>(r: StreamReader<R>, data_len: u64) -> WavView<R> {
    WavView { inner: r, header: None, data_len, pos: 0 }
}

impl<R: Read + Seek> WavView<R> {
    /// Bytes of sample data the header announces.
    pub fn data_len(&self) -> u64 {
        self.data_len
    }

    pub fn into_inner(self) -> StreamReader<R> {
        self.inner
    }
}

impl<R: Read + Seek> Read for WavView<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let Some(header) = &self.header else { return self.inner.read(out) };
        let total = WAV_HEADER_LEN + self.data_len;
        if self.pos >= total || out.is_empty() {
            return Ok(0);
        }
        if self.pos < WAV_HEADER_LEN {
            let from = self.pos as usize;
            let n = (WAV_HEADER_LEN as usize - from).min(out.len());
            out[..n].copy_from_slice(&header[from..from + n]);
            self.pos += n as u64;
            return Ok(n);
        }
        let want = ((total - self.pos) as usize).min(out.len());
        self.inner.seek(SeekFrom::Start(self.pos - WAV_HEADER_LEN))?;
        let n = self.inner.read(&mut out[..want])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for WavView<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        if self.header.is_none() {
            return self.inner.seek(to);
        }
        let total = WAV_HEADER_LEN + self.data_len;
        let target = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => total.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = target.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        Ok(self.pos)
    }
}
