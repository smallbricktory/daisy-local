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
    /// or bytes that are not what their name says.
    Unsupported,
    Io,
}

impl ImportError {
    /// The refusal an error stands for, when retrying the same bytes could
    /// only fail the same way.
    pub fn refusal(self) -> Option<RefusalReason> {
        match self {
            ImportError::Sealed => Some(RefusalReason::Sealed),
            ImportError::Unsupported => Some(RefusalReason::Unsupported),
            ImportError::Missing | ImportError::Io => None,
        }
    }
}

/// The first id this desktop mints for an import whose own id the library
/// already holds. Every device mints a session id from a clock reading, and
/// seconds since 1970 stay ten digits for thousands of years: an id at or
/// above this one is this desktop's own minting and no device's clock ever
/// lands on it.
pub const MINTED_ID_FLOOR: u64 = 900_000_000_000;

/// The number in a session id, or `None` for one that is not a session id.
fn id_number(id: &str) -> Option<u64> {
    if !is_session_id(id) {
        return None;
    }
    id.strip_prefix("daisy-")?.parse().ok()
}

/// Every minted id the library holds: the device it was minted for, then the
/// id that device knows the session by, then the id the library holds it
/// under. Read once per hello — resolving one session at a time would scan
/// the library and read a manifest per minted session for every session the
/// phone lists, inside a window the phone gives ten seconds to.
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

/// True when everything a session needs has arrived: its manifest, its
/// transcript and every chunk the manifest lists. A manifest this machine
/// cannot open counts as ready: the import refuses it and says so, where
/// waiting on it would wait forever.
pub fn is_ready(inbox: &Inbox, device_id: &str, session: &str, key: &MachineKey) -> bool {
    match missing(inbox, device_id, session, key) {
        Ok(paths) => {
            paths.is_empty()
                && (inbox.exists(device_id, session, "transcript.json")
                    || inbox.claim(device_id, session).is_some_and(|c| c.kept.iter().any(|k| k == "transcript")))
        }
        Err(ImportError::Missing) => false,
        Err(_) => true,
    }
}

/// A session's `audio` (§5): `sha256:` over each chunk's plaintext SHA-256 as
/// lowercase hex, in manifest order, joined with `\n`.
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
    // Every file is opened before anything is written, so a session this
    // desktop cannot read leaves nothing half-written behind. The raw bytes
    // stay alongside the plaintext: the revisions map hashes what arrived,
    // the library holds what it says.
    let raw_manifest = inbox.read(device_id, session, "manifest.json").ok_or(ImportError::Missing)?;
    let manifest = open(&raw_manifest, key)?;
    let parsed: recording::manifest::SessionManifest =
        serde_json::from_slice(&manifest).map_err(|_| ImportError::Unsupported)?;
    if !parsed.schema_is_supported() {
        return Err(ImportError::Unsupported);
    }
    // Written back from the phone's own JSON rather than from `parsed`, so
    // keys this desktop has no field for survive the trip.
    let mut value: serde_json::Value = serde_json::from_slice(&manifest).map_err(|_| ImportError::Unsupported)?;
    let paths = chunk_paths(&value);
    if paths.is_empty() {
        return Err(ImportError::Unsupported);
    }
    // What the latest hello said this desktop already holds of the session.
    let claim = inbox.claim(device_id, session).unwrap_or_default();

    // Every device mints `daisy-<unix>`, so two of them land on one id. The
    // session already here keeps it and this one gets an id of this desktop's
    // own; a session that was given one before answers to it again. A phone
    // that comes back under a new device id brings the same recording: the
    // session holding that id with the same audio takes it back. A directory
    // whose manifest cannot be read counts as occupied: overwriting it would
    // land this session on top of another recording.
    let minted = minted_ids(sessions_dir);
    let own = local_id(&minted, device_id, session);
    let ours = match occupant(&sessions_dir.join(&own)) {
        Occupant::Vacant => true,
        Occupant::Device(owner) => owner == device_id,
        Occupant::Unknown => false,
    };
    // Audio the library holds at the revision the hello named stays as it
    // is; any other audio is decoded before anything is written, so a chunk
    // that is not audio refuses the session the way a manifest this build
    // cannot read does.
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
                let decoded = recording::audio_import::decode_bytes_to_mono_16k(bytes.to_vec(), ext)
                    .map_err(|_| ImportError::Unsupported)?;
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

    let transcript = arrived("transcript", "transcript.json", &mut revisions)?;
    let transcript = match transcript {
        Some(plain) => {
            // Validated as the manifest is: a transcript this build cannot
            // read lands nowhere, instead of landing and failing the pipeline
            // in silence.
            serde_json::from_slice::<transcript::SessionTranscript>(&plain).map_err(|_| ImportError::Unsupported)?;
            Some(plain)
        }
        None if kept("transcript", &mut revisions) => None,
        None => return Err(ImportError::Missing),
    };
    let speakers = arrived("speakers", "speakers.json", &mut revisions)?;
    if speakers.is_none() {
        kept("speakers", &mut revisions);
    }
    let notes = arrived("notes", recording::notes::FILE_NAME, &mut revisions)?;
    let notes_kept = notes.is_none() && kept("notes", &mut revisions);

    // Every key this desktop derives is written here whatever the phone sent,
    // so none of them can arrive from the phone and be read back as this
    // desktop's own. `origin_session` has no field on the manifest, so it
    // rides in the untouched `extra` keys unless it is authored on every path.
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

    std::fs::create_dir_all(&dir).map_err(|_| ImportError::Io)?;
    write(&dir.join("manifest.json"), &manifest_out)?;
    if let Some(plain) = &transcript {
        write(&dir.join("transcript.json"), plain)?;
    }
    if let Some(plain) = &speakers {
        write(&dir.join("speakers.json"), plain)?;
    }
    // A phone that dropped its notes since the last import sends none, and
    // the copy from the earlier import must not outlive them.
    let notes_path = dir.join(recording::notes::FILE_NAME);
    match &notes {
        Some(plain) => write(&notes_path, plain)?,
        None if notes_kept => {}
        None => {
            if notes_path.is_file() {
                std::fs::remove_file(&notes_path).map_err(|_| ImportError::Io)?;
            }
        }
    }
    for (path, pcm) in &chunks {
        let wav = dir.join(wav_path_of(path));
        if let Some(parent) = wav.parent() {
            std::fs::create_dir_all(parent).map_err(|_| ImportError::Io)?;
        }
        recording::write_wav_mono_i16(&wav, pcm, recording::audio_import::TARGET_RATE).map_err(|_| ImportError::Io)?;
    }

    inbox.record_import(device_id, &local, &revisions).map_err(|_| ImportError::Io)?;
    inbox.forget_refusal(device_id, session).map_err(|_| ImportError::Io)?;
    inbox.clear_staged(device_id, session).map_err(|_| ImportError::Io)?;
    Ok(local)
}

/// Gives up on a staged session at the revisions it arrived at: the
/// refusal is recorded against those revisions and the staged copies go.
/// A hello naming the same revisions asks for nothing; one naming new
/// revisions, which is what a re-pair or an update on the phone produces,
/// brings the session over again.
pub fn refuse(inbox: &Inbox, session: &str, device_id: &str, reason: RefusalReason) -> Result<(), ImportError> {
    let inputs = INPUTS
        .iter()
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
    /// A directory whose manifest this desktop cannot read. A sync tool holds
    /// a file on a live profile open for moments at a time, so this is
    /// occupied by something, never free to write over.
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
    const SESSION: &str = "daisy-1787249858";

    fn manifest_json(session: &str, chunk: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 4,
            "session_id": session,
            "created_at_unix_seconds": 1_787_249_858i64,
            "sample_rate": 16000,
            "channels": 1,
            "language": "fr-CA",
            "chunks": [{
                "index": 1,
                "started_at_unix_seconds": 1_787_249_858i64,
                "ended_at_unix_seconds": 1_787_249_868i64,
                "duration_seconds": 9.4,
                "mic_samples": 150_393,
                "mic_wav_relative": chunk,
            }],
        }))
        .unwrap()
    }

    fn transcript_json(session: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "session_id": session,
            "provider": "phone",
            "model": "on-device",
            "transcribed_at_unix_seconds": 1_787_249_870i64,
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
        assert!(!is_ready(&inbox, PHONE, SESSION, &key()), "the transcript and the chunk are still missing");

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

        // A newer revision from the tablet lands in the directory it already
        // has, rather than minting a third.
        other_recording(&inbox, TABLET);
        let newer = br#"{"schema_version":1,"session_id":"daisy-1787249858","provider":"phone","model":"on-device","transcribed_at_unix_seconds":1787249999,"chunks":[]}"#;
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
        std::fs::write(dir.join("manifest.json"), br#"{"schema_version":4,"session_id":"daisy-1787249858"}"#).unwrap();

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
    fn audio_that_does_not_decode_is_unsupported_before_anything_is_written() {
        let (inbox, _td, _, _) = staged();
        inbox.store(PHONE, SESSION, "chunks/0001/mic.m4a", &body_hash(b"not audio"), b"not audio").unwrap();
        let lib = library();
        assert_eq!(
            import_session(&inbox, lib.path(), PHONE, SESSION, &key()),
            Err(ImportError::Unsupported)
        );
        assert!(!lib.path().join(SESSION).exists(), "nothing half-written");
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

}
