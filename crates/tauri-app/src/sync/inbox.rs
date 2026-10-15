// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Where a phone's uploads land before the pipeline takes them, and the
//! tombstones that refuse a session for good. Sections 6 and 8 of the
//! protocol. This module owns the bytes on disk and the names allowed to
//! reach a path; `import` moves a session into `sessions/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sync::signing::body_hash;

/// Section 6's size cap: 256 MiB.
pub const FILE_CAP_BYTES: usize = 256 * 1024 * 1024;

/// What `note_plaintext` appends to a staged file's name for the record of
/// its plaintext hash.
const PLAINTEXT_SUFFIX: &str = ".plaintext";

/// The files a session's revisions name, in the order a hello asks for
/// them. `live_transcript` is the phone's live captions, offered only while
/// its own transcript has not been written.
pub const INPUTS: [(&str, &str); 5] = [
    ("manifest", "manifest.json"),
    ("transcript", "transcript.json"),
    ("speakers", "speakers.json"),
    ("notes", "notes.json"),
    ("live_transcript", LIVE_TRANSCRIPT),
];

/// The phone's live captions as it offers them: one final mic line per row.
pub const LIVE_TRANSCRIPT: &str = "live_transcript.jsonl";

/// The inputs an artifact is built from. `notes` stays outside.
pub const BUILT_FROM: [&str; 3] = ["manifest", "transcript", "speakers"];

/// Why a staged session was refused for good at the revisions it arrived at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// Sealed to a key this desktop does not hold. A re-pair is the remedy.
    Sealed,
    /// Written by a newer Daisy than this one reads. An update is the remedy.
    Unsupported,
    /// Audio that does not decode: damaged, or holding no sound. No update
    /// or re-pair changes it; deleting it on the device clears it.
    Undecodable,
}

/// A session this desktop cannot open, at the input revisions it arrived
/// at. A hello naming the same revisions asks for nothing; one naming new
/// revisions brings the session over again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub device_id: String,
    pub reason: RefusalReason,
    pub inputs: BTreeMap<String, String>,
}

impl Refusal {
    /// True when every input this refusal recorded is what the phone still
    /// offers, so the bytes a retry would bring are the ones refused.
    pub fn covers(&self, offered: &BTreeMap<String, String>) -> bool {
        self.inputs.iter().all(|(k, v)| offered.get(k) == Some(v))
    }
}

/// What a hello said about a session it is sending over, and what of it this
/// desktop already holds. The import reads it to leave in place what did
/// not change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    /// The hello's `inputs`: the revision each input is on the phone now.
    pub inputs: BTreeMap<String, String>,
    /// The hello's `content`.
    pub content: BTreeMap<String, String>,
    /// The inputs whose content this desktop holds at that revision; they
    /// are not asked for and the library keeps its copy.
    pub kept: Vec<String>,
    /// The session's audio, when the library holds that exact audio; its
    /// chunks are not asked for and the library keeps its WAVs.
    pub held_audio: Option<String>,
    /// True when the library holds the session at every revision the hello
    /// named but its live captions: the import then waits for them.
    #[serde(default)]
    pub live_only: bool,
}

/// Why an upload was refused. Each maps to the status section 6 names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    BadName,
    HashMismatch,
    TooLarge,
    Entombed,
    Io,
}

/// `^daisy-[0-9]{1,12}$`.
pub fn is_session_id(id: &str) -> bool {
    let Some(digits) = id.strip_prefix("daisy-") else { return false };
    (1..=12).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
}

/// A session id a device may name on the wire: a session id whose number is below the band this desktop mints from.
pub fn is_wire_session_id(id: &str) -> bool {
    is_session_id(id) && id["daisy-".len()..].parse::<u64>().is_ok_and(|n| n < crate::sync::import::MINTED_ID_FLOOR)
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`.
pub fn is_file_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else { return false };
    first.is_ascii_alphanumeric()
        && name.len() <= 64
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// A manifest's `mic_wav_relative` and the three input names: every
/// `/`-separated component is a file name, so no component can be `.`, `..`
/// or empty and no path can leave its session.
pub fn is_relative_file_path(path: &str) -> bool {
    !path.is_empty() && path.split('/').all(is_file_name)
}

/// The name a device's own corner of the staging area takes: a hash of the device id.
fn device_tag(device: &str) -> String {
    body_hash(device.as_bytes())["sha256:".len()..][..16].to_string()
}

/// The file inside a device's staging directory that names the device, so a
/// staged session is imported under the device that sent it. Never a session
/// id, so it is not one of the staged sessions.
const DEVICE_FILE: &str = "device";

/// The staging area one desktop keeps for phone uploads. Per machine, keyed by the device that sent the bytes.
#[derive(Clone)]
pub struct Inbox {
    root: PathBuf,
}

impl Inbox {
    pub fn at(root: impl AsRef<Path>) -> Self {
        Self { root: root.as_ref().to_path_buf() }
    }

    fn device_dir(&self, device: &str) -> PathBuf {
        self.root.join("sessions").join(device_tag(device))
    }

    fn session_dir(&self, device: &str, session: &str) -> PathBuf {
        self.device_dir(device).join(session)
    }

    /// The device directories one area holds, whoever they belong to.
    fn tag_dirs(&self, area: &str) -> Vec<PathBuf> {
        std::fs::read_dir(self.root.join(area))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect()
    }

    /// The marker for a tombstone aimed at one device.
    fn tombstone_path_for(&self, session: &str, device: &str) -> PathBuf {
        self.root.join("tombstones").join(format!("{session}.{}", device_tag(device)))
    }

    /// The marker for a tombstone with no device on record: every device is
    /// offered it.
    fn tombstone_path(&self, session: &str) -> PathBuf {
        self.root.join("tombstones").join(session)
    }

    fn refused_path(&self, device: &str, session: &str) -> PathBuf {
        self.root.join("refused").join(device_tag(device)).join(format!("{session}.json"))
    }

    /// The path a file sits at, or `None` when either name is one this
    /// desktop refuses to build a path from.
    fn path_of(&self, device: &str, session: &str, file: &str) -> Option<PathBuf> {
        (is_wire_session_id(session) && is_relative_file_path(file))
            .then(|| self.session_dir(device, session).join(file))
    }

    /// Records which device a staging directory belongs to, so its sessions
    /// are imported under the device that sent them.
    fn note_device(&self, device: &str) -> Result<(), StoreError> {
        let dir = self.device_dir(device);
        let path = dir.join(DEVICE_FILE);
        if path.is_file() {
            return Ok(());
        }
        std::fs::create_dir_all(&dir).map_err(|_| StoreError::Io)?;
        syncsafe::write_atomic(&path, device.as_bytes()).map_err(|_| StoreError::Io)
    }

    /// Writes the body under `device/session/file` once it hashes to `hash`.
    /// Idempotent: the same bytes twice leave one file, and a later revision
    /// replaces the one before it. A name ending in `PLAINTEXT_SUFFIX` is
    /// refused: that name is the record `note_plaintext` keeps beside a file.
    pub fn store(&self, device: &str, session: &str, file: &str, hash: &str, body: &[u8]) -> Result<(), StoreError> {
        if file.ends_with(PLAINTEXT_SUFFIX) {
            return Err(StoreError::BadName);
        }
        let path = self.path_of(device, session, file).ok_or(StoreError::BadName)?;
        if body.len() > FILE_CAP_BYTES {
            return Err(StoreError::TooLarge);
        }
        if body_hash(body) != hash {
            return Err(StoreError::HashMismatch);
        }
        self.note_device(device)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| StoreError::Io)?;
        }
        syncsafe::write_atomic(&path, body).map_err(|e| {
            log::warn!("sync: inbox write {session}/{file}: {e}");
            StoreError::Io
        })?;
        Ok(())
    }

    pub fn read(&self, device: &str, session: &str, file: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path_of(device, session, file)?).ok()
    }

    /// True when the file is staged, checked on the filesystem without reading the bytes.
    pub fn exists(&self, device: &str, session: &str, file: &str) -> bool {
        self.path_of(device, session, file).is_some_and(|p| p.is_file())
    }

    /// Records the hash of a staged file's plaintext beside it, or drops the
    /// record when `hash` is `None`.
    pub fn note_plaintext(&self, device: &str, session: &str, file: &str, hash: Option<&str>) -> Result<(), StoreError> {
        let path = self.path_of(device, session, &format!("{file}{PLAINTEXT_SUFFIX}")).ok_or(StoreError::BadName)?;
        match hash {
            Some(h) => syncsafe::write_atomic(&path, h.as_bytes()).map_err(|_| StoreError::Io),
            None => remove_file(&path),
        }
    }

    /// The hash of a staged file's plaintext, as `note_plaintext` recorded it.
    pub fn plaintext(&self, device: &str, session: &str, file: &str) -> Option<String> {
        std::fs::read_to_string(self.path_of(device, session, &format!("{file}{PLAINTEXT_SUFFIX}"))?).ok()
    }

    /// True when anything of a device's session waits here: staged files, or
    /// the claim of a hello that offered it.
    pub fn holds_staged(&self, device: &str, session: &str) -> bool {
        is_wire_session_id(session)
            && (self.session_dir(device, session).is_dir() || self.claim_path(device, session).is_file())
    }

    /// Drops everything a device has staged here: its session files, the
    /// claims of its hellos and its staged records. Its import records and
    /// tombstones stay.
    pub fn drop_device(&self, device: &str) -> Result<(), StoreError> {
        for area in ["sessions", "claims", "records"] {
            remove_dir(&self.root.join(area).join(device_tag(device)))?;
        }
        Ok(())
    }

    /// True when a device's claim on a session was last written longer ago
    /// than `age`.
    pub fn claim_older_than(&self, device: &str, session: &str, age: std::time::Duration) -> bool {
        std::fs::metadata(self.claim_path(device, session))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|elapsed| elapsed > age)
    }

    /// Moves a claim's last write back by `by`.
    #[cfg(test)]
    pub(crate) fn age_claim(&self, device: &str, session: &str, by: std::time::Duration) {
        let file = std::fs::File::options().write(true).open(self.claim_path(device, session)).unwrap();
        file.set_modified(std::time::SystemTime::now() - by).unwrap();
    }

    /// True when this desktop already holds that device's exact bytes, which
    /// is what a hello's wants are computed from.
    pub fn has(&self, device: &str, session: &str, file: &str, hash: &str) -> bool {
        self.read(device, session, file).is_some_and(|bytes| body_hash(&bytes) == hash)
    }

    /// Records the tombstone and drops everything the session had. `record` is the signed body from section 8, kept so a hello can offer it back until the device stops listing the session. A later upload of that session id by `target` is refused.
    /// `target` is the device whose recording this was; `None` records a tombstone every device is offered, and drops every device's copy.
    pub fn entomb(&self, session: &str, target: Option<&str>, record: &[u8]) -> Result<(), StoreError> {
        if !is_wire_session_id(session) {
            return Err(StoreError::BadName);
        }
        // The files go before the marker.
        match target {
            Some(device) => {
                self.clear_staged(device, session)?;
                self.forget_refusal(device, session)?;
                remove_dir(&self.root.join("records").join(device_tag(device)).join(session))?;
            }
            None => {
                for dir in self.tag_dirs("sessions") {
                    remove_dir(&dir.join(session))?;
                }
                for dir in self.tag_dirs("claims") {
                    remove_file(&dir.join(format!("{session}.json")))?;
                }
                for dir in self.tag_dirs("refused") {
                    remove_file(&dir.join(format!("{session}.json")))?;
                }
                for dir in self.tag_dirs("records") {
                    remove_dir(&dir.join(session))?;
                }
            }
        }
        let marker = match target {
            Some(device) => self.tombstone_path_for(session, device),
            None => self.tombstone_path(session),
        };
        if let Some(parent) = marker.parent() {
            std::fs::create_dir_all(parent).map_err(|_| StoreError::Io)?;
        }
        syncsafe::write_atomic(&marker, record).map_err(|_| StoreError::Io)
    }

    /// Records the input revisions a device's session was imported at, under
    /// the id the library holds it by. A later hello naming the same
    /// revisions asks for nothing; one naming different revisions (a rename
    /// or a trim on the phone) brings the file over again.
    pub fn record_import(
        &self,
        device: &str,
        session: &str,
        inputs: &BTreeMap<String, String>,
    ) -> Result<(), StoreError> {
        if !is_session_id(session) {
            return Err(StoreError::BadName);
        }
        let path = self.imported_path(device, session);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| StoreError::Io)?;
        }
        let bytes = serde_json::to_vec(inputs).map_err(|_| StoreError::Io)?;
        syncsafe::write_atomic(&path, &bytes).map_err(|_| StoreError::Io)?;
        Ok(())
    }

    /// The revisions a device's session is on this desktop at, if it is here.
    pub fn imported(&self, device: &str, session: &str) -> Option<BTreeMap<String, String>> {
        let bytes = std::fs::read(self.imported_path(device, session)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Drops the staged copies of a device's session once it has been
    /// imported. The import record stays: it is what keeps the phone from
    /// sending it again.
    pub fn clear_staged(&self, device: &str, session: &str) -> Result<(), StoreError> {
        if !is_session_id(session) {
            return Err(StoreError::BadName);
        }
        remove_file(&self.claim_path(device, session))?;
        remove_dir(&self.session_dir(device, session))
    }

    /// Records what the latest hello said about a session it is sending.
    pub fn record_claim(&self, device: &str, session: &str, claim: &Claim) -> Result<(), StoreError> {
        if !is_wire_session_id(session) {
            return Err(StoreError::BadName);
        }
        let path = self.claim_path(device, session);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| StoreError::Io)?;
        }
        let bytes = serde_json::to_vec(claim).map_err(|_| StoreError::Io)?;
        syncsafe::write_atomic(&path, &bytes).map_err(|_| StoreError::Io)
    }

    /// What the latest hello said about a session being sent, if it said
    /// anything this desktop holds already.
    pub fn claim(&self, device: &str, session: &str) -> Option<Claim> {
        let bytes = std::fs::read(self.claim_path(device, session)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Drops the claim once a hello names nothing this desktop holds.
    pub fn forget_claim(&self, device: &str, session: &str) -> Result<(), StoreError> {
        remove_file(&self.claim_path(device, session))
    }

    /// The sessions with files waiting to be imported, each with the device
    /// that staged it, in device and id order. A staging directory whose
    /// device this desktop cannot read is skipped: nothing can say which
    /// device's recording it is.
    pub fn staged_sessions(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for dir in self.tag_dirs("sessions") {
            let Ok(device) = syncsafe::read_to_string(dir.join(DEVICE_FILE)) else {
                log::warn!("sync: staged files at {} name no device", dir.display());
                continue;
            };
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
                if is_session_id(&name) && e.path().is_dir() {
                    out.push((device.clone(), name));
                }
            }
        }
        out.sort();
        out
    }

    /// The counter for the next tombstone this desktop originates, which
    /// section 8 requires to rise per device. Recorded as it is handed out.
    pub fn next_tombstone_seq(&self) -> u64 {
        let path = self.root.join("tombstone_seq");
        let next = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0)
            + 1;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = syncsafe::write_atomic(&path, next.to_string().as_bytes()) {
            log::warn!("sync: tombstone counter not saved: {e}");
        }
        next
    }

    /// Records that a device's session cannot be opened at these revisions
    /// and drops its staged copies: what a hello reads to stop asking for it,
    /// and what the settings screen lists.
    pub fn record_refusal(&self, session: &str, refusal: &Refusal) -> Result<(), StoreError> {
        if !is_session_id(session) {
            return Err(StoreError::BadName);
        }
        let path = self.refused_path(&refusal.device_id, session);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| StoreError::Io)?;
        }
        let bytes = serde_json::to_vec(refusal).map_err(|_| StoreError::Io)?;
        syncsafe::write_atomic(&path, &bytes).map_err(|_| StoreError::Io)?;
        self.clear_staged(&refusal.device_id, session)
    }

    /// The refusal standing against a device's session, if one does.
    pub fn refusal(&self, device: &str, session: &str) -> Option<Refusal> {
        if !is_session_id(session) {
            return None;
        }
        let bytes = std::fs::read(self.refused_path(device, session)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Lifts a refusal: the next hello asks for the session again.
    pub fn forget_refusal(&self, device: &str, session: &str) -> Result<(), StoreError> {
        if !is_session_id(session) {
            return Err(StoreError::BadName);
        }
        remove_file(&self.refused_path(device, session))
    }

    /// Lifts every refusal a phone's sessions carry, down to a record this
    /// build cannot read. Called when that phone pairs again, which is what
    /// can make its files open, and when it is unpaired.
    pub fn forget_refusals_of(&self, device_id: &str) {
        let dir = self.root.join("refused").join(device_tag(device_id));
        if dir.is_dir() {
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                log::warn!("sync: refusals of {device_id} not cleared: {e}");
            }
        }
    }

    /// Every session refused on this desktop, in id order. A record this
    /// build cannot read is left out: it names no reason to show.
    pub fn refused_sessions(&self) -> Vec<(String, Refusal)> {
        let mut out: Vec<(String, Refusal)> = Vec::new();
        for dir in self.tag_dirs("refused") {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let Some(name) = e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(str::to_string)
                else {
                    continue;
                };
                if !is_session_id(&name) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(e.path()) else { continue };
                if let Ok(refusal) = serde_json::from_slice(&bytes) {
                    out.push((name, refusal));
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Drops the record that a device's session is held here, so nothing
    /// offers it.
    pub fn forget_import(&self, device: &str, session: &str) -> Result<(), StoreError> {
        if !is_session_id(session) {
            return Err(StoreError::BadName);
        }
        remove_file(&self.imported_path(device, session))
    }

    /// Every session this desktop has imported, whoever sent it, in id order.
    pub fn imported_sessions(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for dir in self.tag_dirs("imported") {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            out.extend(
                entries
                    .flatten()
                    .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(str::to_string))
                    .filter(|name| is_session_id(name)),
            );
        }
        out.sort();
        out.dedup();
        out
    }

    fn claim_path(&self, device: &str, session: &str) -> PathBuf {
        self.root.join("claims").join(device_tag(device)).join(format!("{session}.json"))
    }

    fn imported_path(&self, device: &str, session: &str) -> PathBuf {
        self.root.join("imported").join(device_tag(device)).join(format!("{session}.json"))
    }

    /// Where a device's copy of a record waits: `records/<device>/<session>/
    /// <file>`, with `profile` as the session of the profile files. `None`
    /// for anything that is not one of the records.
    fn record_path(&self, device: &str, session: &str, file: &str) -> Option<PathBuf> {
        crate::sync::records::Channel::of_upload(session, file)
            .map(|_| self.root.join("records").join(device_tag(device)).join(session).join(file))
    }

    fn merge_path(&self, device: &str, session: &str, file: &str) -> Option<PathBuf> {
        self.record_path(device, session, file).map(|p| p.with_extension("merged.json"))
    }

    /// Stages a device's copy of a record once it hashes to `hash`: the hash
    /// the hello named it by, which the merge records.
    pub fn stage_record(&self, device: &str, session: &str, file: &str, hash: &str, body: &[u8]) -> Result<(), StoreError> {
        let path = self.record_path(device, session, file).ok_or(StoreError::BadName)?;
        if body.len() > FILE_CAP_BYTES {
            return Err(StoreError::TooLarge);
        }
        if body_hash(body) != hash {
            return Err(StoreError::HashMismatch);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| StoreError::Io)?;
        }
        syncsafe::write_atomic(&path, body).map_err(|_| StoreError::Io)
    }

    pub fn staged_record(&self, device: &str, session: &str, file: &str) -> Option<Vec<u8>> {
        std::fs::read(self.record_path(device, session, file)?).ok()
    }

    /// True when that device's revision of a record is staged here.
    pub fn has_staged_record(&self, device: &str, session: &str, file: &str, hash: &str) -> bool {
        self.staged_record(device, session, file).is_some_and(|b| body_hash(&b) == hash)
    }

    pub fn drop_staged_record(&self, device: &str, session: &str, file: &str) {
        if let Some(p) = self.record_path(device, session, file) {
            let _ = std::fs::remove_file(p);
        }
    }

    /// The records that device has staged, as (session, file).
    pub fn staged_records(&self, device: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (session, dir) in entries_in(&self.root.join("records").join(device_tag(device))) {
            for (file, _) in entries_in(&dir) {
                if crate::sync::records::Channel::of_upload(&session, &file).is_some() {
                    out.push((session.clone(), file));
                }
            }
        }
        out.sort();
        out
    }

    /// Records that a device's revision of a record was merged into this
    /// desktop's, and the ids that device has to rewrite for it.
    pub fn record_merge(
        &self,
        device: &str,
        session: &str,
        file: &str,
        hash: &str,
        ids: &BTreeMap<String, String>,
    ) -> Result<(), StoreError> {
        let path = self.merge_path(device, session, file).ok_or(StoreError::BadName)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| StoreError::Io)?;
        }
        let bytes = serde_json::to_vec(&serde_json::json!({ "hash": hash, "ids": ids })).map_err(|_| StoreError::Io)?;
        syncsafe::write_atomic(&path, &bytes).map_err(|_| StoreError::Io)
    }

    /// The revision of a record last merged for a device, with the ids it has
    /// to rewrite.
    pub fn merged_record(&self, device: &str, session: &str, file: &str) -> Option<(String, BTreeMap<String, String>)> {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(self.merge_path(device, session, file)?).ok()?).ok()?;
        let hash = v["hash"].as_str()?.to_string();
        let ids = serde_json::from_value(v["ids"].clone()).unwrap_or_default();
        Some((hash, ids))
    }

    /// Drops the legacy staging areas the records were kept in: a staged copy is asked for again at the next hello, and a merge record that goes makes the next hello ask for that revision again.
    pub fn drop_split_record_areas(&self) -> crate::error::Result<()> {
        drop_path(&self.root.join("profile"))?;
        drop_path(&self.root.join("meta"))
    }

    /// True when this desktop has buried that device's session. A tombstone
    /// with a device on record is that device's alone: another device's
    /// recording of the same id is untouched by it.
    pub fn is_entombed(&self, session: &str, device: &str) -> bool {
        is_session_id(session)
            && (self.tombstone_path(session).is_file() || self.tombstone_path_for(session, device).is_file())
    }

    /// The signed tombstone body a device is owed for a session this desktop
    /// has buried.
    pub fn tombstone(&self, session: &str, device: &str) -> Option<Vec<u8>> {
        if !is_session_id(session) {
            return None;
        }
        std::fs::read(self.tombstone_path_for(session, device))
            .or_else(|_| std::fs::read(self.tombstone_path(session)))
            .ok()
    }

    /// Moves an inbox keyed by session id alone under the device each record belongs to. An import record's device is the origin the library holds the session under; one whose manifest cannot be read fails the step and is tried again at the next unlock. A refusal names its own device, and one this build cannot read goes. Staged files go. Returns the records moved.
    pub fn adopt_device_paths(&self, sessions_dir: &Path) -> crate::error::Result<usize> {
        for (name, path) in entries_in(&self.root.join("sessions")) {
            if is_session_id(&name) {
                drop_path(&path)?;
            }
        }
        let mut moved = 0;
        for (session, path) in bare_records(&self.root.join("imported")) {
            match origin_of(sessions_dir, &session)? {
                Some(device) => {
                    move_record(&path, &self.imported_path(&device, &session))?;
                    moved += 1;
                }
                None => drop_path(&path)?,
            }
        }
        for (session, path) in bare_records(&self.root.join("refused")) {
            let device = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Refusal>(&b).ok())
                .map(|r| r.device_id);
            match device {
                Some(device) => {
                    move_record(&path, &self.refused_path(&device, &session))?;
                    moved += 1;
                }
                None => drop_path(&path)?,
            }
        }
        Ok(moved)
    }
}

/// The device a session in the library came from, or `None` when the library does not hold it. A directory whose manifest cannot be read is an error.
fn origin_of(sessions_dir: &Path, session: &str) -> crate::error::Result<Option<String>> {
    match syncsafe::read(sessions_dir.join(session).join("manifest.json")) {
        Ok(bytes) => Ok(serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|v| v["origin_device"].as_str().filter(|s| !s.is_empty()).map(str::to_string))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(crate::error::AppError::Io(format!("read the origin of {session}: {e}"))),
    }
}

fn entries_in(dir: &Path) -> Vec<(String, PathBuf)> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.path())))
        .collect()
}

/// The `<session>.json` records sitting directly in an area.
fn bare_records(dir: &Path) -> Vec<(String, PathBuf)> {
    entries_in(dir)
        .into_iter()
        .filter(|(_, p)| p.is_file())
        .filter_map(|(name, p)| {
            let session = name.strip_suffix(".json")?.to_string();
            is_session_id(&session).then_some((session, p))
        })
        .collect()
}

fn move_record(from: &Path, to: &Path) -> crate::error::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| crate::error::AppError::Io(format!("create {}: {e}", parent.display())))?;
    }
    std::fs::rename(from, to).map_err(|e| crate::error::AppError::Io(format!("move {}: {e}", from.display())))
}

fn drop_path(path: &Path) -> crate::error::Result<()> {
    let gone = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    match gone {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(crate::error::AppError::Io(format!("drop {}: {e}", path.display())))
        }
        _ => Ok(()),
    }
}

fn remove_dir(path: &Path) -> Result<(), StoreError> {
    if path.is_dir() {
        std::fs::remove_dir_all(path).map_err(|_| StoreError::Io)?;
    }
    Ok(())
}

fn remove_file(path: &Path) -> Result<(), StoreError> {
    if path.is_file() {
        std::fs::remove_file(path).map_err(|_| StoreError::Io)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHONE: &str = "4a68f72f5a22110b2bfbe71b6588833c";
    const TABLET: &str = "9d1c4e2b7a05f3681c4ea9b20d773f55";

    fn inbox() -> (Inbox, tempfile::TempDir) {
        let td = tempfile::tempdir().unwrap();
        (Inbox::at(td.path().join("inbox")), td)
    }

    fn refusal_of(device: &str, bytes: &[u8]) -> Refusal {
        Refusal {
            device_id: device.to_string(),
            reason: RefusalReason::Sealed,
            inputs: BTreeMap::from([("manifest".to_string(), body_hash(bytes))]),
        }
    }

    #[test]
    fn a_tombstone_buries_the_refusal_with_the_files() {
        let (inbox, _td) = inbox();
        let bytes = b"sealed elsewhere";
        inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(bytes), bytes).unwrap();
        let refusal = refusal_of(PHONE, bytes);
        inbox.record_refusal("daisy-1", &refusal).unwrap();
        assert!(inbox.read(PHONE, "daisy-1", "manifest.json").is_none(), "refusing drops the staged copies");
        assert_eq!(inbox.refused_sessions(), vec![("daisy-1".to_string(), refusal)]);

        inbox.entomb("daisy-1", Some(PHONE), b"{}").unwrap();
        assert!(inbox.refusal(PHONE, "daisy-1").is_none(), "a deleted session has nothing to report");
    }

    #[test]
    fn a_refusal_covers_the_revisions_it_recorded_and_no_others() {
        let refusal = Refusal {
            device_id: "phone".into(),
            reason: RefusalReason::Unsupported,
            inputs: BTreeMap::from([("manifest".to_string(), "sha256:aa".to_string())]),
        };
        let same = BTreeMap::from([
            ("manifest".to_string(), "sha256:aa".to_string()),
            ("speakers".to_string(), "sha256:cc".to_string()),
        ]);
        assert!(refusal.covers(&same), "an input that arrived later changes nothing about the refused one");
        let moved = BTreeMap::from([("manifest".to_string(), "sha256:a2".to_string())]);
        assert!(!refusal.covers(&moved));
    }

    #[test]
    fn a_session_id_is_daisy_and_digits() {
        assert!(is_session_id("daisy-1700000000"));
        assert!(!is_session_id("daisy-"));
        assert!(!is_session_id("../etc"));
        assert!(!is_session_id("daisy-12345678901234"));
        assert!(!is_session_id("daisy-17872w9858"));
    }

    #[test]
    fn a_wire_session_id_is_one_below_the_band_this_desktop_mints_in() {
        assert!(is_wire_session_id("daisy-1700000000"));
        assert!(!is_wire_session_id("daisy-900000000000"), "the first id this desktop mints");
        assert!(!is_wire_session_id("daisy-999999999999"));
        assert!(is_session_id("daisy-900000000000"), "a minted id is still a session id in the library");
        assert!(!is_wire_session_id("daisy-"));
    }

    #[test]
    fn a_relative_file_path_has_only_plain_components() {
        assert!(is_relative_file_path("manifest.json"));
        assert!(is_relative_file_path("chunks/0001/mic.m4a"));
        assert!(!is_relative_file_path("../manifest.json"));
        assert!(!is_relative_file_path("chunks/./mic.m4a"));
        assert!(!is_relative_file_path("/manifest.json"));
        assert!(!is_relative_file_path(""));
        assert!(!is_relative_file_path(".hidden"));
        assert!(!is_relative_file_path("chunks//mic.m4a"));
    }

    #[test]
    fn an_upload_named_like_a_plaintext_record_is_refused() {
        let (inbox, _td) = inbox();
        let forged = b"sha256:0000000000000000000000000000000000000000000000000000000000000000";
        let name = format!("{LIVE_TRANSCRIPT}.plaintext");
        assert_eq!(inbox.store(PHONE, "daisy-1", &name, &body_hash(forged), forged), Err(StoreError::BadName));
        assert_eq!(inbox.plaintext(PHONE, "daisy-1", LIVE_TRANSCRIPT), None);
    }

    #[test]
    fn a_device_never_reaches_a_path_as_itself() {
        let (inbox, td) = inbox();
        let bytes = b"x";
        inbox.store("../../escape", "daisy-1", "manifest.json", &body_hash(bytes), bytes).unwrap();
        assert_eq!(inbox.read("../../escape", "daisy-1", "manifest.json").unwrap(), bytes);
        assert!(td.path().join("inbox/sessions").is_dir(), "everything stayed inside the inbox");
        assert_eq!(std::fs::read_dir(td.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_file_lands_under_its_session_and_reads_back() {
        let (inbox, _td) = inbox();
        let bytes = b"manifest bytes";
        let hash = body_hash(bytes);
        inbox.store(PHONE, "daisy-1", "manifest.json", &hash, bytes).unwrap();
        assert!(inbox.has(PHONE, "daisy-1", "manifest.json", &hash));
        assert_eq!(inbox.read(PHONE, "daisy-1", "manifest.json").unwrap(), bytes);
    }

    #[test]
    fn two_devices_staging_one_id_keep_their_own_files() {
        let (inbox, _td) = inbox();
        let mine = b"the phone's manifest";
        let theirs = b"the tablet's manifest";
        inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(mine), mine).unwrap();
        inbox.store(TABLET, "daisy-1", "manifest.json", &body_hash(theirs), theirs).unwrap();

        assert_eq!(inbox.read(PHONE, "daisy-1", "manifest.json").unwrap(), mine);
        assert_eq!(inbox.read(TABLET, "daisy-1", "manifest.json").unwrap(), theirs);
        assert!(inbox.has(PHONE, "daisy-1", "manifest.json", &body_hash(mine)));
        assert!(!inbox.has(PHONE, "daisy-1", "manifest.json", &body_hash(theirs)), "neither holds the other's");
        assert_eq!(
            inbox.staged_sessions(),
            vec![(PHONE.to_string(), "daisy-1".to_string()), (TABLET.to_string(), "daisy-1".to_string())]
        );
    }

    #[test]
    fn one_devices_staged_files_are_only_that_devices_to_drop() {
        let (inbox, _td) = inbox();
        let bytes = b"x";
        inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(bytes), bytes).unwrap();
        inbox.store(TABLET, "daisy-1", "manifest.json", &body_hash(bytes), bytes).unwrap();

        inbox.entomb("daisy-1", Some(PHONE), b"{}").unwrap();
        assert!(inbox.read(PHONE, "daisy-1", "manifest.json").is_none());
        assert!(inbox.read(TABLET, "daisy-1", "manifest.json").is_some(), "the tablet's upload is not the phone's to bury");

        inbox.record_refusal("daisy-1", &refusal_of(TABLET, bytes)).unwrap();
        inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(bytes), bytes).unwrap();
        inbox.record_refusal("daisy-1", &refusal_of(PHONE, bytes)).unwrap();
        assert_eq!(inbox.refused_sessions().len(), 2, "each device is refused on its own");
        inbox.forget_refusals_of(PHONE);
        assert!(inbox.refusal(PHONE, "daisy-1").is_none());
        assert!(inbox.refusal(TABLET, "daisy-1").is_some(), "a re-pair lifts that phone's refusals alone");
    }

    #[test]
    fn a_refusal_this_build_cannot_read_is_not_listed_and_is_cleared_with_the_rest() {
        let (inbox, td) = inbox();
        let path = td.path().join("inbox/refused").join(device_tag(PHONE));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("daisy-1.json"), br#"{"device_id":"p","reason":"collision","inputs":{}}"#).unwrap();
        assert!(inbox.refused_sessions().is_empty(), "it names no reason to show");
        assert!(inbox.refusal(PHONE, "daisy-1").is_none(), "and stops nothing from arriving");

        inbox.forget_refusals_of(PHONE);
        assert!(!path.join("daisy-1.json").exists(), "and it can be cleaned up");
    }

    #[test]
    fn presence_is_answered_without_reading_the_file() {
        let (inbox, _td) = inbox();
        let bytes = b"chunk audio";
        assert!(!inbox.exists(PHONE, "daisy-1", "chunks/0001/mic.m4a"));
        inbox.store(PHONE, "daisy-1", "chunks/0001/mic.m4a", &body_hash(bytes), bytes).unwrap();
        assert!(inbox.exists(PHONE, "daisy-1", "chunks/0001/mic.m4a"));
        assert!(!inbox.exists(PHONE, "daisy-1", "../escape"), "a name that could escape is never here");
    }

    #[test]
    fn a_body_that_does_not_hash_to_its_name_is_refused() {
        let (inbox, _td) = inbox();
        let wrong = body_hash(b"other bytes");
        assert_eq!(
            inbox.store(PHONE, "daisy-1", "manifest.json", &wrong, b"manifest bytes"),
            Err(StoreError::HashMismatch)
        );
        assert!(!inbox.has(PHONE, "daisy-1", "manifest.json", &wrong));
    }

    #[test]
    fn storing_the_same_bytes_twice_is_the_same_file() {
        let (inbox, _td) = inbox();
        let bytes = b"manifest bytes";
        let hash = body_hash(bytes);
        inbox.store(PHONE, "daisy-1", "manifest.json", &hash, bytes).unwrap();
        inbox.store(PHONE, "daisy-1", "manifest.json", &hash, bytes).unwrap();
        assert_eq!(inbox.read(PHONE, "daisy-1", "manifest.json").unwrap(), bytes);
    }

    #[test]
    fn a_newer_revision_replaces_the_one_before_it() {
        let (inbox, _td) = inbox();
        let first = b"revision one";
        let second = b"revision two";
        inbox.store(PHONE, "daisy-1", "transcript.json", &body_hash(first), first).unwrap();
        inbox.store(PHONE, "daisy-1", "transcript.json", &body_hash(second), second).unwrap();
        assert_eq!(inbox.read(PHONE, "daisy-1", "transcript.json").unwrap(), second);
        assert!(!inbox.has(PHONE, "daisy-1", "transcript.json", &body_hash(first)));
    }

    #[test]
    fn a_name_that_could_escape_the_session_never_reaches_a_path() {
        let (inbox, _td) = inbox();
        let bytes = b"x";
        let hash = body_hash(bytes);
        assert_eq!(inbox.store(PHONE, "daisy-1", "../../escape.json", &hash, bytes), Err(StoreError::BadName));
        assert_eq!(inbox.store(PHONE, "../escape", "manifest.json", &hash, bytes), Err(StoreError::BadName));
    }

    #[test]
    fn a_body_over_the_cap_is_refused_before_it_is_written() {
        let (inbox, _td) = inbox();
        let big = vec![0u8; FILE_CAP_BYTES + 1];
        let hash = body_hash(&big);
        assert_eq!(inbox.store(PHONE, "daisy-1", "chunks/0001/mic.m4a", &hash, &big), Err(StoreError::TooLarge));
    }

    #[test]
    fn the_sessions_with_files_waiting_are_listed_with_the_device_that_sent_them() {
        let (inbox, _td) = inbox();
        assert!(inbox.staged_sessions().is_empty());
        let bytes = b"x";
        let hash = body_hash(bytes);
        inbox.store(PHONE, "daisy-1", "manifest.json", &hash, bytes).unwrap();
        inbox.store(PHONE, "daisy-2", "manifest.json", &hash, bytes).unwrap();
        assert_eq!(
            inbox.staged_sessions(),
            vec![(PHONE.to_string(), "daisy-1".to_string()), (PHONE.to_string(), "daisy-2".to_string())]
        );
        inbox.clear_staged(PHONE, "daisy-1").unwrap();
        assert_eq!(inbox.staged_sessions(), vec![(PHONE.to_string(), "daisy-2".to_string())]);
    }

    #[test]
    fn a_tombstoned_session_loses_what_it_had() {
        let (inbox, _td) = inbox();
        let bytes = b"manifest bytes";
        let hash = body_hash(bytes);
        inbox.store(PHONE, "daisy-1", "manifest.json", &hash, bytes).unwrap();
        inbox.entomb("daisy-1", Some(PHONE), b"{}").unwrap();
        assert!(inbox.is_entombed("daisy-1", PHONE));
        assert!(inbox.read(PHONE, "daisy-1", "manifest.json").is_none());
    }

    #[test]
    fn dropping_a_device_takes_everything_it_staged_and_nothing_of_anothers() {
        let (inbox, _td) = inbox();
        inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(b"m"), b"m").unwrap();
        inbox.store(TABLET, "daisy-1", "manifest.json", &body_hash(b"t"), b"t").unwrap();
        inbox.record_claim(PHONE, "daisy-1", &Claim::default()).unwrap();
        inbox.stage_record(PHONE, "daisy-1", "meta.json", &body_hash(b"{}"), b"{}").unwrap();
        inbox.record_import(PHONE, "daisy-2", &BTreeMap::new()).unwrap();

        inbox.drop_device(PHONE).unwrap();
        assert_eq!(inbox.staged_sessions(), vec![(TABLET.to_string(), "daisy-1".to_string())]);
        assert!(inbox.claim(PHONE, "daisy-1").is_none());
        assert!(inbox.staged_records(PHONE).is_empty());
        assert!(inbox.imported(PHONE, "daisy-2").is_some(), "what was imported stays known");
    }

    #[test]
    fn a_tombstone_is_the_devices_alone() {
        let (inbox, _td) = inbox();
        inbox.entomb("daisy-1", Some(PHONE), b"{}").unwrap();
        assert!(inbox.is_entombed("daisy-1", PHONE));
        assert!(!inbox.is_entombed("daisy-1", TABLET), "another device's recording of that id is its own");
        assert!(inbox.tombstone("daisy-1", TABLET).is_none());
    }

    #[test]
    fn a_tombstone_with_no_device_on_record_is_every_devices() {
        let (inbox, _td) = inbox();
        let bytes = b"x";
        inbox.store(PHONE, "daisy-1", "manifest.json", &body_hash(bytes), bytes).unwrap();
        inbox.store(TABLET, "daisy-1", "manifest.json", &body_hash(bytes), bytes).unwrap();
        inbox.entomb("daisy-1", None, b"{}").unwrap();
        assert!(inbox.is_entombed("daisy-1", PHONE));
        assert!(inbox.is_entombed("daisy-1", TABLET));
        assert_eq!(inbox.tombstone("daisy-1", TABLET).unwrap(), b"{}");
        assert!(inbox.staged_sessions().is_empty(), "every device's copy is gone");
    }

    #[test]
    fn a_tombstone_outlives_the_process() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().join("inbox");
        Inbox::at(&root).entomb("daisy-1", Some(PHONE), b"{}").unwrap();
        assert!(Inbox::at(&root).is_entombed("daisy-1", PHONE));
        assert_eq!(Inbox::at(&root).tombstone("daisy-1", PHONE).unwrap(), b"{}");
    }

    #[test]
    fn an_import_record_written_before_devices_moves_under_the_device_the_library_names() {
        let (inbox, td) = inbox();
        let root = td.path().join("inbox");
        let sessions = td.path().join("sessions");
        std::fs::create_dir_all(sessions.join("daisy-1")).unwrap();
        std::fs::write(
            sessions.join("daisy-1").join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({ "session_id": "daisy-1", "origin_device": PHONE })).unwrap(),
        )
        .unwrap();
        let inputs = BTreeMap::from([("manifest".to_string(), "sha256:aa".to_string())]);
        std::fs::create_dir_all(root.join("imported")).unwrap();
        std::fs::write(root.join("imported/daisy-1.json"), serde_json::to_vec(&inputs).unwrap()).unwrap();

        // A part-arrived upload and a refusal at the bare paths too.
        std::fs::create_dir_all(root.join("sessions/daisy-9")).unwrap();
        std::fs::write(root.join("sessions/daisy-9/manifest.json"), b"half here").unwrap();
        std::fs::create_dir_all(root.join("refused")).unwrap();
        std::fs::write(root.join("refused/daisy-2.json"), serde_json::to_vec(&refusal_of(TABLET, b"x")).unwrap())
            .unwrap();
        std::fs::write(root.join("refused/daisy-3.json"), b"not a refusal this build reads").unwrap();

        assert_eq!(inbox.adopt_device_paths(&sessions).unwrap(), 2);
        assert_eq!(inbox.imported(PHONE, "daisy-1"), Some(inputs), "the device does not upload its library again");
        assert_eq!(inbox.imported_sessions(), vec!["daisy-1".to_string()]);
        assert_eq!(inbox.refusal(TABLET, "daisy-2").unwrap().device_id, TABLET);
        assert!(!root.join("refused/daisy-3.json").exists(), "a record naming no device goes");
        assert!(!root.join("sessions/daisy-9").exists(), "a part-arrived upload is asked for again");

        assert_eq!(inbox.adopt_device_paths(&sessions).unwrap(), 0, "running it twice changes nothing");
        assert_eq!(inbox.imported(PHONE, "daisy-1").unwrap().len(), 1);
    }

    #[test]
    fn dropping_the_split_record_areas_of_an_inbox_that_never_had_them_succeeds() {
        let (inbox, td) = inbox();
        let root = td.path().join("inbox");
        inbox.drop_split_record_areas().unwrap();

        std::fs::create_dir_all(root.join("profile/daisy-1")).unwrap();
        std::fs::create_dir_all(root.join("meta")).unwrap();
        std::fs::write(root.join("meta/daisy-1.json"), b"{}").unwrap();
        inbox.drop_split_record_areas().unwrap();
        assert!(!root.join("profile").exists());
        assert!(!root.join("meta").exists());
        inbox.drop_split_record_areas().unwrap();
    }

    #[test]
    fn an_import_record_whose_session_cannot_be_read_stops_the_move() {
        let (inbox, td) = inbox();
        let root = td.path().join("inbox");
        let sessions = td.path().join("sessions");
        std::fs::create_dir_all(sessions.join("daisy-1")).unwrap();
        // A directory with no manifest stands in for one held open by a sync tool.
        std::fs::create_dir_all(root.join("imported")).unwrap();
        std::fs::write(root.join("imported/daisy-1.json"), b"{}").unwrap();
        assert_eq!(inbox.adopt_device_paths(&sessions).unwrap(), 0);
        assert!(!root.join("imported/daisy-1.json").exists(), "a session with no origin is not a phone's");
    }

    #[test]
    fn each_device_has_its_own_staged_records_and_merge_records() {
        let (inbox, _td) = inbox();
        let mine = br#"{"schema_version":1,"tags":[]}"#;
        let meta = br#"{"schema_version":1}"#;
        assert!(inbox.staged_records(PHONE).is_empty());
        inbox.stage_record(PHONE, "profile", "tags.json", &body_hash(mine), mine).unwrap();
        inbox.stage_record(PHONE, "daisy-1", "meta.json", &body_hash(meta), meta).unwrap();
        assert_eq!(
            inbox.staged_records(PHONE),
            [("daisy-1".to_string(), "meta.json".to_string()), ("profile".to_string(), "tags.json".to_string())]
        );
        assert!(inbox.staged_records(TABLET).is_empty(), "the tablet has sent nothing");
        assert!(inbox.has_staged_record(PHONE, "profile", "tags.json", &body_hash(mine)));
        assert!(!inbox.has_staged_record(TABLET, "profile", "tags.json", &body_hash(mine)));

        let ids = BTreeMap::from([("p1".to_string(), "d1".to_string())]);
        inbox.record_merge(PHONE, "profile", "tags.json", "sha256:phone", &ids).unwrap();
        assert_eq!(inbox.merged_record(PHONE, "profile", "tags.json"), Some(("sha256:phone".to_string(), ids)));
        assert!(inbox.merged_record(TABLET, "profile", "tags.json").is_none(), "the tablet's revision is its own");
        inbox.drop_staged_record(PHONE, "profile", "tags.json");
        assert_eq!(inbox.staged_records(PHONE).len(), 1, "a merge record is not a staged file");
    }

    #[test]
    fn only_a_record_reaches_a_path_in_the_record_area() {
        let (inbox, _td) = inbox();
        let body = b"{}";
        for (session, file) in [("profile", "notes.json"), ("profile", "../tags.json"), ("daisy-1", "tags.json"), ("daisy-900000000001", "meta.json")] {
            assert_eq!(inbox.stage_record(PHONE, session, file, &body_hash(body), body), Err(StoreError::BadName), "{session}/{file}");
        }
    }
}
