// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Whole-file reads and writes with the signatures of `syncsafe`'s. A file
//! under a session folder (`<..>/sessions/<id>/…`, or `.<id>.importing/…`
//! while an import runs) is sealed on write as an opaque stream under the
//! session's key, with its path inside the folder as associated data, and
//! opened on read. A plaintext session file reads as it is. Every other path
//! goes straight to `syncsafe`. Append-only logs (JSONL) go through
//! [`RecordLog`], [`read_records`] and [`write_records`]; audio and other
//! segmented files through [`create_stream`], [`append_stream`],
//! [`open_stream`] and [`open_wav`].
//!
//! A sealed file opens under its session's own key, else under one of the
//! session's other keys ([`keyring::other_session_keys`]); writes seal
//! under the session's own key. A session key that is not available (the
//! vault is locked and the session is not pinned, or the session's
//! `key.json` is wrapped to a content key not held) fails the call with
//! `PermissionDenied`; nothing is written. [`rekey`] moves a file onto its
//! session's own key.
//!
//! Every write, append and reseal of a session file holds that session's
//! write lock ([`lock_session`]) while it runs. A writer that keeps a
//! plaintext session file open to append to it (a log or a WAV file an
//! older build started) marks it held open ([`is_held_open`]) until it is
//! dropped.

use std::io;
use std::path::{Path, PathBuf};

use crate::keyring::{self, KEY_FILE};
use crate::records::{open_records_any, seal_record, RECORDS_FIRST_LINE};
use crate::stream::{open_whole_any, seal_whole, StreamHeader, StreamReader, StreamWriter};
use crate::wav_view::WavView;
use crate::{is_sealed, Format, SealError, SessionKey};

/// The sessions whose write lock a caller in this process holds.
static WRITING: std::sync::Mutex<Option<std::collections::HashSet<PathBuf>>> = std::sync::Mutex::new(None);
/// Signalled each time a session write lock is released.
static WRITE_DONE: std::sync::Condvar = std::sync::Condvar::new();

/// A session's write lock, held until it drops. One holder per session at
/// a time across the process; a second caller waits. Not reentrant: a
/// holder that calls a write of this module on the same session waits
/// forever.
#[must_use = "the session is locked only while the guard lives"]
pub struct SessionWriteLock {
    dir: PathBuf,
}

/// Takes the write lock of the session folder `dir`, waiting while another
/// caller holds it.
pub fn lock_session(dir: &Path) -> SessionWriteLock {
    let mut held = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    while held.get_or_insert_with(Default::default).contains(dir) {
        held = WRITE_DONE.wait(held).unwrap_or_else(|e| e.into_inner());
    }
    held.get_or_insert_with(Default::default).insert(dir.to_path_buf());
    SessionWriteLock { dir: dir.to_path_buf() }
}

impl Drop for SessionWriteLock {
    fn drop(&mut self) {
        let mut held = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(set) = held.as_mut() {
            set.remove(&self.dir);
        }
        WRITE_DONE.notify_all();
    }
}

/// The write lock of the session `path` is in; `None` outside a session.
fn lock_for(path: &Path) -> Option<SessionWriteLock> {
    session_of(path).map(|(dir, _)| lock_session(&dir))
}

/// Plaintext session files a writer in this process holds open to append
/// to, with the number of holders.
static HELD_OPEN: std::sync::Mutex<Option<std::collections::HashMap<PathBuf, usize>>> = std::sync::Mutex::new(None);

/// True while a writer in this process holds the plaintext file at `path`
/// open to append to it.
pub fn is_held_open(path: &Path) -> bool {
    HELD_OPEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|m| m.contains_key(path))
}

/// Marks a plaintext session file held open until it drops.
pub(crate) struct OpenHold {
    path: PathBuf,
}

fn hold_open(path: &Path) -> OpenHold {
    *HELD_OPEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .entry(path.to_path_buf())
        .or_insert(0) += 1;
    OpenHold { path: path.to_path_buf() }
}

impl Drop for OpenHold {
    fn drop(&mut self) {
        let mut held = HELD_OPEN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = held.as_mut() {
            if let Some(n) = m.get_mut(&self.path) {
                *n -= 1;
                if *n == 0 {
                    m.remove(&self.path);
                }
            }
        }
    }
}

/// The session folder and the `/`-separated path inside it, for a file
/// under `<..>/sessions/<id>/` or `<..>/sessions/.<id>.importing/`.
pub fn session_of(path: &Path) -> Option<(PathBuf, String)> {
    syncsafe::session_file(path)
}

/// Runs `open` with the session's own key, then, when that key is not held
/// or does not authenticate, with the session's other keys. When none
/// authenticates and the own key is not held, the error is why it is not.
fn open_with<T>(dir: &Path, mut open: impl FnMut(&[SessionKey]) -> Result<T, SealError>) -> Result<T, SealError> {
    let own = keyring::session_key(dir);
    if let Ok(key) = &own {
        match open(std::slice::from_ref(key)) {
            Err(SealError::Auth) => {}
            done => return done,
        }
    }
    let others = keyring::other_session_keys(dir);
    let unavailable = |own: Result<SessionKey, SealError>| own.err().unwrap_or(SealError::Auth);
    if others.is_empty() {
        return Err(unavailable(own));
    }
    match open(&others) {
        Err(SealError::Auth) if own.is_err() => Err(unavailable(own)),
        done => done,
    }
}

/// The records of the session `dir`'s sealed records file, each line opened
/// under the session key that opens it.
fn session_records(dir: &Path, rel: &str, bytes: &[u8]) -> Result<Vec<Vec<u8>>, SealError> {
    let own = keyring::session_key(dir);
    if let Ok(key) = &own {
        let (records, missed) = open_records_any(std::slice::from_ref(key), rel, bytes);
        if missed == 0 {
            return Ok(records);
        }
    }
    let mut keys: Vec<SessionKey> = own.as_ref().ok().cloned().into_iter().collect();
    keys.extend(keyring::other_session_keys(dir));
    let (records, missed) = open_records_any(&keys, rel, bytes);
    match own {
        Err(e) if records.is_empty() && (missed > 0 || keys.is_empty()) => Err(e),
        _ => Ok(records),
    }
}

/// The file's plaintext. A sealed session file is opened with its session's
/// keys; anything else is returned as read.
pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let path = path.as_ref();
    let bytes = syncsafe::read(path)?;
    match (session_of(path), is_sealed(&bytes)) {
        (Some((dir, rel)), Some(Format::Stream)) => open_with(&dir, |keys| open_whole_any(keys, &rel, &bytes)).map_err(to_io),
        (Some((dir, rel)), Some(Format::Records)) => {
            let mut out = Vec::with_capacity(bytes.len());
            for record in session_records(&dir, &rel, &bytes).map_err(to_io)? {
                out.extend_from_slice(&record);
                out.push(b'\n');
            }
            Ok(out)
        }
        _ => Ok(bytes),
    }
}

pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    String::from_utf8(read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let path = path.as_ref();
    let _lock = lock_for(path);
    match sealed(path, contents.as_ref())? {
        Some(bytes) => syncsafe::write(path, bytes),
        None => syncsafe::write(path, contents),
    }
}

/// [`syncsafe::write_atomic`], sealing a session file first.
pub fn write_atomic(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let path = path.as_ref();
    let _lock = lock_for(path);
    match sealed(path, contents.as_ref())? {
        Some(bytes) => syncsafe::write_atomic(path, bytes),
        None => syncsafe::write_atomic(path, contents),
    }
}

/// [`syncsafe::write_atomic_synced`], sealing a session file first.
pub fn write_atomic_synced(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let path = path.as_ref();
    let _lock = lock_for(path);
    match sealed(path, contents.as_ref())? {
        Some(bytes) => syncsafe::write_atomic_synced(path, bytes),
        None => syncsafe::write_atomic_synced(path, contents),
    }
}

/// True when the file reads back as exactly `candidate`. Any read failure,
/// a missing file included, is false.
pub fn plaintext_equals(path: &Path, candidate: &[u8]) -> bool {
    read(path).is_ok_and(|plain| plain == candidate)
}

/// Writes the sealed session file `from` to `to`, re-sealed under `to`'s
/// session key and path: a stream keeps its header's format and, when torn,
/// what authenticates; a records file keeps the lines that open. `from` is
/// left as it is. `InvalidInput` when `from` is not a sealed session file
/// or `to` is not a session file.
pub fn reseal_to(from: &Path, to: &Path) -> io::Result<()> {
    use std::io::{Read as _, Write as _};
    let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidInput, format!("{what}: {} -> {}", from.display(), to.display()));
    let (Some((from_dir, from_rel)), Some((to_dir, to_rel))) = (session_of(from), session_of(to)) else {
        return Err(invalid("not a session file"));
    };
    let mut file = syncsafe::open(from)?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).map_err(|_| invalid("not sealed"))?;
    io::Seek::rewind(&mut file)?;
    let format = is_sealed(&magic).ok_or_else(|| invalid("not sealed"))?;
    if !to_dir.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("no session folder: {}", to_dir.display())));
    }
    let _lock = lock_session(&to_dir);
    let to_key = keyring::create_session_key(&to_dir).map_err(to_io)?;
    match format {
        Format::Stream => {
            let mut reader =
                open_with(&from_dir, |keys| StreamReader::open_any(file.try_clone()?, keys, &from_rel)).map_err(to_io)?;
            let torn = reader.truncated();
            let tmp = to.with_file_name(format!(
                "{}.reseal.tmp",
                to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ));
            let written = (|| -> io::Result<()> {
                let header = *reader.header();
                let mut w = crate::stream::StreamWriter::new(syncsafe::create_sealed(&tmp)?, &to_key, &to_rel, header)?;
                io::copy(&mut reader, &mut w)?;
                let mut out = if torn { w.into_inner_unfinished() } else { w.finish()? };
                out.flush()?;
                out.sync_all()?;
                syncsafe::rename(&tmp, to)
            })();
            if written.is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
            written
        }
        Format::Records => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            let mut out = format!("{}\n", crate::records::RECORDS_FIRST_LINE);
            for record in session_records(&from_dir, &from_rel, &bytes).map_err(to_io)? {
                out.push_str(&crate::records::seal_record(&to_key, &to_rel, &record));
                out.push('\n');
            }
            syncsafe::write_atomic_synced(to, out)
        }
    }
}

/// [`syncsafe::rename`], holding the write lock of the session `to` is in.
pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    let _lock = lock_for(to.as_ref());
    syncsafe::rename(from, to)
}

/// Creates the file at `path`, replacing one that is there, to write a
/// stream: sealed under its session's key for a session file, a plaintext
/// file elsewhere (a WAV file for PCM; see [`StreamWriter::plain`]).
pub fn create_stream(path: impl AsRef<Path>, header: StreamHeader) -> io::Result<StreamWriter<std::fs::File>> {
    create_stream_as(path.as_ref(), path.as_ref(), header)
}

/// [`create_stream`] at `path`, sealed as the file at `sealed_as` (its
/// session's key and relative path), for a file written beside its
/// destination and renamed over it.
pub fn create_stream_as(
    path: impl AsRef<Path>,
    sealed_as: impl AsRef<Path>,
    header: StreamHeader,
) -> io::Result<StreamWriter<std::fs::File>> {
    let path = path.as_ref();
    let _lock = lock_for(path);
    create_stream_unlocked(path, sealed_as.as_ref(), header)
}

/// [`create_stream_as`] for a caller that holds the session's write lock.
fn create_stream_unlocked(path: &Path, sealed_as: &Path, header: StreamHeader) -> io::Result<StreamWriter<std::fs::File>> {
    match write_key(sealed_as)? {
        Some((key, rel)) => StreamWriter::new(syncsafe::create_sealed(path)?, &key, &rel, header),
        None => StreamWriter::plain(syncsafe::create(path)?, header),
    }
}

/// Opens the stream at `path` to write more at its end
/// ([`StreamWriter::append`]). A plaintext file stays plaintext and is
/// appended to as it is; a WAV file is cut after the samples its header
/// counts. A session file too short to hold a stream header is started
/// again as a sealed stream with `header`.
pub fn append_stream(path: impl AsRef<Path>, header: StreamHeader) -> io::Result<StreamWriter<std::fs::File>> {
    use io::Read as _;
    let path = path.as_ref();
    let _lock = lock_for(path);
    let mut file = syncsafe::retry(|| std::fs::OpenOptions::new().read(true).write(true).open(path))?;
    let len = file.metadata()?.len();
    let session = session_of(path).filter(|(_, rel)| rel != KEY_FILE);
    let mut head = [0u8; 4];
    let format = if len >= 4 {
        file.read_exact(&mut head)?;
        is_sealed(&head)
    } else {
        None
    };
    match (session, format) {
        (Some((dir, rel)), Some(Format::Stream)) => {
            let key = open_with(&dir, |keys| crate::stream::opening_key(&mut file, keys, &rel).map(|i| keys[i].clone()))
                .map_err(to_io)?;
            StreamWriter::append(file, &key, &rel).map_err(to_io)
        }
        (_, Some(_)) => Err(io::Error::new(io::ErrorKind::InvalidData, format!("not an appendable stream: {}", path.display()))),
        (Some(_), None) if len < crate::stream::HEADER_LEN as u64 => {
            drop(file);
            create_stream_unlocked(path, path, header)
        }
        (session, None) => {
            let wav = crate::wav_view::parse_wav(&mut file)?.map(|l| (l.data_at, l.data_len));
            let mut w = StreamWriter::plain_append(file, wav)?;
            w.hold = session.map(|_| hold_open(path));
            Ok(w)
        }
    }
}

/// The stream at `path` for reading. A plaintext WAV file passes its
/// samples through with `header().pcm` from its header; any other
/// plaintext file passes through whole as opaque bytes. A PCM stream torn
/// inside its first segment reads as empty ([`StreamReader::torn_pcm`])
/// while its session is pinned ([`keyring::is_pinned`]) or every content
/// key of the profile is held ([`keyring::holds_every_profile_key`]);
/// otherwise it does not open.
pub fn open_stream(path: impl AsRef<Path>) -> io::Result<StreamReader<std::fs::File>> {
    let path = path.as_ref();
    let (mut file, format) = open_with_format(path)?;
    match format {
        Some(Format::Stream) => {
            let (dir, rel) = session_of(path).ok_or_else(|| outside_session(path))?;
            match open_with(&dir, |keys| StreamReader::open_any(file.try_clone()?, keys, &rel)) {
                Err(SealError::Auth) if keyring::is_pinned(&dir) || keyring::holds_every_profile_key() => {
                    StreamReader::torn_pcm(file).ok_or_else(|| to_io(SealError::Auth))
                }
                opened => opened.map_err(to_io),
            }
        }
        Some(Format::Records) => Err(io::Error::new(io::ErrorKind::InvalidData, format!("not a stream: {}", path.display()))),
        None => match crate::wav_view::parse_wav(&mut file)? {
            Some(l) => Ok(StreamReader::plain(file, StreamHeader::pcm(l.format), l.data_at, l.data_len)),
            None => {
                let len = file.metadata()?.len();
                Ok(StreamReader::plain(file, StreamHeader::opaque(), 0, len))
            }
        },
    }
}

/// The file at `path` as a WAV file: a sealed PCM stream through
/// [`crate::wav_view::wav_view`], a plaintext file as it is.
pub fn open_wav(path: impl AsRef<Path>) -> io::Result<WavView<std::fs::File>> {
    let path = path.as_ref();
    let (mut file, format) = open_with_format(path)?;
    if format.is_some() {
        return Ok(crate::wav_view::wav_view(open_stream(path)?));
    }
    let layout = crate::wav_view::parse_wav(&mut file)?;
    let len = file.metadata()?.len();
    let header = layout.as_ref().map_or(StreamHeader::opaque(), |l| StreamHeader::pcm(l.format));
    let data_len = layout.map_or(0, |l| l.data_len);
    Ok(crate::wav_view::wav_passthrough(StreamReader::plain(file, header, 0, len), data_len))
}

/// The file at `path` and the sealed format its first bytes announce.
fn open_with_format(path: &Path) -> io::Result<(std::fs::File, Option<Format>)> {
    use io::Read as _;
    let mut file = syncsafe::open(path)?;
    let mut head = [0u8; 4];
    let format = match file.read_exact(&mut head) {
        Ok(()) => is_sealed(&head),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => None,
        Err(e) => return Err(e),
    };
    Ok((file, format))
}

fn outside_session(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("sealed file outside a session: {}", path.display()))
}

/// True when the file holds no plaintext: it is empty, or a sealed stream
/// with no segment or a single empty one. Needs no key.
pub fn holds_no_plaintext(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if meta.len() == 0 {
        return true;
    }
    if !crate::stream::is_empty_len(meta.len()) {
        return false;
    }
    let mut magic = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| io::Read::read_exact(&mut f, &mut magic)).is_ok()
        && is_sealed(&magic) == Some(Format::Stream)
}

/// The sealed bytes to write for a session file; `None` for a path that is
/// written as it is (outside a session, or the session's `key.json`). The
/// session's key is created on its first write.
fn sealed(path: &Path, plain: &[u8]) -> io::Result<Option<Vec<u8>>> {
    Ok(write_key(path)?.map(|(key, rel)| seal_whole(&key, &rel, plain)))
}

/// The key and relative path a write to `path` seals under, creating the
/// session's key on its first write; `None` for a path written as it is
/// (outside a session, or the session's `key.json`). `NotFound` when the
/// session folder does not exist.
fn write_key(path: &Path) -> io::Result<Option<(SessionKey, String)>> {
    let Some((dir, rel)) = session_of(path) else { return Ok(None) };
    if rel == KEY_FILE {
        return Ok(None);
    }
    if !dir.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("no session folder: {}", dir.display())));
    }
    let key = keyring::create_session_key(&dir).map_err(to_io)?;
    Ok(Some((key, rel)))
}

/// True when the sealed session file at `path` opens under one of its
/// session's other keys and not under the session's own key, judged by its
/// first segment or first record. False for a plaintext file, for
/// `key.json`, and while the session's own key is not held.
pub fn needs_rekey(path: &Path) -> bool {
    use io::Read as _;
    let Some((dir, rel)) = session_of(path).filter(|(_, rel)| rel != KEY_FILE) else { return false };
    let Ok(own) = keyring::session_key(&dir) else { return false };
    let Ok((mut file, format)) = open_with_format(path) else { return false };
    let own = std::slice::from_ref(&own);
    match format {
        Some(Format::Stream) => {
            let opens = |file: &mut std::fs::File, keys: &[SessionKey]| {
                matches!(crate::stream::first_opening_key(file, keys, &rel), Ok(Some(_)))
            };
            !opens(&mut file, own) && opens(&mut file, &keyring::other_session_keys(&dir))
        }
        Some(Format::Records) => {
            let mut head = Vec::new();
            if io::Seek::rewind(&mut file).and_then(|_| file.take(REKEY_RECORDS_HEAD).read_to_end(&mut head)).is_err() {
                return false;
            }
            crate::records::first_opening_key(own, &rel, &head).is_none()
                && crate::records::first_opening_key(&keyring::other_session_keys(&dir), &rel, &head).is_some()
        }
        None => false,
    }
}

/// How much of a records file [`needs_rekey`] reads to find its first
/// record.
const REKEY_RECORDS_HEAD: u64 = 64 * 1024;

/// Re-seals the sealed session file at `path` under its session's own key
/// when it needs it ([`needs_rekey`]), holding the session's write lock: a
/// stream is written beside it as `<name>.rekey.tmp`, synced, read back
/// and compared, and renamed over it, torn or not as it was; a records file
/// is replaced atomically with every record that opens. Returns whether
/// the file was re-sealed. Left as it is, and false: a file that changed
/// on disk since it was read (its length, modification time or SHA-256),
/// and a records file with a line no held key opens other than a torn
/// last line.
pub fn rekey(path: &Path) -> io::Result<bool> {
    rekey_with(path, &|_, _| false)
}

/// A point in [`rekey`] a test acts at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RekeyStep {
    /// The file is read and hashed.
    Read,
    /// A stream's re-sealed copy is written, synced and read back beside
    /// the file.
    TmpWritten,
}

/// [`rekey`], calling `step` at each [`RekeyStep`]; true from it stops the
/// call there with nothing cleaned up, as a kill would.
pub(crate) fn rekey_with(path: &Path, step: &dyn Fn(&Path, RekeyStep) -> bool) -> io::Result<bool> {
    use io::{Read as _, Seek as _};
    let stopped = || io::Error::new(io::ErrorKind::Interrupted, "stopped");
    let Some((dir, rel)) = session_of(path) else { return Ok(false) };
    let _lock = lock_session(&dir);
    if !needs_rekey(path) {
        return Ok(false);
    }
    let own = keyring::session_key(&dir).map_err(to_io)?;
    let mut keys = vec![own.clone()];
    keys.extend(keyring::other_session_keys(&dir));
    let (mut file, format) = open_with_format(path)?;
    let before = file.metadata()?;
    file.rewind()?;
    if format == Some(Format::Records) {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let whole = &bytes[..bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1)];
        if open_records_any(&keys, &rel, whole).1 > 0 {
            return Ok(false);
        }
        let mut out = format!("{RECORDS_FIRST_LINE}\n");
        for record in open_records_any(&keys, &rel, &bytes).0 {
            out.push_str(&seal_record(&own, &rel, &record));
            out.push('\n');
        }
        let digest: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(&bytes).into();
        if step(path, RekeyStep::Read) {
            return Err(stopped());
        }
        if !unchanged_since(path, &before, &digest) {
            return Ok(false);
        }
        syncsafe::write_atomic_synced(path, out)?;
        sync_parent(path)?;
        return Ok(true);
    }
    let digest = copy_hashed(&mut file, &mut io::sink())?;
    file.rewind()?;
    if step(path, RekeyStep::Read) {
        return Err(stopped());
    }
    let mut reader = StreamReader::open_any(file, &keys[1..], &rel).map_err(to_io)?;
    let torn = reader.truncated();
    let tmp = path.with_file_name(format!("{}.rekey.tmp", path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
    let written = (|| -> io::Result<Option<bool>> {
        use io::Write as _;
        let mut w = StreamWriter::new(syncsafe::create_sealed(&tmp)?, &own, &rel, *reader.header())?;
        let want = copy_hashed(&mut reader, &mut w)?;
        let mut out = if torn { w.into_inner_unfinished() } else { w.finish()? };
        out.flush()?;
        out.sync_all()?;
        let mut back = StreamReader::open(syncsafe::open(&tmp)?, &own, &rel).map_err(to_io)?;
        if back.truncated() != torn || copy_hashed(&mut back, &mut io::sink())? != want {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("the re-sealed copy of {rel} does not read back")));
        }
        if step(path, RekeyStep::TmpWritten) {
            return Ok(None);
        }
        if !unchanged_since(path, &before, &digest) {
            return Ok(Some(false));
        }
        syncsafe::rename(&tmp, path)?;
        Ok(Some(true))
    })();
    match written {
        Ok(None) => Err(stopped()),
        Ok(Some(true)) => {
            sync_parent(path)?;
            Ok(true)
        }
        other => {
            let _ = std::fs::remove_file(&tmp);
            other.map(|_| false)
        }
    }
}

/// True when the file at `path` still has the length and modification
/// time `before` recorded and the SHA-256 `digest`.
fn unchanged_since(path: &Path, before: &std::fs::Metadata, digest: &[u8; 32]) -> bool {
    let same_meta = std::fs::metadata(path).is_ok_and(|now| {
        now.len() == before.len() && matches!((now.modified(), before.modified()), (Ok(a), Ok(b)) if a == b)
    });
    same_meta && syncsafe::open(path).and_then(|mut f| copy_hashed(&mut f, &mut io::sink())).is_ok_and(|d| d == *digest)
}

/// Syncs the folder holding `path` (a rename in it) to disk. Windows opens
/// no folder as a file; there it does nothing.
pub(crate) fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Copies `from` to `to` and returns the SHA-256 of what passed.
fn copy_hashed(from: &mut impl io::Read, to: &mut impl io::Write) -> io::Result<[u8; 32]> {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; crate::stream::SEGMENT];
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            return Ok(hasher.finalize().into());
        }
        hasher.update(&buf[..n]);
        to.write_all(&buf[..n])?;
    }
}

/// An append-only log (JSONL) open for appending, one record per line. A
/// session file is in the records format (`DSR1`), each line sealed on its
/// own; a session file that already holds plaintext lines, and every file
/// outside a session, gets plaintext lines. Each record is written with a
/// single write, so records appended through two handles land whole.
pub struct RecordLog {
    file: std::fs::File,
    seal: Option<(SessionKey, String)>,
    /// The session folder whose write lock each append holds.
    session: Option<PathBuf>,
    /// Set while a plaintext session log is open.
    _hold: Option<OpenHold>,
}

impl RecordLog {
    /// Opens `path` for appending, creating it when missing. A torn last
    /// line is ended first, so the next record starts a line of its own. A
    /// session log written whole as a stream is rewritten in the records
    /// format first. A session log that holds only the start of the records
    /// first line (a crash while it was written) is started again.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        use io::{Read as _, Seek as _, Write as _};
        let path = path.as_ref();
        let open = || -> io::Result<(std::fs::File, Option<Format>)> {
            let mut file =
                syncsafe::retry(|| std::fs::OpenOptions::new().read(true).append(true).create(true).open(path))?;
            let mut head = [0u8; 4];
            let mut got = 0;
            while got < head.len() {
                match file.read(&mut head[got..])? {
                    0 => break,
                    n => got += n,
                }
            }
            Ok((file, if got == head.len() { is_sealed(&head) } else { None }))
        };
        let session = session_of(path).filter(|(_, rel)| rel != KEY_FILE);
        let _lock = session.as_ref().map(|(dir, _)| lock_session(dir));
        let (mut file, mut format) = open()?;
        if session.is_some() && format == Some(Format::Stream) {
            let records = read_records(path)?;
            write_records_unlocked(path, &records)?;
            drop(file);
            (file, format) = open()?;
        }
        let dir = session.as_ref().map(|(dir, _)| dir.clone());
        let mut len = file.metadata()?.len();
        if session.is_some() && len > 0 && len < RECORDS_FIRST_LINE.len() as u64 {
            let mut head = Vec::new();
            file.seek(io::SeekFrom::Start(0))?;
            file.read_to_end(&mut head)?;
            if RECORDS_FIRST_LINE.as_bytes().starts_with(&head) {
                file.set_len(0)?;
                len = 0;
            }
        }
        let seal = match session {
            Some((dir, rel)) if len == 0 => {
                let key = keyring::create_session_key(&dir).map_err(to_io)?;
                file.write_all(format!("{RECORDS_FIRST_LINE}\n").as_bytes())?;
                Some((key, rel))
            }
            Some((dir, rel)) if format == Some(Format::Records) => Some((keyring::session_key(&dir).map_err(to_io)?, rel)),
            _ => None,
        };
        if len > 0 {
            let mut last = [0u8; 1];
            file.seek(io::SeekFrom::Start(len - 1))?;
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.write_all(b"\n")?;
            }
        }
        let hold = (dir.is_some() && seal.is_none()).then(|| hold_open(path));
        Ok(Self { file, seal, session: dir, _hold: hold })
    }

    /// Appends one record and its newline.
    pub fn append(&mut self, record: &[u8]) -> io::Result<()> {
        use io::Write as _;
        let _lock = self.session.as_deref().map(lock_session);
        let mut line = match &self.seal {
            Some((key, rel)) => seal_record(key, rel, record).into_bytes(),
            None => record.to_vec(),
        };
        line.push(b'\n');
        self.file.write_all(&line)
    }
}

/// Appends one record to the log at `path` (see [`RecordLog`]).
pub fn append_record(path: impl AsRef<Path>, record: &[u8]) -> io::Result<()> {
    RecordLog::open(path)?.append(record)
}

/// The records of a log, in order, whatever form it is in: sealed records,
/// a stream written whole, or plaintext lines. Blank lines are skipped; a
/// sealed line that does not open (torn by a crash) is dropped.
pub fn read_records(path: impl AsRef<Path>) -> io::Result<Vec<Vec<u8>>> {
    let path = path.as_ref();
    let bytes = syncsafe::read(path)?;
    let session = session_of(path);
    let plain = match (&session, is_sealed(&bytes)) {
        (Some((dir, rel)), Some(Format::Records)) => return session_records(dir, rel, &bytes).map_err(to_io),
        (Some((dir, rel)), Some(Format::Stream)) => open_with(dir, |keys| open_whole_any(keys, rel, &bytes)).map_err(to_io)?,
        _ => bytes,
    };
    Ok(plain
        .split(|b| *b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
        .map(<[u8]>::to_vec)
        .collect())
}

/// [`read_records`] for a caller that writes the log back whole with
/// [`write_records`]. A sealed session log with a well-formed line before
/// its last line break that no held key opens fails with
/// `PermissionDenied`, so a rewrite never drops lines sealed under a key
/// that has not opened yet. A damaged line (not base64 of a nonce and a
/// tag) and a torn last line are dropped, as [`read_records`] drops them.
pub fn read_records_to_rewrite(path: impl AsRef<Path>) -> io::Result<Vec<Vec<u8>>> {
    let path = path.as_ref();
    let bytes = syncsafe::read(path)?;
    let Some((dir, rel)) = session_of(path).filter(|_| is_sealed(&bytes) == Some(Format::Records)) else {
        return read_records(path);
    };
    let mut keys: Vec<SessionKey> = keyring::session_key(&dir).ok().into_iter().collect();
    keys.extend(keyring::other_session_keys(&dir));
    let whole = &bytes[..bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1)];
    if open_records_any(&keys, &rel, whole).1 > 0 {
        return Err(to_io(SealError::OtherContentKey));
    }
    Ok(open_records_any(&keys, &rel, &bytes).0)
}

/// Replaces the log at `path` with `records`, atomically: sealed records
/// for a session file, plaintext lines elsewhere.
pub fn write_records<R: AsRef<[u8]>>(path: impl AsRef<Path>, records: &[R]) -> io::Result<()> {
    let path = path.as_ref();
    let _lock = lock_for(path);
    write_records_unlocked(path, records)
}

/// [`write_records`] for a caller that holds the session's write lock.
fn write_records_unlocked<R: AsRef<[u8]>>(path: &Path, records: &[R]) -> io::Result<()> {
    let mut out = Vec::new();
    match write_key(path)? {
        Some((key, rel)) => {
            out.extend_from_slice(RECORDS_FIRST_LINE.as_bytes());
            out.push(b'\n');
            for record in records {
                out.extend_from_slice(seal_record(&key, &rel, record.as_ref()).as_bytes());
                out.push(b'\n');
            }
        }
        None => {
            for record in records {
                out.extend_from_slice(record.as_ref());
                out.push(b'\n');
            }
        }
    }
    syncsafe::write_atomic(path, out)
}

/// A key that is not available is `PermissionDenied`; bytes that do not
/// open are `InvalidData`.
fn to_io(e: SealError) -> io::Error {
    match e {
        SealError::Io(e) => e,
        SealError::Locked | SealError::OtherContentKey | SealError::NoContentKey => {
            io::Error::new(io::ErrorKind::PermissionDenied, e)
        }
        SealError::Auth | SealError::Format(_) => io::Error::new(io::ErrorKind::InvalidData, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyfile::{derive_session_key, unwrap_session_key, wrap_session_key, wrapped_to};
    use crate::stream::seal_whole;
    use crate::ContentSecret;

    /// The keyring is process-wide; these tests take turns.
    fn alone() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn copy(secret: &ContentSecret) -> ContentSecret {
        ContentSecret::from_bytes(*secret.to_bytes())
    }

    const ID: &str = "daisy-1700000000";

    /// A session whose `key.json` is wrapped to a key `a` and whose
    /// `notes.md` and `log.jsonl` are sealed under the key `b` derives, with
    /// `a` the active key and both held.
    struct Merged {
        _td: tempfile::TempDir,
        dir: PathBuf,
        b: ContentSecret,
    }

    fn merged() -> Merged {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("sessions").join(ID);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (ContentSecret::generate().0, ContentSecret::generate().0);
        let kb = derive_session_key(&b, ID);
        std::fs::write(dir.join(KEY_FILE), wrap_session_key(&a.public(), &derive_session_key(&a, ID))).unwrap();
        std::fs::write(dir.join("notes.md"), seal_whole(&kb, "notes.md", b"from b")).unwrap();
        let log = format!("{RECORDS_FIRST_LINE}\n{}\n", seal_record(&kb, "log.jsonl", b"{\"n\":1}"));
        std::fs::write(dir.join("log.jsonl"), log).unwrap();
        keyring::lock();
        keyring::unlock(copy(&a));
        keyring::add_secret(copy(&b));
        Merged { _td: td, dir, b }
    }

    fn tmp_of(path: &Path) -> PathBuf {
        path.with_file_name(format!("{}.rekey.tmp", path.file_name().unwrap().to_string_lossy()))
    }

    #[test]
    fn rekey_leaves_a_file_the_sync_tool_replaced_while_it_ran() {
        let _g = alone();
        let m = merged();
        let kb = derive_session_key(&m.b, ID);
        for (name, newer) in [
            ("notes.md", seal_whole(&kb, "notes.md", b"newer from b")),
            ("log.jsonl", format!("{RECORDS_FIRST_LINE}\n{}\n", seal_record(&kb, "log.jsonl", b"{\"n\":2}")).into_bytes()),
        ] {
            let path = m.dir.join(name);
            assert!(needs_rekey(&path), "{name}");
            let arrive = |p: &Path, at: RekeyStep| {
                if at == RekeyStep::Read {
                    std::fs::write(p, &newer).unwrap();
                }
                false
            };
            assert!(!rekey_with(&path, &arrive).unwrap(), "{name}: left for the next pass");
            assert_eq!(std::fs::read(&path).unwrap(), newer, "{name}: the newer version stays");
            assert!(!tmp_of(&path).exists(), "{name}");
            assert!(rekey(&path).unwrap(), "{name}: the next pass moves it");
            assert!(!needs_rekey(&path));
        }
        assert_eq!(read(m.dir.join("notes.md")).unwrap(), b"newer from b");
        assert_eq!(read_records(m.dir.join("log.jsonl")).unwrap(), vec![b"{\"n\":2}".to_vec()]);
        keyring::lock();
    }

    #[test]
    fn a_rekey_cut_short_leaves_the_file_whole_and_the_next_pass_finishes() {
        let _g = alone();
        let m = merged();
        let path = m.dir.join("notes.md");
        let before = std::fs::read(&path).unwrap();
        let kill = |_: &Path, at: RekeyStep| at == RekeyStep::TmpWritten;
        assert!(rekey_with(&path, &kill).is_err());
        assert!(tmp_of(&path).is_file(), "the copy a kill leaves");
        assert_eq!(std::fs::read(&path).unwrap(), before, "the file is untouched");
        assert_eq!(read(&path).unwrap(), b"from b");

        assert!(rekey(&path).unwrap());
        assert!(!tmp_of(&path).exists());
        assert!(!needs_rekey(&path));
        assert_eq!(read(&path).unwrap(), b"from b");
        keyring::lock();
    }

    #[test]
    fn records_rekey_refuses_a_line_no_held_key_opens_but_not_a_torn_last_line() {
        let _g = alone();
        let m = merged();
        let path = m.dir.join("log.jsonl");
        let kb = derive_session_key(&m.b, ID);
        let kc = derive_session_key(&ContentSecret::generate().0, ID);
        let line = |k: &SessionKey, n: u8| seal_record(k, "log.jsonl", format!("{{\"n\":{n}}}").as_bytes());
        let not_held = format!("{RECORDS_FIRST_LINE}\n{}\n{}\n", line(&kb, 1), line(&kc, 2));
        std::fs::write(&path, &not_held).unwrap();
        assert!(needs_rekey(&path));
        assert!(!rekey(&path).unwrap(), "a line under a key not held stays");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), not_held);

        let torn = format!("{RECORDS_FIRST_LINE}\n{}\n{}", line(&kb, 1), &line(&kb, 2)[..20]);
        std::fs::write(&path, torn).unwrap();
        assert!(rekey(&path).unwrap(), "a torn last line is a crash's");
        assert_eq!(read_records(&path).unwrap(), vec![b"{\"n\":1}".to_vec()]);
        keyring::lock();
    }

    #[test]
    fn a_read_to_rewrite_refuses_a_log_with_a_line_no_held_key_opens_but_not_a_torn_last_line() {
        let _g = alone();
        let m = merged();
        let path = m.dir.join("log.jsonl");
        let kb = derive_session_key(&m.b, ID);
        let kc = derive_session_key(&ContentSecret::generate().0, ID);
        let line = |k: &SessionKey, n: u8| seal_record(k, "log.jsonl", format!("{{\"n\":{n}}}").as_bytes());
        std::fs::write(&path, format!("{RECORDS_FIRST_LINE}\n{}\n{}\n", line(&kb, 1), line(&kc, 2))).unwrap();
        assert_eq!(read_records(&path).unwrap(), vec![b"{\"n\":1}".to_vec()]);
        let refused = read_records_to_rewrite(&path).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);

        std::fs::write(&path, format!("{RECORDS_FIRST_LINE}\n{}\n{}", line(&kb, 1), &line(&kb, 2)[..20])).unwrap();
        assert_eq!(read_records_to_rewrite(&path).unwrap(), vec![b"{\"n\":1}".to_vec()]);
        keyring::lock();
    }

    #[test]
    fn a_read_to_rewrite_drops_a_damaged_line_inside_the_log() {
        let _g = alone();
        let m = merged();
        let path = m.dir.join("log.jsonl");
        let kb = derive_session_key(&m.b, ID);
        let line = |n: u8| seal_record(&kb, "log.jsonl", format!("{{\"n\":{n}}}").as_bytes());
        // A line torn by a crash with the next append after it, a line cut
        // short of a nonce and a tag, and one that is not base64.
        let torn = &line(2)[..21];
        let short = &line(3)[..20];
        let text = format!("{RECORDS_FIRST_LINE}\n{}\n{torn}\n{short}\n#not base64#\n{}\n", line(1), line(4));
        std::fs::write(&path, text).unwrap();
        assert_eq!(read_records_to_rewrite(&path).unwrap(), vec![b"{\"n\":1}".to_vec(), b"{\"n\":4}".to_vec()]);
        keyring::lock();
    }

    #[test]
    fn a_rewrap_cut_short_keeps_key_json_and_the_next_pass_finishes() {
        let _g = alone();
        let td = tempfile::tempdir().unwrap();
        let dir = td.path().join("sessions").join(ID);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (ContentSecret::generate().0, ContentSecret::generate().0);
        // A key made while locked: key.json is its only copy.
        let random = SessionKey::random();
        std::fs::write(dir.join(KEY_FILE), wrap_session_key(&b.public(), &random)).unwrap();
        keyring::lock();
        keyring::unlock(copy(&a));
        keyring::add_secret(copy(&b));
        assert!(keyring::wrapped_to_other_open_key(&dir));
        let before = std::fs::read(dir.join(KEY_FILE)).unwrap();

        assert!(keyring::rewrap_session_key_with(&dir, &|| true).is_err());
        assert!(dir.join(keyring::REWRAP_TMP).is_file(), "the copy a kill leaves");
        assert_eq!(std::fs::read(dir.join(KEY_FILE)).unwrap(), before);
        assert_eq!(unwrap_session_key(&b, &before).unwrap(), random);

        assert!(keyring::rewrap_session_key(&dir).unwrap());
        assert!(!dir.join(keyring::REWRAP_TMP).exists());
        let after = std::fs::read(dir.join(KEY_FILE)).unwrap();
        assert_eq!(wrapped_to(&after).unwrap(), a.public());
        assert_eq!(unwrap_session_key(&a, &after).unwrap(), random);
        assert!(!keyring::rewrap_session_key(&dir).unwrap());
        keyring::lock();
    }
}
