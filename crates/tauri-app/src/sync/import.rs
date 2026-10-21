// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Moving a phone's session out of the staging area and into the library:
//! open what the phone sealed, check the manifest is one this desktop reads,
//! write the session where every other session lives, and remember the
//! revisions it arrived at.

use std::collections::BTreeMap;
use std::path::Path;

use zeroize::Zeroizing;

use crate::sync::inbox::{is_relative_file_path, is_session_id, is_wire_session_id, Inbox, Refusal, RefusalReason, INPUTS};
use crate::sync::signing::body_hash;

/// This machine's envelope key: what a phone wrapped a file's data key to.
#[derive(Clone)]
pub struct MachineKey {
    pub x25519_secret: [u8; 32],
    pub device_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportError {
    /// A file the session needs is not staged. The next hello brings it.
    Missing,
    /// A sealed file this desktop's key does not open. Only a re-pair
    /// changes that.
    Sealed,
    /// A file this build does not read: a newer manifest or envelope schema,
    /// a transcript that does not parse, or audio in a container the sync
    /// contract does not name.
    Unsupported,
    /// An audio chunk in a container the sync contract names that does not
    /// decode: damaged, or holding no sound.
    Undecodable,
    /// The device deleted the session while it was being imported. Nothing
    /// more is written and nothing is reported.
    Entombed,
    Io,
}

impl ImportError {
    /// The refusal an error stands for, when retrying the same bytes could
    /// only fail the same way.
    pub fn refusal(self) -> Option<RefusalReason> {
        match self {
            ImportError::Sealed => Some(RefusalReason::Sealed),
            ImportError::Unsupported => Some(RefusalReason::Unsupported),
            ImportError::Undecodable => Some(RefusalReason::Undecodable),
            ImportError::Missing | ImportError::Io | ImportError::Entombed => None,
        }
    }
}

/// The first id this desktop mints for an import whose own id the library already holds.
pub const MINTED_ID_FLOOR: u64 = 900_000_000_000;

/// The number in a session id, or `None` for one that is not a session id.
fn id_number(id: &str) -> Option<u64> {
    if !is_session_id(id) {
        return None;
    }
    id.strip_prefix("daisy-")?.parse().ok()
}

/// The second a device's clock read when it minted a session id, or `None`
/// for an id this desktop minted.
pub fn minted_at(session: &str) -> Option<u64> {
    id_number(session).filter(|n| *n < MINTED_ID_FLOOR)
}

/// The longest title a refusal names, in characters.
const REFUSED_TITLE_MAX: usize = 80;

/// The containers the sync contract names for chunk audio: the phone's AAC
/// `.m4a`, and the WAV every desktop reader expects.
const CHUNK_CONTAINERS: [&str; 2] = ["m4a", "wav"];

/// The title a person gave a staged session: its `meta.json`'s, else its
/// manifest's, without control or bidi characters and cut to
/// `REFUSED_TITLE_MAX` characters. `None` when it has none, or when neither
/// opens.
pub fn staged_title(inbox: &Inbox, device_id: &str, session: &str, key: &MachineKey) -> Option<String> {
    let from_meta = inbox
        .staged_record(device_id, session, "meta.json")
        .and_then(|b| serde_json::from_slice::<recording::session_meta::SessionMeta>(&b).ok())
        .and_then(|m| m.title)
        .map(|c| c.value);
    let from_manifest = || {
        let raw = inbox.read(device_id, session, "manifest.json")?;
        let plain = open(&raw, key).ok()?;
        let value: serde_json::Value = serde_json::from_slice(&plain).ok()?;
        value["title"].as_str().map(str::to_string)
    };
    let shown = |t: String| -> Option<String> {
        let clean: String = t.chars().filter(|c| !c.is_control() && !is_bidi_control(*c)).collect();
        let clean = clean.trim();
        if clean.is_empty() {
            return None;
        }
        if clean.chars().count() <= REFUSED_TITLE_MAX {
            return Some(clean.to_string());
        }
        let cut: String = clean.chars().take(REFUSED_TITLE_MAX - 1).collect();
        Some(format!("{}…", cut.trim_end()))
    };
    from_meta.and_then(shown).or_else(|| from_manifest().and_then(shown))
}

/// The marks and isolates that reorder the text around them.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Every minted id the library holds: the device it was minted for, then the id that device knows the session by, then the id the library holds it under. Read once per hello.
pub type MintedIds = BTreeMap<String, BTreeMap<String, String>>;

pub fn minted_ids(sessions_dir: &Path) -> MintedIds {
    let Ok(entries) = std::fs::read_dir(sessions_dir) else { return MintedIds::new() };
    let mut out = MintedIds::new();
    for name in entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|name| id_number(name).is_some_and(|n| n >= MINTED_ID_FLOOR))
    {
        let Ok(bytes) = syncsafe::read(sessions_dir.join(&name).join("manifest.json")) else { continue };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        let (Some(device), Some(wire)) = (value["origin_device"].as_str(), value["origin_session"].as_str()) else {
            continue;
        };
        out.entry(device.to_string()).or_default().insert(wire.to_string(), name);
    }
    out
}

/// The id the library holds a device's session under: the minted one when
/// that device's id was taken here, and the device's own id otherwise.
/// Everything a hello says about a session resolves through this.
pub fn local_id(minted: &MintedIds, device_id: &str, session: &str) -> String {
    minted
        .get(device_id)
        .and_then(|m| m.get(session))
        .cloned()
        .unwrap_or_else(|| session.to_string())
}

/// The lowest minted id no session holds.
fn mint_id(sessions_dir: &Path) -> Result<String, ImportError> {
    (MINTED_ID_FLOOR..=999_999_999_999)
        .map(|n| format!("daisy-{n}"))
        .find(|id| !sessions_dir.join(id).exists())
        .ok_or(ImportError::Io)
}

/// The plaintext of a staged file: opened when the phone sealed it, taken as
/// it is when the phone wrote it in the clear.
pub(crate) fn open(bytes: &[u8], key: &MachineKey) -> Result<Zeroizing<Vec<u8>>, ImportError> {
    if !vault::envelope_v2::is_v2(bytes) {
        return Ok(Zeroizing::new(bytes.to_vec()));
    }
    vault::envelope_v2::open_v2(bytes, &key.x25519_secret, &key.device_id).map_err(|e| match e {
        vault::VaultError::UnsupportedVersion(_) | vault::VaultError::Json(_) | vault::VaultError::Base64(_) => {
            ImportError::Unsupported
        }
        vault::VaultError::NoRecipient
        | vault::VaultError::AuthenticationFailed
        | vault::VaultError::Cipher(_)
        | vault::VaultError::Kdf(_)
        | vault::VaultError::PassphraseTooShort(..)
        | vault::VaultError::WeakPassphrase { .. } => ImportError::Sealed,
    })
}

fn staged(
    inbox: &Inbox,
    device_id: &str,
    session: &str,
    file: &str,
    key: &MachineKey,
) -> Result<Zeroizing<Vec<u8>>, ImportError> {
    open(&inbox.read(device_id, session, file).ok_or(ImportError::Missing)?, key)
}

/// The audio a manifest names, skipping any path that could leave the
/// session directory.
fn chunk_paths(manifest: &serde_json::Value) -> Vec<String> {
    manifest["chunks"]
        .as_array()
        .map(|chunks| {
            chunks
                .iter()
                .filter_map(|c| c["mic_wav_relative"].as_str())
                .filter(|p| is_relative_file_path(p))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The chunk audio a staged session's manifest names that has not arrived:
/// what a hello asks for once the manifest is here. `Missing` when the
/// manifest is not staged; `Sealed` or `Unsupported` for one this desktop
/// cannot open or read, or that names no audio at all.
pub fn missing(inbox: &Inbox, device_id: &str, session: &str, key: &MachineKey) -> Result<Vec<String>, ImportError> {
    let manifest = staged(inbox, device_id, session, "manifest.json", key)?;
    let value: serde_json::Value = serde_json::from_slice(&manifest).map_err(|_| ImportError::Unsupported)?;
    let paths = chunk_paths(&value);
    if paths.is_empty() {
        return Err(ImportError::Unsupported);
    }
    if audio_is_held(inbox, device_id, session) {
        return Ok(Vec::new());
    }
    Ok(paths.into_iter().filter(|p| !inbox.exists(device_id, session, p)).collect())
}

/// True when the latest hello named audio the library already holds for
/// this session: its chunks stay where they are.
fn audio_is_held(inbox: &Inbox, device_id: &str, session: &str) -> bool {
    inbox.claim(device_id, session).is_some_and(|c| c.held_audio.is_some())
}

/// How much of a staged session's audio is here: the chunks that have
/// arrived and the chunks its manifest names. `None` until the manifest
/// itself is staged and this desktop can open it.
pub fn chunk_progress(inbox: &Inbox, device_id: &str, session: &str, key: &MachineKey) -> Option<(usize, usize)> {
    let manifest = staged(inbox, device_id, session, "manifest.json", key).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&manifest).ok()?;
    let paths = chunk_paths(&value);
    if paths.is_empty() {
        return None;
    }
    if audio_is_held(inbox, device_id, session) {
        return Some((paths.len(), paths.len()));
    }
    Some((paths.iter().filter(|p| inbox.exists(device_id, session, p)).count(), paths.len()))
}

/// How far past this desktop's clock a phone transcript's stamps may run
/// before they are read as this desktop's clock plus this.
const STAMP_SKEW_SECONDS: i64 = 5 * 60;

/// A phone transcript is untrusted input: its transcription and edit stamps
/// are held to `now` plus `STAMP_SKEW_SECONDS`, word times that do not match
/// their segment's text and span are dropped, then split groups are kept only
/// where a re-diarize can rejoin them and renumbered from 0
/// (`transcript::split::normalize_split_groups`). Returns the rewritten
/// transcript, or `None` when nothing changed and the original bytes stand.
fn sanitized_transcript(mut parsed: transcript::SessionTranscript, now: i64) -> Result<Option<Vec<u8>>, ImportError> {
    // A transcript from the phone is never provisional.
    let mut changed = std::mem::take(&mut parsed.provisional);
    let latest = now.saturating_add(STAMP_SKEW_SECONDS);
    if parsed.transcribed_at_unix_seconds > latest {
        parsed.transcribed_at_unix_seconds = latest;
        changed = true;
    }
    if let Some(edited) = parsed.edited_at_unix_seconds.as_mut().filter(|e| **e > latest) {
        *edited = latest;
        changed = true;
    }
    for seg in parsed.chunks.iter_mut().flat_map(|c| &mut c.tracks).flat_map(|t| &mut t.segments) {
        if !seg.words_consistent() {
            seg.words.clear();
            changed = true;
        }
    }
    changed |= transcript::split::normalize_split_groups(&mut parsed);
    if !changed {
        return Ok(None);
    }
    serde_json::to_vec_pretty(&parsed).map(Some).map_err(|_| ImportError::Io)
}

/// True when everything a session needs has arrived: its manifest, every
/// chunk the manifest lists, and every other input the latest hello offered
/// and asked for. A session the phone offers no transcript for imports
/// without one. A manifest this machine cannot open counts as ready: the
/// import refuses it and says so, where waiting on it would wait forever.
pub fn is_ready(inbox: &Inbox, device_id: &str, session: &str, key: &MachineKey) -> bool {
    match missing(inbox, device_id, session, key) {
        Ok(paths) => paths.is_empty() && offered_inputs_staged(inbox, device_id, session),
        Err(ImportError::Missing) => false,
        Err(_) => true,
    }
}

/// True when every input the latest hello offered, other than the manifest
/// and what this desktop keeps, is staged. Live captions are waited on only
/// for a session the library holds at every other revision
/// (`Claim::live_only`). With no hello on record, or none within
/// `CLAIM_LIFETIME`, nothing beyond the manifest is waited on.
fn offered_inputs_staged(inbox: &Inbox, device_id: &str, session: &str) -> bool {
    if inbox.claim_older_than(device_id, session, CLAIM_LIFETIME) {
        return true;
    }
    let Some(claim) = inbox.claim(device_id, session) else { return true };
    INPUTS
        .iter()
        .filter(|(k, _)| *k != "manifest" && claim.inputs.contains_key(*k) && !claim.kept.iter().any(|c| c == k))
        .filter(|(k, _)| *k != "live_transcript" || claim.live_only)
        .all(|(_, file)| inbox.exists(device_id, session, file))
}

/// How long the inputs a hello offered are waited on after the last hello
/// that listed the session: past it, the session imports from its manifest
/// and audio.
pub const CLAIM_LIFETIME: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// Import-record keys beside the revisions: the phone transcript this
/// desktop declined because the one it holds is newer, the phone speaker
/// table it kept out, and the stamps of the transcript the last import
/// wrote.
pub const REJECTED_TRANSCRIPT: &str = "rejected.transcript";
pub const REJECTED_TRANSCRIPT_CONTENT: &str = "rejected.content.transcript";
pub const REJECTED_SPEAKERS: &str = "rejected.speakers";
pub const REJECTED_SPEAKERS_CONTENT: &str = "rejected.content.speakers";
const WRITTEN_TRANSCRIPT: &str = "written.transcript";

/// The largest live-captions file read. One past it is recorded and not
/// promoted; the session imports without it.
pub const LIVE_MAX_BYTES: usize = 16 * 1024 * 1024;

/// The most live-caption lines read from one file.
const LIVE_MAX_LINES: usize = 200_000;

/// Stages an uploaded session file. Live captions also get the hash of their
/// plaintext recorded beside them, which is the revision a hello names them
/// by; captions this desktop cannot open get none.
pub fn stage(
    inbox: &Inbox,
    device_id: &str,
    session: &str,
    file: &str,
    hash: &str,
    body: &[u8],
    key: &MachineKey,
) -> Result<(), crate::sync::inbox::StoreError> {
    inbox.store(device_id, session, file, hash, body)?;
    if file == crate::sync::inbox::LIVE_TRANSCRIPT {
        let plain = open(body, key).ok().map(|p| body_hash(&p));
        inbox.note_plaintext(device_id, session, file, plain.as_deref())?;
    }
    Ok(())
}

/// True when the transcript the last import wrote was provisional.
pub fn holds_provisional(record: &BTreeMap<String, String>) -> bool {
    record.get(WRITTEN_TRANSCRIPT).is_some_and(|s| s.ends_with("/true"))
}

/// What identifies one revision of a transcript for `replaces`: when it was
/// transcribed and edited, and whether it is provisional.
fn stamp(t: &transcript::SessionTranscript) -> String {
    let edited = t.edited_at_unix_seconds.map(|e| e.to_string()).unwrap_or_default();
    format!("{}/{edited}/{}", t.transcribed_at_unix_seconds, t.provisional)
}

/// Whether an arriving transcript takes the place of the one the library
/// holds. A provisional transcript never replaces one that is not. Nothing
/// held, what an earlier import wrote (`written` is its stamp), or a
/// transcript stamped as the arriving one is, is replaced. Otherwise the
/// later `rank` wins and a tie keeps what is held.
fn replaces(
    held: Option<&transcript::SessionTranscript>,
    written: Option<&str>,
    arriving: &transcript::SessionTranscript,
) -> bool {
    let Some(held) = held else { return true };
    if arriving.provisional && !held.provisional {
        return false;
    }
    let held_stamp = stamp(held);
    if written == Some(held_stamp.as_str()) || held_stamp == stamp(arriving) {
        return true;
    }
    arriving.rank() > held.rank()
}

/// The final mic lines of a phone's live captions, up to `LIVE_MAX_LINES`.
/// Everything after the last newline, and a line that is not UTF-8 or does
/// not decode, is skipped; `kind` is not read.
fn live_lines(plain: &[u8]) -> Vec<recording::live_transcript::LiveTranscriptLine> {
    let whole = match plain.iter().rposition(|&b| b == b'\n') {
        Some(end) => &plain[..end],
        None => &[],
    };
    whole
        .split(|&b| b == b'\n')
        .take(LIVE_MAX_LINES)
        .filter_map(|l| std::str::from_utf8(l).ok())
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let mut value: serde_json::Value = serde_json::from_str(l).ok()?;
            value.as_object_mut()?.remove("kind");
            serde_json::from_value::<recording::live_transcript::LiveTranscriptLine>(value).ok()
        })
        .filter(|l| l.is_final && l.track == recording::live_transcript::LiveTrack::Mic)
        .collect()
}

/// Serializes writing an imported session into the library with burying one, and with every other write of a library session's `transcript.json`.
pub(crate) fn library_write_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A provisional transcript promoted from a phone's live captions onto the
/// manifest's chunks, or `None` when the captions hold no final mic line.
fn provisional_transcript(
    manifest: &recording::manifest::SessionManifest,
    session: &str,
    lines: &[recording::live_transcript::LiveTranscriptLine],
    now: i64,
) -> Option<transcript::SessionTranscript> {
    if lines.is_empty() {
        return None;
    }
    let offsets = manifest.capture_offsets_ms();
    let mut spans: Vec<transcript::promote::ChunkSpan> = manifest
        .chunks
        .iter()
        .filter_map(|c| {
            let wav = c.mic_wav_relative.to_str().filter(|p| is_relative_file_path(p))?;
            Some(transcript::promote::ChunkSpan {
                index: c.index,
                start_ms: offsets.get(&c.index).copied().unwrap_or(0),
                mic_track: transcript::Track::Mic,
                mic_wav: wav_path_of(wav).into(),
                system_wav: std::path::PathBuf::new(),
                mic_host_ns: c.mic_host_ns,
                system_host_ns: None,
                mic_samples: c.mic_samples,
                system_samples: None,
            })
        })
        .collect();
    spans.sort_by_key(|s| s.start_ms);
    let segs: Vec<transcript::promote::LiveSeg> = lines
        .iter()
        .map(|l| transcript::promote::LiveSeg {
            is_system: false,
            start_ms: l.start_ms,
            end_ms: l.end_ms,
            text: l.text.clone(),
            words: l.words.clone(),
        })
        .collect();
    let mut st = transcript::promote::promote_mic_only(&segs, &spans, session, "live", now);
    st.provisional = true;
    Some(st)
}

/// A session's `audio`: `sha256:` over each chunk's plaintext SHA-256 as lowercase hex, in manifest order, joined with `\n`.
pub fn audio_hash<'a>(chunks: impl IntoIterator<Item = &'a [u8]>) -> String {
    audio_of(chunks.into_iter().map(body_hash))
}

/// `audio` from each chunk's `sha256:` hash, in manifest order.
fn audio_of(chunk_hashes: impl IntoIterator<Item = String>) -> String {
    let hex: Vec<String> = chunk_hashes.into_iter().map(|h| h["sha256:".len()..].to_string()).collect();
    body_hash(hex.join("\n").as_bytes())
}

/// The WAV a phone chunk becomes: `chunks/0001/mic.m4a` → `chunks/0001/mic.wav`.
pub(crate) fn wav_path_of(chunk: &str) -> String {
    match chunk.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/mic.wav"),
        None => "mic.wav".to_string(),
    }
}

/// Writes the staged session into `sessions_dir` as a session of this
/// desktop's own: the manifest gains `recording_kind` and `origin_device`
/// and says the mic heard the whole room, each audio chunk is decoded to
/// the 16 kHz mono WAV every reader here expects and the manifest names
/// that file, the rest lands in the clear (the phone's `notes.json` among
/// it, when it sent one), and the revisions are recorded so the phone stops
/// offering them. The staging copies go once the session is in place.
///
/// The transcript: the phone's when it sent one; otherwise its live captions
/// promoted into a provisional one; otherwise none, and the session waits
/// for transcription like a recording made here. An arriving transcript
/// replaces the library's only by `replaces`; one it does not replace is
/// recorded under `REJECTED_TRANSCRIPT`, and the phone's speaker table stays
/// out while the library keeps its own transcript.
///
/// Returns the id the library holds the session under. It is the phone's own
/// id unless another device's recording already holds it, in which case this
/// desktop mints one and the manifest keeps the phone's id as
/// `origin_session` so everything that travels back travels under it.
pub fn import_session(
    inbox: &Inbox,
    sessions_dir: &Path,
    device_id: &str,
    session: &str,
    key: &MachineKey,
) -> Result<String, ImportError> {
    if !is_wire_session_id(session) {
        return Err(ImportError::Unsupported);
    }
    // Every file is opened before anything is written.
    let raw_manifest = inbox.read(device_id, session, "manifest.json").ok_or(ImportError::Missing)?;
    let manifest = open(&raw_manifest, key)?;
    let parsed: recording::manifest::SessionManifest =
        serde_json::from_slice(&manifest).map_err(|_| ImportError::Unsupported)?;
    if !parsed.schema_is_supported() {
        return Err(ImportError::Unsupported);
    }
    // Written back from the phone's own JSON.
    let mut value: serde_json::Value = serde_json::from_slice(&manifest).map_err(|_| ImportError::Unsupported)?;
    let paths = chunk_paths(&value);
    if paths.is_empty() {
        return Err(ImportError::Unsupported);
    }
    // What the latest hello said this desktop already holds of the session.
    let claim = inbox.claim(device_id, session).unwrap_or_default();

    // A session id already held by another device's recording gets an id of this desktop's own minting; a directory whose manifest cannot be read counts as occupied.
    let minted = minted_ids(sessions_dir);
    let own = local_id(&minted, device_id, session);
    let ours = match occupant(&sessions_dir.join(&own)) {
        Occupant::Vacant => true,
        Occupant::Device(owner) => owner == device_id,
        Occupant::Unknown => false,
    };
    // Audio the library holds at the revision the hello named stays as it is; any other audio is decoded before anything is written.
    let held_audio = claim
        .held_audio
        .clone()
        .filter(|_| ours && paths.iter().all(|p| sessions_dir.join(&own).join(wav_path_of(p)).is_file()));
    let mut chunk_hashes = Vec::new();
    let chunks: Vec<(String, Vec<i16>)> = if held_audio.is_some() {
        Vec::new()
    } else {
        paths
            .iter()
            .map(|p| {
                let bytes = staged(inbox, device_id, session, p, key)?;
                chunk_hashes.push(body_hash(&bytes));
                let ext = p.rsplit_once('.').map(|(_, e)| e);
                let named = ext.is_some_and(|e| CHUNK_CONTAINERS.iter().any(|c| e.eq_ignore_ascii_case(c)));
                let decoded = recording::audio_import::decode_bytes_to_mono_16k(bytes.to_vec(), ext)
                    .map_err(|_| if named { ImportError::Undecodable } else { ImportError::Unsupported })?;
                Ok((p.clone(), decoded.pcm))
            })
            .collect::<Result<_, _>>()?
    };
    let audio = held_audio.unwrap_or_else(|| audio_of(chunk_hashes));
    let local = if ours {
        own
    } else {
        match same_recording(sessions_dir, &minted, session, &chunks) {
            Some(held) => held,
            None => mint_id(sessions_dir)?,
        }
    };
    let dir = sessions_dir.join(&local);

    // The revisions this import leaves the session at: each input's sealed
    // hash and plaintext hash, and the audio.
    let mut revisions = BTreeMap::from([
        ("manifest".to_string(), body_hash(&raw_manifest)),
        ("content.manifest".to_string(), body_hash(&manifest)),
        ("audio".to_string(), audio),
    ]);
    // An input the hello named as unchanged keeps the library's copy, at the
    // revision the phone holds it at now.
    let kept = |name: &str, revisions: &mut BTreeMap<String, String>| -> bool {
        if !claim.kept.iter().any(|k| k == name) {
            return false;
        }
        let (Some(sealed), Some(content)) = (claim.inputs.get(name), claim.content.get(name)) else { return false };
        revisions.insert(name.to_string(), sealed.clone());
        revisions.insert(format!("content.{name}"), content.clone());
        true
    };
    let arrived = |name: &str, file: &str, revisions: &mut BTreeMap<String, String>| -> Result<Option<Zeroizing<Vec<u8>>>, ImportError> {
        let Some(raw) = inbox.read(device_id, session, file) else { return Ok(None) };
        let plain = open(&raw, key)?;
        revisions.insert(name.to_string(), body_hash(&raw));
        revisions.insert(format!("content.{name}"), body_hash(&plain));
        Ok(Some(plain))
    };

    // The transcript the library holds, and what the last import recorded
    // about it, are read under the lock every writer of a library
    // transcript takes, and stay current until this import has written.
    let _writing = library_write_lock();
    let previous = inbox.imported(device_id, &local).unwrap_or_default();
    let held = syncsafe::read(dir.join("transcript.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<transcript::SessionTranscript>(&b).ok());
    for k in [REJECTED_TRANSCRIPT, REJECTED_TRANSCRIPT_CONTENT, REJECTED_SPEAKERS, REJECTED_SPEAKERS_CONTENT, WRITTEN_TRANSCRIPT] {
        if let Some(v) = previous.get(k) {
            revisions.insert(k.to_string(), v.clone());
        }
    }

    // The phone's transcript, validated as the manifest is: one this build
    // cannot read lands nowhere.
    let phone_transcript = match arrived("transcript", "transcript.json", &mut revisions)? {
        Some(plain) => {
            let parsed = serde_json::from_slice::<transcript::SessionTranscript>(&plain)
                .map_err(|_| ImportError::Unsupported)?;
            let bytes = match sanitized_transcript(parsed, crate::now_unix())? {
                Some(clean) => Zeroizing::new(clean),
                None => plain,
            };
            let parsed = serde_json::from_slice::<transcript::SessionTranscript>(&bytes)
                .map_err(|_| ImportError::Unsupported)?;
            Some((parsed, bytes))
        }
        None => {
            // While the phone still offers a transcript, the library's copy
            // stands at the revision on record.
            if !kept("transcript", &mut revisions) && claim.inputs.contains_key("transcript") {
                for k in ["transcript", "content.transcript"] {
                    if let Some(v) = previous.get(k) {
                        revisions.insert(k.to_string(), v.clone());
                    }
                }
            }
            None
        }
    };
    // Live captions are recorded by their plaintext, which is what a hello
    // names them by. Captions this desktop cannot open, or past
    // `LIVE_MAX_BYTES`, are recorded at the revision offered and not read;
    // the session imports without them.
    let live = match inbox.read(device_id, session, crate::sync::inbox::LIVE_TRANSCRIPT) {
        Some(raw) => match open(&raw, key) {
            Ok(plain) => {
                let hash = body_hash(&plain);
                revisions.insert("live_transcript".to_string(), hash.clone());
                revisions.insert("content.live_transcript".to_string(), hash);
                (plain.len() <= LIVE_MAX_BYTES).then_some(plain)
            }
            Err(e) => {
                log::warn!("sync: live captions of {session} not read: {e:?}");
                if let Some(offered) = claim.inputs.get("live_transcript") {
                    revisions.insert("live_transcript".to_string(), offered.clone());
                }
                None
            }
        },
        None => {
            kept("live_transcript", &mut revisions);
            None
        }
    };
    // Without the phone's transcript, its live captions stand in for one.
    let promoted = match (&phone_transcript, &live) {
        (None, Some(plain)) => match provisional_transcript(&parsed, &local, &live_lines(plain), crate::now_unix()) {
            Some(mut st) => {
                for seg in st.chunks.iter_mut().flat_map(|c| &mut c.tracks).flat_map(|t| &mut t.segments) {
                    if !seg.words_consistent() {
                        seg.words.clear();
                    }
                }
                let bytes = serde_json::to_vec_pretty(&st).map_err(|_| ImportError::Io)?;
                Some((st, Zeroizing::new(bytes)))
            }
            None => None,
        },
        _ => None,
    };
    let written = previous.get(WRITTEN_TRANSCRIPT).map(String::as_str);
    let transcript = match phone_transcript.or(promoted) {
        Some((st, bytes)) if replaces(held.as_ref(), written, &st) => {
            revisions.insert(WRITTEN_TRANSCRIPT.to_string(), stamp(&st));
            for k in [REJECTED_TRANSCRIPT, REJECTED_TRANSCRIPT_CONTENT, REJECTED_SPEAKERS, REJECTED_SPEAKERS_CONTENT] {
                revisions.remove(k);
            }
            Some(bytes)
        }
        Some((st, _)) => {
            log::info!("sync: {local} keeps its transcript; the arriving one ranks no later");
            if !st.provisional {
                for (from, to) in [("transcript", REJECTED_TRANSCRIPT), ("content.transcript", REJECTED_TRANSCRIPT_CONTENT)] {
                    if let Some(v) = revisions.get(from).cloned() {
                        revisions.insert(to.to_string(), v);
                    }
                }
            }
            None
        }
        None => None,
    };
    // The library keeps a transcript of its own when it declined the
    // phone's, or when the transcript it holds is not the one the last
    // import wrote. A phone speaker table then stays out: it is recorded as
    // declined, never as held.
    let own_transcript = transcript.is_none()
        && held.as_ref().is_some_and(|h| {
            (revisions.contains_key(REJECTED_TRANSCRIPT_CONTENT)
                && revisions.get(REJECTED_TRANSCRIPT_CONTENT) == revisions.get("content.transcript"))
                || written.is_some_and(|w| w != stamp(h))
        });
    let speakers = match arrived("speakers", "speakers.json", &mut revisions)? {
        Some(_) if own_transcript => {
            for (from, to) in [("speakers", REJECTED_SPEAKERS), ("content.speakers", REJECTED_SPEAKERS_CONTENT)] {
                if let Some(v) = revisions.remove(from) {
                    revisions.insert(to.to_string(), v);
                }
            }
            None
        }
        Some(plain) => {
            revisions.remove(REJECTED_SPEAKERS);
            revisions.remove(REJECTED_SPEAKERS_CONTENT);
            Some(plain)
        }
        None => {
            let declined = claim.inputs.get("speakers").is_some_and(|h| revisions.get(REJECTED_SPEAKERS) == Some(h))
                || claim.content.get("speakers").is_some_and(|c| revisions.get(REJECTED_SPEAKERS_CONTENT) == Some(c));
            if !declined {
                kept("speakers", &mut revisions);
            }
            None
        }
    };
    let notes = arrived("notes", recording::notes::FILE_NAME, &mut revisions)?;
    let notes_kept = notes.is_none() && kept("notes", &mut revisions);

    // Every key this desktop derives is written here whatever the phone sent.
    value["recording_kind"] = serde_json::json!("phone_room");
    value["origin_device"] = serde_json::json!(device_id);
    value["session_id"] = serde_json::json!(local);
    if let Some(map) = value.as_object_mut() {
        map.remove("origin_session");
    }
    if local != session {
        value["origin_session"] = serde_json::json!(session);
    }
    // One microphone in a room: every voice on it is a room voice, and the
    // diarizer clusters the mic track.
    value["single_local_speaker"] = serde_json::json!(false);
    // A manifest's run belongs to a transcript this desktop keeps; the
    // phone's speaker table names its own.
    let run = match transcript.is_none() && speakers.is_none() {
        true => crate::commands::session_store::read_manifest_opt(&dir).and_then(|m| m.diarization_id),
        false => None,
    };
    match (run, value.as_object_mut()) {
        (Some(run), _) => value["diarization_id"] = serde_json::json!(run),
        (None, Some(map)) => {
            map.remove("diarization_id");
        }
        (None, None) => {}
    }
    if let Some(list) = value["chunks"].as_array_mut() {
        for c in list {
            if let Some(p) = c["mic_wav_relative"].as_str() {
                if paths.iter().any(|q| q == p) {
                    c["mic_wav_relative"] = serde_json::json!(wav_path_of(p));
                }
            }
        }
    }
    let manifest_out = serde_json::to_vec_pretty(&value).map_err(|_| ImportError::Io)?;

    if inbox.is_entombed(session, device_id) {
        return Err(ImportError::Entombed);
    }
    // A session new to the library is written whole beside it and moved into place; an existing session gets its audio and other files first and its manifest last.
    let fresh = !dir.exists();
    let target = match fresh {
        true => sessions_dir.join(format!(".{local}.importing")),
        false => dir.clone(),
    };
    if fresh && target.exists() {
        std::fs::remove_dir_all(&target).map_err(|_| ImportError::Io)?;
    }
    let written = (|| -> Result<(), ImportError> {
        std::fs::create_dir_all(&target).map_err(|_| ImportError::Io)?;
        for (path, pcm) in &chunks {
            let wav = target.join(wav_path_of(path));
            if let Some(parent) = wav.parent() {
                std::fs::create_dir_all(parent).map_err(|_| ImportError::Io)?;
            }
            recording::write_wav_mono_i16(&wav, pcm, recording::audio_import::TARGET_RATE)
                .map_err(|_| ImportError::Io)?;
        }
        if let Some(plain) = &transcript {
            write(&target.join("transcript.json"), plain)?;
            let dedup = target.join("transcript.dedup.json");
            if dedup.is_file() {
                std::fs::remove_file(&dedup).map_err(|_| ImportError::Io)?;
            }
        }
        if let Some(plain) = &live {
            write(&target.join(crate::sync::inbox::LIVE_TRANSCRIPT), plain)?;
        }
        if let Some(plain) = &speakers {
            write(&target.join("speakers.json"), plain)?;
        }
        // The copy of notes from an earlier import is removed when the phone sends none.
        let notes_path = target.join(recording::notes::FILE_NAME);
        match &notes {
            Some(plain) => write(&notes_path, plain)?,
            None if notes_kept => {}
            None => {
                if notes_path.is_file() {
                    std::fs::remove_file(&notes_path).map_err(|_| ImportError::Io)?;
                }
            }
        }
        write(&target.join("manifest.json"), &manifest_out)?;

        // Clusters with no speaker table to name their run are numbered by a run of this desktop's own.
        if recording::session_meta::current_run(&target).is_none()
            && crate::commands::session_store::is_diarized(&target)
        {
            recording::session_meta::mint_run(&target).map_err(|_| ImportError::Io)?;
        }
        // What a person wrote on the session lives in its `meta.json`, which
        // the phone's manifest was just written over: the manifest's copy is
        // projected from it again, after following every id the catalog and
        // the contact list retired.
        let (tags, contacts) =
            sessions_dir.parent().map(crate::sync::records::catalog_retirements).unwrap_or_default();
        recording::session_meta::update(&target, |m| crate::sync::records::settle_references(m, &tags, &contacts))
            .and_then(|_| recording::session_meta::project_file(&target))
            .map_err(|_| ImportError::Io)?;
        if fresh {
            std::fs::rename(&target, &dir).map_err(|_| ImportError::Io)?;
        }
        Ok(())
    })();
    if let Err(e) = written {
        if fresh {
            let _ = std::fs::remove_dir_all(&target);
        }
        return Err(e);
    }

    inbox.record_import(device_id, &local, &revisions).map_err(|_| ImportError::Io)?;
    inbox.forget_refusal(device_id, session).map_err(|_| ImportError::Io)?;
    inbox.clear_staged(device_id, session).map_err(|_| ImportError::Io)?;
    Ok(local)
}

/// Takes out a session an import landed under `local` after `device_id`
/// deleted it: the library copy goes while its manifest names that device,
/// and the record of its import goes with it. True when a session was
/// removed.
pub fn bury_landed(inbox: &Inbox, sessions_dir: &Path, device_id: &str, local: &str) -> bool {
    let _writing = library_write_lock();
    let dir = sessions_dir.join(local);
    let removed = match occupant(&dir) {
        Occupant::Device(owner) if owner == device_id => {
            match crate::commands::session::remove_contained_session_dir(sessions_dir, &dir) {
                Ok(()) => true,
                Err(e) => {
                    log::warn!("sync: session {local} not removed: {e}");
                    return false;
                }
            }
        }
        _ => false,
    };
    if let Err(e) = inbox.forget_import(device_id, local) {
        log::warn!("sync: import record {local} not cleared: {e:?}");
    }
    removed
}

/// Gives up on a staged session at the revisions it arrived at: the
/// refusal is recorded against those revisions and the staged copies go.
/// A hello naming the same revisions asks for nothing; one naming new
/// revisions, which is what a re-pair or an update on the phone produces,
/// brings the session over again.
pub fn refuse(inbox: &Inbox, session: &str, device_id: &str, reason: RefusalReason) -> Result<(), ImportError> {
    let inputs = INPUTS
        .iter()
        .filter(|(k, _)| *k != "live_transcript")
        .filter_map(|(k, file)| inbox.read(device_id, session, file).map(|bytes| (k.to_string(), body_hash(&bytes))))
        .collect();
    inbox
        .record_refusal(session, &Refusal { device_id: device_id.to_string(), reason, inputs })
        .map_err(|_| ImportError::Io)
}

/// What the library holds under a session id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Occupant {
    /// Nothing is there.
    Vacant,
    /// A session from this device; empty for one this desktop recorded.
    Device(String),
    /// A directory whose manifest this desktop cannot read; it counts as occupied.
    Unknown,
}

pub(crate) fn occupant(dir: &Path) -> Occupant {
    if !dir.is_dir() {
        return Occupant::Vacant;
    }
    let Ok(bytes) = syncsafe::read(dir.join("manifest.json")) else { return Occupant::Unknown };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return Occupant::Unknown };
    Occupant::Device(value["origin_device"].as_str().unwrap_or_default().to_string())
}

/// Hands a library session to the device now listing it: the session another
/// device's import left under the same wire id with the same `audio` on
/// record answers to `device_id` from here on, at the revisions it was
/// imported at. True when a session changed hands.
pub fn adopt(inbox: &Inbox, sessions_dir: &Path, minted: &MintedIds, device_id: &str, session: &str, audio: &str) -> bool {
    if !is_wire_session_id(session) || inbox.imported(device_id, &local_id(minted, device_id, session)).is_some() {
        return false;
    }
    let candidates = std::iter::once(session.to_string()).chain(minted.values().filter_map(|m| m.get(session).cloned()));
    for id in candidates {
        let dir = sessions_dir.join(&id);
        let Occupant::Device(owner) = occupant(&dir) else { continue };
        if owner.is_empty() || owner == device_id {
            continue;
        }
        let Some(record) = inbox.imported(&owner, &id) else { continue };
        if record.get("audio").map(String::as_str) != Some(audio) {
            continue;
        }
        let Ok(bytes) = syncsafe::read(dir.join("manifest.json")) else { continue };
        let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        value["origin_device"] = serde_json::json!(device_id);
        let Ok(out) = serde_json::to_vec_pretty(&value) else { continue };
        if write(&dir.join("manifest.json"), &out).is_err() || inbox.record_import(device_id, &id, &record).is_err() {
            continue;
        }
        log::info!("sync: {id} answers to {device_id} now; {owner} listed it before");
        return true;
    }
    false
}

/// The library session a device's recording already is, under any device id:
/// one known by the same wire id (held under it, or minted for it) whose
/// every chunk holds the same samples. A session recorded here, or one whose
/// audio is gone, matches nothing.
fn same_recording(sessions_dir: &Path, minted: &MintedIds, session: &str, chunks: &[(String, Vec<i16>)]) -> Option<String> {
    std::iter::once(session.to_string()).chain(minted.values().filter_map(|m| m.get(session).cloned())).find(|id| {
        let dir = sessions_dir.join(id);
        matches!(occupant(&dir), Occupant::Device(owner) if !owner.is_empty())
            && chunks.iter().all(|(path, pcm)| {
                transcript::wav_io::read_wav_mono_i16(&dir.join(wav_path_of(path)))
                    .is_ok_and(|(held, rate)| rate == recording::audio_import::TARGET_RATE && held == *pcm)
            })
    })
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), ImportError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| ImportError::Io)?;
    }
    syncsafe::write_atomic(path, bytes).map_err(|_| ImportError::Io)
}

#[cfg(test)]
/// Seals bytes the way a locked phone does: a data key wrapped to the
/// desktop's X25519 key.
pub(crate) fn seal_to(secret: &[u8; 32], device_id: &str, plaintext: &[u8]) -> Vec<u8> {
    use aes_gcm::aead::{Aead, KeyInit};
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    let recipient_pub = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*secret)).to_bytes();
    let data_key = [7u8; 32];
    let body = aes_gcm::Aes256Gcm::new_from_slice(&data_key)
        .unwrap()
        .encrypt(aes_gcm::Nonce::from_slice(&[1u8; 12]), plaintext)
        .unwrap();
    let ephemeral = x25519_dalek::StaticSecret::from([9u8; 32]);
    let ephemeral_pub = x25519_dalek::PublicKey::from(&ephemeral);
    let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(recipient_pub));
    let mut wrap_key = [0u8; 32];
    hkdf::Hkdf::<sha2::Sha256>::new(Some(b"daisy-envelope-v2"), shared.as_bytes())
        .expand(&[b"x25519-wrap".as_slice(), ephemeral_pub.as_bytes(), &recipient_pub].concat(), &mut wrap_key)
        .unwrap();
    let wrapped = aes_gcm::Aes256Gcm::new_from_slice(&wrap_key)
        .unwrap()
        .encrypt(aes_gcm::Nonce::from_slice(&[2u8; 12]), data_key.as_slice())
        .unwrap();
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 2,
        "cipher": "aes-256-gcm",
        "nonce_b64": B64.encode([1u8; 12]),
        "ciphertext_b64": B64.encode(&body),
        "recipients": [{
            "kind": "x25519",
            "id": device_id,
            "ephemeral_pub_b64": B64.encode(ephemeral_pub.as_bytes()),
            "nonce_b64": B64.encode([2u8; 12]),
            "wrapped_b64": B64.encode(&wrapped),
        }],
    }))
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::inbox::Inbox;
    use crate::sync::signing::body_hash;

    /// This desktop's envelope key; the sealed cases seal to `[5u8; 32]` / `mac-1`.
    fn key() -> MachineKey {
        MachineKey { x25519_secret: [5u8; 32], device_id: "mac-1".into() }
    }

    fn fresh() -> (Inbox, tempfile::TempDir) {
        let td = tempfile::tempdir().unwrap();
        (Inbox::at(td.path().join("inbox")), td)
    }

    #[test]
    fn a_manifest_that_is_not_staged_is_missing() {
        let (inbox, _td) = fresh();
        assert_eq!(missing(&inbox, PHONE, SESSION, &key()), Err(ImportError::Missing));
    }

    #[test]
    fn a_clear_manifest_names_the_chunks_that_have_not_arrived() {
        let (inbox, _td) = fresh();
        let manifest = manifest_json(SESSION, "chunks/0001/mic.m4a");
        inbox.store(PHONE, SESSION, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        assert_eq!(missing(&inbox, PHONE, SESSION, &key()), Ok(vec!["chunks/0001/mic.m4a".to_string()]));
        inbox.store(PHONE, SESSION, "chunks/0001/mic.m4a", &body_hash(b"aac"), b"aac").unwrap();
        assert_eq!(missing(&inbox, PHONE, SESSION, &key()), Ok(vec![]));
    }

    #[test]
    fn a_sealed_manifest_names_its_chunks_once_this_desktop_opens_it() {
        let (inbox, _td) = fresh();
        let sealed = seal_to(&[5u8; 32], "mac-1", &manifest_json(SESSION, "chunks/0001/mic.m4a"));
        inbox.store(PHONE, SESSION, "manifest.json", &body_hash(&sealed), &sealed).unwrap();
        assert_eq!(missing(&inbox, PHONE, SESSION, &key()), Ok(vec!["chunks/0001/mic.m4a".to_string()]));
    }

    #[test]
    fn a_manifest_sealed_to_another_desktop_is_sealed_here() {
        let (inbox, _td) = fresh();
        let sealed = seal_to(&[6u8; 32], "mac-2", &manifest_json(SESSION, "chunks/0001/mic.m4a"));
        inbox.store(PHONE, SESSION, "manifest.json", &body_hash(&sealed), &sealed).unwrap();
        assert_eq!(missing(&inbox, PHONE, SESSION, &key()), Err(ImportError::Sealed));
    }

    #[test]
    fn a_manifest_that_names_no_audio_is_unsupported() {
        let (inbox, _td) = fresh();
        let manifest = br#"{"chunks": []}"#;
        inbox.store(PHONE, SESSION, "manifest.json", &body_hash(manifest), manifest).unwrap();
        assert_eq!(missing(&inbox, PHONE, SESSION, &key()), Err(ImportError::Unsupported));
        assert!(is_ready(&inbox, PHONE, SESSION, &key()), "the import refuses it rather than waiting");
    }

    const PHONE: &str = "4a68f72f5a22110b2bfbe71b6588833c";
    const SESSION: &str = "daisy-1700000000";

    fn manifest_json(session: &str, chunk: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 4,
            "session_id": session,
            "created_at_unix_seconds": 1_700_000_000i64,
            "sample_rate": 16000,
            "channels": 1,
            "language": "fr-CA",
            "chunks": [{
                "index": 1,
                "started_at_unix_seconds": 1_700_000_000i64,
                "ended_at_unix_seconds": 1_700_000_010i64,
                "duration_seconds": 9.4,
                "mic_samples": 150_393,
                "mic_wav_relative": chunk,
            }],
        }))
        .unwrap()
    }

    #[test]
    fn a_phone_transcript_loses_words_that_contradict_their_segment() {
        let json = |words: serde_json::Value| {
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1, "session_id": "s", "provider": "phone", "model": "m",
                "transcribed_at_unix_seconds": 0,
                "chunks": [{ "chunk_index": 1, "tracks": [{
                    "track": "mic", "source_wav_relative": "a.wav",
                    "segments": [{ "start_ms": 0, "end_ms": 1000, "text": "hello there",
                                   "confidence": null, "words": words }]
                }]}]
            }))
            .unwrap()
        };
        let bad = json(serde_json::json!([{ "text": "bye", "start_ms": 0, "end_ms": 5000 }]));
        let parsed: transcript::SessionTranscript = serde_json::from_slice(&bad).unwrap();
        let out = sanitized_transcript(parsed, NOW).unwrap().expect("rewritten");
        let back: transcript::SessionTranscript = serde_json::from_slice(&out).unwrap();
        assert!(back.chunks[0].tracks[0].segments[0].words.is_empty());

        let good = json(serde_json::json!([
            { "text": "hello", "start_ms": 0, "end_ms": 400 },
            { "text": "there", "start_ms": 400, "end_ms": 900 },
        ]));
        let parsed: transcript::SessionTranscript = serde_json::from_slice(&good).unwrap();
        assert!(sanitized_transcript(parsed, NOW).unwrap().is_none(), "consistent words keep their bytes");
    }

    #[test]
    fn a_phone_transcript_keeps_only_split_groups_a_rerun_can_rejoin() {
        let piece = |start: u32, text: &str, group: u32| {
            serde_json::json!({ "start_ms": start, "end_ms": start + 400, "text": text, "confidence": null,
                "words": [{ "text": text, "start_ms": start, "end_ms": start + 400 }], "split_group": group })
        };
        let json = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "session_id": "s", "provider": "phone", "model": "m",
            "transcribed_at_unix_seconds": 0,
            "chunks": [{ "chunk_index": 1, "tracks": [{
                "track": "mic", "source_wav_relative": "a.wav",
                "segments": [
                    piece(0, "hello", u32::MAX), piece(500, "there", u32::MAX),
                    piece(1_000, "one", 8), piece(1_500, "between", 3), piece(2_000, "two", 8),
                ]
            }]}]
        }))
        .unwrap();
        let parsed: transcript::SessionTranscript = serde_json::from_slice(&json).unwrap();
        let out = sanitized_transcript(parsed, NOW).unwrap().expect("rewritten");
        let back: transcript::SessionTranscript = serde_json::from_slice(&out).unwrap();
        let groups: Vec<_> = back.chunks[0].tracks[0].segments.iter().map(|s| s.split_group).collect();
        assert_eq!(groups, vec![Some(0), Some(0), None, Some(1), None]);
    }

    /// This desktop's clock in the sanitizing cases: after every stamp they
    /// carry.
    const NOW: i64 = 1_800_000_000;

    #[test]
    fn a_phone_transcript_stamped_in_the_future_is_held_to_this_clock() {
        let json = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "session_id": "s", "provider": "phone", "model": "m",
            "transcribed_at_unix_seconds": NOW + 3_600,
            "edited_at_unix_seconds": NOW + 86_400,
            "chunks": [],
        }))
        .unwrap();
        let parsed: transcript::SessionTranscript = serde_json::from_slice(&json).unwrap();
        let out = sanitized_transcript(parsed, NOW).unwrap().expect("rewritten");
        let back: transcript::SessionTranscript = serde_json::from_slice(&out).unwrap();
        assert_eq!(back.transcribed_at_unix_seconds, NOW + STAMP_SKEW_SECONDS);
        assert_eq!(back.edited_at_unix_seconds, Some(NOW + STAMP_SKEW_SECONDS));

        let within: transcript::SessionTranscript = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "session_id": "s", "provider": "phone", "model": "m",
            "transcribed_at_unix_seconds": NOW + STAMP_SKEW_SECONDS,
            "edited_at_unix_seconds": NOW,
            "chunks": [],
        }))
        .unwrap();
        assert!(sanitized_transcript(within, NOW).unwrap().is_none(), "stamps within the skew stand");
    }

    fn transcript_json(session: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "session_id": session,
            "provider": "phone",
            "model": "on-device",
            "transcribed_at_unix_seconds": 1_700_000_012i64,
            "chunks": [],
        }))
        .unwrap()
    }

    /// Two seconds of 16 kHz mono AAC with a burst at 0.5 s: what a phone
    /// chunk is.
    fn aac() -> Vec<u8> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../recording/tests/fixtures/burst-16k.m4a")).unwrap()
    }

    /// A staged session with everything the desktop asked for.
    fn staged() -> (Inbox, tempfile::TempDir, Vec<u8>, Vec<u8>) {
        let td = tempfile::tempdir().unwrap();
        let inbox = Inbox::at(td.path().join("inbox"));
        let (manifest, transcript) = stage_into(&inbox, PHONE);
        (inbox, td, manifest, transcript)
    }

    /// Stages the session under one device, which is what that device
    /// offering it does.
    fn stage_into(inbox: &Inbox, device: &str) -> (Vec<u8>, Vec<u8>) {
        stage_audio_into(inbox, device, "chunks/0001/mic.m4a", &aac())
    }

    /// Stages the session under one device with the given chunk audio.
    fn stage_audio_into(inbox: &Inbox, device: &str, chunk: &str, audio: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let manifest = manifest_json(SESSION, chunk);
        let transcript = transcript_json(SESSION);
        inbox.store(device, SESSION, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        inbox.store(device, SESSION, "transcript.json", &body_hash(&transcript), &transcript).unwrap();
        inbox.store(device, SESSION, chunk, &body_hash(audio), audio).unwrap();
        (manifest, transcript)
    }

    /// A second device's recording that minted the same second: a 16 kHz
    /// mono tone, audio the phone's chunk does not hold.
    fn other_recording(inbox: &Inbox, device: &str) {
        let mut wav = std::io::Cursor::new(Vec::new());
        let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::new(&mut wav, spec).unwrap();
        for n in 0..16_000 {
            w.write_sample(((n as f32 * 0.1).sin() * 8_000.0) as i16).unwrap();
        }
        w.finalize().unwrap();
        stage_audio_into(inbox, device, "chunks/0001/mic.wav", &wav.into_inner());
    }

    fn library() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn a_session_is_ready_only_once_every_file_it_names_is_here() {
        let td = tempfile::tempdir().unwrap();
        let inbox = Inbox::at(td.path().join("inbox"));
        let manifest = manifest_json(SESSION, "chunks/0001/mic.m4a");
        assert!(!is_ready(&inbox, PHONE, SESSION, &key()), "nothing has arrived");

        inbox.store(PHONE, SESSION, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        assert!(!is_ready(&inbox, PHONE, SESSION, &key()), "the chunk is still missing");

        let transcript = transcript_json(SESSION);
        inbox.store(PHONE, SESSION, "transcript.json", &body_hash(&transcript), &transcript).unwrap();
        assert!(!is_ready(&inbox, PHONE, SESSION, &key()), "the chunk is still missing");

        let audio = b"aac bytes";
        inbox.store(PHONE, SESSION, "chunks/0001/mic.m4a", &body_hash(audio), audio).unwrap();
        assert!(is_ready(&inbox, PHONE, SESSION, &key()));
    }

    #[test]
    fn an_imported_session_lands_where_every_session_lives() {
        let (inbox, _td, manifest, transcript) = staged();
        let lib = library();
        let root = lib.path().to_path_buf();
        import_session(&inbox, &root, PHONE, SESSION, &key()).unwrap();

        let dir = root.join(SESSION);
        assert_eq!(std::fs::read(dir.join("transcript.json")).unwrap(), transcript);
        // The phone's AAC lands as the WAV every reader here expects, on the
        // phone transcript's timeline (the encoder's priming dropped).
        let (pcm, rate) = transcript::wav_io::read_wav_mono_i16(&dir.join("chunks/0001/mic.wav")).unwrap();
        assert_eq!(rate, 16_000);
        assert!((31_000..=33_100).contains(&pcm.len()), "two seconds of audio, got {} samples", pcm.len());
        let first = pcm.iter().position(|&v| i32::from(v).abs() > 3000).expect("the burst is in the file");
        assert!((7_600..=8_400).contains(&first), "burst starts at sample {first}");
        assert!(!dir.join("chunks/0001/mic.m4a").exists(), "one copy of the audio");
        let written: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(written["chunks"][0]["mic_wav_relative"], "chunks/0001/mic.wav");
        assert_eq!(written["single_local_speaker"], false, "a phone's mic hears the whole room");
        assert_eq!(written["recording_kind"], "phone_room", "no AEC arm, no system track");
        assert_eq!(written["origin_device"], PHONE, "the artifacts know where to go back to");
        assert_eq!(written["language"], "fr-CA", "the summarizer reads what the phone listened in");
        assert_eq!(written["session_id"], SESSION);
        assert_ne!(std::fs::read(dir.join("manifest.json")).unwrap(), manifest, "the manifest gained its origin");
    }

    #[test]
    fn an_origin_session_the_phone_sent_does_not_survive_the_import() {
        let td = tempfile::tempdir().unwrap();
        let inbox = Inbox::at(td.path().join("inbox"));
        // A manifest carrying a second session's id in the key this desktop
        // authors. `origin_session` has no field on the manifest, so it
        // arrives among the keys an import carries through untouched.
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&manifest_json(SESSION, "chunks/0001/mic.m4a")).unwrap();
        manifest["origin_session"] = serde_json::json!("daisy-1787000000");
        manifest["origin_device"] = serde_json::json!("0000000000000000000000000000ffff");
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let transcript = transcript_json(SESSION);
        let audio = aac();
        inbox.store(PHONE, SESSION, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        inbox.store(PHONE, SESSION, "transcript.json", &body_hash(&transcript), &transcript).unwrap();
        inbox.store(PHONE, SESSION, "chunks/0001/mic.m4a", &body_hash(&audio), &audio).unwrap();

        let lib = library();
        let local = import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();
        assert_eq!(local, SESSION);
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(lib.path().join(&local).join("manifest.json")).unwrap()).unwrap();
        assert!(
            written.get("origin_session").is_none(),
            "a delete here buries the id this desktop wrote, never one the phone chose"
        );
        assert_eq!(written["origin_device"], PHONE, "the device that sent it, not the one it named");
    }

    #[test]
    fn a_session_named_from_the_minted_band_is_never_staged_or_imported() {
        let td = tempfile::tempdir().unwrap();
        let inbox = Inbox::at(td.path().join("inbox"));
        let squatted = format!("daisy-{MINTED_ID_FLOOR}");
        let manifest = manifest_json(&squatted, "chunks/0001/mic.m4a");
        assert_eq!(
            inbox.store(PHONE, &squatted, "manifest.json", &body_hash(&manifest), &manifest),
            Err(crate::sync::inbox::StoreError::BadName),
            "an id from the band this desktop mints in is not a device's to name"
        );
        assert_eq!(
            import_session(&inbox, library().path(), PHONE, &squatted, &key()),
            Err(ImportError::Unsupported)
        );
    }

    #[test]
    fn importing_records_the_revisions_and_clears_the_staging_copies() {
        let (inbox, _td, manifest, transcript) = staged();
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();

        let held = inbox.imported(PHONE, SESSION).expect("the revisions are recorded");
        assert_eq!(held.get("manifest"), Some(&body_hash(&manifest)));
        assert_eq!(held.get("transcript"), Some(&body_hash(&transcript)));
        assert!(inbox.read(PHONE, SESSION, "manifest.json").is_none(), "one copy of a session, not two");
    }

    /// The other device in the collision cases; it minted the same
    /// `daisy-<unix>` second this phone did.
    const TABLET: &str = "0000000000000000000000000000ffff";

    #[test]
    fn two_devices_that_minted_one_id_both_land_in_the_library() {
        let (inbox, _td, _, _) = staged();
        let lib = library();
        assert_eq!(import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap(), SESSION);

        other_recording(&inbox, TABLET);
        let minted = import_session(&inbox, lib.path(), TABLET, SESSION, &key()).unwrap();
        assert_eq!(minted, format!("daisy-{MINTED_ID_FLOOR}"), "the session already here keeps the id");
        assert!(is_session_id(&minted), "a minted id is a session id everywhere in the app");

        let first: serde_json::Value =
            serde_json::from_slice(&std::fs::read(lib.path().join(SESSION).join("manifest.json")).unwrap()).unwrap();
        assert_eq!(first["origin_device"], PHONE);
        assert!(first.get("origin_session").is_none(), "it is here under the id its phone knows");
        let second: serde_json::Value =
            serde_json::from_slice(&std::fs::read(lib.path().join(&minted).join("manifest.json")).unwrap()).unwrap();
        assert_eq!(second["origin_device"], TABLET);
        assert_eq!(second["origin_session"], SESSION, "what travels back travels under the id it knows");
        assert_eq!(second["session_id"], minted);
    }

    #[test]
    fn a_minted_session_answers_to_its_devices_id_again() {
        let (inbox, _td, _, phone_transcript) = staged();
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();
        other_recording(&inbox, TABLET);
        let minted = import_session(&inbox, lib.path(), TABLET, SESSION, &key()).unwrap();

        assert_eq!(local_id(&minted_ids(lib.path()), TABLET, SESSION), minted);
        assert_eq!(local_id(&minted_ids(lib.path()), PHONE, SESSION), SESSION);
        assert_eq!(local_id(&minted_ids(lib.path()), "0000000000000000000000000000dddd", SESSION), SESSION, "a device with nothing here");

        // A newer revision from the tablet lands in the directory it already has.
        other_recording(&inbox, TABLET);
        let newer = br#"{"schema_version":1,"session_id":"daisy-1700000000","provider":"phone","model":"on-device","transcribed_at_unix_seconds":1700000141,"chunks":[]}"#;
        inbox.store(TABLET, SESSION, "transcript.json", &body_hash(newer), newer).unwrap();
        assert_eq!(import_session(&inbox, lib.path(), TABLET, SESSION, &key()).unwrap(), minted);
        assert_eq!(std::fs::read_dir(lib.path()).unwrap().count(), 2, "two sessions, not three");
        assert_eq!(inbox.imported(TABLET, &minted).unwrap().get("transcript"), Some(&body_hash(newer)));
        assert_eq!(
            inbox.imported(PHONE, SESSION).unwrap().get("transcript"),
            Some(&body_hash(&phone_transcript)),
            "the phone's own session is held at its own revisions"
        );
    }

    /// The same phone after its sync identity was reset: a new device id,
    /// the same recordings.
    const RESET_PHONE: &str = "0000000000000000000000000000eeee";

    #[test]
    fn a_phone_back_under_a_new_device_id_lands_its_recording_where_it_already_is() {
        let (inbox, _td, _, _) = staged();
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();

        stage_into(&inbox, RESET_PHONE);
        assert_eq!(import_session(&inbox, lib.path(), RESET_PHONE, SESSION, &key()).unwrap(), SESSION);
        assert_eq!(std::fs::read_dir(lib.path()).unwrap().count(), 1, "one session, not two");
        let held: serde_json::Value =
            serde_json::from_slice(&std::fs::read(lib.path().join(SESSION).join("manifest.json")).unwrap()).unwrap();
        assert_eq!(held["origin_device"], RESET_PHONE, "the phone that holds it now is the one it answers to");
        assert!(inbox.imported(RESET_PHONE, SESSION).is_some(), "its next hello finds it here at these revisions");
        assert_eq!(local_id(&minted_ids(lib.path()), RESET_PHONE, SESSION), SESSION);
    }

    #[test]
    fn a_minted_session_is_found_again_by_its_audio_under_a_new_device_id() {
        let (inbox, _td, _, _) = staged();
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();
        other_recording(&inbox, TABLET);
        let minted = import_session(&inbox, lib.path(), TABLET, SESSION, &key()).unwrap();

        other_recording(&inbox, RESET_PHONE);
        assert_eq!(import_session(&inbox, lib.path(), RESET_PHONE, SESSION, &key()).unwrap(), minted);
        assert_eq!(std::fs::read_dir(lib.path()).unwrap().count(), 2, "two sessions, not three");
        assert_eq!(local_id(&minted_ids(lib.path()), RESET_PHONE, SESSION), minted);
    }

    #[test]
    fn a_session_whose_audio_is_gone_is_not_matched() {
        let (inbox, _td, _, _) = staged();
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();
        std::fs::remove_dir_all(lib.path().join(SESSION).join("chunks")).unwrap();

        stage_into(&inbox, RESET_PHONE);
        let minted = import_session(&inbox, lib.path(), RESET_PHONE, SESSION, &key()).unwrap();
        assert_eq!(minted, format!("daisy-{MINTED_ID_FLOOR}"), "nothing proves it is the same recording");
    }

    #[test]
    fn a_session_this_desktop_recorded_keeps_its_id() {
        let (inbox, _td, _, _) = staged();
        let lib = library();
        // No origin_device: a meeting recorded here that minted the same second.
        let dir = lib.path().join(SESSION);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), br#"{"schema_version":4,"session_id":"daisy-1700000000"}"#).unwrap();

        let minted = import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();
        assert_eq!(minted, format!("daisy-{MINTED_ID_FLOOR}"));
        let mine: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert!(mine.get("origin_device").is_none(), "this desktop's own recording is untouched");
    }

    #[test]
    fn a_session_whose_manifest_cannot_be_read_holds_its_directory() {
        let (inbox, _td, _, _) = staged();
        let lib = library();
        let dir = lib.path().join(SESSION);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("summary.json"), b"{}").unwrap();

        let minted = import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();
        assert_eq!(minted, format!("daisy-{MINTED_ID_FLOOR}"), "a directory that cannot be read is occupied");
        assert!(!dir.join("manifest.json").exists(), "and nothing was written over it");
        assert_eq!(std::fs::read(dir.join("summary.json")).unwrap(), b"{}");
    }

    #[test]
    fn audio_that_does_not_decode_is_undecodable_before_anything_is_written() {
        let (inbox, _td, _, _) = staged();
        inbox.store(PHONE, SESSION, "chunks/0001/mic.m4a", &body_hash(b"not audio"), b"not audio").unwrap();
        let lib = library();
        assert_eq!(
            import_session(&inbox, lib.path(), PHONE, SESSION, &key()),
            Err(ImportError::Undecodable)
        );
        assert!(!lib.path().join(SESSION).exists(), "nothing half-written");
    }

    #[test]
    fn a_staged_session_is_named_by_its_meta_title_then_its_manifest_title() {
        let (inbox, _td, manifest, _) = staged();
        assert_eq!(staged_title(&inbox, PHONE, SESSION, &key()), None, "an untitled session has no name");

        let mut titled: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        titled["title"] = "Standup".into();
        let titled = serde_json::to_vec(&titled).unwrap();
        inbox.store(PHONE, SESSION, "manifest.json", &body_hash(&titled), &titled).unwrap();
        assert_eq!(staged_title(&inbox, PHONE, SESSION, &key()).as_deref(), Some("Standup"));

        let cleared = br#"{"title":{"value":" ","updated_at_unix_seconds":5}}"#;
        inbox.stage_record(PHONE, SESSION, "meta.json", &body_hash(cleared), cleared).unwrap();
        assert_eq!(staged_title(&inbox, PHONE, SESSION, &key()).as_deref(), Some("Standup"), "a cleared title names nothing");

        let renamed = br#"{"title":{"value":"Standup, renamed","updated_at_unix_seconds":6}}"#;
        inbox.stage_record(PHONE, SESSION, "meta.json", &body_hash(renamed), renamed).unwrap();
        assert_eq!(staged_title(&inbox, PHONE, SESSION, &key()).as_deref(), Some("Standup, renamed"));
    }

    #[test]
    fn a_refused_title_loses_its_reordering_marks_and_its_length() {
        let (inbox, _td, _, _) = staged();
        let meta = |title: &str| {
            serde_json::to_vec(&serde_json::json!({ "title": { "value": title, "updated_at_unix_seconds": 5 } })).unwrap()
        };
        let spoofed = meta("Stand\u{202E}pu\nlater");
        inbox.stage_record(PHONE, SESSION, "meta.json", &body_hash(&spoofed), &spoofed).unwrap();
        assert_eq!(staged_title(&inbox, PHONE, SESSION, &key()).as_deref(), Some("Standpulater"));

        let long = meta(&"a".repeat(500));
        inbox.stage_record(PHONE, SESSION, "meta.json", &body_hash(&long), &long).unwrap();
        let title = staged_title(&inbox, PHONE, SESSION, &key()).unwrap();
        assert_eq!(title.chars().count(), REFUSED_TITLE_MAX);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn audio_in_a_container_the_contract_does_not_name_is_unsupported() {
        let td = tempfile::tempdir().unwrap();
        let inbox = Inbox::at(td.path().join("inbox"));
        stage_audio_into(&inbox, PHONE, "chunks/0001/mic.opus", b"not audio");
        let lib = library();
        assert_eq!(import_session(&inbox, lib.path(), PHONE, SESSION, &key()), Err(ImportError::Unsupported));
    }

    #[test]
    fn a_device_minted_id_reads_as_the_second_it_was_minted() {
        assert_eq!(minted_at("daisy-1790963366"), Some(1_790_963_366));
        assert_eq!(minted_at(&format!("daisy-{MINTED_ID_FLOOR}")), None, "this desktop's own ids are no clock reading");
        assert_eq!(minted_at("notes"), None);
    }

    #[test]
    fn an_m4a_whose_recording_never_closed_is_undecodable() {
        // What a phone Stop leaves when no audio was captured: `ftyp`, `wide`
        // and an empty `mdat`, with no `moov`.
        let mut m4a = Vec::new();
        m4a.extend_from_slice(&[0, 0, 0, 0x14]);
        m4a.extend_from_slice(b"ftypM4A \0\0\0\0isom");
        m4a.extend_from_slice(&[0, 0, 0, 8]);
        m4a.extend_from_slice(b"wide");
        m4a.extend_from_slice(&[0, 0, 0, 8]);
        m4a.extend_from_slice(b"mdat");
        let (inbox, _td, _, _) = staged();
        inbox.store(PHONE, SESSION, "chunks/0001/mic.m4a", &body_hash(&m4a), &m4a).unwrap();
        let lib = library();
        let err = import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap_err();
        assert_eq!(err, ImportError::Undecodable);
        assert_eq!(err.refusal(), Some(RefusalReason::Undecodable));
    }

    #[test]
    fn a_malformed_transcript_is_unsupported_before_anything_is_written() {
        let (inbox, _td, _, _) = staged();
        inbox.store(PHONE, SESSION, "transcript.json", &body_hash(b"not json"), b"not json").unwrap();
        let lib = library();
        assert_eq!(
            import_session(&inbox, lib.path(), PHONE, SESSION, &key()),
            Err(ImportError::Unsupported)
        );
        assert!(!lib.path().join(SESSION).exists(), "nothing half-written");
    }

    #[test]
    fn re_importing_the_same_phones_session_replaces_it() {
        let (inbox, _td, _, _) = staged();
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();

        let (inbox2, _td2, _, _) = staged();
        let newer = transcript_json(SESSION);
        inbox2.store(PHONE, SESSION, "transcript.json", &body_hash(&newer), &newer).unwrap();
        assert!(import_session(&inbox2, lib.path(), PHONE, SESSION, &key()).is_ok(), "a rename on the phone comes back");
    }

    /// A transcript at these stamps.
    fn stamped(transcribed: i64, edited: Option<i64>) -> Vec<u8> {
        let mut t: serde_json::Value = serde_json::from_slice(&transcript_json(SESSION)).unwrap();
        t["transcribed_at_unix_seconds"] = serde_json::json!(transcribed);
        if let Some(e) = edited {
            t["edited_at_unix_seconds"] = serde_json::json!(e);
        }
        serde_json::to_vec(&t).unwrap()
    }

    #[test]
    fn an_edit_that_lands_while_an_import_waits_on_the_library_is_kept() {
        let (inbox, _td, _, _) = staged();
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();

        // The phone's next revision, which ranks below the edit made here.
        stage_into(&inbox, PHONE);
        let newer = stamped(1_700_000_042, None);
        inbox.store(PHONE, SESSION, "transcript.json", &body_hash(&newer), &newer).unwrap();
        let writing = library_write_lock();
        let importing = {
            let (inbox, root) = (inbox.clone(), lib.path().to_path_buf());
            std::thread::spawn(move || import_session(&inbox, &root, PHONE, SESSION, &key()))
        };
        std::thread::sleep(std::time::Duration::from_millis(1_500));
        let edited = stamped(1_700_000_012, Some(1_787_300_000));
        std::fs::write(lib.path().join(SESSION).join("transcript.json"), &edited).unwrap();
        drop(writing);
        importing.join().unwrap().unwrap();

        let held: transcript::SessionTranscript =
            serde_json::from_slice(&std::fs::read(lib.path().join(SESSION).join("transcript.json")).unwrap()).unwrap();
        assert_eq!(held.edited_at_unix_seconds, Some(1_787_300_000), "the edit made here stands");
    }

    #[test]
    fn a_new_session_whose_audio_fails_to_land_leaves_nothing_in_the_library() {
        let (inbox, _td) = fresh();
        // The second chunk's WAV would sit inside the first chunk's WAV,
        // which is a file: the write fails after the first chunk landed.
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&manifest_json(SESSION, "chunks/0001/mic.m4a")).unwrap();
        let mut second = manifest["chunks"][0].clone();
        second["index"] = serde_json::json!(2);
        second["mic_wav_relative"] = serde_json::json!("chunks/0001/mic.wav/mic.m4a");
        manifest["chunks"].as_array_mut().unwrap().push(second);
        let manifest = serde_json::to_vec(&manifest).unwrap();
        inbox.store(PHONE, SESSION, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        for chunk in ["chunks/0001/mic.m4a", "chunks/0001/mic.wav/mic.m4a"] {
            inbox.store(PHONE, SESSION, chunk, &body_hash(&aac()), &aac()).unwrap();
        }
        let lib = library();
        assert_eq!(import_session(&inbox, lib.path(), PHONE, SESSION, &key()), Err(ImportError::Io));
        assert_eq!(std::fs::read_dir(lib.path()).unwrap().count(), 0, "no session and no staging copy");
    }

    #[test]
    fn a_held_session_whose_audio_fails_to_land_keeps_its_manifest() {
        let (inbox, _td, _, _) = staged();
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();
        let before = std::fs::read(lib.path().join(SESSION).join("manifest.json")).unwrap();

        let mut manifest: serde_json::Value =
            serde_json::from_slice(&manifest_json(SESSION, "chunks/0001/mic.m4a")).unwrap();
        manifest["language"] = serde_json::json!("en-CA");
        let manifest = serde_json::to_vec(&manifest).unwrap();
        stage_into(&inbox, PHONE);
        inbox.store(PHONE, SESSION, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        let wav = lib.path().join(SESSION).join("chunks/0001/mic.wav");
        std::fs::remove_file(&wav).unwrap();
        std::fs::create_dir_all(wav.join("blocked")).unwrap();
        assert_eq!(import_session(&inbox, lib.path(), PHONE, SESSION, &key()), Err(ImportError::Io));
        let after = std::fs::read(lib.path().join(SESSION).join("manifest.json")).unwrap();
        assert_eq!(String::from_utf8(after).unwrap(), String::from_utf8(before).unwrap(), "the manifest lands last");
    }

    #[test]
    fn a_session_deleted_on_the_device_as_it_landed_is_taken_out_again() {
        let (inbox, _td, _, _) = staged();
        let lib = library();
        let local = import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();
        inbox.entomb(SESSION, Some(PHONE), b"{}").unwrap();
        assert!(bury_landed(&inbox, lib.path(), PHONE, &local));
        assert!(!lib.path().join(&local).exists());
        assert!(inbox.imported(PHONE, &local).is_none(), "the import record goes with it");
    }

    #[test]
    fn a_late_burial_leaves_another_devices_session_where_it_is() {
        let (inbox, _td, _, _) = staged();
        let lib = library();
        let local = import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();
        assert!(!bury_landed(&inbox, lib.path(), "0000000000000000000000000000ffff", &local));
        assert!(lib.path().join(&local).join("manifest.json").is_file());
    }

    /// A phone transcript whose one segment the phone's diarizer put in
    /// cluster 0.
    fn diarized_transcript_json(session: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "session_id": session,
            "provider": "phone",
            "model": "on-device",
            "transcribed_at_unix_seconds": 1_700_000_012i64,
            "chunks": [{ "chunk_index": 1, "tracks": [{
                "track": "mic", "source_wav_relative": "chunks/0001/mic.wav",
                "segments": [{ "start_ms": 0, "end_ms": 900, "text": "Morning.", "confidence": null, "speaker_id": 0 }],
            }] }],
        }))
        .unwrap()
    }

    #[test]
    fn clusters_that_land_ahead_of_their_table_get_a_run_to_be_named_under() {
        let (inbox, _td, _, _) = staged();
        let transcript = diarized_transcript_json(SESSION);
        inbox.store(PHONE, SESSION, "transcript.json", &body_hash(&transcript), &transcript).unwrap();
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();

        let dir = lib.path().join(SESSION);
        let run = recording::session_meta::current_run(&dir).expect("the clusters have a run");
        let manifest = crate::commands::session_store::read_manifest_opt(&dir).unwrap();
        assert_eq!(manifest.diarization_id.as_deref(), Some(run.as_str()), "recorded on the manifest");
        assert!(recording::session_meta::name_speaker(&dir, 0, "Priya", None, 900).unwrap());

        // The table lands with the next import and names the run from then on.
        let (inbox2, _td2, _, _) = staged();
        inbox2.store(PHONE, SESSION, "transcript.json", &body_hash(&transcript), &transcript).unwrap();
        let table = br#"{"schema_version":2,"diarization_id":"run-phone","speakers":[{"id":0}]}"#;
        inbox2.store(PHONE, SESSION, "speakers.json", &body_hash(table), table).unwrap();
        import_session(&inbox2, lib.path(), PHONE, SESSION, &key()).unwrap();
        assert_eq!(recording::session_meta::current_run(&dir).as_deref(), Some("run-phone"));
        let manifest = crate::commands::session_store::read_manifest_opt(&dir).unwrap();
        assert!(manifest.diarization_id.is_none(), "the table's run stands alone");
    }

    #[test]
    fn a_session_with_no_clusters_arrives_with_no_run() {
        let (inbox, _td, _, _) = staged();
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();
        assert!(recording::session_meta::current_run(&lib.path().join(SESSION)).is_none());
    }

    const NOTES: &[u8] = br#"{"v":1,"runs":[{"text":"Budget question\n","stamp_ms":754000}],"annotations":[]}"#;

    #[test]
    fn the_phones_notes_land_beside_the_transcript_and_their_revision_is_recorded() {
        let (inbox, _td, _, _) = staged();
        inbox.store(PHONE, SESSION, "notes.json", &body_hash(NOTES), NOTES).unwrap();
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();

        assert_eq!(std::fs::read(lib.path().join(SESSION).join("notes.json")).unwrap(), NOTES);
        let held = inbox.imported(PHONE, SESSION).expect("the revisions are recorded");
        assert_eq!(held.get("notes"), Some(&body_hash(NOTES)), "a note edit on the phone comes back");
    }

    #[test]
    fn a_session_without_notes_imports_as_before_and_drops_notes_from_an_earlier_import() {
        let (inbox, _td, _, _) = staged();
        inbox.store(PHONE, SESSION, "notes.json", &body_hash(NOTES), NOTES).unwrap();
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();
        assert!(lib.path().join(SESSION).join("notes.json").is_file());

        let (inbox2, _td2, _, _) = staged();
        import_session(&inbox2, lib.path(), PHONE, SESSION, &key()).unwrap();
        assert!(!lib.path().join(SESSION).join("notes.json").exists(), "the phone cleared its page");
        let held = inbox2.imported(PHONE, SESSION).unwrap();
        assert!(!held.contains_key("notes"), "only what arrived is recorded");
        assert!(lib.path().join(SESSION).join("transcript.json").is_file());
    }

    #[test]
    fn notes_sealed_to_another_machine_are_refused_before_anything_is_written() {
        let (inbox, _td, _, _) = staged();
        let sealed = seal_to(&[6u8; 32], "mac-2", NOTES);
        inbox.store(PHONE, SESSION, "notes.json", &body_hash(&sealed), &sealed).unwrap();

        let key = MachineKey { x25519_secret: [5u8; 32], device_id: "mac-1".into() };
        let lib = library();
        assert_eq!(import_session(&inbox, lib.path(), PHONE, SESSION, &key), Err(ImportError::Sealed));
        assert!(!lib.path().join(SESSION).exists(), "nothing half-written");
    }

    #[test]
    fn notes_sealed_to_this_machine_land_in_the_clear() {
        let (inbox, _td, _, _) = staged();
        let secret = [5u8; 32];
        let sealed = seal_to(&secret, "mac-1", NOTES);
        inbox.store(PHONE, SESSION, "notes.json", &body_hash(&sealed), &sealed).unwrap();
        let key = MachineKey { x25519_secret: secret, device_id: "mac-1".into() };
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key).unwrap();
        assert_eq!(std::fs::read(lib.path().join(SESSION).join("notes.json")).unwrap(), NOTES);
        let held = inbox.imported(PHONE, SESSION).unwrap();
        assert_eq!(held.get("notes"), Some(&body_hash(&sealed)), "the revision is the hash of what arrived");
    }

    #[test]
    fn a_manifest_this_desktop_cannot_read_is_refused_before_anything_is_written() {
        let td = tempfile::tempdir().unwrap();
        let inbox = Inbox::at(td.path().join("inbox"));
        let manifest = serde_json::to_vec(&serde_json::json!({ "schema_version": 1, "session_id": SESSION })).unwrap();
        inbox.store(PHONE, SESSION, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        let lib = library();
        assert_eq!(import_session(&inbox, lib.path(), PHONE, SESSION, &key()), Err(ImportError::Unsupported));
        assert!(!lib.path().join(SESSION).exists(), "nothing half-written");
    }

    #[test]
    fn a_manifest_this_desktop_cannot_open_is_ready_so_the_import_can_refuse_it() {
        let td = tempfile::tempdir().unwrap();
        let inbox = Inbox::at(td.path().join("inbox"));
        let sealed = seal_to(&[6u8; 32], "mac-2", &manifest_json(SESSION, "chunks/0001/mic.m4a"));
        inbox.store(PHONE, SESSION, "manifest.json", &body_hash(&sealed), &sealed).unwrap();
        let key = MachineKey { x25519_secret: [5u8; 32], device_id: "mac-1".into() };
        assert!(is_ready(&inbox, PHONE, SESSION, &key), "waiting on it would wait forever");
        let lib = library();
        assert_eq!(import_session(&inbox, lib.path(), PHONE, SESSION, &key), Err(ImportError::Sealed));
    }

    #[test]
    fn a_refusal_drops_the_staged_copies_and_remembers_the_revisions_it_refused() {
        let (inbox, _td, manifest, transcript) = staged();
        refuse(&inbox, SESSION, PHONE, RefusalReason::Sealed).unwrap();

        let refusal = inbox.refusal(PHONE, SESSION).expect("the refusal is recorded");
        assert_eq!(refusal.device_id, PHONE);
        assert_eq!(refusal.reason, RefusalReason::Sealed);
        assert_eq!(refusal.inputs.get("manifest"), Some(&body_hash(&manifest)));
        assert_eq!(refusal.inputs.get("transcript"), Some(&body_hash(&transcript)));
        assert!(refusal.inputs.get("speakers").is_none(), "only what was staged is recorded");
        assert!(inbox.read(PHONE, SESSION, "manifest.json").is_none(), "the bytes this desktop cannot open do not stay");
        assert!(inbox.staged_sessions().is_empty(), "nothing is left to try again");
    }

    #[test]
    fn a_session_that_imports_lifts_the_refusal_standing_against_it() {
        let (inbox, _td, manifest, transcript) = staged();
        refuse(&inbox, SESSION, PHONE, RefusalReason::Sealed).unwrap();

        // The phone offers the session again; the same bytes stand in for
        // the re-sealed ones a re-pair produces.
        let audio = aac();
        inbox.store(PHONE, SESSION, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        inbox.store(PHONE, SESSION, "transcript.json", &body_hash(&transcript), &transcript).unwrap();
        inbox.store(PHONE, SESSION, "chunks/0001/mic.m4a", &body_hash(&audio), &audio).unwrap();
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();
        assert!(inbox.refusal(PHONE, SESSION).is_none(), "an imported session is not a refused one");
    }

    #[test]
    fn a_sealed_session_is_opened_with_this_machines_key() {
        let td = tempfile::tempdir().unwrap();
        let inbox = Inbox::at(td.path().join("inbox"));
        let secret = [5u8; 32];
        let manifest = manifest_json(SESSION, "chunks/0001/mic.m4a");
        let transcript = transcript_json(SESSION);
        let sealed_manifest = seal_to(&secret, "mac-1", &manifest);
        let sealed_transcript = seal_to(&secret, "mac-1", &transcript);
        inbox.store(PHONE, SESSION, "manifest.json", &body_hash(&sealed_manifest), &sealed_manifest).unwrap();
        inbox.store(PHONE, SESSION, "transcript.json", &body_hash(&sealed_transcript), &sealed_transcript).unwrap();
        let audio = aac();
        let sealed_audio = seal_to(&secret, "mac-1", &audio);
        inbox.store(PHONE, SESSION, "chunks/0001/mic.m4a", &body_hash(&sealed_audio), &sealed_audio).unwrap();

        let key = MachineKey { x25519_secret: secret, device_id: "mac-1".into() };
        let lib = library();
        assert!(is_ready(&inbox, PHONE, SESSION, &key), "a sealed manifest names its chunks once opened");
        import_session(&inbox, lib.path(), PHONE, SESSION, &key).unwrap();
        let dir = lib.path().join(SESSION);
        let written: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(written["session_id"], SESSION, "stored in the clear, which is this desktop's norm");
        assert!(
            dir.join("chunks/0001/mic.wav").is_file(),
            "the audio decodes: an envelope never reaches the decoder"
        );
    }

    #[test]
    fn a_chunk_sealed_to_another_machine_is_refused_before_anything_is_written() {
        let (inbox, _td, _, _) = staged();
        let sealed_audio = seal_to(&[6u8; 32], "mac-2", b"aac bytes");
        inbox.store(PHONE, SESSION, "chunks/0001/mic.m4a", &body_hash(&sealed_audio), &sealed_audio).unwrap();

        let key = MachineKey { x25519_secret: [5u8; 32], device_id: "mac-1".into() };
        let lib = library();
        assert_eq!(import_session(&inbox, lib.path(), PHONE, SESSION, &key), Err(ImportError::Sealed));
        assert!(!lib.path().join(SESSION).exists(), "nothing half-written");
        assert!(inbox.read(PHONE, SESSION, "manifest.json").is_some(), "the import writes nothing and drops nothing; refusing is the caller's call");
    }

    #[test]
    fn a_speakers_file_sealed_to_another_machine_is_refused_before_anything_is_written() {
        let (inbox, _td, _, _) = staged();
        let sealed_speakers = seal_to(&[6u8; 32], "mac-2", br#"{"speakers":[]}"#);
        inbox.store(PHONE, SESSION, "speakers.json", &body_hash(&sealed_speakers), &sealed_speakers).unwrap();

        let key = MachineKey { x25519_secret: [5u8; 32], device_id: "mac-1".into() };
        let lib = library();
        assert_eq!(import_session(&inbox, lib.path(), PHONE, SESSION, &key), Err(ImportError::Sealed));
        assert!(!lib.path().join(SESSION).exists(), "nothing half-written");
    }

    fn seam(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/sync-seam/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn the_seam_live_lines_promote_onto_the_seam_manifest() {
        let lines = live_lines(&seam("live_transcript.phone-v5.jsonl"));
        assert_eq!(lines.len(), 2, "both of the recorder's lines read");
        let manifest: recording::manifest::SessionManifest =
            serde_json::from_slice(&seam("manifest.phone-v4.json")).unwrap();
        let st = provisional_transcript(&manifest, "daisy-1700000000", &lines, 1_700_002_000).unwrap();
        assert!(st.provisional);
        assert_eq!(st.rank(), 0, "never transcribed, never edited");
        let first = &st.chunks[0];
        assert_eq!(first.chunk_index, 0);
        assert_eq!(first.tracks.len(), 1);
        assert_eq!(first.tracks[0].source_wav_relative, std::path::PathBuf::from("chunks/0000/mic.wav"));
        let texts: Vec<_> = first.tracks[0].segments.iter().map(|s| (s.start_ms, s.text.as_str())).collect();
        assert_eq!(texts, [(1_200, "Bonjour à tous."), (5_000, "On commence par le calendrier du four.")]);
    }

    #[test]
    fn the_seam_phone_transcript_carries_its_edit_stamp() {
        let bytes = seam("transcript.phone-v5.json");
        let st: transcript::SessionTranscript = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(st.edited_at_unix_seconds, Some(1_700_003_600));
        assert!(!st.provisional);
        assert_eq!(st.rank(), 1_700_003_600, "the edit is later than the transcription");
        let back: serde_json::Value = serde_json::to_value(&st).unwrap();
        assert_eq!(back, serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(), "nothing lost or added");
    }

    #[test]
    fn a_torn_last_live_line_is_dropped_and_the_rest_kept() {
        let whole = seam("live_transcript.phone-v5.jsonl");
        let text = std::str::from_utf8(&whole).unwrap();
        // A whole object the recorder had not finished the line of.
        let unterminated = text.trim_end_matches('\n');
        assert_eq!(live_lines(unterminated.as_bytes()).len(), 1);
        // Half an object, newline and all.
        let half = format!("{text}{{\"end_ms\":12000,\"fin\n");
        assert_eq!(live_lines(half.as_bytes()).len(), 2);
        // Half an object still being written.
        let open = format!("{text}{{\"end_ms\":12000,\"fin");
        assert_eq!(live_lines(open.as_bytes()).len(), 2);
    }

    #[test]
    fn a_live_snapshot_cut_mid_character_or_holding_a_bad_byte_keeps_its_good_lines() {
        let whole = seam("live_transcript.phone-v5.jsonl");
        // Cut inside the "à" of the first line's text, as a copy of a file
        // still being written can be.
        let at = whole.windows(2).position(|w| w == "à".as_bytes()).unwrap() + 1;
        assert_eq!(live_lines(&whole[..at]).len(), 0, "the one line is torn");
        let mut cut = whole.clone();
        cut.extend_from_slice(&"{\"text\":\"déj".as_bytes()[..11]);
        assert_eq!(live_lines(&cut).len(), 2, "a torn character after the last newline costs nothing");
        // One line holding a byte that is not UTF-8, between two good ones.
        let first_end = whole.iter().position(|&b| b == b'\n').unwrap() + 1;
        let mut bad = whole[..first_end].to_vec();
        bad.extend_from_slice(b"{\"text\":\"\xff\"}\n");
        bad.extend_from_slice(&whole[first_end..]);
        let texts: Vec<_> = live_lines(&bad).into_iter().map(|l| l.text).collect();
        assert_eq!(texts, ["Bonjour à tous.", "On commence par le calendrier du four."]);
    }

    #[test]
    fn a_session_deleted_on_the_device_before_it_is_written_is_not_imported() {
        let (inbox, _td, _, _) = staged();
        // The tombstone landed; the staged copies are back, as an upload
        // racing it would leave them.
        inbox.entomb(SESSION, Some(PHONE), b"{}").unwrap();
        stage_into(&inbox, PHONE);
        let lib = library();
        assert_eq!(import_session(&inbox, lib.path(), PHONE, SESSION, &key()), Err(ImportError::Entombed));
        assert!(!lib.path().join(SESSION).exists(), "nothing written");
        assert!(inbox.imported(PHONE, SESSION).is_none());
        assert_eq!(ImportError::Entombed.refusal(), None, "and nothing refused");
    }

    #[test]
    fn live_captions_past_the_cap_are_recorded_and_not_read() {
        let (inbox, _td) = fresh();
        let manifest = manifest_json(SESSION, "chunks/0001/mic.m4a");
        let audio = aac();
        inbox.store(PHONE, SESSION, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        inbox.store(PHONE, SESSION, "chunks/0001/mic.m4a", &body_hash(&audio), &audio).unwrap();
        let line = r#"{"end_ms":900,"final":true,"kind":"final","received_at_unix":1700000001,"start_ms":0,"text":"one","track":"mic"}"#;
        let mut big = Vec::new();
        while big.len() <= LIVE_MAX_BYTES {
            big.extend_from_slice(line.as_bytes());
            big.push(b'\n');
        }
        stage(&inbox, PHONE, SESSION, crate::sync::inbox::LIVE_TRANSCRIPT, &body_hash(&big), &big, &key()).unwrap();
        assert_eq!(inbox.plaintext(PHONE, SESSION, crate::sync::inbox::LIVE_TRANSCRIPT), Some(body_hash(&big)));
        let lib = library();
        import_session(&inbox, lib.path(), PHONE, SESSION, &key()).unwrap();
        let dir = lib.path().join(SESSION);
        assert!(dir.join("chunks/0001/mic.wav").is_file(), "the session imports");
        assert!(!dir.join("transcript.json").exists() && !dir.join("live_transcript.jsonl").exists());
        let record = inbox.imported(PHONE, SESSION).unwrap();
        assert_eq!(record.get("live_transcript"), Some(&body_hash(&big)), "a hello naming it asks for nothing");
    }

    #[test]
    fn live_lines_keep_final_mic_lines_whatever_their_kind() {
        let lines = [
            r#"{"end_ms":900,"final":true,"kind":"final","received_at_unix":1700000001,"start_ms":0,"text":"one","track":"mic"}"#,
            r#"{"end_ms":1900,"final":false,"kind":"interim","received_at_unix":1700000002,"start_ms":1000,"text":"tw","track":"mic"}"#,
            r#"{"end_ms":1950,"final":false,"kind":"possiblemissedspeech","received_at_unix":1700000002,"start_ms":1000,"text":"","track":"mic"}"#,
            r#"{"end_ms":2900,"final":true,"kind":"a_kind_from_later","received_at_unix":1700000003,"start_ms":2000,"text":"three","track":"mic"}"#,
            r#"{"end_ms":3900,"final":true,"kind":"final","received_at_unix":1700000004,"start_ms":3000,"text":"four","track":"system"}"#,
        ];
        let bytes = format!("{}\n", lines.join("\n"));
        let texts: Vec<_> = live_lines(bytes.as_bytes()).into_iter().map(|l| l.text).collect();
        assert_eq!(texts, ["one", "three"]);
    }

    fn ranked(transcribed: i64, edited: Option<i64>, provisional: bool) -> transcript::SessionTranscript {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1, "session_id": "s", "provider": "p", "model": "m",
            "transcribed_at_unix_seconds": transcribed, "chunks": [],
            "edited_at_unix_seconds": edited, "provisional": provisional,
        }))
        .unwrap()
    }

    #[test]
    fn an_arriving_transcript_replaces_by_these_rules() {
        let phone = ranked(1_700_000_500, None, false);
        assert!(replaces(None, None, &phone), "nothing held");
        // The phone's copy as an earlier import wrote it: the phone's next
        // copy replaces it even stamped earlier or the same.
        let held = ranked(1_700_000_900, None, false);
        assert!(replaces(Some(&held), Some(&stamp(&held)), &phone));
        assert!(replaces(Some(&held), None, &ranked(1_700_000_900, None, false)), "a phone copy stamped as the held one");
        // A transcript made here ranks against the phone's.
        let here = ranked(1_700_000_900, None, false);
        assert!(!replaces(Some(&here), Some("1700000100//false"), &phone), "made here later");
        assert!(replaces(Some(&here), Some("1700000100//false"), &ranked(1_700_000_901, None, false)));
        assert!(!replaces(Some(&here), Some("1700000100//false"), &ranked(1_700_000_100, Some(1_700_000_900), false)), "a tie keeps what is held");
        // The provisional transcript: replaced untouched, ranked by its edit once edited.
        let promoted = ranked(1_700_000_000, None, true);
        assert!(replaces(Some(&promoted), Some(&stamp(&promoted)), &phone));
        let edited = ranked(1_700_000_000, Some(1_700_000_700), true);
        assert!(!replaces(Some(&edited), Some(&stamp(&promoted)), &phone));
        assert!(replaces(Some(&edited), Some(&stamp(&promoted)), &ranked(1_700_000_100, Some(1_700_000_800), false)));
        // Live captions never replace a transcript that is not provisional.
        assert!(!replaces(Some(&held), Some(&stamp(&held)), &ranked(1_700_002_000, None, true)));
    }

}
