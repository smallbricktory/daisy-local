// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The four records both devices write: `tags.json`,
//! `contacts.json`, `voices.json` and each session's `meta.json`. One set of
//! rules merges all four:
//!
//! - every value carries the second it was written, a missing second reads
//!   as 0, the later second wins and a tie keeps the desktop's ([`replaces`]);
//! - a deleted or merged-away row leaves a [`Tombstone`], kept for good, and
//!   a row no later than its tombstone is dropped;
//! - rows that are one row collapse into the one with the later edit, and
//!   every id that lost gets a tombstone;
//! - unknown keys at every level are the union of both sides', the
//!   desktop's value on a clash ([`union_extra`]);
//! - the bytes are [`canonical`].
//!
//! A phone names each file by the hash of its plaintext; a revision this
//! desktop has not merged is wanted, merged off the connection, and offered
//! back with `merged_from` naming it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::Signer;
use serde::{Deserialize, Serialize};

pub use recording::session_meta::{canonical, Extra};

use crate::error::{AppError, Result};
use crate::state::{AppState, VaultState};
use crate::sync::exchange::{Offer, PROTOCOL_V};
use crate::sync::import::{self, MachineKey};
use crate::sync::inbox::{is_wire_session_id, Inbox};
use crate::sync::signing::body_hash;

/// Which device a value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    Phone,
    Desktop,
}

/// The one tie rule, for rows, cells, samples and tombstones alike: true
/// when the value arriving replaces the one held. The later second wins; on
/// the same second the desktop's stands.
pub fn replaces(held: (i64, Side), arriving: (i64, Side)) -> bool {
    arriving > held
}

/// Unknown keys from both sides: the union, the desktop's value on a clash.
pub fn union_extra(desktop: &Extra, phone: &Extra) -> Extra {
    let mut out = desktop.clone();
    for (k, v) in phone {
        out.entry(k.clone()).or_insert_with(|| v.clone());
    }
    out
}

/// A row that is gone: deleted, or merged `into` the row that stands for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub id: String,
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub into: Option<String>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl Tombstone {
    pub fn delete(id: &str, at: i64) -> Self {
        Self { id: id.to_string(), at, into: None, extra: Extra::new() }
    }
}

/// Tombstones by id, each with the side that wrote the one standing.
pub type Graves = BTreeMap<String, (Tombstone, Side)>;

/// Both sides' tombstones, one per id.
pub fn union_tombstones(desktop: &[Tombstone], phone: &[Tombstone]) -> Graves {
    let mut out = Graves::new();
    for (list, side) in [(desktop, Side::Desktop), (phone, Side::Phone)] {
        for t in list {
            bury(&mut out, t.clone(), side);
        }
    }
    out
}

/// Records a tombstone. One already held for the id stands unless this one
/// [`replaces`] it, `into` and all; the unknown keys of both are kept.
pub fn bury(graves: &mut Graves, t: Tombstone, side: Side) {
    let Some((held, held_side)) = graves.get_mut(&t.id) else {
        graves.insert(t.id.clone(), (t, side));
        return;
    };
    let wins = replaces((held.at, *held_side), (t.at, side));
    let extra = if (side, wins) > (*held_side, !wins) {
        union_extra(&t.extra, &held.extra)
    } else {
        union_extra(&held.extra, &t.extra)
    };
    if wins {
        *held = t;
        *held_side = side;
    }
    held.extra = extra;
}

/// The tombstones in the order the file holds them: by id, comparing bytes.
pub fn tombstones(graves: Graves) -> Vec<Tombstone> {
    graves.into_values().map(|(t, _)| t).collect()
}

/// The id that stands for `id`: itself while it is live, else the end of its
/// `into` chain. `None` when the chain ends at a delete, at an id nothing
/// holds, or in a cycle.
pub fn standing(id: &str, live: &BTreeSet<String>, dead: &[Tombstone]) -> Option<String> {
    let mut at = id.to_string();
    for _ in 0..=dead.len() {
        if live.contains(&at) {
            return Some(at);
        }
        at = dead.iter().find(|d| d.id == at)?.into.clone()?;
    }
    None
}

/// What a set of tombstones asks of every reference: ids to follow to the id
/// that stands, and ids deleted, each with its tombstone's second.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Retirements {
    pub moved: BTreeMap<String, String>,
    pub gone: BTreeMap<String, i64>,
}

impl Retirements {
    pub fn of<'a>(tombs: impl IntoIterator<Item = &'a Tombstone>, live: &BTreeSet<String>, dead: &[Tombstone]) -> Self {
        let mut r = Self::default();
        for t in tombs {
            if live.contains(&t.id) {
                continue;
            }
            match standing(&t.id, live, dead) {
                Some(to) => {
                    r.moved.insert(t.id.clone(), to);
                }
                None => {
                    r.gone.insert(t.id.clone(), t.at);
                }
            }
        }
        r
    }

    pub fn is_empty(&self) -> bool {
        self.moved.is_empty() && self.gone.is_empty()
    }

    pub fn gone_ids(&self) -> BTreeSet<String> {
        self.gone.keys().cloned().collect()
    }
}

/// One row of a file that merges by identity.
pub trait Record: Clone {
    fn id(&self) -> &str;
    /// The same row under a different id.
    fn same(&self, other: &Self) -> bool;
    fn updated_at(&self) -> Option<i64>;
    /// The row's latest moment, which a tombstone is weighed against.
    fn row_time(&self) -> i64;
    fn extra(&self) -> &Extra;
    fn extra_mut(&mut self) -> &mut Extra;
    /// What the row that stands keeps from every row it stands for. `group`
    /// holds each of them once, itself included, the desktop's first and
    /// this row first among its own side's.
    fn absorb(&mut self, group: &[(&Self, Side)]);
    /// A row that outlived its tombstone keeps only what came after it.
    fn outlive(&mut self, _at: i64) {}
}

/// Two sides' rows merged.
pub struct Merged<T> {
    pub rows: Vec<T>,
    pub graves: Graves,
    /// Every id that lost, to the id that stands for it.
    pub retired: BTreeMap<String, String>,
    held: [BTreeSet<String>; 2],
}

impl<T> Merged<T> {
    /// The ids one side held that lost, to the id that stands for each.
    pub fn retired_on(&self, side: Side) -> BTreeMap<String, String> {
        let held = &self.held[side as usize];
        self.retired.iter().filter(|(id, _)| held.contains(*id)).map(|(a, b)| (a.clone(), b.clone())).collect()
    }
}

fn rank<T: Record>((row, side): &(T, Side)) -> (i64, Side, std::cmp::Reverse<Vec<u8>>) {
    (row.updated_at().unwrap_or(0), *side, std::cmp::Reverse(row.id().as_bytes().to_vec()))
}

/// Merges two sides' rows, the same on either side of the seam:
///
/// 1. a row no later than its tombstone is dropped, and one later keeps only
///    what came after it;
/// 2. rows with one id, or [`Record::same`] — two of one side's included —
///    are one row;
/// 3. the row that stands is the later `updated_at_unix_seconds` (missing is
///    0), then the desktop's, then the smaller id; it absorbs the rest and
///    takes the union of their unknown keys, the desktop's on a clash;
/// 4. every id that lost is buried at the later of its row's and the
///    winner's latest moment, `into` the winner when `into` is set.
pub fn merge_rows<T: Record>(desktop: &[T], phone: &[T], mut graves: Graves, into: bool) -> Merged<T> {
    let mut rows: Vec<(T, Side)> = Vec::new();
    let mut held = [BTreeSet::new(), BTreeSet::new()];
    for (list, side) in [(desktop, Side::Desktop), (phone, Side::Phone)] {
        for row in list {
            let mut row = row.clone();
            if let Some((t, _)) = graves.get(row.id()) {
                if row.row_time() <= t.at {
                    continue;
                }
                row.outlive(t.at);
            }
            held[side as usize].insert(row.id().to_string());
            rows.push((row, side));
        }
    }

    let n = rows.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..n {
        for j in (i + 1)..n {
            let (a, b) = (&rows[i].0, &rows[j].0);
            if a.id() == b.id() || a.same(b) {
                let (ri, rj) = (root(&mut parent, i), root(&mut parent, j));
                parent[ri.max(rj)] = ri.min(rj);
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }

    let mut out = Vec::new();
    let mut retired = BTreeMap::new();
    for members in groups.values() {
        let w = *members.iter().max_by_key(|&&i| rank(&rows[i])).expect("a group has a member");
        let mut ordered = members.clone();
        ordered.sort_by(|&a, &b| {
            rows[b].1
                .cmp(&rows[a].1)
                .then((b == w).cmp(&(a == w)))
                .then_with(|| rows[a].0.id().as_bytes().cmp(rows[b].0.id().as_bytes()))
        });
        let group: Vec<(&T, Side)> = ordered.iter().map(|&i| (&rows[i].0, rows[i].1)).collect();
        let mut winner = rows[w].0.clone();
        winner.absorb(&group);
        let mut extra = Extra::new();
        for (row, _) in &group {
            for (k, v) in row.extra() {
                extra.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        *winner.extra_mut() = extra;
        let winner_time = rows[w].0.row_time();
        for &i in members {
            let (row, side) = &rows[i];
            if row.id() == winner.id() {
                continue;
            }
            retired.insert(row.id().to_string(), winner.id().to_string());
            let t = Tombstone {
                id: row.id().to_string(),
                at: winner_time.max(row.row_time()),
                into: into.then(|| winner.id().to_string()),
                extra: Extra::new(),
            };
            bury(&mut graves, t, *side);
        }
        out.push(winner);
    }
    out.sort_by(|a, b| a.id().as_bytes().cmp(b.id().as_bytes()));
    Merged { rows: out, graves, retired, held }
}

/// Deletes row `id` from this desktop's copy and buries it no earlier than
/// the row's own latest moment, so every copy of it drops. Returns the
/// tombstone's second.
pub fn delete_row<T: Record>(rows: &mut Vec<T>, deleted: &mut Vec<Tombstone>, id: &str, at: i64) -> i64 {
    let at = rows.iter().find(|r| r.id() == id).map_or(at, |r| r.row_time().max(at));
    rows.retain(|r| r.id() != id);
    let mut graves = union_tombstones(deleted, &[]);
    bury(&mut graves, Tombstone::delete(id, at), Side::Desktop);
    *deleted = tombstones(graves);
    at
}

// ---- the profile files --------------------------------------------------------

/// A profile file at `path`; an absent file is the empty one.
pub fn read_file<F: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<F> {
    if !path.is_file() {
        return Ok(F::default());
    }
    let bytes = syncsafe::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        AppError::Config(format!("parse {name}: {e}"))
    })
}

/// Writes a profile file's bytes and drops the cached parse of it.
pub fn write_file(path: &Path, bytes: Vec<u8>) -> Result<()> {
    syncsafe::write_atomic(path, bytes)?;
    crate::commands::file_cache::invalidate(path);
    Ok(())
}

/// Takes a profile file's write lock, which is held from the read to the
/// write of every change to it.
pub fn hold(lock: &'static std::sync::Mutex<()>) -> std::sync::MutexGuard<'static, ()> {
    lock.lock().unwrap_or_else(|e| e.into_inner())
}

/// Reads the file at `path`, applies `f` and writes the result as `encode`
/// spells it, with no other writer holding `lock` in between.
pub fn edit_file<F: serde::de::DeserializeOwned + Default, R>(
    lock: &'static std::sync::Mutex<()>,
    path: &Path,
    encode: fn(&F) -> serde_json::Result<Vec<u8>>,
    f: impl FnOnce(&mut F) -> Result<R>,
) -> Result<R> {
    let _held = hold(lock);
    let mut file = read_file(path)?;
    let out = f(&mut file)?;
    write_file(path, encode(&file)?)?;
    Ok(out)
}

/// A file merged, and the ids each side has to rewrite: each id that side
/// held that lost, to the id that stands for it.
#[derive(Debug, Clone, PartialEq)]
pub struct FileMerge<F> {
    pub file: F,
    /// Every id that lost, to the id that stands for it.
    pub retired: BTreeMap<String, String>,
    pub desktop_ids: BTreeMap<String, String>,
    pub phone_ids: BTreeMap<String, String>,
}

impl<F> FileMerge<F> {
    pub fn of<T>(file: F, m: &Merged<T>) -> Self {
        Self {
            file,
            retired: m.retired.clone(),
            desktop_ids: m.retired_on(Side::Desktop),
            phone_ids: m.retired_on(Side::Phone),
        }
    }
}

// ---- the wire ---------------------------------------------------------------

/// The `X-Daisy-Session` the profile files are uploaded under.
pub const PROFILE: &str = "profile";

/// The four records, each with the name it travels under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Tags,
    Contacts,
    Voices,
    Meta,
}

impl Channel {
    /// The records under `profile`, in the order a reply names them.
    pub const PROFILE: [Channel; 3] = [Channel::Tags, Channel::Contacts, Channel::Voices];

    /// The key a hello's `profile` names the file by.
    pub fn hello_key(self) -> &'static str {
        match self {
            Channel::Tags => "tags",
            Channel::Contacts => "contacts",
            Channel::Voices => "voices",
            Channel::Meta => "meta",
        }
    }

    pub fn file(self) -> &'static str {
        match self {
            Channel::Tags => "tags.json",
            Channel::Contacts => "contacts.json",
            Channel::Voices => "voices.json",
            Channel::Meta => recording::session_meta::FILE_NAME,
        }
    }

    /// The record an upload carries, from its `X-Daisy-Session` and
    /// `X-Daisy-File`; `None` for anything else.
    pub fn of_upload(session: &str, file: &str) -> Option<Channel> {
        if session == PROFILE {
            return Channel::PROFILE.into_iter().find(|c| c.file() == file);
        }
        (is_wire_session_id(session) && file == Channel::Meta.file()).then_some(Channel::Meta)
    }
}

/// The one artifact header shape: the fields every header carries, and
/// the ones this artifact adds.
pub fn offer(
    key: &ed25519_dalek::SigningKey,
    session: &str,
    name: &str,
    artifact_id: String,
    size: u64,
    fields: serde_json::Value,
) -> Offer {
    let mut header = serde_json::json!({
        "v": PROTOCOL_V,
        "artifact_id": artifact_id,
        "session": session,
        "name": name,
        "size": size,
    });
    if let (Some(h), serde_json::Value::Object(more)) = (header.as_object_mut(), fields) {
        h.extend(more);
    }
    let header_bytes = serde_json::to_vec(&header).unwrap_or_default();
    Offer {
        artifact_id,
        session: session.to_string(),
        name: name.to_string(),
        size,
        header_sig_b64: B64.encode(key.sign(&header_bytes).to_bytes()),
        header_b64: B64.encode(&header_bytes),
    }
}

/// What one record owes the phone and is owed by it. `mine` is this
/// desktop's bytes, `theirs` the hash the phone named. A revision this
/// desktop holds, or holds staged, asks for nothing; one it merged is
/// answered with the merged file under `merged_from`; any other is wanted.
/// A phone that names none is given this desktop's file with `merged_from`
/// null, which it installs whole.
pub fn exchange(
    inbox: &Inbox,
    device: &str,
    session: &str,
    channel: Channel,
    mine: Option<&[u8]>,
    theirs: Option<&str>,
    key: &ed25519_dalek::SigningKey,
) -> (bool, Option<Offer>) {
    let file = channel.file();
    let give = |merged_from: Option<&str>, ids: &BTreeMap<String, String>| {
        let mut fields = serde_json::json!({
            // Built from no session's inputs; the phone decodes one shape.
            "built_from": {},
            "merged_from": merged_from,
        });
        if channel != Channel::Meta {
            fields["ids"] = serde_json::json!(ids);
        }
        mine.map(|bytes| offer(key, session, file, body_hash(bytes), bytes.len() as u64, fields))
    };
    match theirs {
        None => (false, give(None, &BTreeMap::new())),
        Some(theirs) => {
            if mine.map(body_hash).as_deref() == Some(theirs) || inbox.has_staged_record(device, session, file, theirs) {
                return (false, None);
            }
            match inbox.merged_record(device, session, file) {
                Some((merged_from, ids)) if merged_from == theirs => (false, give(Some(theirs), &ids)),
                _ => (true, None),
            }
        }
    }
}

/// This desktop's bytes of a profile record; `None` for a file it does not
/// hold. `voices` is the vault's file, which is none while it holds nothing.
pub fn profile_bytes(profile_root: &Path, channel: Channel, voices: Option<&[u8]>) -> Option<Vec<u8>> {
    match channel {
        Channel::Voices => voices.map(<[u8]>::to_vec),
        Channel::Meta => None,
        _ => std::fs::read(profile_root.join(channel.file())).ok(),
    }
}

/// What a hello's `profile` earns: the files wanted from the phone and the
/// merged files offered back.
pub fn profile_exchange(
    inbox: &Inbox,
    profile_root: &Path,
    device: &str,
    phone: &BTreeMap<String, String>,
    voices: Option<&[u8]>,
    key: &ed25519_dalek::SigningKey,
) -> (Vec<String>, Vec<Offer>) {
    let mut wants = Vec::new();
    let mut offers = Vec::new();
    for channel in Channel::PROFILE {
        let mine = profile_bytes(profile_root, channel, voices);
        let theirs = phone.get(channel.hello_key()).map(String::as_str);
        let (want, offer) = exchange(inbox, device, PROFILE, channel, mine.as_deref(), theirs, key);
        if want {
            wants.push(channel.file().to_string());
        }
        offers.extend(offer);
    }
    log::info!(
        "sync: profile hello — phone {:?}, wants {:?}, offers {:?}",
        phone.keys().collect::<Vec<_>>(),
        wants,
        offers.iter().map(|o| format!("{} {}", o.name, o.artifact_id)).collect::<Vec<_>>()
    );
    (wants, offers)
}

/// What one session's `meta.json` earns. Only a file that exists travels:
/// where neither device has authored one, both read the same values off the
/// manifest at the same second and there is nothing to settle.
pub fn meta_exchange(
    inbox: &Inbox,
    sessions_dir: &Path,
    minted: &import::MintedIds,
    device: &str,
    session: &str,
    theirs: Option<&str>,
    key: &ed25519_dalek::SigningKey,
) -> (Option<String>, Option<Offer>) {
    if !is_wire_session_id(session) {
        return (None, None);
    }
    let local = import::local_id(minted, device, session);
    let mine = syncsafe::read(recording::session_meta::path(&sessions_dir.join(local))).ok();
    let (want, offer) = exchange(inbox, device, session, Channel::Meta, mine.as_deref(), theirs, key);
    (want.then(|| session.to_string()), offer)
}

/// The bytes of a record this desktop holds, by the hash a phone asks for:
/// a profile file, or the `meta.json` of a session a phone sent.
pub fn artifact(inbox: &Inbox, sessions_dir: &Path, voices: Option<&[u8]>, id: &str) -> Option<Vec<u8>> {
    if let Some(root) = sessions_dir.parent() {
        for channel in Channel::PROFILE {
            if let Some(bytes) = profile_bytes(root, channel, voices).filter(|b| body_hash(b) == id) {
                return Some(bytes);
            }
        }
    }
    inbox
        .imported_sessions()
        .into_iter()
        .filter_map(|s| syncsafe::read(recording::session_meta::path(&sessions_dir.join(s))).ok())
        .find(|bytes| body_hash(bytes) == id)
}

// ---- merging what arrived -----------------------------------------------------

/// What the catalog has retired, over every tombstone it holds.
pub fn tag_retirements(profile_root: &Path) -> Retirements {
    let tags = read_file::<crate::commands::tags::TagsFile>(&profile_root.join(Channel::Tags.file())).unwrap_or_default();
    let live: BTreeSet<String> = tags.tags.iter().map(|t| t.id.clone()).collect();
    Retirements::of(&tags.deleted, &live, &tags.deleted)
}

/// What the catalog and the contact list have retired, over every tombstone
/// they hold: what a reference arriving from the phone is followed through.
pub fn catalog_retirements(profile_root: &Path) -> (Retirements, Retirements) {
    let contacts: crate::commands::contacts::ContactsFile =
        read_file(&profile_root.join(Channel::Contacts.file())).unwrap_or_default();
    let live: BTreeSet<String> = contacts.contacts.iter().map(|c| c.id.clone()).collect();
    (tag_retirements(profile_root), Retirements::of(&contacts.deleted, &live, &contacts.deleted))
}

/// Follows every id a session's file names through what the catalog and the
/// contact list retired.
pub fn settle_references(meta: &mut recording::session_meta::SessionMeta, tags: &Retirements, contacts: &Retirements) {
    meta.retire_tags(&tags.moved, &tags.gone_ids());
    meta.retire_contacts(&contacts.moved, &contacts.gone_ids());
}

/// Merges every staged `meta.json` from a device into this desktop's and
/// records the revision merged, so the next hello offers the result back. A
/// file this build cannot read is not merged and not kept.
pub fn merge_staged_meta(inbox: &Inbox, sessions_dir: &Path, device: &str) {
    let minted = import::minted_ids(sessions_dir);
    let (tags, contacts) = sessions_dir.parent().map(catalog_retirements).unwrap_or_default();
    for (session, file) in inbox.staged_records(device) {
        if Channel::of_upload(&session, &file) != Some(Channel::Meta) {
            continue;
        }
        let Some(bytes) = inbox.staged_record(device, &session, &file) else { continue };
        let Ok(theirs) = serde_json::from_slice::<recording::session_meta::SessionMeta>(&bytes) else {
            inbox.drop_staged_record(device, &session, &file);
            continue;
        };
        let dir = sessions_dir.join(import::local_id(&minted, device, &session));
        if !dir.is_dir() {
            continue;
        }
        let merged = recording::session_meta::update(&dir, |mine| {
            *mine = crate::sync::session_meta::merge(mine, &theirs);
            settle_references(mine, &tags, &contacts);
        });
        if let Err(e) = merged {
            log::warn!("sync: {session}/meta.json from {device} could not be written: {e}");
            continue;
        }
        if let Err(e) = inbox.record_merge(device, &session, &file, &body_hash(&bytes), &BTreeMap::new()) {
            log::warn!("sync: {session}/meta.json merge not recorded: {e:?}");
        }
        inbox.drop_staged_record(device, &session, &file);
    }
}

/// Merges every profile file one phone has staged. Returns true when a file
/// here changed. A file this desktop cannot open is dropped and asked for
/// again at the next hello.
pub fn merge_staged_profile(app: &AppState, vs: &VaultState, inbox: &Inbox, device: &str, key: &MachineKey) -> bool {
    let mut changed = false;
    for channel in Channel::PROFILE {
        let file = channel.file();
        let Some(bytes) = inbox.staged_record(device, PROFILE, file) else { continue };
        let plain = match import::open(&bytes, key) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("sync: profile {file} from the phone does not open here: {e:?}");
                inbox.drop_staged_record(device, PROFILE, file);
                continue;
            }
        };
        let phone_ids = match channel {
            Channel::Tags => merge_tags(app, &plain),
            Channel::Contacts => merge_contacts(app, vs, &plain),
            _ => merge_voices(app, vs, &plain),
        };
        match phone_ids {
            Ok(ids) => {
                if let Err(e) = inbox.record_merge(device, PROFILE, file, &body_hash(&bytes), &ids) {
                    log::warn!("sync: profile {file} merge not recorded: {e:?}");
                }
                log::info!("sync: merged the phone's {file} ({} id(s) for the phone to rewrite)", ids.len());
                changed = true;
            }
            Err(e) => log::warn!("sync: profile {file} not merged: {e}"),
        }
        inbox.drop_staged_record(device, PROFILE, file);
    }
    changed
}

fn parse<T: serde::de::DeserializeOwned>(file: &str, plain: &[u8]) -> Result<T> {
    serde_json::from_slice(plain).map_err(|e| AppError::Config(format!("parse the phone's {file}: {e}")))
}

/// The tombstones a merge added or moved: the ones this desktop has yet to
/// follow.
fn arrived<'a>(merged: &'a [Tombstone], held: &[Tombstone]) -> Vec<&'a Tombstone> {
    merged.iter().filter(|t| !held.contains(t)).collect()
}

fn merge_tags(app: &AppState, plain: &[u8]) -> Result<BTreeMap<String, String>> {
    let theirs: crate::commands::tags::TagsFile = parse("tags.json", plain)?;
    let (m, fresh) = crate::commands::tags::edit_tags(app, |mine| {
        let m = crate::commands::tags::merge_files(mine, &theirs);
        let live: BTreeSet<String> = m.file.tags.iter().map(|t| t.id.clone()).collect();
        let fresh = Retirements::of(arrived(&m.file.deleted, &mine.deleted), &live, &m.file.deleted);
        *mine = m.file.clone();
        Ok((m, fresh))
    })?;
    apply_retirements(app, None, Channel::Tags, &fresh)?;
    Ok(m.phone_ids)
}

fn merge_contacts(app: &AppState, vs: &VaultState, plain: &[u8]) -> Result<BTreeMap<String, String>> {
    let theirs: crate::commands::contacts::ContactsFile = parse("contacts.json", plain)?;
    let (m, fresh, every) = crate::commands::contacts::edit_contacts(app, |mine| {
        let m = crate::commands::contacts::merge_files(mine, &theirs);
        let live: BTreeSet<String> = m.file.contacts.iter().map(|c| c.id.clone()).collect();
        let fresh = Retirements::of(arrived(&m.file.deleted, &mine.deleted), &live, &m.file.deleted);
        let every = Retirements::of(&m.file.deleted, &live, &m.file.deleted);
        *mine = m.file.clone();
        Ok((m, fresh, every))
    })?;
    // A deleted contact's voices go on every merge, so one that failed
    // partway catches up on the next; the other references follow only the
    // tombstones new here.
    retire_voices_of(app, vs, &every.gone)?;
    apply_retirements(app, Some(vs), Channel::Contacts, &fresh)?;
    Ok(m.phone_ids)
}

fn merge_voices(app: &AppState, vs: &VaultState, plain: &[u8]) -> Result<BTreeMap<String, String>> {
    use crate::sync::voices;
    let theirs = voices::decode(plain).map_err(|e| AppError::Config(format!("parse the phone's voices.json: {e}")))?;
    let mut merged = None;
    crate::commands::voiceprints::update_vault(app, vs, |keys| {
        let m = voices::merge(&voices::project(keys), &theirs);
        voices::install(keys, m.file.clone(), &m.retired);
        merged = Some(m);
        Ok(())
    })?;
    let m = merged.expect("the vault ran the merge");
    let moved = Retirements { moved: m.desktop_ids.clone(), gone: BTreeMap::new() };
    apply_retirements(app, Some(vs), Channel::Voices, &moved)?;
    Ok(m.phone_ids)
}

// ---- following a retirement everywhere ---------------------------------------

/// Every reference this desktop holds follows what a record retired: ids
/// that moved point at the id that stands, ids deleted go. Called by every
/// local delete and every merge. Names on sessions live in each `meta.json`,
/// and the manifest follows from there.
pub fn apply_retirements(app: &AppState, vs: Option<&VaultState>, channel: Channel, r: &Retirements) -> Result<()> {
    if r.is_empty() {
        return Ok(());
    }
    let gone = r.gone_ids();
    match channel {
        Channel::Tags => {
            each_session_meta(app, |m| m.retire_tags(&r.moved, &gone));
            rewrite_workflows(app, &r.moved, &BTreeMap::new())?;
        }
        Channel::Contacts => {
            each_session_meta(app, |m| m.retire_contacts(&r.moved, &gone));
            rewrite_workflows(app, &BTreeMap::new(), &r.moved)?;
            if let Some(vs) = vs {
                crate::commands::voiceprints::update_vault(app, vs, |keys| {
                    for v in &mut keys.voiceprints {
                        if let Some(to) = v.contact_id.as_deref().and_then(|c| r.moved.get(c)) {
                            v.contact_id = Some(to.clone());
                        }
                    }
                    Ok(())
                })?;
                retire_voices_of(app, vs, &r.gone)?;
            }
        }
        Channel::Voices => rewrite_voiceprint_ids(app, &r.moved)?,
        Channel::Meta => {}
    }
    Ok(())
}

/// A deleted contact takes its voices with it, each at the later of the
/// contact's delete and the voice's own latest moment.
fn retire_voices_of(app: &AppState, vs: &VaultState, gone: &BTreeMap<String, i64>) -> Result<()> {
    let voiced = {
        let g = vs.keys.lock().unwrap();
        let keys = g.as_ref().ok_or(AppError::VaultLocked)?;
        keys.voiceprints.iter().any(|v| v.contact_id.as_ref().is_some_and(|c| gone.contains_key(c)))
    };
    if !voiced {
        return Ok(());
    }
    crate::commands::voiceprints::update_vault(app, vs, |keys| {
        crate::sync::voices::retire_contacts(keys, gone);
        Ok(())
    })
}

fn each_session_meta(app: &AppState, mut edit: impl FnMut(&mut recording::session_meta::SessionMeta)) {
    let mut touched = false;
    for dir in crate::commands::session_store::session_dirs(&app.profile.sessions_dir()).unwrap_or_default() {
        match recording::session_meta::update(&dir, &mut edit) {
            Ok(changed) => touched |= changed,
            Err(e) => log::warn!("sync: {}/meta.json could not follow the retired ids: {e}", dir.display()),
        }
    }
    if touched {
        crate::commands::library::invalidate_library_cache();
    }
}

/// Speaker rows name a voice by id, which is this machine's own and lives on
/// the manifest; one merged into another follows it.
fn rewrite_voiceprint_ids(app: &AppState, ids: &BTreeMap<String, String>) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    for dir in crate::commands::session_store::session_dirs(&app.profile.sessions_dir()).unwrap_or_default() {
        if !dir.join("manifest.json").is_file() {
            continue;
        }
        let followed = recording::session::Session::load(&dir).and_then(|mut s| {
            s.update_manifest(|m| {
                for l in &mut m.speaker_map {
                    if let Some(to) = l.voiceprint_id.as_deref().and_then(|v| ids.get(v)) {
                        l.voiceprint_id = Some(to.clone());
                    }
                }
            })
        });
        if let Err(e) = followed {
            log::warn!("sync: {} could not follow the merged voices: {e}", dir.display());
        }
    }
    crate::commands::library::invalidate_library_cache();
    Ok(())
}

fn map_conditions(
    node: &mut crate::commands::workflows::ConditionNode,
    tags: &BTreeMap<String, String>,
    contacts: &BTreeMap<String, String>,
) -> bool {
    use crate::commands::workflows::ConditionNode::*;
    let follow = |id: &mut String, ids: &BTreeMap<String, String>| match ids.get(id.as_str()) {
        Some(to) => {
            *id = to.clone();
            true
        }
        None => false,
    };
    match node {
        All { children } | Any { children } => children.iter_mut().map(|n| map_conditions(n, tags, contacts)).filter(|c| *c).count() > 0,
        HasTag { tag_id } => follow(tag_id, tags),
        HasParticipant { contact_id } => follow(contact_id, contacts),
        TitleContains { .. } => false,
    }
}

fn rewrite_workflows(app: &AppState, tags: &BTreeMap<String, String>, contacts: &BTreeMap<String, String>) -> Result<()> {
    if tags.is_empty() && contacts.is_empty() {
        return Ok(());
    }
    let mut file = crate::commands::workflows::load_workflows_file(app)?;
    let changed = file.workflows.iter_mut().map(|w| map_conditions(&mut w.condition, tags, contacts)).filter(|c| *c).count() > 0;
    if changed {
        crate::commands::workflows::save_workflows_file(app, &file)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Row {
        id: String,
        key: String,
        at: Option<i64>,
        extra: Extra,
    }

    impl Record for Row {
        fn id(&self) -> &str {
            &self.id
        }
        fn same(&self, other: &Self) -> bool {
            self.key == other.key
        }
        fn updated_at(&self) -> Option<i64> {
            self.at
        }
        fn row_time(&self) -> i64 {
            self.at.unwrap_or(0)
        }
        fn extra(&self) -> &Extra {
            &self.extra
        }
        fn extra_mut(&mut self) -> &mut Extra {
            &mut self.extra
        }
        fn absorb(&mut self, _group: &[(&Self, Side)]) {}
    }

    fn row(id: &str, key: &str, at: Option<i64>) -> Row {
        Row { id: id.into(), key: key.into(), at, extra: Extra::new() }
    }

    #[test]
    fn a_tie_keeps_the_desktops_and_a_missing_second_is_zero() {
        assert!(!replaces((10, Side::Desktop), (10, Side::Phone)));
        assert!(replaces((10, Side::Phone), (10, Side::Desktop)));
        let m = merge_rows(&[row("d", "k", None)], &[row("p", "k", Some(0))], Graves::new(), true);
        assert_eq!(m.rows[0].id, "d", "no second and 0 are the same second, and the desktop's stands");
        let m = merge_rows(&[row("d", "k", None)], &[row("p", "k", Some(1))], Graves::new(), true);
        assert_eq!(m.rows[0].id, "p");
    }

    #[test]
    fn two_rows_of_one_side_collapse_into_the_smaller_id_on_a_tie() {
        let m = merge_rows(&[row("b", "k", Some(5)), row("a", "k", Some(5))], &[], Graves::new(), true);
        assert_eq!(m.rows.len(), 1);
        assert_eq!(m.rows[0].id, "a");
        assert_eq!(m.retired_on(Side::Desktop), BTreeMap::from([("b".to_string(), "a".to_string())]));
        let (t, _) = &m.graves["b"];
        assert_eq!((t.at, t.into.as_deref()), (5, Some("a")));
    }

    #[test]
    fn a_row_no_later_than_its_tombstone_is_gone_and_a_later_one_stands() {
        let graves = union_tombstones(&[Tombstone::delete("a", 10), Tombstone::delete("b", 10)], &[]);
        let m = merge_rows(&[], &[row("a", "x", Some(10)), row("b", "y", Some(11))], graves, true);
        assert_eq!(m.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["b"]);
    }

    #[test]
    fn unknown_keys_are_the_union_and_the_desktops_value_stands_on_a_clash() {
        let mut d = row("d", "k", Some(1));
        d.extra.insert("shared".into(), 1.into());
        d.extra.insert("desk".into(), 2.into());
        let mut p = row("p", "k", Some(9));
        p.extra.insert("shared".into(), 3.into());
        p.extra.insert("phone".into(), 4.into());
        let m = merge_rows(&[d], &[p], Graves::new(), true);
        assert_eq!(m.rows[0].id, "p", "the later edit stands");
        assert_eq!(serde_json::Value::Object(m.rows[0].extra.clone()), serde_json::json!({ "shared": 1, "desk": 2, "phone": 4 }));
    }

    #[test]
    fn the_later_tombstone_stands_with_its_into_and_a_tie_keeps_the_desktops() {
        let t = |at: i64, into: Option<&str>| Tombstone { id: "x".into(), at, into: into.map(Into::into), extra: Extra::new() };
        assert_eq!(tombstones(union_tombstones(&[t(10, Some("y"))], &[t(20, None)])), [t(20, None)]);
        assert_eq!(tombstones(union_tombstones(&[t(20, None)], &[t(10, Some("y"))])), [t(20, None)]);
        assert_eq!(tombstones(union_tombstones(&[t(20, Some("y"))], &[t(20, None)])), [t(20, Some("y"))]);
    }

    #[test]
    fn an_into_chain_that_reaches_no_live_row_is_a_delete() {
        let into = |id: &str, to: Option<&str>| Tombstone { id: id.into(), at: 1, into: to.map(Into::into), extra: Extra::new() };
        let dead = vec![
            into("a", Some("b")),
            into("b", Some("live")),
            into("c", Some("d")),
            into("d", Some("c")),
            into("e", Some("missing")),
            into("f", Some("gone")),
            into("gone", None),
        ];
        let live = BTreeSet::from(["live".to_string()]);
        assert_eq!(standing("a", &live, &dead).as_deref(), Some("live"));
        assert_eq!(standing("c", &live, &dead), None, "a cycle");
        assert_eq!(standing("e", &live, &dead), None, "a missing id");
        assert_eq!(standing("f", &live, &dead), None, "a deleted id");
        let r = Retirements::of(&dead, &live, &dead);
        assert_eq!(r.moved.get("a").map(String::as_str), Some("live"));
        assert!(r.gone.contains_key("gone") && r.gone.contains_key("c"));
    }

    #[test]
    fn an_upload_names_its_record() {
        assert_eq!(Channel::of_upload("profile", "tags.json"), Some(Channel::Tags));
        assert_eq!(Channel::of_upload("profile", "voices.json"), Some(Channel::Voices));
        assert_eq!(Channel::of_upload("daisy-1", "meta.json"), Some(Channel::Meta));
        assert_eq!(Channel::of_upload("profile", "meta.json"), None);
        assert_eq!(Channel::of_upload("daisy-1", "tags.json"), None);
        assert_eq!(Channel::of_upload("daisy-900000000001", "meta.json"), None, "an id from the band this desktop mints");
    }
}
