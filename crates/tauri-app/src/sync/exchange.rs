// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The hello exchange: what the desktop wants from a phone, what it offers back, and the tombstones each side carries. The reply is built from what is already on disk.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::Signer;
use serde::{Deserialize, Serialize};

use crate::sync::import::{self, MachineKey};
use crate::sync::inbox::{is_wire_session_id, Inbox, BUILT_FROM, INPUTS};
use crate::sync::signing::body_hash;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TombstoneRecord {
    pub v: u32,
    pub session: String,
    pub seq: u64,
    pub origin: String,
    pub sig_b64: String,
}

/// The rest of a hello, once the gate has read its `v` and `device_id`.
#[derive(Debug, Deserialize)]
struct HelloRequest {
    #[serde(default)]
    sessions: Vec<HelloSession>,
    #[serde(default)]
    tombstones: Vec<TombstoneRecord>,
    /// The phone's profile files by their hash on disk (section 5a); absent
    /// from a phone that does not carry them.
    #[serde(default)]
    profile: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct HelloSession {
    id: String,
    #[serde(default)]
    inputs: std::collections::BTreeMap<String, String>,
    /// The artifact ids the phone already holds; those are not offered again.
    #[serde(default)]
    artifacts: Vec<String>,
    /// The plaintext hash of each input the phone can open now, keyed like
    /// `inputs`. A reseal leaves it as it was.
    #[serde(default)]
    content: std::collections::BTreeMap<String, String>,
    /// The session's audio, once the phone has hashed every chunk.
    #[serde(default)]
    audio: Option<String>,
    /// The plaintext hash of the session's `meta.json`, absent from a session
    /// that has none. Unlike `inputs`, this names the bytes that travel.
    #[serde(default)]
    meta: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Want {
    pub session: String,
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Offer {
    pub artifact_id: String,
    pub session: String,
    pub name: String,
    pub size: u64,
    pub header_b64: String,
    pub header_sig_b64: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HelloReply {
    pub v: u32,
    pub wants: Vec<Want>,
    pub offers: Vec<Offer>,
    pub tombstones: Vec<TombstoneRecord>,
    /// The profile files this desktop lacks the phone's revision of.
    pub profile_wants: Vec<String>,
    /// The merged profile files the phone has yet to install.
    pub profile_offers: Vec<Offer>,
    /// The sessions whose `meta.json` this desktop lacks the phone's revision
    /// of. Bare ids: the file name is the same every time.
    pub meta_wants: Vec<String>,
    /// The merged `meta.json` files the phone has yet to install.
    pub meta_offers: Vec<Offer>,
    /// The sessions whose tombstones this hello buried: the id the phone
    /// knows each by, the id the library held it under, and whether files
    /// or a claim of it were still waiting here. Never sent.
    #[serde(skip)]
    pub buried: Vec<(String, String, bool)>,
}

/// What section 8 signs: `tombstone`, the session and the counter.
pub fn tombstone_signing_bytes(session: &str, seq: u64) -> Vec<u8> {
    format!("tombstone\n{session}\n{seq}").into_bytes()
}

/// A tombstone signed by `key`, which is the origin device's Ed25519 key.
pub fn tombstone_record(session: &str, seq: u64, origin: &str, key: &ed25519_dalek::SigningKey) -> TombstoneRecord {
    TombstoneRecord {
        v: 1,
        session: session.to_string(),
        seq,
        origin: origin.to_string(),
        sig_b64: B64.encode(key.sign(&tombstone_signing_bytes(session, seq)).to_bytes()),
    }
}

/// True when the record is a version-1 tombstone for a real session id,
/// signed by the key it names as its origin.
pub fn tombstone_is_valid(record: &TombstoneRecord, origin_ed25519_pub_b64: &str) -> bool {
    record.v == 1
        && is_wire_session_id(&record.session)
        && crate::sync::signing::verify_request(
            origin_ed25519_pub_b64,
            &record.sig_b64,
            &String::from_utf8_lossy(&tombstone_signing_bytes(&record.session, record.seq)),
        )
}

/// What one tombstone did on this desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tombstoned {
    /// Buried here: the device can drop the session for good.
    Done,
    /// Not signed by the device it names. Nothing changed, and sending it
    /// again brings the same answer.
    Refused,
    /// Nothing was decided and nothing was written: the device sends it
    /// again, and the route says so rather than answering that it is done.
    Retry,
}

/// Buries the session `device_id` deleted. The library copy goes only when the manifest there names the device that sent the tombstone.
pub fn accept_tombstone(
    inbox: &Inbox,
    sessions_dir: &std::path::Path,
    minted: &import::MintedIds,
    device_id: &str,
    record: &TombstoneRecord,
    origin_ed25519_pub_b64: &str,
) -> Tombstoned {
    if !tombstone_is_valid(record, origin_ed25519_pub_b64) {
        return Tombstoned::Refused;
    }
    let Ok(bytes) = serde_json::to_vec(record) else { return Tombstoned::Refused };
    let _writing = import::library_write_lock();
    // A delete on either device is a delete on both: the imported session
    // goes with the staged files.
    let local = import::local_id(minted, device_id, &record.session);
    let dir = sessions_dir.join(&local);
    let held_here = match import::occupant(&dir) {
        import::Occupant::Device(owner) if owner == device_id => {
            if let Err(e) = crate::commands::session::remove_contained_session_dir(sessions_dir, &dir) {
                log::warn!("sync: session {local} not removed: {e}");
                return Tombstoned::Retry;
            }
            true
        }
        import::Occupant::Vacant => true,
        import::Occupant::Device(_) => {
            log::info!("sync: {local} stays: its manifest does not name {device_id}");
            false
        }
        import::Occupant::Unknown => {
            log::warn!("sync: tombstone {} waits: {local} cannot be read", record.session);
            return Tombstoned::Retry;
        }
    };
    if let Err(e) = inbox.entomb(&record.session, Some(device_id), &bytes) {
        log::warn!("sync: tombstone {} not recorded: {e:?}", record.session);
        return Tombstoned::Retry;
    }
    if held_here {
        if let Err(e) = inbox.forget_import(device_id, &local) {
            log::warn!("sync: import record {local} not cleared: {e:?}");
        }
    }
    Tombstoned::Done
}

/// Buries a phone session on this desktop and signs the tombstone that phone
/// collects at its next hello. `wire` is the id the phone knows the session
/// by and `local` the id the library held it under. The session directory
/// itself is removed by the caller, which holds `import::library_write_lock`
/// across the removal and this: this is the record, not the deletion.
pub fn entomb_here(
    inbox: &Inbox,
    wire: &str,
    local: &str,
    phone_id: &str,
    device_id: &str,
    key: &ed25519_dalek::SigningKey,
) -> bool {
    let seq = inbox.next_tombstone_seq();
    let record = tombstone_record(wire, seq, device_id, key);
    let Ok(bytes) = serde_json::to_vec(&record) else { return false };
    if let Err(e) = inbox.entomb(wire, Some(phone_id), &bytes) {
        log::warn!("sync: tombstone {wire} not recorded: {e:?}");
        return false;
    }
    if let Err(e) = inbox.forget_import(phone_id, local) {
        log::warn!("sync: import record {local} not cleared: {e:?}");
    }
    true
}

/// The protocol version this desktop speaks. A phone on any other version is refused at the hello (`sync::gate`).
pub const PROTOCOL_V: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloError {
    Malformed,
    /// The phone speaks another protocol version; the reply is 426 and
    /// nothing of the hello is read.
    OtherVersion(crate::sync::gate::Outdated),
}

/// The files a phone can install, each named by the hash of its bytes: the summary and the notes typed here.
const ARTIFACTS: [&str; 2] = ["summary.json", NOTES];

const NOTES: &str = "notes.md";

/// The largest notes file offered.
const NOTES_MAX_BYTES: usize = 256 * 1024;

/// Whether these bytes may travel under this name: a summary only in the
/// wire shape (one this desktop wrote before v2 is rewritten the next time it
/// is loaded here), notes only as UTF-8 within the cap.
fn travels(name: &str, bytes: &[u8]) -> bool {
    match name {
        NOTES => bytes.len() <= NOTES_MAX_BYTES && std::str::from_utf8(bytes).is_ok(),
        _ => summarize::SessionSummary::is_v2_bytes(bytes),
    }
}

/// What one artifact file hashed to when it was last read, held so a hello
/// does not digest every session's artifact again. A file whose length or
/// modification time has moved since is read afresh, and so is one that was
/// still within `TIMESTAMP_TICK` of its modification time when read.
struct HashedArtifact {
    modified: std::time::SystemTime,
    len: u64,
    settled: bool,
    artifact_id: String,
    travels: bool,
}

fn hash_cache() -> &'static std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, HashedArtifact>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, HashedArtifact>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The coarsest file-system timestamp granularity in use (FAT: two seconds).
/// A write inside the same tick as an earlier one leaves the modification
/// time unchanged.
const TIMESTAMP_TICK: std::time::Duration = std::time::Duration::from_secs(2);

/// The id of one artifact on disk, whether it travels, and its length. Reads
/// the file only when the cache has nothing settled for it at its current
/// length and time; a file the read cannot open is answered from what the
/// cache holds.
fn artifact_digest(path: &std::path::Path) -> Option<(String, bool, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let (len, modified) = (meta.len(), meta.modified().ok()?);
    let mut unsettled = None;
    if let Ok(cache) = hash_cache().lock() {
        if let Some(hit) = cache.get(path) {
            if hit.modified == modified && hit.len == len {
                let held = (hit.artifact_id.clone(), hit.travels, hit.len);
                if hit.settled {
                    return Some(held);
                }
                unsettled = Some(held);
            }
        }
    }
    let read_at = std::time::SystemTime::now();
    let Ok(bytes) = std::fs::read(path) else { return unsettled };
    let settled = read_at.duration_since(modified).is_ok_and(|age| age >= TIMESTAMP_TICK);
    let artifact_id = body_hash(&bytes);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let travels = travels(name, &bytes);
    let len = bytes.len() as u64;
    if let Ok(mut cache) = hash_cache().lock() {
        // The map is cleared when it reaches its size limit.
        if cache.len() >= 4096 {
            cache.clear();
        }
        cache.insert(
            path.to_path_buf(),
            HashedArtifact { modified, len, settled, artifact_id: artifact_id.clone(), travels },
        );
    }
    Some((artifact_id, travels, len))
}

/// Drops what was remembered about one file, so the next hello reads it.
fn forget_artifact(path: &std::path::Path) {
    if let Ok(mut cache) = hash_cache().lock() {
        cache.remove(path);
    }
}

/// What this desktop has built for a session and the phone does not hold.
/// Each offer carries a header signed by this machine, naming the input
/// revisions the artifact was built from, so a phone can tell an artifact
/// built from what it has now from one built before its last edit. Offers
/// name the session by the id the phone knows it by, whatever this library
/// holds it under.
pub fn artifact_offers(
    inbox: &Inbox,
    sessions_dir: &std::path::Path,
    minted: &import::MintedIds,
    device_id: &str,
    session: &str,
    phone_holds: &[String],
    key: &ed25519_dalek::SigningKey,
) -> Vec<Offer> {
    if !is_wire_session_id(session) {
        return Vec::new();
    }
    let local = import::local_id(minted, device_id, session);
    let mut built_from = inbox.imported(device_id, &local).unwrap_or_default();
    built_from.retain(|k, _| BUILT_FROM.contains(&k.as_str()));
    let dir = sessions_dir.join(&local);
    let empty = std::collections::BTreeMap::<String, String>::new();
    let mut offers = Vec::new();
    for name in ARTIFACTS {
        let Some((artifact_id, travels, len)) = artifact_digest(&dir.join(name)) else { continue };
        if !travels {
            continue;
        }
        if phone_holds.iter().any(|held| held == &artifact_id) {
            continue;
        }
        // Notes are read from nothing the phone holds, so no edit there
        // makes them stale.
        let fields = serde_json::json!({ "built_from": if name == NOTES { &empty } else { &built_from } });
        offers.push(crate::sync::records::offer(key, session, name, artifact_id, len, fields));
    }
    offers
}

/// The bytes of one artifact this desktop holds, found by the hash a phone
/// asks for: a record (`sync::records::artifact`), or a file built for an
/// imported session. `voices` is the vault's `voices.json`.
pub fn artifact_bytes(
    inbox: &Inbox,
    sessions_dir: &std::path::Path,
    voices: Option<&[u8]>,
    artifact_id: &str,
) -> Option<Vec<u8>> {
    if let Some(bytes) = crate::sync::records::artifact(inbox, sessions_dir, voices, artifact_id) {
        return Some(bytes);
    }
    for session in inbox.imported_sessions() {
        let dir = sessions_dir.join(&session);
        for name in ARTIFACTS {
            let path = dir.join(name);
            let Some((held, _, _)) = artifact_digest(&path) else { continue };
            if held != artifact_id {
                continue;
            }
            // A digest that disagrees with the bytes is dropped.
            let Ok(bytes) = std::fs::read(&path) else { continue };
            if body_hash(&bytes) == artifact_id {
                return Some(bytes);
            }
            forget_artifact(&path);
        }
    }
    None
}

/// Answers one hello: takes the phone's tombstones, then names every file
/// this desktop lacks and every tombstone the phone has yet to act on.
/// `voices` is the bytes of this desktop's `voices.json`, which live in the
/// vault; `None` while it is locked or holds none.
#[allow(clippy::too_many_arguments)]
pub fn hello_reply(
    inbox: &Inbox,
    sessions_dir: &std::path::Path,
    device_id: &str,
    phone_ed25519_pub_b64: &str,
    key: &ed25519_dalek::SigningKey,
    envelope_key: &MachineKey,
    body: &[u8],
    voices: Option<&[u8]>,
) -> Result<HelloReply, HelloError> {
    crate::sync::gate::check(body, device_id).map_err(|g| match g {
        crate::sync::gate::Gate::Malformed => HelloError::Malformed,
        crate::sync::gate::Gate::Refused(o) => HelloError::OtherVersion(o),
    })?;
    let hello: HelloRequest = serde_json::from_slice(body).map_err(|_| HelloError::Malformed)?;
    // A hello carries the device's tombstones; they resolve against one read of the library.
    let before = import::minted_ids(sessions_dir);
    let mut buried = Vec::new();
    for record in &hello.tombstones {
        let waiting = inbox.holds_staged(device_id, &record.session);
        match accept_tombstone(inbox, sessions_dir, &before, device_id, record, phone_ed25519_pub_b64) {
            Tombstoned::Done => buried.push((
                record.session.clone(),
                import::local_id(&before, device_id, &record.session),
                waiting,
            )),
            Tombstoned::Retry => log::warn!("sync: tombstone {} from {device_id} is not settled yet", record.session),
            Tombstoned::Refused => {}
        }
    }

    // Read again after the tombstones, once for the whole reply.
    let mut minted = import::minted_ids(sessions_dir);
    let mut adopted = false;
    for session in &hello.sessions {
        if let Some(audio) = &session.audio {
            adopted |= import::adopt(inbox, sessions_dir, &minted, device_id, &session.id, audio);
        }
    }
    if adopted {
        minted = import::minted_ids(sessions_dir);
    }

    let mut wants = Vec::new();
    let mut offers = Vec::new();
    let mut tombstones = Vec::new();
    for session in &hello.sessions {
        if !is_wire_session_id(&session.id) {
            log::warn!("sync: {device_id} named a session this desktop cannot hold: {}", crate::sync::shown(&session.id));
            continue;
        }
        if inbox.is_entombed(&session.id, device_id) {
            if let Some(record) = inbox.tombstone(&session.id, device_id).and_then(|b| serde_json::from_slice(&b).ok()) {
                tombstones.push(record);
            }
            continue;
        }
        if inbox.refusal(device_id, &session.id).is_some_and(|r| r.covers(&session.inputs)) {
            continue;
        }
        // An input is at the revision on record when its sealed hash is the
        // one recorded, or its plaintext hash is: a reseal moves the first
        // and leaves the second.
        let local = import::local_id(&minted, device_id, &session.id);
        let imported = inbox.imported(device_id, &local);
        type Record = std::collections::BTreeMap<String, String>;
        // The offered input `k` is the revision `held` names under these
        // keys, by its sealed hash or its plaintext hash.
        let names = |held: &Record, k: &str, sealed: &str, content: &str| {
            session.inputs.get(k).is_some_and(|h| held.get(sealed) == Some(h))
                || session.content.get(k).is_some_and(|c| held.get(content) == Some(c))
        };
        let recorded = |held: &Record, k: &str| {
            held.get(k) == session.inputs.get(k)
                || session.content.get(k).is_some_and(|c| held.get(&format!("content.{k}")) == Some(c))
        };
        // The phone transcript and speaker table this desktop declined are
        // never asked for again.
        let declined = |held: &Record| {
            names(held, "transcript", import::REJECTED_TRANSCRIPT, import::REJECTED_TRANSCRIPT_CONTENT)
        };
        let speakers_declined =
            |held: &Record| names(held, "speakers", import::REJECTED_SPEAKERS, import::REJECTED_SPEAKERS_CONTENT);
        // A phone transcript is fetched when it is neither declined nor on
        // record, and any is while the library's transcript is provisional.
        let fetching_transcript = |held: &Record| {
            session.inputs.contains_key("transcript")
                && !declined(held)
                && (!recorded(held, "transcript") || import::holds_provisional(held))
        };
        // Live captions count as held once a phone transcript is on record,
        // and a declined speaker table while no transcript is fetched.
        let same = |held: &Record, k: &str| match k {
            "live_transcript" if held.contains_key("transcript") => true,
            "transcript" if fetching_transcript(held) => false,
            "speakers" if speakers_declined(held) && !fetching_transcript(held) => true,
            _ => recorded(held, k),
        };
        // A session already imported at exactly these revisions is here in
        // full; nothing about it needs to travel again. The sealed hashes it
        // is listed under now are the ones on record from here on.
        if let Some(held) = imported.as_ref().filter(|held| INPUTS.iter().all(|(k, _)| same(held, k))) {
            let mut resealed = held.clone();
            for (k, _) in INPUTS {
                if k == "speakers" && speakers_declined(held) {
                    if let Some(hash) = session.inputs.get(k) {
                        resealed.insert(import::REJECTED_SPEAKERS.to_string(), hash.clone());
                    }
                    continue;
                }
                if k == "transcript" && declined(held) {
                    if let Some(hash) = session.inputs.get(k) {
                        resealed.insert(import::REJECTED_TRANSCRIPT.to_string(), hash.clone());
                    }
                }
                match session.inputs.get(k) {
                    Some(hash) => resealed.insert(k.to_string(), hash.clone()),
                    None => resealed.remove(k),
                };
            }
            if &resealed != held {
                let _ = inbox.record_import(device_id, &local, &resealed);
            }
            offers.extend(artifact_offers(inbox, sessions_dir, &minted, device_id, &session.id, &session.artifacts, key));
            continue;
        }
        // What of the session this desktop holds already stays where it is:
        // the inputs at their recorded plaintext and the audio on record.
        // The manifest always travels; the import reads the session from it.
        let imported_here = imported.is_some();
        let held = imported.unwrap_or_default();
        let mut claim = crate::sync::inbox::Claim {
            inputs: session.inputs.clone(),
            content: session.content.clone(),
            kept: INPUTS
                .iter()
                .filter(|(k, _)| *k != "manifest" && session.inputs.contains_key(*k))
                .filter(|(k, _)| match *k {
                    "transcript" => {
                        declined(&held) || session.content.get(*k).is_some_and(|c| held.get("content.transcript") == Some(c))
                    }
                    // Live captions are not asked for beside a phone
                    // transcript, or once this desktop has one.
                    "live_transcript" => session.inputs.contains_key("transcript") || held.contains_key("transcript"),
                    // A speaker table travels with a transcript fetched to
                    // replace one this desktop kept as its own.
                    "speakers" => {
                        let replacing = fetching_transcript(&held)
                            && (held.contains_key(import::REJECTED_TRANSCRIPT) || speakers_declined(&held));
                        !replacing
                            && (speakers_declined(&held)
                                || session.content.get(*k).is_some_and(|c| held.get("content.speakers") == Some(c)))
                    }
                    _ => session.content.get(*k).is_some_and(|c| held.get(&format!("content.{k}")) == Some(c)),
                })
                .map(|(k, _)| k.to_string())
                .collect(),
            held_audio: session.audio.clone().filter(|a| held.get("audio") == Some(a)),
            live_only: false,
        };
        claim.live_only = imported_here
            && claim.held_audio.is_some()
            && session.inputs.contains_key("live_transcript")
            && !claim.kept.iter().any(|k| k == "live_transcript")
            && INPUTS.iter().filter(|(k, _)| *k != "live_transcript").all(|(k, _)| same(&held, k));
        // The claim also says which inputs the import waits for.
        let _ = if session.inputs.is_empty() {
            inbox.forget_claim(device_id, &session.id)
        } else {
            inbox.record_claim(device_id, &session.id, &claim)
        };
        for (key, file) in INPUTS {
            let Some(hash) = session.inputs.get(key) else { continue };
            if claim.kept.iter().any(|k| k == key) {
                continue;
            }
            let staged = match key {
                "live_transcript" => inbox.plaintext(device_id, &session.id, file).as_deref() == Some(hash.as_str()),
                _ => inbox.has(device_id, &session.id, file, hash),
            };
            if !staged {
                wants.push(Want { session: session.id.clone(), file: file.to_string() });
            }
        }
        // Chunk audio is named by the manifest, so it can only be asked for
        // once the manifest itself has arrived and this desktop has opened it.
        if let Ok(paths) = import::missing(inbox, device_id, &session.id, envelope_key) {
            wants.extend(paths.into_iter().map(|file| Want { session: session.id.clone(), file }));
        }
    }

    let (profile_wants, profile_offers) = match (&hello.profile, sessions_dir.parent()) {
        (Some(phone), Some(root)) => crate::sync::records::profile_exchange(inbox, root, device_id, phone, voices, key),
        _ => (Vec::new(), Vec::new()),
    };

    // Every staged metadata file is merged first.
    let (mut meta_wants, mut meta_offers) = (Vec::new(), Vec::new());
    crate::sync::records::merge_staged_meta(inbox, sessions_dir, device_id);
    for session in &hello.sessions {
        let (want, offer) = crate::sync::records::meta_exchange(
            inbox,
            sessions_dir,
            &minted,
            device_id,
            &session.id,
            session.meta.as_deref(),
            key,
        );
        meta_wants.extend(want);
        meta_offers.extend(offer);
    }

    Ok(HelloReply {
        v: PROTOCOL_V,
        wants,
        offers,
        tombstones,
        profile_wants,
        profile_offers,
        meta_wants,
        meta_offers,
        buried,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::inbox::Inbox;
    use crate::sync::signing::body_hash;
    use base64::engine::general_purpose::STANDARD as B64;
    
    #[test]
    fn live_speaker_overlays_never_enter_the_exchange() {
        for f in crate::live_names::overlay::FILES {
            assert!(!ARTIFACTS.contains(&f), "{f} offered as an artifact");
        }
        let needle = ["live", "speakers"].join("_");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("sync");
        for entry in std::fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            if p.extension().is_some_and(|e| e == "rs") {
                let src = std::fs::read_to_string(&p).unwrap();
                assert!(!src.contains(&needle), "{} names the live overlay", p.display());
            }
        }
    }

    const PHONE: &str = "4a68f72f5a22110b2bfbe71b6588833c";

    fn phone_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[11u8; 32])
    }

    fn desktop_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[31u8; 32])
    }

    fn phone_pub() -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(phone_key().verifying_key().to_bytes())
    }

    fn hello(sessions: serde_json::Value) -> Vec<u8> {
        hello_v(PROTOCOL_V, sessions)
    }

    fn hello_v(v: u32, sessions: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "v": v, "device_id": PHONE, "sessions": sessions, "tombstones": [],
        }))
        .unwrap()
    }

    fn hello_profile(profile: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "v": PROTOCOL_V, "device_id": PHONE, "sessions": [], "tombstones": [], "profile": profile,
        }))
        .unwrap()
    }

    fn header_of(offer: &Offer) -> serde_json::Value {
        serde_json::from_slice(&B64.decode(&offer.header_b64).unwrap()).unwrap()
    }

    #[test]
    fn a_phone_that_carries_no_profile_is_asked_for_nothing_and_offered_nothing() {
        let (inbox, _td) = inbox();
        std::fs::write(_td.path().join("tags.json"), br#"{"schema_version":1,"tags":[]}"#).unwrap();
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &hello(serde_json::json!([])), None).unwrap();
        assert!(reply.profile_wants.is_empty() && reply.profile_offers.is_empty());
    }

    #[test]
    fn a_phones_profile_file_this_desktop_has_not_merged_is_wanted() {
        let (inbox, _td) = inbox();
        let body = hello_profile(serde_json::json!({ "tags": "sha256:aa", "contacts": "sha256:bb" }));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert_eq!(reply.profile_wants, ["tags.json", "contacts.json"]);
        assert!(reply.profile_offers.is_empty());
    }

    #[test]
    fn a_staged_profile_file_is_not_wanted_again_while_it_waits_to_merge() {
        let (inbox, _td) = inbox();
        let theirs = br#"{"schema_version":1,"tags":[]}"#;
        inbox.stage_record(PHONE, "profile", "tags.json", &body_hash(theirs), theirs).unwrap();
        let body = hello_profile(serde_json::json!({ "tags": body_hash(theirs) }));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(reply.profile_wants.is_empty() && reply.profile_offers.is_empty());
    }

    #[test]
    fn once_merged_the_desktops_file_is_offered_with_the_ids_the_phone_rewrites() {
        let (inbox, _td) = inbox();
        let mine = br#"{"schema_version":1,"tags":[{"id":"d1","name":"PHSA"}]}"#;
        std::fs::write(_td.path().join("tags.json"), mine).unwrap();
        let ids = std::collections::BTreeMap::from([("p1".to_string(), "d1".to_string())]);
        inbox.record_merge(PHONE, "profile", "tags.json", "sha256:phone", &ids).unwrap();
        let body = hello_profile(serde_json::json!({ "tags": "sha256:phone" }));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(reply.profile_wants.is_empty());
        assert_eq!(reply.profile_offers.len(), 1);
        let offer = &reply.profile_offers[0];
        assert_eq!(offer.artifact_id, body_hash(mine));
        assert_eq!((offer.session.as_str(), offer.name.as_str()), ("profile", "tags.json"));
        let header = header_of(offer);
        assert_eq!(header["merged_from"], "sha256:phone");
        assert_eq!(header["ids"]["p1"], "d1");
        assert_eq!(artifact_bytes(&inbox, &sessions_dir(&_td), None, &offer.artifact_id).unwrap(), mine);

        // The phone installed it: its hash is this desktop's, so nothing
        // moves either way.
        let body = hello_profile(serde_json::json!({ "tags": body_hash(mine) }));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(reply.profile_wants.is_empty() && reply.profile_offers.is_empty());
    }

    #[test]
    fn a_phone_with_no_catalog_yet_is_offered_this_desktops() {
        let (inbox, _td) = inbox();
        let mine = br#"{"schema_version":1,"contacts":[]}"#;
        std::fs::write(_td.path().join("contacts.json"), mine).unwrap();
        let body = hello_profile(serde_json::json!({}));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert_eq!(reply.profile_offers.len(), 1);
        assert_eq!(reply.profile_offers[0].name, "contacts.json");
        assert!(header_of(&reply.profile_offers[0])["merged_from"].is_null());
    }

    /// A summary in the shape that travels; anything else is not offered.
    fn v2_summary() -> Vec<u8> {
        summarize::SessionSummary::hand_written("daisy-1", "test", "# md", 1).encode()
    }

    fn inbox() -> (Inbox, tempfile::TempDir) {
        let td = tempfile::tempdir().unwrap();
        (Inbox::at(td.path().join("inbox")), td)
    }

    fn sessions_dir(td: &tempfile::TempDir) -> std::path::PathBuf {
        td.path().join("sessions")
    }

    #[test]
    fn a_session_the_desktop_has_never_seen_is_wanted_whole() {
        let (inbox, _td) = inbox();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [],
            "inputs": { "manifest": "sha256:aa", "transcript": "sha256:bb", "speakers": "sha256:cc" },
        }]));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        let files: Vec<&str> = reply.wants.iter().map(|w| w.file.as_str()).collect();
        assert_eq!(files, ["manifest.json", "transcript.json", "speakers.json"]);
        assert!(reply.wants.iter().all(|w| w.session == "daisy-1"));
    }

    #[test]
    fn the_phones_notes_are_the_fourth_input_wanted() {
        let (inbox, _td) = inbox();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [],
            "inputs": { "manifest": "sha256:aa", "transcript": "sha256:bb", "speakers": "sha256:cc", "notes": "sha256:dd" },
        }]));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        let files: Vec<&str> = reply.wants.iter().map(|w| w.file.as_str()).collect();
        assert_eq!(files, ["manifest.json", "transcript.json", "speakers.json", "notes.json"]);
    }

    #[test]
    fn a_note_typed_after_import_brings_the_session_over_again() {
        let (inbox, _td) = inbox();
        let inputs = std::collections::BTreeMap::from([
            ("manifest".to_string(), "sha256:aa".to_string()),
            ("transcript".to_string(), "sha256:bb".to_string()),
        ]);
        inbox.record_import(PHONE, "daisy-1", &inputs).unwrap();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [],
            "inputs": { "manifest": "sha256:aa", "transcript": "sha256:bb", "notes": "sha256:dd" },
        }]));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        let files: Vec<&str> = reply.wants.iter().map(|w| w.file.as_str()).collect();
        assert_eq!(files, ["manifest.json", "transcript.json", "notes.json"], "a note edit re-imports the session");
    }

    #[test]
    fn an_input_already_on_disk_is_not_wanted_again() {
        let (inbox, _td) = inbox();
        let bytes = b"manifest bytes";
        inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(bytes), bytes).unwrap();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [],
            "inputs": { "manifest": body_hash(bytes), "transcript": "sha256:bb" },
        }]));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        let files: Vec<&str> = reply.wants.iter().map(|w| w.file.as_str()).collect();
        assert_eq!(files, ["transcript.json"], "an input the phone omits is not wanted either");
    }

    #[test]
    fn a_revision_that_moved_on_is_wanted_again() {
        let (inbox, _td) = inbox();
        let old = b"revision one";
        inbox.store(PHONE, "daisy-1", "transcript.json", &body_hash(old), old).unwrap();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [], "inputs": { "transcript": body_hash(b"revision two") },
        }]));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert_eq!(reply.wants.len(), 1);
        assert_eq!(reply.wants[0].file, "transcript.json");
    }

    #[test]
    fn the_chunks_a_manifest_lists_are_wanted_once_the_manifest_is_here() {
        let (inbox, _td) = inbox();
        let manifest = serde_json::to_vec(&serde_json::json!({
            "chunks": [
                { "mic_wav_relative": "chunks/0001/mic.m4a" },
                { "mic_wav_relative": "chunks/0002/mic.m4a" },
            ]
        }))
        .unwrap();
        inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        let audio = b"aac bytes";
        inbox.store(PHONE, "daisy-1", "chunks/0001/mic.m4a", &body_hash(audio), audio).unwrap();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [], "inputs": { "manifest": body_hash(&manifest) },
        }]));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        let files: Vec<&str> = reply.wants.iter().map(|w| w.file.as_str()).collect();
        assert_eq!(files, ["chunks/0002/mic.m4a"], "only the chunk that is missing");
    }

    #[test]
    fn a_chunk_path_that_could_escape_the_session_is_never_wanted() {
        let (inbox, _td) = inbox();
        let manifest = serde_json::to_vec(&serde_json::json!({
            "chunks": [{ "mic_wav_relative": "../../escape.m4a" }]
        }))
        .unwrap();
        inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [], "inputs": { "manifest": body_hash(&manifest) },
        }]));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(reply.wants.is_empty());
    }

    /// This desktop's envelope key.
    fn key() -> MachineKey {
        MachineKey { x25519_secret: [5u8; 32], device_id: "mac-1".into() }
    }

    fn sealed_manifest() -> (Vec<u8>, MachineKey) {
        let secret = [5u8; 32];
        let manifest = serde_json::to_vec(&serde_json::json!({
            "chunks": [{ "mic_wav_relative": "chunks/0001/mic.m4a" }]
        }))
        .unwrap();
        let sealed = import::seal_to(&secret, "mac-1", &manifest);
        (sealed, MachineKey { x25519_secret: secret, device_id: "mac-1".into() })
    }

    #[test]
    fn the_chunks_of_a_manifest_the_phone_sealed_are_wanted_once_this_desktop_opens_it() {
        let (inbox, _td) = inbox();
        let (sealed, key) = sealed_manifest();
        inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(&sealed), &sealed).unwrap();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [], "inputs": { "manifest": body_hash(&sealed) },
        }]));
        let reply =
            hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key, &body, None).unwrap();
        let files: Vec<&str> = reply.wants.iter().map(|w| w.file.as_str()).collect();
        assert_eq!(files, ["chunks/0001/mic.m4a"]);
    }

    #[test]
    fn a_manifest_sealed_to_another_desktop_names_no_chunks() {
        let (inbox, _td) = inbox();
        let (sealed, _) = sealed_manifest();
        inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(&sealed), &sealed).unwrap();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [], "inputs": { "manifest": body_hash(&sealed) },
        }]));
        let other = MachineKey { x25519_secret: [6u8; 32], device_id: "mac-2".into() };
        let reply =
            hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &other, &body, None).unwrap();
        assert!(reply.wants.is_empty(), "the import refuses it; asking for its audio would bring bytes nobody can open");
    }

    #[test]
    fn a_session_refused_at_those_revisions_is_wanted_no_further() {
        let (inbox, _td) = inbox();
        let refusal = crate::sync::inbox::Refusal {
            device_id: PHONE.into(),
            reason: crate::sync::inbox::RefusalReason::Sealed,
            inputs: std::collections::BTreeMap::from([
                ("manifest".to_string(), "sha256:aa".to_string()),
                ("transcript".to_string(), "sha256:bb".to_string()),
            ]),
        };
        inbox.record_refusal("daisy-1", &refusal).unwrap();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [],
            "inputs": { "manifest": "sha256:aa", "transcript": "sha256:bb", "speakers": "sha256:cc" },
        }]));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(reply.wants.is_empty(), "the same bytes would be refused the same way");
        assert!(reply.offers.is_empty(), "nothing was built from it");
    }

    #[test]
    fn a_refused_session_the_phone_re_sealed_is_wanted_again() {
        let (inbox, _td) = inbox();
        let refusal = crate::sync::inbox::Refusal {
            device_id: PHONE.into(),
            reason: crate::sync::inbox::RefusalReason::Sealed,
            inputs: std::collections::BTreeMap::from([
                ("manifest".to_string(), "sha256:aa".to_string()),
                ("transcript".to_string(), "sha256:bb".to_string()),
            ]),
        };
        inbox.record_refusal("daisy-1", &refusal).unwrap();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [],
            "inputs": { "manifest": "sha256:a2", "transcript": "sha256:b2" },
        }]));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        let files: Vec<&str> = reply.wants.iter().map(|w| w.file.as_str()).collect();
        assert_eq!(files, ["manifest.json", "transcript.json"]);
    }

    #[test]
    fn a_session_already_imported_at_those_revisions_is_wanted_no_further() {
        let (inbox, _td) = inbox();
        let inputs = std::collections::BTreeMap::from([
            ("manifest".to_string(), "sha256:aa".to_string()),
            ("transcript".to_string(), "sha256:bb".to_string()),
        ]);
        inbox.record_import(PHONE, "daisy-1", &inputs).unwrap();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [],
            "inputs": { "manifest": "sha256:aa", "transcript": "sha256:bb" },
        }]));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(reply.wants.is_empty(), "the desktop has this session, at these revisions");
    }

    #[test]
    fn a_revision_that_moved_on_after_import_is_wanted_again() {
        let (inbox, _td) = inbox();
        let inputs = std::collections::BTreeMap::from([("transcript".to_string(), "sha256:bb".to_string())]);
        inbox.record_import(PHONE, "daisy-1", &inputs).unwrap();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [], "inputs": { "transcript": "sha256:cc" },
        }]));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        let files: Vec<&str> = reply.wants.iter().map(|w| w.file.as_str()).collect();
        assert_eq!(files, ["transcript.json"], "a speaker rename on the phone comes back over");
    }

    #[test]
    fn the_artifacts_a_session_has_are_offered_with_a_signed_header() {
        let (inbox, td) = inbox();
        let sessions = td.path().join("sessions");
        let dir = sessions.join("daisy-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("summary.json"), v2_summary()).unwrap();
        let inputs = std::collections::BTreeMap::from([
            ("manifest".to_string(), "sha256:aa".to_string()),
            ("notes".to_string(), "sha256:dd".to_string()),
        ]);
        inbox.record_import(PHONE, "daisy-1", &inputs).unwrap();

        let key = ed25519_dalek::SigningKey::from_bytes(&[31u8; 32]);
        let offers = artifact_offers(&inbox, &sessions, &import::minted_ids(&sessions), PHONE, "daisy-1", &[], &key);
        assert_eq!(offers.len(), 1);
        let offer = &offers[0];
        assert_eq!(offer.name, "summary.json");
        assert_eq!(offer.session, "daisy-1");
        assert_eq!(offer.artifact_id, body_hash(&v2_summary()));
        assert_eq!(offer.size, v2_summary().len() as u64);

        // The header the phone verifies: signed bytes, carrying the revisions
        // the artifact was built from.
        let header_bytes = B64.decode(&offer.header_b64).unwrap();
        let sig = B64.decode(&offer.header_sig_b64).unwrap();
        ed25519_dalek::Verifier::verify(
            &key.verifying_key(),
            &header_bytes,
            &ed25519_dalek::Signature::from_bytes(&sig.try_into().unwrap()),
        )
        .expect("signed by this desktop");
        let header: serde_json::Value = serde_json::from_slice(&header_bytes).unwrap();
        assert_eq!(header["v"], PROTOCOL_V);
        assert_eq!(header["artifact_id"], offer.artifact_id);
        assert_eq!(header["built_from"]["manifest"], "sha256:aa");
        assert!(header["built_from"].get("notes").is_none(), "a note edit must not make a summary stale");
    }

    #[test]
    fn notes_typed_here_are_offered_and_never_stale() {
        let (inbox, td) = inbox();
        let sessions = td.path().join("sessions");
        let dir = sessions.join("daisy-1");
        std::fs::create_dir_all(&dir).unwrap();
        let notes = "# Follow-ups\n\nAsk the surveyor about the east lot.\n".as_bytes();
        std::fs::write(dir.join("notes.md"), notes).unwrap();
        let inputs = std::collections::BTreeMap::from([("manifest".to_string(), "sha256:aa".to_string())]);
        inbox.record_import(PHONE, "daisy-1", &inputs).unwrap();

        let key = ed25519_dalek::SigningKey::from_bytes(&[31u8; 32]);
        let offers = artifact_offers(&inbox, &sessions, &import::minted_ids(&sessions), PHONE, "daisy-1", &[], &key);
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].name, "notes.md");
        assert_eq!(offers[0].artifact_id, body_hash(notes));
        let header: serde_json::Value = serde_json::from_slice(&B64.decode(&offers[0].header_b64).unwrap()).unwrap();
        assert_eq!(header["name"], "notes.md");
        assert_eq!(header["built_from"], serde_json::json!({}), "no edit on the phone makes notes stale");
        assert_eq!(artifact_bytes(&inbox, &sessions, None, &offers[0].artifact_id).as_deref(), Some(notes));

        let held = [offers[0].artifact_id.clone()];
        assert!(artifact_offers(&inbox, &sessions, &import::minted_ids(&sessions), PHONE, "daisy-1", &held, &key).is_empty());
    }

    #[test]
    fn cleared_notes_travel_and_notes_that_are_not_text_or_too_long_do_not() {
        let (inbox, td) = inbox();
        let sessions = td.path().join("sessions");
        let dir = sessions.join("daisy-1");
        std::fs::create_dir_all(&dir).unwrap();
        inbox.record_import(PHONE, "daisy-1", &Default::default()).unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[31u8; 32]);
        let offered = |bytes: &[u8]| {
            std::fs::write(dir.join("notes.md"), bytes).unwrap();
            forget_artifact(&dir.join("notes.md"));
            artifact_offers(&inbox, &sessions, &import::minted_ids(&sessions), PHONE, "daisy-1", &[], &key).len()
        };
        assert_eq!(offered(b""), 1, "clearing the notes reaches the phone as an empty file");
        assert_eq!(offered(&[0xff, 0xfe, 0x00]), 0);
        assert_eq!(offered(&vec![b'a'; NOTES_MAX_BYTES + 1]), 0);
        assert_eq!(offered(&vec![b'a'; NOTES_MAX_BYTES]), 1);
    }

    #[test]
    fn a_rewritten_artifact_is_offered_under_its_new_hash() {
        let (inbox, td) = inbox();
        let sessions = td.path().join("sessions");
        let dir = sessions.join("daisy-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("summary.json"), v2_summary()).unwrap();
        inbox.record_import(PHONE, "daisy-1", &Default::default()).unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[31u8; 32]);
        let minted = import::minted_ids(&sessions);
        let first = artifact_offers(&inbox, &sessions, &minted, PHONE, "daisy-1", &[], &key);
        assert_eq!(first[0].artifact_id, body_hash(&v2_summary()));

        let rebuilt = summarize::SessionSummary::hand_written("daisy-1", "test", "# md again", 2).encode();
        std::fs::write(dir.join("summary.json"), &rebuilt).unwrap();
        let second = artifact_offers(&inbox, &sessions, &minted, PHONE, "daisy-1", &[], &key);
        assert_eq!(second[0].artifact_id, body_hash(&rebuilt), "a rebuild is not served under the old hash");
        assert_eq!(second[0].size, rebuilt.len() as u64);
        assert_eq!(
            artifact_bytes(&inbox, &sessions, None, &second[0].artifact_id).unwrap(),
            rebuilt,
            "the bytes behind the new hash are the rebuilt ones"
        );
        assert!(
            artifact_bytes(&inbox, &sessions, None, &first[0].artifact_id).is_none(),
            "the hash of the replaced artifact answers with nothing"
        );

        let same_len = summarize::SessionSummary::hand_written("daisy-1", "test", "# md AGAIN", 2).encode();
        assert_eq!(same_len.len(), rebuilt.len(), "this case is only worth testing at one length");
        assert_ne!(same_len, rebuilt);
        std::fs::write(dir.join("summary.json"), &same_len).unwrap();
        let third = artifact_offers(&inbox, &sessions, &minted, PHONE, "daisy-1", &[], &key);
        assert_eq!(third[0].artifact_id, body_hash(&same_len), "an edit of the same length is still a new artifact");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join("summary.json");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
            let cached = artifact_offers(&inbox, &sessions, &minted, PHONE, "daisy-1", &[], &key);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(cached[0].artifact_id, body_hash(&same_len), "the hello answered without reading the file");
        }
    }

    #[test]
    fn a_hash_that_no_longer_matches_the_file_is_forgotten_not_kept() {
        let (inbox, td) = inbox();
        let sessions = td.path().join("sessions");
        let dir = sessions.join("daisy-1");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("summary.json");
        std::fs::write(&path, v2_summary()).unwrap();
        let settled = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let set_mtime = |t: std::time::SystemTime| {
            std::fs::File::options().write(true).open(&path).unwrap().set_modified(t).unwrap();
        };
        set_mtime(settled);
        inbox.record_import(PHONE, "daisy-1", &Default::default()).unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[31u8; 32]);
        let minted = import::minted_ids(&sessions);
        let stale = artifact_offers(&inbox, &sessions, &minted, PHONE, "daisy-1", &[], &key)[0]
            .artifact_id
            .clone();

        // Two writes inside one timestamp tick at one length: the file moves
        // under a digest that still reads as current.
        let rebuilt = summarize::SessionSummary::hand_written("daisy-1", "test", "# ml", 1).encode();
        assert_eq!(rebuilt.len(), v2_summary().len());
        std::fs::write(&path, &rebuilt).unwrap();
        set_mtime(settled);
        assert_eq!(
            artifact_offers(&inbox, &sessions, &minted, PHONE, "daisy-1", &[], &key)[0].artifact_id,
            stale,
            "this is the collision the cache cannot see"
        );

        // Asking for it proves the digest wrong, and it is dropped.
        assert!(artifact_bytes(&inbox, &sessions, None, &stale).is_none());
        assert_eq!(
            artifact_offers(&inbox, &sessions, &minted, PHONE, "daisy-1", &[], &key)[0].artifact_id,
            body_hash(&rebuilt),
            "the next hello offers what the file actually holds"
        );
    }

    #[test]
    fn an_artifact_the_phone_already_holds_is_not_offered_again() {
        let (inbox, td) = inbox();
        let sessions = td.path().join("sessions");
        std::fs::create_dir_all(sessions.join("daisy-1")).unwrap();
        std::fs::write(sessions.join("daisy-1").join("summary.json"), v2_summary()).unwrap();
        inbox.record_import(PHONE, "daisy-1", &Default::default()).unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[31u8; 32]);
        let held = [body_hash(&v2_summary())];
        assert!(artifact_offers(&inbox, &sessions, &import::minted_ids(&sessions), PHONE, "daisy-1", &held, &key).is_empty());
    }

    #[test]
    fn a_session_with_nothing_built_yet_offers_nothing() {
        let (inbox, td) = inbox();
        let sessions = td.path().join("sessions");
        std::fs::create_dir_all(sessions.join("daisy-1")).unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[31u8; 32]);
        assert!(artifact_offers(&inbox, &sessions, &import::minted_ids(&sessions), PHONE, "daisy-1", &[], &key).is_empty());
    }

    #[test]
    fn a_tombstoned_session_is_offered_its_tombstone_and_wanted_no_further() {
        let (inbox, _td) = inbox();
        let stone = tombstone_record("daisy-1", 3, PHONE, &phone_key());
        inbox.entomb("daisy-1", Some(PHONE), &serde_json::to_vec(&stone).unwrap()).unwrap();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [], "inputs": { "manifest": "sha256:aa" },
        }]));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(reply.wants.is_empty(), "nothing is wanted for a session that is gone");
        assert_eq!(reply.tombstones.len(), 1);
        assert_eq!(reply.tombstones[0].session, "daisy-1");
    }

    #[test]
    fn a_session_the_phone_no_longer_lists_is_not_offered_its_tombstone() {
        let (inbox, _td) = inbox();
        let stone = tombstone_record("daisy-1", 3, PHONE, &phone_key());
        inbox.entomb("daisy-1", Some(PHONE), &serde_json::to_vec(&stone).unwrap()).unwrap();
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &hello(serde_json::json!([])), None).unwrap();
        assert!(reply.tombstones.is_empty(), "the phone has acknowledged it by dropping it");
    }

    #[test]
    fn deleting_a_phone_session_here_writes_the_tombstone_the_phone_collects() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        std::fs::create_dir_all(sessions.join("daisy-1")).unwrap();
        inbox.record_import(PHONE, "daisy-1", &Default::default()).unwrap();
        let desktop = desktop_key();
        let mac_id = "f7c026bbc3b6dcf0b22d8755722436e1";

        assert!(entomb_here(&inbox, "daisy-1", "daisy-1", PHONE, mac_id, &desktop));
        // The phone still lists the session, so the tombstone travels.
        let body = hello(serde_json::json!([{ "id": "daisy-1", "artifacts": [], "inputs": {} }]));
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop, &key(), &body, None).unwrap();
        assert_eq!(reply.tombstones.len(), 1);
        let stone = &reply.tombstones[0];
        assert_eq!(stone.origin, mac_id, "this desktop originated it");
        assert!(tombstone_is_valid(stone, &B64.encode(desktop.verifying_key().to_bytes())));

        // Each delete gets its own counter.
        std::fs::create_dir_all(sessions.join("daisy-2")).unwrap();
        assert!(entomb_here(&inbox, "daisy-2", "daisy-2", PHONE, mac_id, &desktop));
        let body = hello(serde_json::json!([{ "id": "daisy-2", "artifacts": [], "inputs": {} }]));
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop, &key(), &body, None).unwrap();
        assert_eq!(reply.tombstones[0].seq, stone.seq + 1);
    }

    #[test]
    fn a_tombstone_the_phone_sends_deletes_the_session_here() {
        let (inbox, _td) = inbox();
        let bytes = b"manifest bytes";
        inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(bytes), bytes).unwrap();
        let stone = tombstone_record("daisy-1", 1, PHONE, &phone_key());
        let body = serde_json::to_vec(&serde_json::json!({
            "v": PROTOCOL_V, "device_id": PHONE, "sessions": [], "tombstones": [stone],
        }))
        .unwrap();
        hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(inbox.is_entombed("daisy-1", PHONE));
        assert!(inbox.read(PHONE, "daisy-1", "manifest.json").is_none());
    }

    #[test]
    fn a_tombstone_signed_by_no_one_we_know_changes_nothing() {
        let (inbox, _td) = inbox();
        let bytes = b"manifest bytes";
        inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(bytes), bytes).unwrap();
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[12u8; 32]);
        let stone = tombstone_record("daisy-1", 1, PHONE, &stranger);
        let body = serde_json::to_vec(&serde_json::json!({
            "v": PROTOCOL_V, "device_id": PHONE, "sessions": [], "tombstones": [stone],
        }))
        .unwrap();
        hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(!inbox.is_entombed("daisy-1", PHONE));
        assert!(inbox.read(PHONE, "daisy-1", "manifest.json").is_some());
    }

    #[test]
    fn a_hello_offers_what_the_desktop_has_built() {
        let (inbox, td) = inbox();
        let sessions = td.path().join("sessions");
        std::fs::create_dir_all(sessions.join("daisy-1")).unwrap();
        std::fs::write(sessions.join("daisy-1").join("summary.json"), v2_summary()).unwrap();
        let inputs = std::collections::BTreeMap::from([("manifest".to_string(), "sha256:aa".to_string())]);
        inbox.record_import(PHONE, "daisy-1", &inputs).unwrap();
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [], "inputs": { "manifest": "sha256:aa" },
        }]));
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(reply.wants.is_empty());
        assert_eq!(reply.offers.len(), 1);
        assert_eq!(reply.offers[0].name, "summary.json");
    }

    #[test]
    fn a_tombstone_takes_the_imported_session_out_of_the_library() {
        let (inbox, td) = inbox();
        let sessions = td.path().join("sessions");
        imported_from(&sessions, "daisy-1", PHONE, "daisy-1");
        std::fs::write(sessions.join("daisy-1").join("summary.json"), v2_summary()).unwrap();
        inbox.record_import(PHONE, "daisy-1", &Default::default()).unwrap();

        let stone = tombstone_record("daisy-1", 2, PHONE, &phone_key());
        assert_eq!(accept_tombstone(&inbox, &sessions, &import::minted_ids(&sessions), PHONE, &stone, &phone_pub()), Tombstoned::Done);
        assert!(!sessions.join("daisy-1").exists(), "a delete on the phone is a delete here");
        assert!(inbox.imported(PHONE, "daisy-1").is_none(), "and the session is not held any more");
        assert!(inbox.is_entombed("daisy-1", PHONE));
    }

    #[test]
    fn a_tombstone_for_a_recording_that_cannot_be_read_waits_and_buries_nothing() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        std::fs::create_dir_all(sessions.join("daisy-1")).unwrap();
        std::fs::write(sessions.join("daisy-1").join("summary.json"), v2_summary()).unwrap();
        inbox.record_import(PHONE, "daisy-1", &Default::default()).unwrap();

        let stone = tombstone_record("daisy-1", 2, PHONE, &phone_key());
        assert_eq!(accept_tombstone(&inbox, &sessions, &import::minted_ids(&sessions), PHONE, &stone, &phone_pub()), Tombstoned::Retry);
        assert!(
            sessions.join("daisy-1").is_dir(),
            "a recording that does not say whose it is stays until it can be read"
        );
        assert!(inbox.imported(PHONE, "daisy-1").is_some(), "and it is still held here");
        assert!(
            !inbox.is_entombed("daisy-1", PHONE),
            "an id buried now would answer the delete while the recording stayed here for good"
        );

        // The manifest reads again: the same tombstone finishes the delete.
        imported_from(&sessions, "daisy-1", PHONE, "daisy-1");
        assert_eq!(accept_tombstone(&inbox, &sessions, &import::minted_ids(&sessions), PHONE, &stone, &phone_pub()), Tombstoned::Done);
        assert!(!sessions.join("daisy-1").exists());
        assert!(inbox.is_entombed("daisy-1", PHONE));
    }

    #[test]
    fn a_session_the_tombstone_could_not_settle_is_still_taken_in_full() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        std::fs::create_dir_all(sessions.join("daisy-1")).unwrap();
        let stone = tombstone_record("daisy-1", 2, PHONE, &phone_key());
        assert_eq!(accept_tombstone(&inbox, &sessions, &import::minted_ids(&sessions), PHONE, &stone, &phone_pub()), Tombstoned::Retry);

        let bytes = b"manifest bytes";
        assert!(inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(bytes), bytes).is_ok());
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [], "inputs": { "manifest": "sha256:aa" },
        }]));
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert_eq!(reply.wants.len(), 1, "the session is still wanted here");
        assert!(reply.tombstones.is_empty());
    }

    #[test]
    fn a_tombstone_for_an_id_this_desktop_mints_is_refused() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let minted = format!("daisy-{}", crate::sync::import::MINTED_ID_FLOOR);
        let stone = tombstone_record(&minted, 2, PHONE, &phone_key());
        assert_eq!(accept_tombstone(&inbox, &sessions, &import::minted_ids(&sessions), PHONE, &stone, &phone_pub()), Tombstoned::Refused);
        assert!(!inbox.is_entombed(&minted, PHONE), "no device names a session by an id this desktop hands out");
    }

    /// The other device that minted the same `daisy-<unix>` second.
    const TABLET: &str = "0000000000000000000000000000ffff";

    fn hello_from(device: &str, sessions: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "v": PROTOCOL_V, "device_id": device, "sessions": sessions, "tombstones": [],
        }))
        .unwrap()
    }

    /// A session in the library that came from `origin`, known there by
    /// `wire`.
    fn imported_from(sessions: &std::path::Path, local: &str, origin: &str, wire: &str) {
        let dir = sessions.join(local);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = serde_json::json!({
            "schema_version": 4, "session_id": local, "origin_device": origin, "origin_session": wire,
        });
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
    }

    #[test]
    fn a_tombstone_takes_the_devices_own_recording_out_of_the_library() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        imported_from(&sessions, "daisy-1", PHONE, "daisy-1");
        inbox.record_import(PHONE, "daisy-1", &Default::default()).unwrap();

        let stone = tombstone_record("daisy-1", 2, PHONE, &phone_key());
        assert_eq!(accept_tombstone(&inbox, &sessions, &import::minted_ids(&sessions), PHONE, &stone, &phone_pub()), Tombstoned::Done);
        assert!(!sessions.join("daisy-1").exists(), "a delete on the phone is a delete here");
        assert!(inbox.imported(PHONE, "daisy-1").is_none());
    }

    #[test]
    fn a_tombstone_leaves_another_devices_recording_of_that_id_alone() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        imported_from(&sessions, "daisy-1", TABLET, "daisy-1");
        inbox.record_import(PHONE, "daisy-1", &Default::default()).unwrap();

        let stone = tombstone_record("daisy-1", 2, PHONE, &phone_key());
        assert_eq!(accept_tombstone(&inbox, &sessions, &import::minted_ids(&sessions), PHONE, &stone, &phone_pub()), Tombstoned::Done);
        assert!(sessions.join("daisy-1").is_dir(), "the tablet's recording is not the phone's to delete");
        assert!(inbox.imported(PHONE, "daisy-1").is_some(), "and it is still held here");
    }

    #[test]
    fn a_tombstone_is_offered_only_to_the_device_whose_recording_it_buried() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let stone = tombstone_record("daisy-1", 2, PHONE, &phone_key());
        assert_eq!(accept_tombstone(&inbox, &sessions, &import::minted_ids(&sessions), PHONE, &stone, &phone_pub()), Tombstoned::Done);

        let listed = serde_json::json!([{ "id": "daisy-1", "artifacts": [], "inputs": { "manifest": "sha256:aa" } }]);
        let mine = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &hello(listed.clone()), None).unwrap();
        assert_eq!(mine.tombstones.len(), 1, "the phone that deleted it is told it is gone");

        let body = hello_from(TABLET, listed);
        let theirs = hello_reply(&inbox, &sessions, TABLET, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(theirs.tombstones.is_empty(), "the tablet's own daisy-1 is not the one that was deleted");
        let files: Vec<&str> = theirs.wants.iter().map(|w| w.file.as_str()).collect();
        assert_eq!(files, ["manifest.json"], "and it is asked for as any other session is");
    }

    #[test]
    fn a_tombstone_with_no_device_on_record_is_offered_to_every_device() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let stone = tombstone_record("daisy-1", 2, PHONE, &phone_key());
        inbox.entomb("daisy-1", None, &serde_json::to_vec(&stone).unwrap()).unwrap();

        let listed = serde_json::json!([{ "id": "daisy-1", "artifacts": [], "inputs": { "manifest": "sha256:aa" } }]);
        for device in [PHONE, TABLET] {
            let body = hello_from(device, listed.clone());
            let reply = hello_reply(&inbox, &sessions, device, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
            assert_eq!(reply.tombstones.len(), 1, "{device} is owed a tombstone with no device on record");
            assert!(reply.wants.is_empty());
        }
    }

    #[test]
    fn deleting_a_session_this_desktop_minted_an_id_for_buries_the_id_its_device_knows() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let minted = format!("daisy-{}", crate::sync::import::MINTED_ID_FLOOR);
        imported_from(&sessions, &minted, TABLET, "daisy-1");
        inbox.record_import(TABLET, &minted, &Default::default()).unwrap();
        let desktop = desktop_key();

        assert!(entomb_here(&inbox, "daisy-1", &minted, TABLET, "f7c026bbc3b6dcf0b22d8755722436e1", &desktop));
        assert!(inbox.imported(TABLET, &minted).is_none(), "the session is not held here any more");

        let body = hello_from(TABLET, serde_json::json!([{ "id": "daisy-1", "artifacts": [], "inputs": {} }]));
        let reply = hello_reply(&inbox, &sessions, TABLET, &phone_pub(), &desktop, &key(), &body, None).unwrap();
        assert_eq!(reply.tombstones.len(), 1);
        assert_eq!(reply.tombstones[0].session, "daisy-1", "it travels under the id the tablet knows");

        let body = hello_from(PHONE, serde_json::json!([{ "id": "daisy-1", "artifacts": [], "inputs": {} }]));
        let other = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop, &key(), &body, None).unwrap();
        assert!(other.tombstones.is_empty(), "the phone's own daisy-1 is untouched");
    }

    #[test]
    fn a_session_this_desktop_minted_an_id_for_travels_under_the_id_its_device_knows() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let minted = format!("daisy-{}", crate::sync::import::MINTED_ID_FLOOR);
        imported_from(&sessions, &minted, TABLET, "daisy-1");
        std::fs::write(sessions.join(&minted).join("summary.json"), v2_summary()).unwrap();
        let inputs = std::collections::BTreeMap::from([("manifest".to_string(), "sha256:aa".to_string())]);
        inbox.record_import(TABLET, &minted, &inputs).unwrap();

        let body = hello_from(TABLET, serde_json::json!([{
            "id": "daisy-1", "artifacts": [], "inputs": { "manifest": "sha256:aa" },
        }]));
        let reply = hello_reply(&inbox, &sessions, TABLET, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(reply.wants.is_empty(), "the tablet's session is here in full");
        assert_eq!(reply.offers.len(), 1);
        assert_eq!(reply.offers[0].session, "daisy-1", "the tablet knows it by its own id");
        assert_eq!(header_of(&reply.offers[0])["built_from"]["manifest"], "sha256:aa");
    }

    #[test]
    fn a_hello_that_names_another_device_is_refused() {
        let (inbox, _td) = inbox();
        let body = serde_json::to_vec(&serde_json::json!({
            "v": 1, "device_id": "0000000000000000000000000000ffff", "sessions": [], "tombstones": [],
        }))
        .unwrap();
        assert_eq!(
            hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None),
            Err(HelloError::Malformed)
        );
    }

    #[test]
    fn a_hello_that_is_not_a_hello_is_refused() {
        let (inbox, _td) = inbox();
        assert_eq!(
            hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), b"not json", None),
            Err(HelloError::Malformed)
        );
        let no_version = serde_json::to_vec(&serde_json::json!({
            "v": 0, "device_id": PHONE, "sessions": [], "tombstones": [],
        }))
        .unwrap();
        assert_eq!(
            hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &no_version, None),
            Err(HelloError::Malformed)
        );
    }

    /// Setup the version-skew tests share: one imported session with a v2
    /// summary on disk, at the revisions the hello names.
    fn imported_with_a_summary() -> (Inbox, tempfile::TempDir, std::path::PathBuf, serde_json::Value) {
        let (inbox, td) = inbox();
        let sessions = td.path().join("sessions");
        std::fs::create_dir_all(sessions.join("daisy-1")).unwrap();
        std::fs::write(sessions.join("daisy-1").join("summary.json"), v2_summary()).unwrap();
        let inputs = std::collections::BTreeMap::from([("manifest".to_string(), "sha256:aa".to_string())]);
        inbox.record_import(PHONE, "daisy-1", &inputs).unwrap();
        let named = serde_json::json!([{
            "id": "daisy-1", "artifacts": [], "inputs": { "manifest": "sha256:aa" },
        }]);
        (inbox, td, sessions, named)
    }

    #[test]
    fn a_hello_on_another_version_is_refused_and_names_the_older_side() {
        use crate::sync::gate::Outdated;
        for (v, older) in [(1, Outdated::Phone), (PROTOCOL_V - 1, Outdated::Phone), (PROTOCOL_V + 1, Outdated::Desktop)] {
            let (inbox, _td, sessions, named) = imported_with_a_summary();
            let body = hello_v(v, named);
            let err = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap_err();
            assert_eq!(err, HelloError::OtherVersion(older), "v{v}");
        }
    }

    #[test]
    fn a_hello_on_this_version_gets_the_summary_offer() {
        let (inbox, _td, sessions, named) = imported_with_a_summary();
        let body = hello_v(PROTOCOL_V, named);
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert_eq!(reply.v, PROTOCOL_V);
        assert_eq!(reply.offers.len(), 1);
        assert_eq!(reply.offers[0].name, "summary.json");
        let header: serde_json::Value =
            serde_json::from_slice(&B64.decode(&reply.offers[0].header_b64).unwrap()).unwrap();
        assert_eq!(header["v"], PROTOCOL_V);
    }

    #[test]
    fn a_legacy_summary_on_disk_is_not_offered() {
        let (inbox, _td, sessions, named) = imported_with_a_summary();
        std::fs::write(
            sessions.join("daisy-1").join("summary.json"),
            br#"{"schema_version":1,"session_id":"daisy-1","provider":"p","model":"m",
                "generated_at_unix_seconds":1,"source_inputs_hash":"h",
                "structured":{"tldr":"x","action_items":[],"decisions":[],"open_questions":[],"key_topics":[]},
                "markdown":"","user_edited":false}"#,
        )
        .unwrap();
        let body = hello_v(PROTOCOL_V, named);
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(reply.offers.is_empty(), "only the wire shape travels");
    }

    #[test]
    fn chapters_and_chunks_are_never_offered() {
        let (inbox, _td, sessions, named) = imported_with_a_summary();
        std::fs::write(sessions.join("daisy-1").join("chapters.json"), b"{}").unwrap();
        std::fs::write(sessions.join("daisy-1").join("chunks.json"), b"{}").unwrap();
        let body = hello_v(PROTOCOL_V, named);
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert_eq!(reply.offers.len(), 1);
        assert_eq!(reply.offers[0].name, "summary.json");
    }

    // What a session is under its envelopes (§5 `content` and `audio`).

    const WIRE: &str = "daisy-1700000000";
    const CHUNK: &str = "chunks/0001/mic.m4a";
    /// The same phone after its sync identity was reset.
    const RESET: &str = "0000000000000000000000000000eeee";

    fn seam(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/sync-seam/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    fn aac() -> Vec<u8> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../recording/tests/fixtures/burst-16k.m4a")).unwrap()
    }

    fn phone_manifest(title: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 4, "session_id": WIRE, "title": title,
            "created_at_unix_seconds": 1_700_000_000i64, "sample_rate": 16000, "channels": 1,
            "chunks": [{
                "index": 1, "started_at_unix_seconds": 1_700_000_000i64,
                "ended_at_unix_seconds": 1_700_000_010i64, "duration_seconds": 9.4,
                "mic_samples": 150_393, "mic_wav_relative": CHUNK,
            }],
        }))
        .unwrap()
    }

    fn phone_transcript() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "session_id": WIRE, "provider": "phone", "model": "on-device",
            "transcribed_at_unix_seconds": 1_700_000_012i64, "chunks": [],
        }))
        .unwrap()
    }

    /// Seals afresh: every call is a new envelope, and so a new `inputs`
    /// hash, over the same plaintext.
    fn seal(plain: &[u8]) -> Vec<u8> {
        static RESEALS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = RESEALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut sealed = crate::sync::import::seal_to(&[5u8; 32], "mac-1", plain);
        sealed.extend(std::iter::repeat_n(b' ', n + 1));
        sealed
    }

    /// One sync of a phone session sealed afresh, the way a phone does after
    /// every pairing: hellos until nothing is wanted, each wanted file
    /// uploaded, the import run once the session is ready. `audio` is what
    /// the hello lists; `with_content` adds `content`. Returns every file
    /// asked for.
    fn sync_session(
        inbox: &Inbox,
        sessions: &std::path::Path,
        device: &str,
        manifest: &[u8],
        with_content: bool,
        audio: Option<String>,
    ) -> Vec<String> {
        let transcript = phone_transcript();
        let files = [
            ("manifest.json", seal(manifest)),
            ("transcript.json", seal(&transcript)),
            (CHUNK, seal(&aac())),
        ];
        let mut entry = serde_json::json!({
            "id": WIRE, "artifacts": [],
            "inputs": { "manifest": body_hash(&files[0].1), "transcript": body_hash(&files[1].1) },
        });
        if with_content {
            entry["content"] = serde_json::json!({ "manifest": body_hash(manifest), "transcript": body_hash(&transcript) });
        }
        if let Some(audio) = audio {
            entry["audio"] = serde_json::json!(audio);
        }
        let body = hello_from(device, serde_json::json!([entry]));
        let mut asked = Vec::new();
        for _ in 0..3 {
            let reply = hello_reply(inbox, sessions, device, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
            if reply.wants.is_empty() {
                break;
            }
            for want in &reply.wants {
                let (_, bytes) = files.iter().find(|(name, _)| *name == want.file).unwrap();
                inbox.store(device, WIRE, &want.file, &body_hash(bytes), bytes).unwrap();
                asked.push(want.file.clone());
            }
            if import::is_ready(inbox, device, WIRE, &key()) {
                import::import_session(inbox, sessions, device, WIRE, &key()).unwrap();
            }
        }
        asked
    }

    fn the_audio() -> String {
        import::audio_hash([aac().as_slice()])
    }

    fn library_manifest(sessions: &std::path::Path, id: &str) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(sessions.join(id).join("manifest.json")).unwrap()).unwrap()
    }

    fn library_count(sessions: &std::path::Path) -> usize {
        std::fs::read_dir(sessions).unwrap().count()
    }

    #[test]
    fn the_seam_hello_carries_content_and_audio_through_the_decoder() {
        let hello: HelloRequest = serde_json::from_slice(&seam("hello.content.json")).unwrap();
        let vector: serde_json::Value = serde_json::from_slice(&seam("audio.vector-v1.json")).unwrap();
        let session = &hello.sessions[0];
        assert_eq!(session.content.get("manifest"), Some(&body_hash(&seam("manifest.phone-v4.json"))));
        assert_eq!(session.audio.as_deref(), vector["audio"].as_str());
    }

    #[test]
    fn the_seam_audio_vector_is_what_this_desktop_computes() {
        let vector: serde_json::Value = serde_json::from_slice(&seam("audio.vector-v1.json")).unwrap();
        let chunks = vector["chunks"].as_array().unwrap();
        for chunk in chunks {
            let plain = chunk["plaintext_utf8"].as_str().unwrap().as_bytes();
            assert_eq!(body_hash(plain), format!("sha256:{}", chunk["sha256"].as_str().unwrap()));
        }
        let audio = import::audio_hash(chunks.iter().map(|c| c["plaintext_utf8"].as_str().unwrap().as_bytes()));
        assert_eq!(audio, vector["audio"].as_str().unwrap());
    }

    #[test]
    fn a_reseal_under_unchanged_content_asks_for_nothing() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Standup");
        sync_session(&inbox, &sessions, PHONE, &manifest, true, Some(the_audio()));
        assert!(sync_session(&inbox, &sessions, PHONE, &manifest, true, Some(the_audio())).is_empty());
    }

    #[test]
    fn after_a_reseal_a_locked_phone_is_asked_for_nothing_either() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Standup");
        sync_session(&inbox, &sessions, PHONE, &manifest, true, Some(the_audio()));
        let transcript = phone_transcript();
        let (m, t) = (seal(&manifest), seal(&transcript));
        let unlocked = serde_json::json!([{
            "id": WIRE, "artifacts": [],
            "inputs": { "manifest": body_hash(&m), "transcript": body_hash(&t) },
            "content": { "manifest": body_hash(&manifest), "transcript": body_hash(&transcript) },
            "audio": the_audio(),
        }]);
        hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &hello_from(PHONE, unlocked), None).unwrap();
        let locked = serde_json::json!([{
            "id": WIRE, "artifacts": [],
            "inputs": { "manifest": body_hash(&m), "transcript": body_hash(&t) },
        }]);
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &hello_from(PHONE, locked), None).unwrap();
        assert!(reply.wants.is_empty(), "the revisions it saw unlocked are the ones on record");
    }

    #[test]
    fn a_renamed_session_with_the_same_audio_sends_only_its_manifest() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        sync_session(&inbox, &sessions, PHONE, &phone_manifest("Standup"), true, Some(the_audio()));
        let wav = sessions.join(WIRE).join("chunks/0001/mic.wav");
        let before = std::fs::read(&wav).unwrap();

        let renamed = phone_manifest("Standup, renamed");
        assert_eq!(sync_session(&inbox, &sessions, PHONE, &renamed, true, Some(the_audio())), ["manifest.json"]);
        assert_eq!(library_count(&sessions), 1);
        assert_eq!(library_manifest(&sessions, WIRE)["title"], "Standup, renamed");
        assert_eq!(std::fs::read(&wav).unwrap(), before, "the audio held here is the audio it keeps");
        let held = inbox.imported(PHONE, WIRE).unwrap();
        assert_eq!(held.get("content.manifest"), Some(&body_hash(&renamed)));
        assert_eq!(held.get("audio"), Some(&the_audio()));
    }

    #[test]
    fn a_phone_back_under_a_new_device_id_is_asked_for_nothing_and_keeps_its_session() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Standup");
        sync_session(&inbox, &sessions, PHONE, &manifest, true, Some(the_audio()));

        assert!(sync_session(&inbox, &sessions, RESET, &manifest, true, Some(the_audio())).is_empty());
        assert_eq!(library_count(&sessions), 1);
        assert_eq!(library_manifest(&sessions, WIRE)["origin_device"], RESET);
        assert_eq!(import::local_id(&import::minted_ids(&sessions), RESET, WIRE), WIRE);
        assert!(inbox.imported(RESET, WIRE).is_some());
    }

    #[test]
    fn a_phone_back_under_a_new_device_id_with_an_edit_sends_only_what_changed() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        sync_session(&inbox, &sessions, PHONE, &phone_manifest("Standup"), true, Some(the_audio()));

        let renamed = phone_manifest("Standup, renamed");
        assert_eq!(sync_session(&inbox, &sessions, RESET, &renamed, true, Some(the_audio())), ["manifest.json"]);
        assert_eq!(library_count(&sessions), 1);
        let held = library_manifest(&sessions, WIRE);
        assert_eq!(held["title"], "Standup, renamed");
        assert_eq!(held["origin_device"], RESET);
    }

    #[test]
    fn without_content_or_audio_a_reseal_brings_the_session_over_whole() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Standup");
        sync_session(&inbox, &sessions, PHONE, &manifest, false, None);
        assert_eq!(
            sync_session(&inbox, &sessions, PHONE, &manifest, false, None),
            ["manifest.json", "transcript.json", CHUNK]
        );
    }

    #[test]
    fn audio_that_differs_from_the_audio_on_record_is_asked_for() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        sync_session(&inbox, &sessions, PHONE, &phone_manifest("Standup"), true, Some(the_audio()));
        let trimmed = format!("sha256:{}", "f".repeat(64));
        let asked = sync_session(&inbox, &sessions, PHONE, &phone_manifest("Standup, trimmed"), true, Some(trimmed));
        assert_eq!(asked, ["manifest.json", CHUNK]);
    }

    // A session the phone has not finished transcribing (protocol v5).

    /// A phone's live captions: two final mic lines, an interim one, a line
    /// of a kind this desktop does not know, a system line and a torn write.
    fn live_captions() -> Vec<u8> {
        [
            r#"{"end_ms":2400,"final":true,"kind":"final","received_at_unix":1700000003,"start_ms":300,"text":"Morning, harbour budget first.","track":"mic"}"#,
            r#"{"end_ms":2600,"final":false,"kind":"interim","received_at_unix":1700000003,"start_ms":2400,"text":"and the","track":"mic"}"#,
            r#"{"end_ms":6100,"final":true,"kind":"caption_v2","received_at_unix":1700000008,"start_ms":3000,"text":"Then the ferry schedule.","track":"mic"}"#,
            r#"{"end_ms":7000,"final":true,"kind":"final","received_at_unix":1700000009,"start_ms":6500,"text":"Not this phone's.","track":"system"}"#,
            r#"{"end_ms":8000,"fin"#,
        ]
        .join("\n")
        .into_bytes()
    }

    /// The phone's own transcript, stamped as given.
    fn stamped_transcript(transcribed_at: i64, edited_at: Option<i64>, text: &str) -> Vec<u8> {
        let mut t = serde_json::json!({
            "schema_version": 1, "session_id": WIRE, "provider": "phone", "model": "on-device",
            "transcribed_at_unix_seconds": transcribed_at,
            "chunks": [{ "chunk_index": 1, "tracks": [{
                "track": "mic", "source_wav_relative": "chunks/0001/mic.wav",
                "segments": [{ "start_ms": 0, "end_ms": 900, "text": text, "confidence": null }],
            }] }],
        });
        if let Some(e) = edited_at {
            t["edited_at_unix_seconds"] = serde_json::json!(e);
        }
        serde_json::to_vec(&t).unwrap()
    }

    /// One round of a phone offering `files` in the clear, each as (input
    /// key, file name, bytes), with its audio: hellos until nothing is
    /// wanted, each wanted file uploaded, the import run once the session is
    /// ready. Returns every file asked for.
    fn offer_round(inbox: &Inbox, sessions: &std::path::Path, files: &[(&str, &str, Vec<u8>)]) -> Vec<String> {
        let audio = aac();
        let inputs: serde_json::Map<String, serde_json::Value> =
            files.iter().map(|(k, _, b)| (k.to_string(), serde_json::json!(body_hash(b)))).collect();
        let body = hello(serde_json::json!([{
            "id": WIRE, "artifacts": [], "inputs": inputs, "content": inputs, "audio": the_audio(),
        }]));
        let mut asked = Vec::new();
        for _ in 0..3 {
            let reply = hello_reply(inbox, sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
            if reply.wants.is_empty() {
                break;
            }
            for want in &reply.wants {
                let bytes = match files.iter().find(|(_, name, _)| *name == want.file) {
                    Some((_, _, b)) => b.clone(),
                    None if want.file == CHUNK => audio.clone(),
                    None => panic!("asked for {} which the phone did not offer", want.file),
                };
                import::stage(inbox, PHONE, WIRE, &want.file, &body_hash(&bytes), &bytes, &key()).unwrap();
                asked.push(want.file.clone());
            }
            if import::is_ready(inbox, PHONE, WIRE, &key()) {
                import::import_session(inbox, sessions, PHONE, WIRE, &key()).unwrap();
            }
        }
        asked
    }

    fn library_transcript(sessions: &std::path::Path) -> transcript::SessionTranscript {
        serde_json::from_slice(&std::fs::read(sessions.join(WIRE).join("transcript.json")).unwrap()).unwrap()
    }

    fn texts(t: &transcript::SessionTranscript) -> Vec<String> {
        t.chunks.iter().flat_map(|c| &c.tracks).flat_map(|t| &t.segments).map(|s| s.text.clone()).collect()
    }

    /// What a Regen. Transcript here leaves behind: a fresh transcript
    /// stamped when it ran, and the deduplicated copy built from it.
    fn retranscribe_here(sessions: &std::path::Path, at: i64) {
        let mut t: serde_json::Value =
            serde_json::from_slice(&stamped_transcript(at, None, "Morning, harbour budget first, per the desk.")).unwrap();
        t["provider"] = serde_json::json!("whisper");
        let bytes = serde_json::to_vec(&t).unwrap();
        std::fs::write(sessions.join(WIRE).join("transcript.json"), &bytes).unwrap();
        std::fs::write(sessions.join(WIRE).join("transcript.dedup.json"), &bytes).unwrap();
    }

    #[test]
    fn a_session_with_its_audio_and_no_transcript_imports_awaiting_transcription() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let asked = offer_round(&inbox, &sessions, &[("manifest", "manifest.json", phone_manifest("Harbour"))]);
        assert_eq!(asked, ["manifest.json", CHUNK]);
        let dir = sessions.join(WIRE);
        assert!(dir.join("chunks/0001/mic.wav").is_file(), "the session is in the library");
        assert!(!dir.join("transcript.json").exists(), "no transcript, as a recording made here before it is transcribed");
        assert!(inbox.staged_sessions().is_empty(), "nothing is left waiting");
        let record = inbox.imported(PHONE, WIRE).unwrap();
        assert!(!record.contains_key("transcript"));
    }

    #[test]
    fn live_captions_land_as_a_provisional_transcript_until_the_phones_own_arrives() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let asked = offer_round(
            &inbox,
            &sessions,
            &[("manifest", "manifest.json", phone_manifest("Harbour")), ("live_transcript", "live_transcript.jsonl", live_captions())],
        );
        assert_eq!(asked, ["manifest.json", "live_transcript.jsonl", CHUNK]);
        let held = library_transcript(&sessions);
        assert!(held.provisional);
        assert_eq!(texts(&held), ["Morning, harbour budget first.", "Then the ferry schedule."], "final mic lines only");
        assert_eq!(held.chunks[0].tracks.len(), 1, "one microphone, one track");
        assert_eq!(std::fs::read(sessions.join(WIRE).join("live_transcript.jsonl")).unwrap(), live_captions());

        // The phone finishes its transcript: the captions are no longer
        // offered and the transcript is.
        let mut finished: serde_json::Value = serde_json::from_slice(&phone_manifest("Harbour")).unwrap();
        finished["finalized_at_unix_seconds"] = serde_json::json!(1_700_000_042i64);
        let finished = serde_json::to_vec(&finished).unwrap();
        let theirs = stamped_transcript(1_700_000_092, None, "Morning. Harbour budget first.");
        std::fs::write(sessions.join(WIRE).join("transcript.dedup.json"), b"{}").unwrap();
        let asked = offer_round(
            &inbox,
            &sessions,
            &[("manifest", "manifest.json", finished), ("transcript", "transcript.json", theirs.clone())],
        );
        assert_eq!(asked, ["manifest.json", "transcript.json"]);
        let held = library_transcript(&sessions);
        assert!(!held.provisional);
        assert_eq!(texts(&held), ["Morning. Harbour budget first."]);
        assert!(!sessions.join(WIRE).join("transcript.dedup.json").exists(), "nothing derived from the provisional stays");
    }

    #[test]
    fn the_hello_keeps_asking_for_the_transcript_while_the_held_one_is_provisional() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Harbour");
        offer_round(
            &inbox,
            &sessions,
            &[("manifest", "manifest.json", manifest.clone()), ("live_transcript", "live_transcript.jsonl", live_captions())],
        );
        let theirs = stamped_transcript(1_700_000_092, None, "Morning.");
        let body = hello(serde_json::json!([{
            "id": WIRE, "artifacts": [], "audio": the_audio(),
            "inputs": { "manifest": body_hash(&manifest), "transcript": body_hash(&theirs) },
            "content": { "manifest": body_hash(&manifest), "transcript": body_hash(&theirs) },
        }]));
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        let wanted: Vec<_> = reply.wants.iter().map(|w| w.file.as_str()).collect();
        assert_eq!(wanted, ["manifest.json", "transcript.json"], "the manifest always travels");
        inbox.store(PHONE, WIRE, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        assert!(!import::is_ready(&inbox, PHONE, WIRE, &key()), "the transcript offered is on its way");
        inbox.store(PHONE, WIRE, "transcript.json", &body_hash(&theirs), &theirs).unwrap();
        assert!(import::is_ready(&inbox, PHONE, WIRE, &key()), "the audio is held");
    }

    #[test]
    fn a_retranscription_here_newer_than_the_phones_transcript_is_kept_and_not_asked_for_again() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Harbour");
        offer_round(
            &inbox,
            &sessions,
            &[("manifest", "manifest.json", manifest.clone()), ("live_transcript", "live_transcript.jsonl", live_captions())],
        );
        retranscribe_here(&sessions, 1_787_300_000);

        let older = stamped_transcript(1_700_000_092, None, "Morning.");
        let table = br#"{"schema_version":2,"diarization_id":"run-phone","speakers":[{"id":0}]}"#.to_vec();
        let asked = offer_round(
            &inbox,
            &sessions,
            &[
                ("manifest", "manifest.json", manifest.clone()),
                ("transcript", "transcript.json", older.clone()),
                ("speakers", "speakers.json", table.clone()),
            ],
        );
        assert_eq!(asked, ["manifest.json", "transcript.json", "speakers.json"]);
        let held = library_transcript(&sessions);
        assert_eq!(held.provider, "whisper", "the transcript run here is the newer one");
        assert!(sessions.join(WIRE).join("transcript.dedup.json").is_file());
        assert!(!sessions.join(WIRE).join("speakers.json").exists(), "the phone's table numbers a transcript not held here");
        let record = inbox.imported(PHONE, WIRE).unwrap();
        assert_eq!(record.get(import::REJECTED_TRANSCRIPT), Some(&body_hash(&older)));
        assert_eq!(record.get("transcript"), Some(&body_hash(&older)), "the phone's summary gate reads its own revision");

        // The same transcript offered again is not fetched again.
        let asked = offer_round(
            &inbox,
            &sessions,
            &[
                ("manifest", "manifest.json", phone_manifest("Harbour, renamed")),
                ("transcript", "transcript.json", older),
                ("speakers", "speakers.json", table),
            ],
        );
        assert_eq!(asked, ["manifest.json"]);
        assert_eq!(library_transcript(&sessions).provider, "whisper");

        // A transcript the phone made later than the one here replaces it.
        let newer = stamped_transcript(1_787_400_000, None, "Morning, all.");
        let asked = offer_round(
            &inbox,
            &sessions,
            &[("manifest", "manifest.json", manifest), ("transcript", "transcript.json", newer)],
        );
        assert_eq!(asked, ["manifest.json", "transcript.json"]);
        assert_eq!(texts(&library_transcript(&sessions)), ["Morning, all."]);
        assert!(!sessions.join(WIRE).join("transcript.dedup.json").exists());
        assert!(!inbox.imported(PHONE, WIRE).unwrap().contains_key(import::REJECTED_TRANSCRIPT));
    }

    #[test]
    fn an_edit_here_to_the_provisional_transcript_outranks_an_older_phone_transcript() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Harbour");
        offer_round(
            &inbox,
            &sessions,
            &[("manifest", "manifest.json", manifest.clone()), ("live_transcript", "live_transcript.jsonl", live_captions())],
        );
        crate::commands::pipeline::mark_transcript_edited(&sessions.join(WIRE), 1_787_300_000).unwrap();

        let older = stamped_transcript(1_700_000_092, None, "Morning.");
        offer_round(&inbox, &sessions, &[("manifest", "manifest.json", manifest.clone()), ("transcript", "transcript.json", older)]);
        let held = library_transcript(&sessions);
        assert!(held.provisional, "the edited captions stand");
        assert_eq!(held.edited_at_unix_seconds, Some(1_787_300_000));

        // An edit on the phone after the one here wins.
        let edited = stamped_transcript(1_700_000_092, Some(1_787_300_500), "Morning, everyone.");
        offer_round(&inbox, &sessions, &[("manifest", "manifest.json", manifest), ("transcript", "transcript.json", edited)]);
        let held = library_transcript(&sessions);
        assert!(!held.provisional);
        assert_eq!(texts(&held), ["Morning, everyone."]);
    }

    #[test]
    fn a_phones_next_transcript_replaces_its_last_one_whatever_their_stamps() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Harbour");
        let first = stamped_transcript(1_700_000_092, None, "Morning.");
        offer_round(&inbox, &sessions, &[("manifest", "manifest.json", manifest.clone()), ("transcript", "transcript.json", first)]);
        // The phone grouped its voices again: the same stamps, new clusters.
        let mut regrouped: serde_json::Value = serde_json::from_slice(&stamped_transcript(1_700_000_092, None, "Morning.")).unwrap();
        regrouped["chunks"][0]["tracks"][0]["segments"][0]["speaker_id"] = serde_json::json!(1);
        let regrouped = serde_json::to_vec(&regrouped).unwrap();
        offer_round(&inbox, &sessions, &[("manifest", "manifest.json", manifest), ("transcript", "transcript.json", regrouped)]);
        assert_eq!(library_transcript(&sessions).chunks[0].tracks[0].segments[0].speaker_id, Some(1));
    }

    #[test]
    fn the_import_waits_for_a_transcript_the_hello_offered() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Harbour");
        let theirs = stamped_transcript(1_700_000_092, None, "Morning.");
        let body = hello(serde_json::json!([{
            "id": WIRE, "artifacts": [],
            "inputs": { "manifest": body_hash(&manifest), "transcript": body_hash(&theirs) },
        }]));
        hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        inbox.store(PHONE, WIRE, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        let audio = aac();
        inbox.store(PHONE, WIRE, CHUNK, &body_hash(&audio), &audio).unwrap();
        assert!(!import::is_ready(&inbox, PHONE, WIRE, &key()), "the transcript was offered and is on its way");
        inbox.store(PHONE, WIRE, "transcript.json", &body_hash(&theirs), &theirs).unwrap();
        assert!(import::is_ready(&inbox, PHONE, WIRE, &key()));
    }

    #[test]
    fn a_phone_tombstone_drops_what_was_staged_and_waiting() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Harbour");
        let meta = br#"{"schema_version":1}"#;
        let body = hello(serde_json::json!([{
            "id": WIRE, "artifacts": [], "meta": body_hash(meta),
            "inputs": { "manifest": body_hash(&manifest), "live_transcript": body_hash(&live_captions()) },
        }]));
        hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        inbox.store(PHONE, WIRE, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        inbox.stage_record(PHONE, WIRE, "meta.json", &body_hash(meta), meta).unwrap();
        assert!(inbox.claim(PHONE, WIRE).is_some());

        let stone = tombstone_record(WIRE, 1, PHONE, &phone_key());
        let body = serde_json::to_vec(&serde_json::json!({
            "v": PROTOCOL_V, "device_id": PHONE, "sessions": [], "tombstones": [stone],
        }))
        .unwrap();
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert_eq!(reply.buried, [(WIRE.to_string(), WIRE.to_string(), true)], "the app hears it is gone");
        assert!(inbox.staged_sessions().is_empty());
        assert!(inbox.claim(PHONE, WIRE).is_none(), "nothing waits on the files the hello offered");
        assert!(inbox.staged_records(PHONE).is_empty(), "its meta.json goes with it");
    }

    #[test]
    fn a_session_known_only_by_its_meta_is_not_waiting_to_import() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let meta = br#"{"schema_version":1}"#;
        let body = hello(serde_json::json!([{ "id": WIRE, "artifacts": [], "inputs": {}, "meta": body_hash(meta) }]));
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(reply.wants.is_empty());
        assert_eq!(reply.meta_wants, [WIRE]);
        inbox.stage_record(PHONE, WIRE, "meta.json", &body_hash(meta), meta).unwrap();

        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(reply.wants.is_empty() && reply.meta_wants.is_empty());
        assert!(inbox.staged_sessions().is_empty(), "no session is pending import");
        assert!(inbox.claim(PHONE, WIRE).is_none());
        assert_eq!(inbox.staged_records(PHONE), [(WIRE.to_string(), "meta.json".to_string())], "the stub stays for its manifest");
    }

    #[test]
    fn a_v4_hello_is_refused_at_the_gate() {
        let (inbox, td) = inbox();
        let body = hello_v(4, serde_json::json!([]));
        assert_eq!(
            hello_reply(&inbox, &sessions_dir(&td), PHONE, &phone_pub(), &desktop_key(), &key(), &body, None),
            Err(HelloError::OtherVersion(crate::sync::gate::Outdated::Phone))
        );
    }

    #[test]
    fn the_seam_live_hello_names_the_captions_by_their_plaintext_and_they_are_wanted() {
        let body = seam("hello.live.json");
        let hello: HelloRequest = serde_json::from_slice(&body).unwrap();
        let session = &hello.sessions[0];
        let lines = seam("live_transcript.phone-v5.jsonl");
        assert_eq!(session.inputs.get("live_transcript"), Some(&body_hash(&lines)));
        assert_eq!(session.content.get("live_transcript"), Some(&body_hash(&lines)));
        assert_eq!(session.content.get("manifest"), Some(&body_hash(&seam("manifest.phone-v4.json"))));

        let (inbox, td) = inbox();
        let device = "11111111111111111111111111111111";
        let reply = hello_reply(&inbox, &sessions_dir(&td), device, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        let wanted: Vec<_> = reply.wants.iter().map(|w| w.file.as_str()).collect();
        assert_eq!(wanted, ["manifest.json", "live_transcript.jsonl"]);

        // The captions arrive sealed afresh; the plaintext is the revision.
        let sealed = seal(&lines);
        import::stage(&inbox, device, &session.id, "live_transcript.jsonl", &body_hash(&sealed), &sealed, &key()).unwrap();
        let reply = hello_reply(&inbox, &sessions_dir(&td), device, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        let wanted: Vec<_> = reply.wants.iter().map(|w| w.file.as_str()).collect();
        assert_eq!(wanted, ["manifest.json"]);
    }

    #[test]
    fn live_captions_are_not_asked_for_beside_a_transcript_or_once_one_is_held() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Harbour");
        let theirs = stamped_transcript(1_700_000_092, None, "Morning.");
        let asked = offer_round(
            &inbox,
            &sessions,
            &[
                ("manifest", "manifest.json", manifest.clone()),
                ("transcript", "transcript.json", theirs.clone()),
                ("live_transcript", "live_transcript.jsonl", live_captions()),
            ],
        );
        assert_eq!(asked, ["manifest.json", "transcript.json", CHUNK]);
        assert!(!library_transcript(&sessions).provisional);

        // A finalize still writing lists both; the transcript is here.
        let asked = offer_round(
            &inbox,
            &sessions,
            &[
                ("manifest", "manifest.json", manifest),
                ("transcript", "transcript.json", theirs),
                ("live_transcript", "live_transcript.jsonl", live_captions()),
            ],
        );
        assert!(asked.is_empty(), "{asked:?}");
        assert_eq!(texts(&library_transcript(&sessions)), ["Morning."]);
    }

    #[test]
    fn a_session_is_never_first_imported_waiting_for_its_captions() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Harbour");
        // A locked iPhone lists its plaintext captions and cannot seal them,
        // so it skips the want.
        let body = hello(serde_json::json!([{
            "id": WIRE, "artifacts": [],
            "inputs": { "manifest": body_hash(&manifest), "live_transcript": body_hash(&live_captions()) },
            "content": { "live_transcript": body_hash(&live_captions()) },
        }]));
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(reply.wants.iter().any(|w| w.file == "live_transcript.jsonl"), "asked for at every hello");
        inbox.store(PHONE, WIRE, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        let audio = aac();
        inbox.store(PHONE, WIRE, CHUNK, &body_hash(&audio), &audio).unwrap();
        assert!(import::is_ready(&inbox, PHONE, WIRE, &key()), "the session imports without them");
    }

    #[test]
    fn captions_that_arrive_after_the_import_become_its_provisional_transcript() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Harbour");
        offer_round(&inbox, &sessions, &[("manifest", "manifest.json", manifest.clone())]);
        assert!(!sessions.join(WIRE).join("transcript.json").exists());

        // The captions appear in a later hello. Until they are here, the
        // manifest alone imports nothing again.
        let files = [("manifest", "manifest.json", manifest.clone()), ("live_transcript", "live_transcript.jsonl", live_captions())];
        let inputs: serde_json::Map<String, serde_json::Value> =
            files.iter().map(|(k, _, b)| (k.to_string(), serde_json::json!(body_hash(b)))).collect();
        let body = hello(serde_json::json!([{ "id": WIRE, "artifacts": [], "inputs": inputs, "content": inputs, "audio": the_audio() }]));
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        let wanted: Vec<_> = reply.wants.iter().map(|w| w.file.as_str()).collect();
        assert_eq!(wanted, ["manifest.json", "live_transcript.jsonl"]);
        inbox.store(PHONE, WIRE, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        assert!(!import::is_ready(&inbox, PHONE, WIRE, &key()), "a held session waits for the captions it was offered");

        let asked = offer_round(&inbox, &sessions, &files);
        assert_eq!(asked, ["live_transcript.jsonl"]);
        let held = library_transcript(&sessions);
        assert!(held.provisional);
        assert_eq!(texts(&held), ["Morning, harbour budget first.", "Then the ferry schedule."]);
        assert!(offer_round(&inbox, &sessions, &files).is_empty(), "and then nothing moves");
    }

    #[test]
    fn captions_offered_after_a_phone_transcript_is_held_are_not_asked_for() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Harbour");
        let theirs = stamped_transcript(1_700_000_092, None, "Morning.");
        offer_round(&inbox, &sessions, &[("manifest", "manifest.json", manifest.clone()), ("transcript", "transcript.json", theirs.clone())]);
        let asked = offer_round(
            &inbox,
            &sessions,
            &[
                ("manifest", "manifest.json", manifest),
                ("transcript", "transcript.json", theirs),
                ("live_transcript", "live_transcript.jsonl", live_captions()),
            ],
        );
        assert!(asked.is_empty(), "{asked:?}");
    }

    #[test]
    fn a_declined_transcripts_speaker_table_comes_with_the_transcript_that_replaces_it() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Harbour");
        offer_round(&inbox, &sessions, &[("manifest", "manifest.json", manifest.clone())]);
        retranscribe_here(&sessions, 1_787_300_000);

        let table = br#"{"schema_version":2,"diarization_id":"run-phone","speakers":[{"id":0}]}"#.to_vec();
        let older = stamped_transcript(1_700_000_092, None, "Morning.");
        offer_round(
            &inbox,
            &sessions,
            &[
                ("manifest", "manifest.json", manifest.clone()),
                ("transcript", "transcript.json", older),
                ("speakers", "speakers.json", table.clone()),
            ],
        );
        let record = inbox.imported(PHONE, WIRE).unwrap();
        assert!(!record.contains_key("speakers"), "a table kept out is not held");
        assert_eq!(record.get(import::REJECTED_SPEAKERS), Some(&body_hash(&table)));

        let newer = stamped_transcript(1_787_400_000, None, "Morning, all.");
        let asked = offer_round(
            &inbox,
            &sessions,
            &[
                ("manifest", "manifest.json", manifest),
                ("transcript", "transcript.json", newer),
                ("speakers", "speakers.json", table.clone()),
            ],
        );
        assert_eq!(asked, ["manifest.json", "transcript.json", "speakers.json"]);
        assert_eq!(std::fs::read(sessions.join(WIRE).join("speakers.json")).unwrap(), table);
        assert_eq!(inbox.imported(PHONE, WIRE).unwrap().get("speakers"), Some(&body_hash(&table)));
    }

    #[test]
    fn a_phone_speaker_table_stays_out_of_a_transcript_made_here() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Harbour");
        let theirs = stamped_transcript(1_700_000_092, None, "Morning.");
        let first = br#"{"schema_version":2,"diarization_id":"run-1","speakers":[{"id":0}]}"#.to_vec();
        let files = |table: &[u8]| {
            [
                ("manifest", "manifest.json", manifest.clone()),
                ("transcript", "transcript.json", theirs.clone()),
                ("speakers", "speakers.json", table.to_vec()),
            ]
        };
        offer_round(&inbox, &sessions, &files(&first));
        retranscribe_here(&sessions, 1_787_300_000);

        let regrouped = br#"{"schema_version":2,"diarization_id":"run-2","speakers":[{"id":0},{"id":1}]}"#.to_vec();
        assert_eq!(offer_round(&inbox, &sessions, &files(&regrouped)), ["manifest.json", "speakers.json"]);
        assert_eq!(std::fs::read(sessions.join(WIRE).join("speakers.json")).unwrap(), first, "the library's table stands");
        assert_eq!(library_transcript(&sessions).provider, "whisper");
        assert!(offer_round(&inbox, &sessions, &files(&regrouped)).is_empty(), "the declined table is not fetched again");
    }

    #[test]
    fn an_offered_transcript_is_waited_on_for_seven_days_and_then_the_session_imports_without_it() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Harbour");
        let theirs = stamped_transcript(1_700_000_092, None, "Morning.");
        let body = hello(serde_json::json!([{
            "id": WIRE, "artifacts": [],
            "inputs": { "manifest": body_hash(&manifest), "transcript": body_hash(&theirs) },
        }]));
        hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        inbox.store(PHONE, WIRE, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        let audio = aac();
        inbox.store(PHONE, WIRE, CHUNK, &body_hash(&audio), &audio).unwrap();
        assert!(!import::is_ready(&inbox, PHONE, WIRE, &key()));

        inbox.age_claim(PHONE, WIRE, import::CLAIM_LIFETIME + std::time::Duration::from_secs(3600));
        assert!(import::is_ready(&inbox, PHONE, WIRE, &key()), "the phone has not been back for a week");
        import::import_session(&inbox, &sessions, PHONE, WIRE, &key()).unwrap();
        assert!(!sessions.join(WIRE).join("transcript.json").exists());
        assert!(!inbox.imported(PHONE, WIRE).unwrap().contains_key("transcript"));

        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert!(reply.wants.iter().any(|w| w.file == "transcript.json"), "the returning phone is asked for it");
    }

    #[test]
    fn a_repeated_tombstone_withdraws_nothing() {
        let (inbox, td) = inbox();
        let sessions = sessions_dir(&td);
        let manifest = phone_manifest("Harbour");
        inbox.store(PHONE, WIRE, "manifest.json", &body_hash(&manifest), &manifest).unwrap();
        let stone = tombstone_record(WIRE, 1, PHONE, &phone_key());
        let body = serde_json::to_vec(&serde_json::json!({
            "v": PROTOCOL_V, "device_id": PHONE, "sessions": [], "tombstones": [stone],
        }))
        .unwrap();
        let first = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert_eq!(first.buried, [(WIRE.to_string(), WIRE.to_string(), true)]);
        let again = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body, None).unwrap();
        assert_eq!(again.buried, [(WIRE.to_string(), WIRE.to_string(), false)], "nothing was waiting any more");
    }
}
