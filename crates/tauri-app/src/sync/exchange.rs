// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The hello exchange, section 5: what the desktop wants from a phone, what
//! it offers back, and the tombstones each side carries. The reply is built
//! from what is already on disk, because the phone has about ten seconds for
//! the whole round.

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

#[derive(Debug, Deserialize)]
struct HelloRequest {
    v: u32,
    device_id: String,
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

/// Buries the session `device_id` deleted.
///
/// Every device mints `daisy-<unix seconds>`, so the id alone names a
/// recording on one device and can name an unrelated one on another: the
/// library copy goes only when the manifest there names the device that sent
/// the tombstone. Who holds the id is read before the marker is written,
/// because a directory this desktop cannot read may be the recording the
/// tombstone is about: burying the id first would tell the device the delete
/// was done while the recording stayed in the library, with no path back to
/// it.
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
/// itself is removed by the caller: this is the record, not the deletion.
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

/// The protocol version this desktop speaks (protocol §11). It reads every
/// version since 1 and answers in the one the phone sent.
pub const PROTOCOL_V: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloError {
    Malformed,
    /// The phone speaks a newer protocol; the reply is 426 with the highest
    /// this desktop speaks, and the phone tells its owner to update here.
    NewerPhone { highest: u32 },
}

/// The derived files a phone can install. One artifact, named by the hash of
/// its bytes; chapters ride inside it (§7).
const ARTIFACTS: [&str; 1] = ["summary.json"];

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
    let mut offers = Vec::new();
    for name in ARTIFACTS {
        let Ok(bytes) = std::fs::read(dir.join(name)) else { continue };
        // Only the wire shape travels; a file this desktop wrote before v2 is
        // rewritten the next time it is loaded here.
        if !summarize::SessionSummary::is_v2_bytes(&bytes) {
            continue;
        }
        let artifact_id = body_hash(&bytes);
        if phone_holds.iter().any(|held| held == &artifact_id) {
            continue;
        }
        let header = serde_json::json!({
            "v": PROTOCOL_V,
            "artifact_id": artifact_id,
            "session": session,
            "name": name,
            "size": bytes.len(),
            "built_from": built_from,
        });
        let Ok(header_bytes) = serde_json::to_vec(&header) else { continue };
        offers.push(Offer {
            artifact_id,
            session: session.to_string(),
            name: name.to_string(),
            size: bytes.len() as u64,
            header_sig_b64: B64.encode(key.sign(&header_bytes).to_bytes()),
            header_b64: B64.encode(&header_bytes),
        });
    }
    offers
}

/// The bytes of one artifact this desktop holds, found by the hash a phone
/// asks for. Only files under an imported session are reachable.
pub fn artifact_bytes(inbox: &Inbox, sessions_dir: &std::path::Path, artifact_id: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = sessions_dir.parent().and_then(|root| crate::sync::profile::artifact_bytes(root, artifact_id)) {
        return Some(bytes);
    }
    for session in inbox.imported_sessions() {
        let dir = sessions_dir.join(&session);
        for name in ARTIFACTS {
            let Ok(bytes) = std::fs::read(dir.join(name)) else { continue };
            if body_hash(&bytes) == artifact_id {
                return Some(bytes);
            }
        }
    }
    None
}

/// Answers one hello: takes the phone's tombstones, then names every file
/// this desktop lacks and every tombstone the phone has yet to act on.
pub fn hello_reply(
    inbox: &Inbox,
    sessions_dir: &std::path::Path,
    device_id: &str,
    phone_ed25519_pub_b64: &str,
    key: &ed25519_dalek::SigningKey,
    envelope_key: &MachineKey,
    body: &[u8],
) -> Result<HelloReply, HelloError> {
    let hello: HelloRequest = serde_json::from_slice(body).map_err(|_| HelloError::Malformed)?;
    if hello.device_id != device_id || hello.v == 0 {
        return Err(HelloError::Malformed);
    }
    if hello.v > PROTOCOL_V {
        return Err(HelloError::NewerPhone { highest: PROTOCOL_V });
    }
    // A hello carries the device's tombstones a second time; `POST
    // /tombstone` is where it reads the outcome and stops sending one. They
    // resolve against one read of the library: a delete only takes out the
    // session it names, so the rest of the batch reads the same answer it
    // would have read one scan at a time.
    let before = import::minted_ids(sessions_dir);
    for record in &hello.tombstones {
        if accept_tombstone(inbox, sessions_dir, &before, device_id, record, phone_ed25519_pub_b64)
            == Tombstoned::Retry
        {
            log::warn!("sync: tombstone {} from {device_id} is not settled yet", record.session);
        }
    }

    // Read again after the tombstones, which take sessions out of the
    // library, and once for the whole reply. A session the library holds
    // under another device id with the same audio is this device's from
    // here on.
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
        // A session named from the band this desktop mints ids in would take
        // an id the library hands out; no clock reads that high.
        if !is_wire_session_id(&session.id) {
            log::warn!("sync: {device_id} named a session this desktop cannot hold: {}", session.id);
            continue;
        }
        // A tombstone travels to the device whose recording it buried; the
        // same id on another device is that device's own session.
        if inbox.is_entombed(&session.id, device_id) {
            if let Some(record) = inbox.tombstone(&session.id, device_id).and_then(|b| serde_json::from_slice(&b).ok()) {
                tombstones.push(record);
            }
            continue;
        }
        // A session refused at these revisions cannot be opened here; asking
        // again would bring the same bytes. New revisions are asked for.
        if inbox.refusal(device_id, &session.id).is_some_and(|r| r.covers(&session.inputs)) {
            continue;
        }
        // An input is at the revision on record when its sealed hash is the
        // one recorded, or its plaintext hash is: a reseal moves the first
        // and leaves the second.
        let local = import::local_id(&minted, device_id, &session.id);
        let imported = inbox.imported(device_id, &local);
        let same = |held: &std::collections::BTreeMap<String, String>, k: &str| {
            held.get(k) == session.inputs.get(k)
                || session.content.get(k).is_some_and(|c| held.get(&format!("content.{k}")) == Some(c))
        };
        // A session already imported at exactly these revisions is here in
        // full; nothing about it needs to travel again. The sealed hashes it
        // is listed under now are the ones on record from here on.
        if let Some(held) = imported.as_ref().filter(|held| INPUTS.iter().all(|(k, _)| same(held, k))) {
            let mut resealed = held.clone();
            for (k, _) in INPUTS {
                match session.inputs.get(k) {
                    Some(hash) => resealed.insert(k.to_string(), hash.clone()),
                    None => resealed.remove(k),
                };
            }
            if &resealed != held {
                let _ = inbox.record_import(device_id, &local, &resealed);
            }
            // An artifact returns from v2 on; a v1 phone has no route to
            // install one.
            if hello.v >= 2 {
                offers.extend(artifact_offers(
                    inbox,
                    sessions_dir,
                    &minted,
                    device_id,
                    &session.id,
                    &session.artifacts,
                    key,
                ));
            }
            continue;
        }
        // What of the session this desktop holds already stays where it is:
        // the inputs at their recorded plaintext and the audio on record.
        // The manifest always travels; the import reads the session from it.
        let held = imported.unwrap_or_default();
        let claim = crate::sync::inbox::Claim {
            inputs: session.inputs.clone(),
            content: session.content.clone(),
            kept: INPUTS
                .iter()
                .filter(|(k, _)| *k != "manifest" && session.inputs.contains_key(*k))
                .filter(|(k, _)| session.content.get(*k).is_some_and(|c| held.get(&format!("content.{k}")) == Some(c)))
                .map(|(k, _)| k.to_string())
                .collect(),
            held_audio: session.audio.clone().filter(|a| held.get("audio") == Some(a)),
        };
        let _ = if claim.kept.is_empty() && claim.held_audio.is_none() {
            inbox.forget_claim(device_id, &session.id)
        } else {
            inbox.record_claim(device_id, &session.id, &claim)
        };
        for (key, file) in INPUTS {
            let Some(hash) = session.inputs.get(key) else { continue };
            if claim.kept.iter().any(|k| k == key) {
                continue;
            }
            if !inbox.has(device_id, &session.id, file, hash) {
                wants.push(Want { session: session.id.clone(), file: file.to_string() });
            }
        }
        // Chunk audio is named by the manifest, so it can only be asked for
        // once the manifest itself has arrived and this desktop has opened it.
        if let Ok(paths) = import::missing(inbox, device_id, &session.id, envelope_key) {
            wants.extend(paths.into_iter().map(|file| Want { session: session.id.clone(), file }));
        }
    }

    // Profile files return from v2 on, the way artifacts do.
    let (profile_wants, profile_offers) = match (&hello.profile, sessions_dir.parent()) {
        (Some(phone), Some(root)) if hello.v >= 2 => {
            crate::sync::profile::wants_and_offers(inbox, root, device_id, phone, key)
        }
        _ => (Vec::new(), Vec::new()),
    };

    Ok(HelloReply { v: hello.v, wants, offers, tombstones, profile_wants, profile_offers })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::inbox::Inbox;
    use crate::sync::signing::body_hash;
    use base64::engine::general_purpose::STANDARD as B64;
    
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
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &hello(serde_json::json!([]))).unwrap();
        assert!(reply.profile_wants.is_empty() && reply.profile_offers.is_empty());
    }

    #[test]
    fn a_phones_profile_file_this_desktop_has_not_merged_is_wanted() {
        let (inbox, _td) = inbox();
        let body = hello_profile(serde_json::json!({ "tags": "sha256:aa", "contacts": "sha256:bb" }));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
        assert_eq!(reply.profile_wants, ["tags.json", "contacts.json"]);
        assert!(reply.profile_offers.is_empty());
    }

    #[test]
    fn a_staged_profile_file_is_not_wanted_again_while_it_waits_to_merge() {
        let (inbox, _td) = inbox();
        let theirs = br#"{"schema_version":1,"tags":[]}"#;
        inbox.store_profile(PHONE, "tags.json", &body_hash(theirs), theirs).unwrap();
        let body = hello_profile(serde_json::json!({ "tags": body_hash(theirs) }));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
        assert!(reply.profile_wants.is_empty() && reply.profile_offers.is_empty());
    }

    #[test]
    fn once_merged_the_desktops_file_is_offered_with_the_ids_the_phone_rewrites() {
        let (inbox, _td) = inbox();
        let mine = br#"{"schema_version":1,"tags":[{"id":"d1","name":"PHSA"}]}"#;
        std::fs::write(_td.path().join("tags.json"), mine).unwrap();
        let ids = std::collections::BTreeMap::from([("p1".to_string(), "d1".to_string())]);
        inbox.record_profile_merge(PHONE, "tags.json", "sha256:phone", &ids).unwrap();
        let body = hello_profile(serde_json::json!({ "tags": "sha256:phone" }));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
        assert!(reply.profile_wants.is_empty());
        assert_eq!(reply.profile_offers.len(), 1);
        let offer = &reply.profile_offers[0];
        assert_eq!(offer.artifact_id, body_hash(mine));
        assert_eq!((offer.session.as_str(), offer.name.as_str()), ("profile", "tags.json"));
        let header = header_of(offer);
        assert_eq!(header["merged_from"], "sha256:phone");
        assert_eq!(header["ids"]["p1"], "d1");
        assert_eq!(artifact_bytes(&inbox, &sessions_dir(&_td), &offer.artifact_id).unwrap(), mine);

        // The phone installed it: its hash is this desktop's, so nothing
        // moves either way.
        let body = hello_profile(serde_json::json!({ "tags": body_hash(mine) }));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
        assert!(reply.profile_wants.is_empty() && reply.profile_offers.is_empty());
    }

    #[test]
    fn a_phone_with_no_catalog_yet_is_offered_this_desktops() {
        let (inbox, _td) = inbox();
        let mine = br#"{"schema_version":1,"contacts":[]}"#;
        std::fs::write(_td.path().join("contacts.json"), mine).unwrap();
        let body = hello_profile(serde_json::json!({}));
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
        assert_eq!(reply.profile_offers.len(), 1);
        assert_eq!(reply.profile_offers[0].name, "contacts.json");
        assert!(header_of(&reply.profile_offers[0])["merged_from"].is_null());
    }

    #[test]
    fn a_v1_phone_is_offered_no_profile_files() {
        let (inbox, _td) = inbox();
        std::fs::write(_td.path().join("tags.json"), br#"{"schema_version":1,"tags":[]}"#).unwrap();
        let body = serde_json::to_vec(&serde_json::json!({
            "v": 1, "device_id": PHONE, "sessions": [], "tombstones": [], "profile": {},
        }))
        .unwrap();
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
        assert!(reply.profile_wants.is_empty() && reply.profile_offers.is_empty());
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
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
            hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key, &body).unwrap();
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
            hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &other, &body).unwrap();
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
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
        assert!(reply.wants.is_empty(), "nothing is wanted for a session that is gone");
        assert_eq!(reply.tombstones.len(), 1);
        assert_eq!(reply.tombstones[0].session, "daisy-1");
    }

    #[test]
    fn a_session_the_phone_no_longer_lists_is_not_offered_its_tombstone() {
        let (inbox, _td) = inbox();
        let stone = tombstone_record("daisy-1", 3, PHONE, &phone_key());
        inbox.entomb("daisy-1", Some(PHONE), &serde_json::to_vec(&stone).unwrap()).unwrap();
        let reply = hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &hello(serde_json::json!([]))).unwrap();
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
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop, &key(), &body).unwrap();
        assert_eq!(reply.tombstones.len(), 1);
        let stone = &reply.tombstones[0];
        assert_eq!(stone.origin, mac_id, "this desktop originated it");
        assert!(tombstone_is_valid(stone, &B64.encode(desktop.verifying_key().to_bytes())));

        // Each delete gets its own counter.
        std::fs::create_dir_all(sessions.join("daisy-2")).unwrap();
        assert!(entomb_here(&inbox, "daisy-2", "daisy-2", PHONE, mac_id, &desktop));
        let body = hello(serde_json::json!([{ "id": "daisy-2", "artifacts": [], "inputs": {} }]));
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop, &key(), &body).unwrap();
        assert_eq!(reply.tombstones[0].seq, stone.seq + 1);
    }

    #[test]
    fn a_tombstone_the_phone_sends_deletes_the_session_here() {
        let (inbox, _td) = inbox();
        let bytes = b"manifest bytes";
        inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(bytes), bytes).unwrap();
        let stone = tombstone_record("daisy-1", 1, PHONE, &phone_key());
        let body = serde_json::to_vec(&serde_json::json!({
            "v": 1, "device_id": PHONE, "sessions": [], "tombstones": [stone],
        }))
        .unwrap();
        hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
            "v": 1, "device_id": PHONE, "sessions": [], "tombstones": [stone],
        }))
        .unwrap();
        hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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

        // No marker, so the phone's next upload of that session lands rather
        // than answering 409 for good.
        let bytes = b"manifest bytes";
        assert!(inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(bytes), bytes).is_ok());
        let body = hello(serde_json::json!([{
            "id": "daisy-1", "artifacts": [], "inputs": { "manifest": "sha256:aa" },
        }]));
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
        let mine = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &hello(listed.clone())).unwrap();
        assert_eq!(mine.tombstones.len(), 1, "the phone that deleted it is told it is gone");

        let body = hello_from(TABLET, listed);
        let theirs = hello_reply(&inbox, &sessions, TABLET, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
            let reply = hello_reply(&inbox, &sessions, device, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
        let reply = hello_reply(&inbox, &sessions, TABLET, &phone_pub(), &desktop, &key(), &body).unwrap();
        assert_eq!(reply.tombstones.len(), 1);
        assert_eq!(reply.tombstones[0].session, "daisy-1", "it travels under the id the tablet knows");

        let body = hello_from(PHONE, serde_json::json!([{ "id": "daisy-1", "artifacts": [], "inputs": {} }]));
        let other = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop, &key(), &body).unwrap();
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
        let reply = hello_reply(&inbox, &sessions, TABLET, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
            hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &body),
            Err(HelloError::Malformed)
        );
    }

    #[test]
    fn a_hello_that_is_not_a_hello_is_refused() {
        let (inbox, _td) = inbox();
        assert_eq!(
            hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), b"not json"),
            Err(HelloError::Malformed)
        );
        let no_version = serde_json::to_vec(&serde_json::json!({
            "v": 0, "device_id": PHONE, "sessions": [], "tombstones": [],
        }))
        .unwrap();
        assert_eq!(
            hello_reply(&inbox, &sessions_dir(&_td), PHONE, &phone_pub(), &desktop_key(), &key(), &no_version),
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
    fn a_v1_hello_gets_v1_wants_and_no_offers() {
        let (inbox, _td, sessions, named) = imported_with_a_summary();
        let body = hello_v(1, named);
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
        assert_eq!(reply.v, 1);
        assert!(reply.offers.is_empty(), "a v1 phone has no route to install one");
    }

    #[test]
    fn a_v2_hello_gets_v2_and_the_summary_offer() {
        let (inbox, _td, sessions, named) = imported_with_a_summary();
        let body = hello_v(2, named);
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
        assert_eq!(reply.v, 2);
        assert_eq!(reply.offers.len(), 1);
        assert_eq!(reply.offers[0].name, "summary.json");
        let header: serde_json::Value =
            serde_json::from_slice(&B64.decode(&reply.offers[0].header_b64).unwrap()).unwrap();
        assert_eq!(header["v"], 2);
    }

    #[test]
    fn a_newer_phone_is_told_the_highest_version() {
        let (inbox, _td, sessions, named) = imported_with_a_summary();
        let body = hello_v(3, named);
        let err = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap_err();
        assert_eq!(err, HelloError::NewerPhone { highest: 2 });
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
        let body = hello_v(2, named);
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
        assert!(reply.offers.is_empty(), "only the wire shape travels");
    }

    #[test]
    fn chapters_and_chunks_are_never_offered() {
        let (inbox, _td, sessions, named) = imported_with_a_summary();
        std::fs::write(sessions.join("daisy-1").join("chapters.json"), b"{}").unwrap();
        std::fs::write(sessions.join("daisy-1").join("chunks.json"), b"{}").unwrap();
        let body = hello_v(2, named);
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
        assert_eq!(reply.offers.len(), 1);
        assert_eq!(reply.offers[0].name, "summary.json");
    }

    // What a session is under its envelopes (§5 `content` and `audio`).

    const WIRE: &str = "daisy-1787249858";
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
            "created_at_unix_seconds": 1_787_249_858i64, "sample_rate": 16000, "channels": 1,
            "chunks": [{
                "index": 1, "started_at_unix_seconds": 1_787_249_858i64,
                "ended_at_unix_seconds": 1_787_249_868i64, "duration_seconds": 9.4,
                "mic_samples": 150_393, "mic_wav_relative": CHUNK,
            }],
        }))
        .unwrap()
    }

    fn phone_transcript() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "session_id": WIRE, "provider": "phone", "model": "on-device",
            "transcribed_at_unix_seconds": 1_787_249_870i64, "chunks": [],
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
            let reply = hello_reply(inbox, sessions, device, &phone_pub(), &desktop_key(), &key(), &body).unwrap();
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
        hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &hello_from(PHONE, unlocked)).unwrap();
        let locked = serde_json::json!([{
            "id": WIRE, "artifacts": [],
            "inputs": { "manifest": body_hash(&m), "transcript": body_hash(&t) },
        }]);
        let reply = hello_reply(&inbox, &sessions, PHONE, &phone_pub(), &desktop_key(), &key(), &hello_from(PHONE, locked)).unwrap();
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
}
