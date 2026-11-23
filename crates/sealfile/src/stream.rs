// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The stream format (`DSS1`): a 24-byte header, then AES-256-GCM segments
//! of 16 KiB plaintext each, the last one shorter.
//!
//! Header: `"DSS1" | version u8 | kind u8 | file_id [7] | sample_rate
//! u32 BE | channels u16 BE | bits_per_sample u16 BE | 3 zero bytes`.
//! Kind 0 is opaque bytes, kind 1 is PCM (the three format fields are zero
//! for opaque streams). `file_id` is random per write; [`StreamWriter::append`]
//! keeps it.
//!
//! Each segment on disk is `nonce (12) || ciphertext || tag (16)`, sealed
//! with its own random nonce and associated data `header (24 bytes) ||
//! relative path || i (u32 BE) || last`, where `i` is the segment's index
//! and `last` is 1 on the final segment and 0 elsewhere. The index and flag
//! in the associated data make a reordered, dropped or moved segment fail
//! to open, and the file id makes a segment from another write of the same
//! path fail.
//! plaintext, so segments sit at fixed offsets and a seek decrypts one
//! segment. A finished stream ends in a last segment of 0 to `SEGMENT - 1`
//! bytes; a stream with no last segment was not finished.
//!
//! A file written whole (JSON, Markdown) is an opaque stream
//! ([`seal_whole`], [`open_whole`]).

use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};

use zeroize::{Zeroize, Zeroizing};

use crate::{gcm_open, gcm_seal, random_array, SealError, SessionKey};

pub const MAGIC: &[u8; 4] = b"DSS1";
const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 24;
/// Plaintext bytes per segment.
pub const SEGMENT: usize = 16 * 1024;
const NONCE: usize = 12;
const TAG: usize = 16;
/// Bytes a segment adds to its plaintext on disk.
const OVERHEAD: usize = NONCE + TAG;
const SEALED_SEGMENT: usize = SEGMENT + OVERHEAD;
const KIND_OPAQUE: u8 = 0;
const KIND_PCM: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
}

/// What a stream holds: PCM samples, or opaque bytes (JSON, Markdown,
/// opus, video, attachments) when `pcm` is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamHeader {
    pub pcm: Option<PcmFormat>,
}

impl StreamHeader {
    pub fn opaque() -> Self {
        Self { pcm: None }
    }

    pub fn pcm(format: PcmFormat) -> Self {
        Self { pcm: Some(format) }
    }

    fn encode(&self, file_id: &[u8; 7]) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[..4].copy_from_slice(MAGIC);
        out[4] = VERSION;
        out[6..13].copy_from_slice(file_id);
        match self.pcm {
            None => out[5] = KIND_OPAQUE,
            Some(f) => {
                out[5] = KIND_PCM;
                out[13..17].copy_from_slice(&f.sample_rate.to_be_bytes());
                out[17..19].copy_from_slice(&f.channels.to_be_bytes());
                out[19..21].copy_from_slice(&f.bits_per_sample.to_be_bytes());
            }
        }
        out
    }

    fn decode(raw: &[u8; HEADER_LEN]) -> Result<Self, SealError> {
        if &raw[..4] != MAGIC {
            return Err(SealError::Format("no stream magic".into()));
        }
        if raw[4] != VERSION {
            return Err(SealError::Format(format!("stream version {}", raw[4])));
        }
        let header = match raw[5] {
            KIND_OPAQUE => Self::opaque(),
            KIND_PCM => Self::pcm(PcmFormat {
                sample_rate: u32::from_be_bytes(raw[13..17].try_into().expect("4-byte slice")),
                channels: u16::from_be_bytes(raw[17..19].try_into().expect("2-byte slice")),
                bits_per_sample: u16::from_be_bytes(raw[19..21].try_into().expect("2-byte slice")),
            }),
            kind => return Err(SealError::Format(format!("stream kind {kind}"))),
        };
        Ok(header)
    }
}

/// Seals and opens the segments of one stream.
struct Segments {
    key: Zeroizing<[u8; 32]>,
    /// `header || rel`; each segment appends its index and last flag.
    aad: Vec<u8>,
}

impl Segments {
    fn new(key: &SessionKey, rel: &str, raw_header: &[u8; HEADER_LEN]) -> Self {
        let mut aad = Vec::with_capacity(HEADER_LEN + rel.len() + 5);
        aad.extend_from_slice(raw_header);
        aad.extend_from_slice(rel.as_bytes());
        Self { key: Zeroizing::new(*key.as_bytes()), aad }
    }

    fn aad(&self, index: u64, last: bool) -> Vec<u8> {
        let mut aad = Vec::with_capacity(self.aad.len() + 5);
        aad.extend_from_slice(&self.aad);
        aad.extend_from_slice(&(index as u32).to_be_bytes());
        aad.push(last as u8);
        aad
    }

    /// `nonce || ciphertext || tag` under a fresh random nonce.
    fn seal(&self, index: u64, last: bool, plain: &[u8]) -> io::Result<Vec<u8>> {
        if index > u32::MAX as u64 {
            return Err(io::Error::other("sealed stream exceeds its segment count"));
        }
        let nonce = random_array::<NONCE>();
        let mut out = Vec::with_capacity(OVERHEAD + plain.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&gcm_seal(&self.key, &nonce, &self.aad(index, last), plain));
        Ok(out)
    }

    fn open(&self, index: u64, last: bool, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, SealError> {
        if index > u32::MAX as u64 || sealed.len() < OVERHEAD {
            return Err(SealError::Auth);
        }
        let (nonce, boxed) = sealed.split_at(NONCE);
        let nonce: &[u8; NONCE] = nonce.try_into().expect("12-byte slice");
        gcm_open(&self.key, nonce, &self.aad(index, last), boxed).map(Zeroizing::new)
    }
}

fn segment_offset(index: u64) -> u64 {
    HEADER_LEN as u64 + index * SEALED_SEGMENT as u64
}

/// The authenticated shape of a stream on disk.
struct Layout {
    header: StreamHeader,
    segments: Segments,
    /// Leading full segments (not last).
    full: u64,
    /// The last segment's plaintext, when the stream was finished.
    last: Option<Zeroizing<Vec<u8>>>,
}

fn read_segment<R: Read + Seek>(r: &mut R, index: u64, len: usize) -> io::Result<Vec<u8>> {
    r.seek(SeekFrom::Start(segment_offset(index)))?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Reads the header and finds where the authenticated data ends. Full
/// segments before the end are authenticated when they are read.
fn scan<R: Read + Seek>(r: &mut R, key: &SessionKey, rel: &str) -> Result<Layout, SealError> {
    r.seek(SeekFrom::Start(0))?;
    let mut raw = [0u8; HEADER_LEN];
    r.read_exact(&mut raw).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => SealError::Format("shorter than a stream header".into()),
        _ => SealError::Io(e),
    })?;
    let header = StreamHeader::decode(&raw)?;
    let segments = Segments::new(key, rel, &raw);
    let file_len = r.seek(SeekFrom::End(0))?;
    let body = file_len - HEADER_LEN as u64;
    let on_disk = body / SEALED_SEGMENT as u64;
    let rem = (body % SEALED_SEGMENT as u64) as usize;

    // A finished stream ends in a short last segment.
    if rem >= OVERHEAD {
        let boxed = read_segment(r, on_disk, rem)?;
        if let Ok(plain) = segments.open(on_disk, true, &boxed) {
            return Ok(Layout { header, segments, full: on_disk, last: Some(plain) });
        }
    }
    // A full last segment.
    if rem == 0 && on_disk > 0 {
        let boxed = read_segment(r, on_disk - 1, SEALED_SEGMENT)?;
        if let Ok(plain) = segments.open(on_disk - 1, true, &boxed) {
            return Ok(Layout { header, segments, full: on_disk - 1, last: Some(plain) });
        }
    }
    // Unfinished or torn: the stream ends after the last full segment that
    // authenticates.
    let mut full = on_disk;
    while full > 0 {
        let boxed = read_segment(r, full - 1, SEALED_SEGMENT)?;
        if segments.open(full - 1, false, &boxed).is_ok() {
            break;
        }
        full -= 1;
    }
    if full == 0 && body > 0 {
        // No segment opens: another key or another path.
        return Err(SealError::Auth);
    }
    Ok(Layout { header, segments, full, last: None })
}

/// True when `key` opens the stream's first segment, as its last segment
/// or as a full one. A stream with no segment opens under any key. Reads
/// the header and at most one segment.
fn first_segment_opens<R: Read + Seek>(r: &mut R, key: &SessionKey, rel: &str) -> Result<bool, SealError> {
    r.seek(SeekFrom::Start(0))?;
    let mut raw = [0u8; HEADER_LEN];
    r.read_exact(&mut raw).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => SealError::Format("shorter than a stream header".into()),
        _ => SealError::Io(e),
    })?;
    StreamHeader::decode(&raw)?;
    let body = r.seek(SeekFrom::End(0))? - HEADER_LEN as u64;
    if body == 0 {
        return Ok(true);
    }
    let len = body.min(SEALED_SEGMENT as u64) as usize;
    let boxed = read_segment(r, 0, len)?;
    let segments = Segments::new(key, rel, &raw);
    Ok(segments.open(0, true, &boxed).is_ok() || (len == SEALED_SEGMENT && segments.open(0, false, &boxed).is_ok()))
}

/// The index in `keys` of the first key that opens the stream's first
/// segment; `None` when none does.
pub fn first_opening_key<R: Read + Seek>(r: &mut R, keys: &[SessionKey], rel: &str) -> Result<Option<usize>, SealError> {
    for (i, key) in keys.iter().enumerate() {
        if first_segment_opens(r, key, rel)? {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

/// The index in `keys` of the key the stream is sealed under: the first
/// that opens its first segment, else the first under which any segment
/// authenticates. [`SealError::Auth`] when none does.
pub fn opening_key<R: Read + Seek>(r: &mut R, keys: &[SessionKey], rel: &str) -> Result<usize, SealError> {
    if let Some(i) = first_opening_key(r, keys, rel)? {
        return Ok(i);
    }
    for (i, key) in keys.iter().enumerate() {
        match scan(r, key, rel) {
            Ok(_) => return Ok(i),
            Err(SealError::Auth) => {}
            Err(e) => return Err(e),
        }
    }
    Err(SealError::Auth)
}

/// Writes a stream. Each segment is sealed and written as it fills;
/// [`StreamWriter::finish`] seals the remainder as the last segment.
///
/// A writer made with [`StreamWriter::plain`] writes a plaintext file in the
/// stream's place instead: opaque bytes as they come, or for PCM a WAV file
/// whose header sizes are rewritten on every flush.
pub struct StreamWriter<W: Write + Seek> {
    w: W,
    body: WriterBody,
    /// Marks a plaintext session file as held open until the writer goes.
    pub(crate) hold: Option<crate::fs::OpenHold>,
}

enum WriterBody {
    Sealed(Sealer),
    /// `wav` is the offset of a WAV file's sample data.
    Plain { wav: Option<u64>, written: u64 },
}

struct Sealer {
    segments: Segments,
    /// Plaintext of the segment being filled; never more than [`SEGMENT`].
    buf: Zeroizing<Vec<u8>>,
    /// Index of the segment being filled.
    next: u64,
}

impl Sealer {
    fn seal_slice<W: Write>(&mut self, w: &mut W, plain: &[u8], last: bool) -> io::Result<()> {
        let boxed = self.segments.seal(self.next, last, plain)?;
        w.write_all(&boxed)?;
        self.next += 1;
        Ok(())
    }

    fn seal_buffer<W: Write>(&mut self, w: &mut W, last: bool) -> io::Result<()> {
        let boxed = self.segments.seal(self.next, last, &self.buf)?;
        w.write_all(&boxed)?;
        self.next += 1;
        self.buf.zeroize();
        Ok(())
    }

    fn write<W: Write>(&mut self, w: &mut W, mut data: &[u8]) -> io::Result<()> {
        if !self.buf.is_empty() {
            let take = (SEGMENT - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buf.len() == SEGMENT {
                self.seal_buffer(w, false)?;
            }
        }
        while data.len() >= SEGMENT {
            self.seal_slice(w, &data[..SEGMENT], false)?;
            data = &data[SEGMENT..];
        }
        self.buf.extend_from_slice(data);
        Ok(())
    }
}

/// Rewrites a WAV file's RIFF and data sizes for `written` bytes of samples
/// at `data_at`, and leaves the writer at the end of the samples.
fn patch_wav<W: Write + Seek>(w: &mut W, data_at: u64, written: u64) -> io::Result<()> {
    let end = data_at + written;
    w.seek(SeekFrom::Start(4))?;
    w.write_all(&(end.saturating_sub(8).min(u64::from(u32::MAX)) as u32).to_le_bytes())?;
    w.seek(SeekFrom::Start(data_at - 4))?;
    w.write_all(&(written.min(u64::from(u32::MAX)) as u32).to_le_bytes())?;
    w.seek(SeekFrom::Start(end))?;
    Ok(())
}

impl<W: Write + Seek> StreamWriter<W> {
    /// Starts a new stream at the writer's current position (the start of
    /// an empty file) with a fresh file id.
    pub fn new(mut w: W, key: &SessionKey, rel: &str, header: StreamHeader) -> io::Result<Self> {
        let raw = header.encode(&random_array::<7>());
        w.write_all(&raw)?;
        let sealer = Sealer { segments: Segments::new(key, rel, &raw), buf: Zeroizing::new(Vec::with_capacity(SEGMENT)), next: 0 };
        Ok(Self { w, body: WriterBody::Sealed(sealer), hold: None })
    }

    /// Starts a plaintext file at the start of the empty `w`: a WAV header
    /// for PCM, nothing for opaque bytes.
    pub fn plain(mut w: W, header: StreamHeader) -> io::Result<Self> {
        let wav = match header.pcm {
            Some(f) => {
                w.write_all(&crate::wav_view::wav_header(f, 0))?;
                Some(crate::wav_view::WAV_HEADER_LEN)
            }
            None => None,
        };
        Ok(Self { w, body: WriterBody::Plain { wav, written: 0 }, hold: None })
    }

    /// Seals every complete segment buffered so far and flushes the inner
    /// writer; a plaintext WAV gets its header rewritten. Syncs nothing to
    /// disk.
    pub fn flush_full_segments(&mut self) -> io::Result<()> {
        match &mut self.body {
            WriterBody::Sealed(s) if s.buf.len() == SEGMENT => s.seal_buffer(&mut self.w, false)?,
            WriterBody::Sealed(_) => {}
            WriterBody::Plain { wav: Some(at), written } => patch_wav(&mut self.w, *at, *written)?,
            WriterBody::Plain { wav: None, .. } => {}
        }
        self.w.flush()
    }

    /// Seals the buffered remainder as the last segment and returns the
    /// inner writer.
    pub fn finish(mut self) -> io::Result<W> {
        match &mut self.body {
            WriterBody::Sealed(s) => s.seal_buffer(&mut self.w, true)?,
            WriterBody::Plain { wav: Some(at), written } => patch_wav(&mut self.w, *at, *written)?,
            WriterBody::Plain { wav: None, .. } => {}
        }
        self.w.flush()?;
        Ok(self.w)
    }

    /// Returns the inner writer without a last segment; the buffered
    /// partial segment is dropped. A reader sees the full segments and
    /// reports the stream as truncated. A plaintext file keeps everything
    /// written.
    pub fn into_inner_unfinished(mut self) -> W {
        if let WriterBody::Plain { wav: Some(at), written } = self.body {
            let _ = patch_wav(&mut self.w, at, written);
        }
        self.w
    }

    /// Plaintext bytes written so far, the buffered ones included.
    pub fn position(&self) -> u64 {
        match &self.body {
            WriterBody::Sealed(s) => s.next * SEGMENT as u64 + s.buf.len() as u64,
            WriterBody::Plain { written, .. } => *written,
        }
    }

    /// True when the file is sealed.
    pub fn is_sealed(&self) -> bool {
        matches!(self.body, WriterBody::Sealed(_))
    }
}

impl<W: Write + Seek> Write for StreamWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match &mut self.body {
            WriterBody::Sealed(s) => s.write(&mut self.w, data)?,
            WriterBody::Plain { written, .. } => {
                self.w.write_all(data)?;
                *written += data.len() as u64;
            }
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_full_segments()
    }
}

/// Files whose length can be cut, for [`StreamWriter::append`].
pub trait SetLen {
    fn set_len(&mut self, len: u64) -> io::Result<()>;
}

impl SetLen for std::fs::File {
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        std::fs::File::set_len(self, len)
    }
}

impl SetLen for Cursor<Vec<u8>> {
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.get_mut().resize(len as usize, 0);
        Ok(())
    }
}

impl<W: Read + Write + Seek + SetLen> StreamWriter<W> {
    /// Reopens an existing stream, finished or not, to write more. An
    /// unfinished stream is cut after its last full authenticated segment.
    /// A finished stream's last segment is decrypted into the buffer so the
    /// next segment continues it, and stays on disk until that segment, or
    /// the new last segment, is written over it under a fresh nonce. The
    /// header, and with it the file id, stays as it is. A stream no segment
    /// of which opens is [`SealError::Auth`], and the file is left as it is.
    pub fn append(mut w: W, key: &SessionKey, rel: &str) -> Result<Self, SealError> {
        let Layout { segments, full, last, .. } = scan(&mut w, key, rel)?;
        let cut = segment_offset(full);
        if last.is_none() {
            w.set_len(cut)?;
        }
        w.seek(SeekFrom::Start(cut))?;
        let mut buf = Zeroizing::new(Vec::with_capacity(SEGMENT));
        if let Some(last) = &last {
            buf.extend_from_slice(last);
        }
        let mut writer = Self { w, body: WriterBody::Sealed(Sealer { segments, buf, next: full }), hold: None };
        // A full last segment continues as a full segment.
        writer.flush_full_segments()?;
        Ok(writer)
    }

    /// Reopens a plaintext file to write more at its end. `wav` is the
    /// offset and length of a WAV file's sample data; the file is cut after
    /// the samples its header counts.
    pub fn plain_append(mut w: W, wav: Option<(u64, u64)>) -> io::Result<Self> {
        let body = match wav {
            Some((at, len)) => {
                w.set_len(at + len)?;
                w.seek(SeekFrom::Start(at + len))?;
                WriterBody::Plain { wav: Some(at), written: len }
            }
            None => WriterBody::Plain { wav: None, written: w.seek(SeekFrom::End(0))? },
        };
        Ok(Self { w, body, hold: None })
    }
}

/// Reads a stream's plaintext with `Read + Seek`. A stream cut short reads
/// up to its last authenticated segment and reports [`truncated`].
///
/// A reader made by `sealfile::fs` for a plaintext file passes its bytes
/// (a WAV file's samples) through.
///
/// [`truncated`]: StreamReader::truncated
pub struct StreamReader<R: Read + Seek> {
    r: R,
    header: StreamHeader,
    body: ReaderBody,
    len: u64,
    pos: u64,
}

enum ReaderBody {
    Sealed {
        segments: Segments,
        full: u64,
        last: Option<Zeroizing<Vec<u8>>>,
        cached: Option<(u64, Zeroizing<Vec<u8>>)>,
    },
    /// Plaintext starting at `offset` of the inner reader, read ahead into
    /// `buf`, which holds the bytes from position `buf_at`.
    Plain { offset: u64, buf: Vec<u8>, buf_at: u64 },
}

/// Bytes a plaintext reader reads ahead.
const PLAIN_READ_AHEAD: usize = 64 * 1024;

impl<R: Read + Seek> StreamReader<R> {
    pub fn open(mut r: R, key: &SessionKey, rel: &str) -> Result<Self, SealError> {
        let layout = scan(&mut r, key, rel)?;
        let len = layout.full * SEGMENT as u64 + layout.last.as_ref().map_or(0, |l| l.len() as u64);
        Ok(Self {
            r,
            header: layout.header,
            body: ReaderBody::Sealed { segments: layout.segments, full: layout.full, last: layout.last, cached: None },
            len,
            pos: 0,
        })
    }

    /// [`StreamReader::open`] under the key of `keys` the stream is sealed
    /// under ([`opening_key`]).
    pub fn open_any(mut r: R, keys: &[SessionKey], rel: &str) -> Result<Self, SealError> {
        let i = opening_key(&mut r, keys, rel)?;
        Self::open(r, &keys[i], rel)
    }

    /// An empty, truncated reader for a PCM stream shorter than one sealed
    /// segment: a stream torn inside its first segment. `None` for any other
    /// stream.
    pub fn torn_pcm(mut r: R) -> Option<Self> {
        r.seek(SeekFrom::Start(0)).ok()?;
        let mut raw = [0u8; HEADER_LEN];
        r.read_exact(&mut raw).ok()?;
        let header = StreamHeader::decode(&raw).ok()?;
        let body = r.seek(SeekFrom::End(0)).ok()? - HEADER_LEN as u64;
        if header.pcm.is_none() || body == 0 || body >= SEALED_SEGMENT as u64 {
            return None;
        }
        let segments = Segments::new(&SessionKey::from_bytes([0u8; 32]), "", &raw);
        Some(Self { r, header, body: ReaderBody::Sealed { segments, full: 0, last: None, cached: None }, len: 0, pos: 0 })
    }

    /// `len` plaintext bytes of `r` from `offset`, as they are.
    pub(crate) fn plain(r: R, header: StreamHeader, offset: u64, len: u64) -> Self {
        Self { r, header, body: ReaderBody::Plain { offset, buf: Vec::new(), buf_at: 0 }, len, pos: 0 }
    }

    pub fn header(&self) -> &StreamHeader {
        &self.header
    }

    /// Plaintext length.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True when the stream has no last segment: the writer never finished,
    /// or the file was cut. False for a plaintext file.
    pub fn truncated(&self) -> bool {
        matches!(self.body, ReaderBody::Sealed { last: None, .. })
    }

    /// True when the file is sealed.
    pub fn is_sealed(&self) -> bool {
        matches!(self.body, ReaderBody::Sealed { .. })
    }

    pub fn into_inner(self) -> R {
        self.r
    }
}

impl<R: Read + Seek> Read for StreamReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || out.is_empty() {
            return Ok(0);
        }
        match &mut self.body {
            ReaderBody::Plain { offset, buf, buf_at } => {
                if self.pos < *buf_at || self.pos >= *buf_at + buf.len() as u64 {
                    let want = ((self.len - self.pos) as usize).min(PLAIN_READ_AHEAD);
                    buf.resize(want, 0);
                    self.r.seek(SeekFrom::Start(*offset + self.pos))?;
                    let mut got = 0;
                    while got < want {
                        match self.r.read(&mut buf[got..])? {
                            0 => break,
                            n => got += n,
                        }
                    }
                    buf.truncate(got);
                    *buf_at = self.pos;
                    if got == 0 {
                        return Ok(0);
                    }
                }
                let from = (self.pos - *buf_at) as usize;
                let n = (buf.len() - from).min(out.len());
                out[..n].copy_from_slice(&buf[from..from + n]);
                self.pos += n as u64;
                Ok(n)
            }
            ReaderBody::Sealed { segments, full, last, cached } => {
                let index = self.pos / SEGMENT as u64;
                let offset = (self.pos % SEGMENT as u64) as usize;
                let plain: &[u8] = if index == *full {
                    last.as_deref().map_or(&[][..], |v| v.as_slice())
                } else {
                    if cached.as_ref().map(|(i, _)| *i) != Some(index) {
                        let boxed = read_segment(&mut self.r, index, SEALED_SEGMENT)?;
                        let plain = segments.open(index, false, &boxed).map_err(|_| {
                            io::Error::new(io::ErrorKind::InvalidData, "sealed segment did not authenticate")
                        })?;
                        *cached = Some((index, plain));
                    }
                    cached.as_ref().map(|(_, p)| p.as_slice()).expect("cached above")
                };
                let n = (plain.len() - offset).min(out.len());
                out[..n].copy_from_slice(&plain[offset..offset + n]);
                self.pos += n as u64;
                Ok(n)
            }
        }
    }
}

impl<R: Read + Seek> Seek for StreamReader<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        let target = target.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        self.pos = target;
        Ok(target)
    }
}

/// True when a stream of `len` bytes on disk holds no plaintext: a header
/// alone, or a header and one empty segment.
pub fn is_empty_len(len: u64) -> bool {
    len == HEADER_LEN as u64 || len == (HEADER_LEN + OVERHEAD) as u64
}

/// An opaque stream holding `plain`, finished.
pub fn seal_whole(key: &SessionKey, rel: &str, plain: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + plain.len() + (plain.len() / SEGMENT + 1) * OVERHEAD);
    let mut w = StreamWriter::new(Cursor::new(&mut out), key, rel, StreamHeader::opaque()).expect("in-memory write");
    w.write_all(plain).expect("in-memory write");
    w.finish().expect("in-memory write");
    out
}

/// The plaintext of a stream written whole. A torn or unfinished stream is
/// [`SealError::Auth`].
pub fn open_whole(key: &SessionKey, rel: &str, bytes: &[u8]) -> Result<Vec<u8>, SealError> {
    open_whole_any(std::slice::from_ref(key), rel, bytes)
}

/// [`open_whole`] under the key of `keys` the stream is sealed under.
pub fn open_whole_any(keys: &[SessionKey], rel: &str, bytes: &[u8]) -> Result<Vec<u8>, SealError> {
    let mut r = StreamReader::open_any(Cursor::new(bytes), keys, rel)?;
    if r.truncated() {
        return Err(SealError::Auth);
    }
    let mut out = Vec::with_capacity(r.len() as usize);
    r.read_to_end(&mut out).map_err(|_| SealError::Auth)?;
    Ok(out)
}
