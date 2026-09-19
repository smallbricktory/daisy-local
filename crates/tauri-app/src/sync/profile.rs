// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The profile files both devices write, section 5a of the protocol: the tag
//! catalog and the contacts. A phone sends its copy, this desktop merges it
//! into its own by identity (a tag by name, a contact by email then name),
//! rewrites its own references to the ids that lost, and offers the merged
//! file back with the ids the phone has to rewrite.

use std::collections::BTreeMap;
use std::path::Path;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::Signer;

use crate::commands::contacts::{load_contacts, save_contacts, Contact, ContactsFile};
use crate::commands::tags::{load_tags_file, save_tags_file, TagsFile};
use crate::state::{AppState, Tag, VaultState};
use crate::sync::exchange::{Offer, PROTOCOL_V};
use crate::sync::import::MachineKey;
use crate::sync::inbox::Inbox;
use crate::sync::signing::body_hash;

/// The `X-Daisy-Session` a profile file is uploaded under.
pub const SESSION: &str = "profile";
/// The profile files that travel, with the hello key each is listed under.
pub const FILES: [(&str, &str); 2] = [("tags", "tags.json"), ("contacts", "contacts.json")];

pub fn is_profile_file(name: &str) -> bool {
    FILES.iter().any(|(_, f)| *f == name)
}

/// One row of a catalog that merges by identity.
pub trait Mergeable: Clone {
    fn id(&self) -> &str;
    /// The same thing under a different id.
    fn same(&self, other: &Self) -> bool;
    fn updated_at(&self) -> Option<i64>;
    /// How much a person has written into the row; the tie-break when one
    /// side carries no timestamp.
    fn payload(&self) -> usize;
    /// What the winner keeps from the row it replaced.
    fn absorb(&mut self, loser: &Self);
}

fn fold(s: &str) -> String {
    s.trim().to_lowercase()
}

impl Mergeable for Tag {
    fn id(&self) -> &str {
        &self.id
    }
    fn same(&self, other: &Self) -> bool {
        fold(&self.name) == fold(&other.name)
    }
    fn updated_at(&self) -> Option<i64> {
        self.updated_at_unix_seconds
    }
    fn payload(&self) -> usize {
        self.name.len()
            + self.color_hex.len()
            + self.prompt_md.as_deref().map_or(0, str::len)
            + self.vocab_md.as_deref().map_or(0, str::len)
    }
    fn absorb(&mut self, loser: &Self) {
        self.created_at_unix_seconds = self.created_at_unix_seconds.min(loser.created_at_unix_seconds);
        self.use_count = self.use_count.max(loser.use_count);
        self.updated_at_unix_seconds = self.updated_at_unix_seconds.max(loser.updated_at_unix_seconds);
    }
}

impl Mergeable for Contact {
    fn id(&self) -> &str {
        &self.id
    }
    fn same(&self, other: &Self) -> bool {
        let shared_email = self
            .emails
            .iter()
            .any(|a| other.emails.iter().any(|b| fold(a) == fold(b)));
        shared_email || (fold(&self.display_name) == fold(&other.display_name))
    }
    fn updated_at(&self) -> Option<i64> {
        self.updated_at_unix_seconds
    }
    fn payload(&self) -> usize {
        self.display_name.len() + self.emails.iter().map(String::len).sum::<usize>()
    }
    fn absorb(&mut self, loser: &Self) {
        for e in &loser.emails {
            if !self.emails.iter().any(|x| fold(x) == fold(e)) {
                self.emails.push(e.clone());
            }
        }
        self.created_at_unix_seconds = self.created_at_unix_seconds.min(loser.created_at_unix_seconds);
        self.updated_at_unix_seconds = self.updated_at_unix_seconds.max(loser.updated_at_unix_seconds);
    }
}

/// The catalog after a merge, and the ids each side has to rewrite: every
/// id that lost, to the id that stands for it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merge<T> {
    pub merged: Vec<T>,
    pub mine: BTreeMap<String, String>,
    pub theirs: BTreeMap<String, String>,
}

/// True when the row from the other side replaces this one: the later
/// edit wins; when either side carries no timestamp the fuller row wins;
/// a tie keeps this side's.
fn theirs_win<T: Mergeable>(mine: &T, theirs: &T) -> bool {
    match (mine.updated_at(), theirs.updated_at()) {
        (Some(a), Some(b)) => b > a,
        _ => theirs.payload() > mine.payload(),
    }
}

/// Merges the other side's catalog into this one. A row is the same row
/// when the ids match (a rename on one side) or when the identity matches;
/// the winner's id stands and the loser's is mapped to it on the side that
/// held it. Rows only one side holds are kept as they are.
pub fn merge<T: Mergeable>(mine: Vec<T>, theirs: Vec<T>) -> Merge<T> {
    let my_ids: Vec<String> = mine.iter().map(|t| t.id().to_string()).collect();
    let their_ids: Vec<String> = theirs.iter().map(|t| t.id().to_string()).collect();
    let mut out = mine;
    // Loser id → the id that stands for it, kept current as winners lose
    // in turn.
    let mut lost: BTreeMap<String, String> = BTreeMap::new();
    for t in theirs {
        let at = out
            .iter()
            .position(|m| m.id() == t.id())
            .or_else(|| out.iter().position(|m| m.same(&t)));
        match at {
            None => out.push(t),
            Some(i) => out[i] = settle(&out[i], &t, &mut lost),
        }
    }
    // Two rows of this side's own can collapse once a rename from the other
    // side lands on one of them.
    let mut i = 0;
    while i < out.len() {
        let mut j = i + 1;
        while j < out.len() {
            if out[i].same(&out[j]) {
                let w = settle(&out[i], &out[j], &mut lost);
                out.remove(j);
                out[i] = w;
            } else {
                j += 1;
            }
        }
        i += 1;
    }
    let map_for = |ids: &[String]| -> BTreeMap<String, String> {
        ids.iter().filter_map(|id| Some((id.clone(), lost.get(id)?.clone()))).collect()
    };
    Merge { mine: map_for(&my_ids), theirs: map_for(&their_ids), merged: out }
}

fn settle<T: Mergeable>(mine: &T, theirs: &T, lost: &mut BTreeMap<String, String>) -> T {
    let (mut winner, loser) = if theirs_win(mine, theirs) {
        (theirs.clone(), mine)
    } else {
        (mine.clone(), theirs)
    };
    winner.absorb(loser);
    if loser.id() != winner.id() {
        let w = winner.id().to_string();
        for v in lost.values_mut() {
            if *v == loser.id() {
                *v = w.clone();
            }
        }
        lost.insert(loser.id().to_string(), w);
    }
    winner
}

/// Merges every profile file one phone has staged. Returns true when a
/// catalog here changed. A file this desktop cannot open is dropped and
/// asked for again at the next hello.
pub fn merge_staged(app: &AppState, vs: &VaultState, inbox: &Inbox, device: &str, key: &MachineKey) -> bool {
    let mut changed = false;
    for (_, file) in FILES {
        let Some(bytes) = inbox.read_profile(device, file) else { continue };
        let hash = body_hash(&bytes);
        let plain = match crate::sync::import::open(&bytes, key) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("sync: profile {file} from the phone does not open here: {e:?}");
                inbox.drop_profile(device, file);
                continue;
            }
        };
        let theirs_map = match file {
            "tags.json" => merge_tags(app, &plain),
            _ => merge_contacts(app, vs, &plain),
        };
        match theirs_map {
            Ok(ids) => {
                if let Err(e) = inbox.record_profile_merge(device, file, &hash, &ids) {
                    log::warn!("sync: profile {file} merge not recorded: {e:?}");
                }
                log::info!("sync: merged the phone's {file} ({} id(s) for the phone to rewrite)", ids.len());
                changed = true;
            }
            Err(e) => log::warn!("sync: profile {file} not merged: {e}"),
        }
        inbox.drop_profile(device, file);
    }
    changed
}

fn merge_tags(app: &AppState, plain: &[u8]) -> crate::error::Result<BTreeMap<String, String>> {
    let theirs: TagsFile = serde_json::from_slice(plain)
        .map_err(|e| crate::error::AppError::Config(format!("parse the phone's tags.json: {e}")))?;
    let mine = load_tags_file(app)?;
    let r = merge(mine.tags, theirs.tags);
    rewrite_tag_ids(app, &r.mine)?;
    save_tags_file(app, &TagsFile { schema_version: 1, tags: r.merged })?;
    Ok(r.theirs)
}

fn merge_contacts(app: &AppState, vs: &VaultState, plain: &[u8]) -> crate::error::Result<BTreeMap<String, String>> {
    let theirs: ContactsFile = serde_json::from_slice(plain)
        .map_err(|e| crate::error::AppError::Config(format!("parse the phone's contacts.json: {e}")))?;
    let mine = load_contacts(app)?;
    let r = merge(mine, theirs.contacts);
    rewrite_contact_ids(app, vs, &r.mine)?;
    save_contacts(app, &r.merged)?;
    Ok(r.theirs)
}

/// Maps the ids in a JSON array of strings, dropping duplicates. True when
/// anything changed.
fn map_ids(arr: &mut serde_json::Value, ids: &BTreeMap<String, String>) -> bool {
    let Some(items) = arr.as_array_mut() else { return false };
    let mut changed = false;
    let mut seen: Vec<String> = Vec::new();
    let mut kept = Vec::new();
    for v in items.iter() {
        let Some(s) = v.as_str() else {
            kept.push(v.clone());
            continue;
        };
        let to = ids.get(s).map(String::as_str).unwrap_or(s);
        changed |= to != s;
        if seen.iter().any(|k| k == to) {
            changed = true;
            continue;
        }
        seen.push(to.to_string());
        kept.push(serde_json::Value::String(to.to_string()));
    }
    if changed {
        *items = kept;
    }
    changed
}

fn each_manifest(app: &AppState, mut edit: impl FnMut(&mut serde_json::Value) -> bool) -> crate::error::Result<()> {
    let mut touched = false;
    for dir in crate::commands::session_store::session_dirs(&app.profile.sessions_dir()).unwrap_or_default() {
        let path = dir.join("manifest.json");
        let Ok(bytes) = syncsafe::read(&path) else { continue };
        let Ok(mut m) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        if edit(&mut m) {
            syncsafe::write_atomic(&path, serde_json::to_vec_pretty(&m)?)?;
            touched = true;
        }
    }
    if touched {
        crate::commands::library::invalidate_library_cache();
    }
    Ok(())
}

fn map_conditions(node: &mut crate::commands::workflows::ConditionNode, tags: &BTreeMap<String, String>, contacts: &BTreeMap<String, String>) -> bool {
    use crate::commands::workflows::ConditionNode::*;
    match node {
        All { children } | Any { children } => children.iter_mut().fold(false, |c, n| map_conditions(n, tags, contacts) || c),
        HasTag { tag_id } => match tags.get(tag_id.as_str()) {
            Some(to) => {
                *tag_id = to.clone();
                true
            }
            None => false,
        },
        HasParticipant { contact_id } => match contacts.get(contact_id.as_str()) {
            Some(to) => {
                *contact_id = to.clone();
                true
            }
            None => false,
        },
        TitleContains { .. } => false,
    }
}

fn rewrite_workflows(app: &AppState, tags: &BTreeMap<String, String>, contacts: &BTreeMap<String, String>) -> crate::error::Result<()> {
    let mut file = crate::commands::workflows::load_workflows_file(app)?;
    let changed = file.workflows.iter_mut().fold(false, |c, w| map_conditions(&mut w.condition, tags, contacts) || c);
    if changed {
        crate::commands::workflows::save_workflows_file(app, &file)?;
    }
    Ok(())
}

/// Every place this desktop names a tag by id follows the ids that lost.
pub fn rewrite_tag_ids(app: &AppState, ids: &BTreeMap<String, String>) -> crate::error::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    each_manifest(app, |m| {
        let mut c = false;
        for k in ["tag_ids", "dismissed_suggested_tag_ids"] {
            if let Some(arr) = m.get_mut(k) {
                c |= map_ids(arr, ids);
            }
        }
        c
    })?;
    rewrite_workflows(app, ids, &BTreeMap::new())
}

/// Every place this desktop names a contact by id follows the ids that
/// lost: speaker labels on manifests, workflow conditions, and the
/// voiceprints in the vault.
pub fn rewrite_contact_ids(app: &AppState, vs: &VaultState, ids: &BTreeMap<String, String>) -> crate::error::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    each_manifest(app, |m| {
        let Some(labels) = m.get_mut("speaker_map").and_then(|v| v.as_array_mut()) else { return false };
        let mut c = false;
        for l in labels {
            let Some(from) = l.get("contact_id").and_then(|v| v.as_str()) else { continue };
            if let Some(to) = ids.get(from) {
                l["contact_id"] = serde_json::Value::String(to.clone());
                c = true;
            }
        }
        c
    })?;
    rewrite_workflows(app, &BTreeMap::new(), ids)?;
    crate::commands::voiceprints::update_vault(app, vs, |keys| {
        for v in &mut keys.voiceprints {
            if let Some(to) = v.contact_id.as_deref().and_then(|c| ids.get(c)) {
                v.contact_id = Some(to.clone());
            }
        }
        Ok(())
    })
}

/// What a hello's `profile` earns: the files this desktop lacks the phone's
/// revision of, and the merged files the phone has yet to install. A phone
/// whose file hashes to what sits here has nothing to do for it.
pub fn wants_and_offers(
    inbox: &Inbox,
    profile_root: &Path,
    device: &str,
    phone: &BTreeMap<String, String>,
    key: &ed25519_dalek::SigningKey,
) -> (Vec<String>, Vec<Offer>) {
    let mut wants = Vec::new();
    let mut offers = Vec::new();
    for (k, file) in FILES {
        let mine = std::fs::read(profile_root.join(file)).ok();
        let mine_hash = mine.as_deref().map(body_hash);
        match phone.get(k) {
            Some(theirs) => {
                if mine_hash.as_deref() == Some(theirs.as_str()) || inbox.has_profile(device, file, theirs) {
                    continue;
                }
                match inbox.profile_merge(device, file) {
                    Some((merged_from, ids)) if merged_from == *theirs => {
                        if let Some(bytes) = &mine {
                            offers.push(offer(file, bytes, Some(&merged_from), &ids, key));
                        }
                    }
                    _ => wants.push(file.to_string()),
                }
            }
            None => {
                if let Some(bytes) = &mine {
                    offers.push(offer(file, bytes, None, &BTreeMap::new(), key));
                }
            }
        }
    }
    log::info!(
        "sync: profile hello — phone {:?}, wants {:?}, offers {:?}",
        phone.keys().collect::<Vec<_>>(),
        wants,
        offers.iter().map(|o| format!("{} {}", o.name, o.artifact_id)).collect::<Vec<_>>()
    );
    (wants, offers)
}

fn offer(file: &str, bytes: &[u8], merged_from: Option<&str>, ids: &BTreeMap<String, String>, key: &ed25519_dalek::SigningKey) -> Offer {
    let artifact_id = body_hash(bytes);
    let header = serde_json::json!({
        "v": PROTOCOL_V,
        "artifact_id": artifact_id,
        "session": SESSION,
        "name": file,
        "size": bytes.len(),
        // A profile file is built from no session's inputs, and every v2
        // header carries this key: the phone decodes one shape.
        "built_from": {},
        "merged_from": merged_from,
        "ids": ids,
    });
    let header_bytes = serde_json::to_vec(&header).unwrap_or_default();
    Offer {
        artifact_id,
        session: SESSION.to_string(),
        name: file.to_string(),
        size: bytes.len() as u64,
        header_sig_b64: B64.encode(key.sign(&header_bytes).to_bytes()),
        header_b64: B64.encode(&header_bytes),
    }
}

/// The bytes of a profile file this desktop holds, by hash.
pub fn artifact_bytes(profile_root: &Path, artifact_id: &str) -> Option<Vec<u8>> {
    FILES
        .iter()
        .filter_map(|(_, file)| std::fs::read(profile_root.join(file)).ok())
        .find(|bytes| body_hash(bytes) == artifact_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(id: &str, name: &str, prompt: Option<&str>, at: Option<i64>) -> Tag {
        Tag {
            id: id.into(),
            name: name.into(),
            color_hex: "#123456".into(),
            prompt_md: prompt.map(Into::into),
            vocab_md: None,
            created_at_unix_seconds: 100,
            use_count: 1,
            updated_at_unix_seconds: at,
            extra: Default::default(),
        }
    }

    fn contact(id: &str, name: &str, emails: &[&str], at: Option<i64>) -> Contact {
        Contact {
            id: id.into(),
            display_name: name.into(),
            emails: emails.iter().map(|s| s.to_string()).collect(),
            created_at_unix_seconds: 100,
            updated_at_unix_seconds: at,
            extra: Default::default(),
        }
    }

    #[test]
    fn a_tag_only_one_side_holds_is_kept_with_its_id() {
        let r = merge(vec![tag("d1", "Ops", None, None)], vec![tag("p1", "PHSA", None, None)]);
        let names: Vec<&str> = r.merged.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Ops", "PHSA"]);
        assert_eq!(r.merged[1].id, "p1");
        assert!(r.mine.is_empty() && r.theirs.is_empty());
    }

    #[test]
    fn the_same_name_in_any_case_is_one_tag_and_the_fuller_row_wins_without_timestamps() {
        let r = merge(vec![tag("d1", "phsa", Some("## Names\n* Brewhaven"), None)], vec![tag("p1", "PHSA", None, None)]);
        assert_eq!(r.merged.len(), 1);
        assert_eq!(r.merged[0].id, "d1", "the desktop's row carried more");
        assert_eq!(r.merged[0].name, "phsa");
        assert_eq!(r.theirs, BTreeMap::from([("p1".to_string(), "d1".to_string())]));
        assert!(r.mine.is_empty());
    }

    #[test]
    fn with_timestamps_on_both_the_later_edit_wins_whatever_its_size() {
        let r = merge(vec![tag("d1", "PHSA", Some("long prompt here"), Some(10))], vec![tag("p1", "Phsa", None, Some(20))]);
        assert_eq!(r.merged[0].id, "p1");
        assert_eq!(r.merged[0].name, "Phsa");
        assert_eq!(r.merged[0].prompt_md, None);
        assert_eq!(r.mine, BTreeMap::from([("d1".to_string(), "p1".to_string())]));
        assert!(r.theirs.is_empty());
    }

    #[test]
    fn a_tie_keeps_this_sides_row() {
        let r = merge(vec![tag("d1", "PHSA", None, Some(10))], vec![tag("p1", "PHSA", None, Some(10))]);
        assert_eq!(r.merged[0].id, "d1");
        assert_eq!(r.theirs, BTreeMap::from([("p1".to_string(), "d1".to_string())]));
    }

    #[test]
    fn the_same_id_is_the_same_tag_even_after_a_rename() {
        let r = merge(vec![tag("t1", "Old name", None, Some(10))], vec![tag("t1", "New name", None, Some(20))]);
        assert_eq!(r.merged.len(), 1);
        assert_eq!(r.merged[0].name, "New name");
        assert!(r.mine.is_empty() && r.theirs.is_empty(), "nothing to rewrite when the id stood");
    }

    #[test]
    fn the_winner_keeps_the_earliest_creation_and_the_highest_use_count() {
        let mut mine = tag("d1", "PHSA", None, Some(10));
        mine.created_at_unix_seconds = 50;
        mine.use_count = 7;
        let mut theirs = tag("p1", "PHSA", Some("x"), Some(20));
        theirs.created_at_unix_seconds = 80;
        theirs.use_count = 2;
        let r = merge(vec![mine], vec![theirs]);
        assert_eq!(r.merged[0].created_at_unix_seconds, 50);
        assert_eq!(r.merged[0].use_count, 7);
        assert_eq!(r.merged[0].updated_at_unix_seconds, Some(20));
    }

    #[test]
    fn a_rename_that_lands_on_another_of_this_sides_tags_collapses_them() {
        // Shared id t1 was renamed to "Ops" on the phone; the desktop also
        // holds a separate "Ops". One tag stands and the desktop's other id
        // is mapped to it.
        let r = merge(
            vec![tag("t1", "Operations", None, Some(10)), tag("d2", "Ops", None, Some(5))],
            vec![tag("t1", "Ops", Some("prompt"), Some(20))],
        );
        assert_eq!(r.merged.len(), 1);
        assert_eq!(r.merged[0].id, "t1");
        assert_eq!(r.merged[0].prompt_md.as_deref(), Some("prompt"));
        assert_eq!(r.mine, BTreeMap::from([("d2".to_string(), "t1".to_string())]));
    }

    #[test]
    fn contacts_match_by_email_first_and_their_emails_join() {
        let r = merge(
            vec![contact("d1", "Alexandra Chen", &["achen@example.org"], None)],
            vec![contact("p1", "Alex Chen", &["ACHEN@example.org", "alex@home.example"], None)],
        );
        assert_eq!(r.merged.len(), 1);
        assert_eq!(r.merged[0].id, "p1", "the phone's row carried more");
        assert_eq!(r.merged[0].emails, ["ACHEN@example.org", "alex@home.example"]);
        assert_eq!(r.mine, BTreeMap::from([("d1".to_string(), "p1".to_string())]));
    }

    #[test]
    fn contacts_without_emails_match_by_name_in_any_case() {
        let r = merge(vec![contact("d1", "jordan rivera", &[], Some(30))], vec![contact("p1", "Jordan Rivera", &["jr@example.org"], Some(20))]);
        assert_eq!(r.merged.len(), 1);
        assert_eq!(r.merged[0].id, "d1", "the later edit");
        assert_eq!(r.merged[0].emails, ["jr@example.org"], "the email the loser knew is kept");
        assert_eq!(r.theirs, BTreeMap::from([("p1".to_string(), "d1".to_string())]));
    }

    #[test]
    fn two_names_that_share_no_email_stay_two_contacts() {
        let r = merge(vec![contact("d1", "Sam Lee", &["sam@a.example"], None)], vec![contact("p1", "Sam Li", &["sam@b.example"], None)]);
        assert_eq!(r.merged.len(), 2);
    }

    #[test]
    fn ids_in_a_list_follow_the_map_and_collapse_duplicates() {
        let ids = BTreeMap::from([("p1".to_string(), "d1".to_string())]);
        let mut v = serde_json::json!(["p1", "d1", "x"]);
        assert!(map_ids(&mut v, &ids));
        assert_eq!(v, serde_json::json!(["d1", "x"]));
        let mut untouched = serde_json::json!(["d1", "x"]);
        assert!(!map_ids(&mut untouched, &ids));
    }
}

#[cfg(test)]
mod round_trip {
    //! The sequence a paired phone actually drives: a hello naming a file
    //! this desktop does not hold, the upload, the merge, and the hello after
    //! it. The tests above this one hand-write the merge record, so none of
    //! them cross `merge_staged`, which is where the offer is decided.
    use super::*;
    use crate::commands::tags::{save_tags_file, TagsFile};
    use crate::profile::ProfileDir;
    use crate::state::{AppState, Tag, VaultState};
    use crate::sync::exchange::hello_reply;
    use crate::sync::import::{seal_to, MachineKey};
    use crate::sync::inbox::Inbox;

    const PHONE: &str = "4a68f72f5a22110b2bfbe71b6588833c";
    const SECRET: [u8; 32] = [5u8; 32];

    fn phone_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[11u8; 32])
    }

    fn phone_pub() -> String {
        B64.encode(phone_key().verifying_key().to_bytes())
    }

    fn desktop_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[31u8; 32])
    }

    fn machine_key() -> MachineKey {
        MachineKey { x25519_secret: SECRET, device_id: "mac-1".into() }
    }

    fn tag(id: &str, name: &str) -> Tag {
        Tag {
            id: id.into(),
            name: name.into(),
            color_hex: "#123456".into(),
            prompt_md: None,
            vocab_md: None,
            created_at_unix_seconds: 100,
            use_count: 0,
            updated_at_unix_seconds: Some(100),
            extra: Default::default(),
        }
    }

    /// A desktop holding `names`, with the inbox and sessions directory the
    /// server hands the exchange.
    fn desktop(names: &[&str]) -> (AppState, VaultState, Inbox, tempfile::TempDir) {
        let td = tempfile::tempdir().unwrap();
        let app = AppState::new(ProfileDir::at(td.path().join("daisy")).unwrap());
        let tags = names
            .iter()
            .enumerate()
            .map(|(i, n)| tag(&format!("d{i}"), n))
            .collect();
        save_tags_file(&app, &TagsFile { schema_version: 1, tags }).unwrap();
        let inbox = Inbox::at(td.path().join("inbox"));
        (app, VaultState::new(), inbox, td)
    }

    /// The phone's tags.json as it sits on the phone: sealed to this desktop.
    fn phone_tags(names: &[&str]) -> Vec<u8> {
        let tags: Vec<Tag> = names
            .iter()
            .enumerate()
            .map(|(i, n)| tag(&format!("p{i}"), n))
            .collect();
        let plain = serde_json::to_vec(&TagsFile { schema_version: 1, tags }).unwrap();
        // Sealed to this desktop: `open` matches the recipient id against
        // the machine key's device id, not the phone's.
        seal_to(&SECRET, "mac-1", &plain)
    }

    fn hello(app: &AppState, inbox: &Inbox, phone_hash: &str) -> crate::sync::exchange::HelloReply {
        let body = serde_json::to_vec(&serde_json::json!({
            "v": PROTOCOL_V, "device_id": PHONE, "sessions": [], "tombstones": [],
            "profile": { "tags": phone_hash },
        }))
        .unwrap();
        hello_reply(
            inbox,
            &app.profile.sessions_dir(),
            PHONE,
            &phone_pub(),
            &desktop_key(),
            &machine_key(),
            &body,
        )
        .unwrap()
    }

    fn names(app: &AppState) -> Vec<String> {
        let mut n: Vec<String> = crate::commands::tags::load_tags_file(app)
            .unwrap()
            .tags
            .into_iter()
            .map(|t| t.name)
            .collect();
        n.sort();
        n
    }

    #[test]
    fn the_merged_catalog_is_offered_back_on_the_hello_after_the_upload() {
        let (app, vs, inbox, _td) = desktop(&["Northwind", "Weekly"]);
        let sealed = phone_tags(&["Harbour survey"]);
        let phone_hash = body_hash(&sealed);

        // 1. The phone names a revision this desktop does not hold.
        let first = hello(&app, &inbox, &phone_hash);
        assert_eq!(first.profile_wants, ["tags.json"], "the phone's tags were not asked for");
        assert!(first.profile_offers.is_empty(), "nothing to offer before the merge");

        // 2. The phone uploads it, and the desktop merges what it staged.
        inbox.store_profile(PHONE, "tags.json", &phone_hash, &sealed).unwrap();
        assert!(merge_staged(&app, &vs, &inbox, PHONE, &machine_key()), "the merge changed nothing");
        assert_eq!(
            names(&app),
            ["Harbour survey", "Northwind", "Weekly"],
            "the phone's tag did not land here"
        );

        // 3. The next hello carries the same phone hash, because the phone's
        //    own file has not changed. This is where the desktop's catalog
        //    has to travel the other way.
        let second = hello(&app, &inbox, &phone_hash);
        assert!(
            second.profile_wants.is_empty(),
            "the phone was asked for a file this desktop had already merged"
        );
        assert_eq!(
            second.profile_offers.len(),
            1,
            "the merged catalog was never offered back, so the desktop's tags \
             cannot reach the phone: wants {:?}, offers {:?}",
            second.profile_wants,
            second.profile_offers.iter().map(|o| format!("{} {}", o.name, o.artifact_id)).collect::<Vec<_>>()
        );
        assert_eq!(second.profile_offers[0].name, "tags.json");

        // 4. The phone decodes the header before it downloads anything. Its
        //    `ArtifactHeader` is one shape for every artifact, so a key
        //    missing here is an offer the phone drops without a word — which
        //    is what kept the desktop's tags off the phone. The twin of this
        //    assertion is `theDesktopsProfileOfferHeaderDecodes` in
        //    smbr-daisy-ios DaisyKit SyncArtifactTests.
        let header: serde_json::Value =
            serde_json::from_slice(&B64.decode(&second.profile_offers[0].header_b64).unwrap()).unwrap();
        let mut keys: Vec<&str> = header.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["artifact_id", "built_from", "ids", "merged_from", "name", "session", "size", "v"],
            "the phone reads this header with a strict decoder"
        );
        assert_eq!(header["merged_from"], serde_json::json!(phone_hash));
    }
}
