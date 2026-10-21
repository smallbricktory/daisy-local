// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Contact identity store over a plaintext `<profile>/contacts.json`.
//! A Contact is identity only (name + emails); voice data lives in the vault
//! as a Voiceprint referencing a contact_id. Writes are atomic (`.tmp` +
//! rename). All name/email comparisons are case-insensitive.
use crate::error::Result;
use crate::state::{AppState, VaultState};
use crate::sync::records::{self, merge_rows, tombstones, union_extra, union_tombstones, FileMerge, Record, Side, Tombstone};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::now_unix;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub emails: Vec<String>,
    pub created_at_unix_seconds: i64,
    /// When a person last changed this contact on any device; absent on a
    /// contact nobody has edited since the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_unix_seconds: Option<i64>,
    /// Keys another Daisy client wrote that this build does not declare;
    /// carried through a rewrite untouched.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactsFile {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub contacts: Vec<Contact>,
    /// Contacts deleted, or merged into another; kept for good so neither
    /// device's copy brings them back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<Tombstone>,
    /// Keys another Daisy client wrote at the top of the file that this
    /// build does not declare; carried through a rewrite untouched.
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for ContactsFile {
    fn default() -> Self {
        Self { schema_version: 1, contacts: vec![], deleted: vec![], extra: Default::default() }
    }
}

fn default_schema() -> u32 {
    1
}

/// The file's bytes: canonical, with contacts and tombstones each sorted by id.
pub fn encode(file: &ContactsFile) -> serde_json::Result<Vec<u8>> {
    let mut f = file.clone();
    f.contacts.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    f.deleted.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    crate::sync::records::canonical(&f)
}

fn norm_key(s: &str) -> String {
    s.trim().to_lowercase()
}

impl Record for Contact {
    fn id(&self) -> &str {
        &self.id
    }
    /// Any email in common, ignoring case, or the trimmed names matching
    /// ignoring case.
    fn same(&self, other: &Self) -> bool {
        let shared_email = self.emails.iter().any(|a| other.emails.iter().any(|b| norm_key(a) == norm_key(b)));
        shared_email || norm_key(&self.display_name) == norm_key(&other.display_name)
    }
    fn updated_at(&self) -> Option<i64> {
        self.updated_at_unix_seconds
    }
    fn row_time(&self) -> i64 {
        self.updated_at_unix_seconds.unwrap_or(self.created_at_unix_seconds)
    }
    fn extra(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.extra
    }
    fn extra_mut(&mut self) -> &mut serde_json::Map<String, serde_json::Value> {
        &mut self.extra
    }
    /// The earliest creation, the latest edit, and every email any of them
    /// knew, the standing row's first.
    fn absorb(&mut self, group: &[(&Self, Side)]) {
        for (c, _) in group {
            for e in &c.emails {
                if !self.emails.iter().any(|x| norm_key(x) == norm_key(e)) {
                    self.emails.push(e.clone());
                }
            }
            self.created_at_unix_seconds = self.created_at_unix_seconds.min(c.created_at_unix_seconds);
            self.updated_at_unix_seconds = self.updated_at_unix_seconds.max(c.updated_at_unix_seconds);
        }
    }
}

/// Merges the phone's contacts into this desktop's.
pub fn merge_files(desktop: &ContactsFile, phone: &ContactsFile) -> FileMerge<ContactsFile> {
    let graves = union_tombstones(&desktop.deleted, &phone.deleted);
    let m = merge_rows(&desktop.contacts, &phone.contacts, graves, true);
    let file = ContactsFile {
        schema_version: desktop.schema_version.max(phone.schema_version).max(1),
        contacts: m.rows.clone(),
        deleted: tombstones(m.graves.clone()),
        extra: union_extra(&desktop.extra, &phone.extra),
    };
    FileMerge::of(file, &m)
}

/// Removes a contact and records its tombstone, no earlier than its own
/// latest moment so every copy of it drops, then takes it off every speaker
/// and takes its voices with it.
pub fn delete_contact(app: &AppState, vs: &VaultState, id: &str, at: i64) -> Result<()> {
    let at = edit_contacts(app, |f| Ok(records::delete_row(&mut f.contacts, &mut f.deleted, id, at)))?;
    let gone = records::Retirements { gone: [(id.to_string(), at)].into(), ..Default::default() };
    records::apply_retirements(app, Some(vs), records::Channel::Contacts, &gone)
}

/// Names a contact anew, adding the email when it is one the contact lacks.
pub fn rename_contact(app: &AppState, id: &str, display_name: &str, email: Option<&str>, at: i64) -> Result<()> {
    edit_contacts(app, |f| {
        let Some(c) = f.contacts.iter_mut().find(|c| c.id == id) else { return Ok(()) };
        c.display_name = display_name.to_string();
        if let Some(e) = email {
            if !c.emails.iter().any(|x| x.eq_ignore_ascii_case(e)) {
                c.emails.push(e.to_string());
            }
        }
        c.updated_at_unix_seconds = Some(at);
        Ok(())
    })
}

pub fn contacts_path(app: &AppState) -> PathBuf {
    app.profile.root().join("contacts.json")
}

pub fn load_contacts_file(app: &AppState) -> Result<ContactsFile> {
    records::read_file(&contacts_path(app))
}

pub fn load_contacts(app: &AppState) -> Result<Vec<Contact>> {
    let p = contacts_path(app);
    super::file_cache::cached(&p, || Ok(load_contacts_file(app)?.contacts))
}

/// Every read-modify-write of `contacts.json`, from a person here or a merge
/// of the phone's copy, holds this from the read to the write.
static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn save_contacts_file(app: &AppState, file: &ContactsFile) -> Result<()> {
    let _held = records::hold(&WRITE);
    records::write_file(&contacts_path(app), encode(file)?)
}

/// Reads the contacts, applies `f` and writes the result, with no other
/// writer in between.
pub fn edit_contacts<R>(app: &AppState, f: impl FnOnce(&mut ContactsFile) -> Result<R>) -> Result<R> {
    records::edit_file(&WRITE, &contacts_path(app), encode, f)
}

/// Replaces the contacts, keeping the tombstones and unknown keys the file
/// already holds.
pub fn save_contacts(app: &AppState, contacts: &[Contact]) -> Result<()> {
    edit_contacts(app, |file| {
        file.contacts = contacts.to_vec();
        Ok(())
    })
}

fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Find-or-create a Contact by email (strong key) then by normalized name.
/// Returns the contact id. Adds a new email to an existing match. Never merges
/// two distinct names that share no email. `now` is the creation timestamp for
/// new contacts. Mutates `contacts` in place; the caller persists.
pub fn upsert_contact(
    contacts: &mut Vec<Contact>,
    display_name: &str,
    email: Option<&str>,
    now: i64,
) -> String {
    let name = display_name.trim();
    let email_n = email.map(norm).filter(|e| !e.is_empty());

    // 1. Match by email (case-insensitive) when one is given.
    if let Some(e) = &email_n {
        if let Some(c) = contacts
            .iter_mut()
            .find(|c| c.emails.iter().any(|x| norm(x) == *e))
        {
            return c.id.clone();
        }
    }
    // 2. Else match by normalized name; attach a newly-seen email to it.
    if let Some(c) = contacts
        .iter_mut()
        .find(|c| norm(&c.display_name) == norm(name))
    {
        if let Some(e) = email {
            let e = e.trim();
            if !e.is_empty() && !c.emails.iter().any(|x| norm(x) == norm(e)) {
                c.emails.push(e.to_string());
                c.updated_at_unix_seconds = Some(now);
            }
        }
        return c.id.clone();
    }
    // 3. New contact.
    let id = uuid::Uuid::new_v4().to_string();
    contacts.push(Contact {
        extra: Default::default(),
        id: id.clone(),
        display_name: name.to_string(),
        emails: email
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(|e| vec![e.to_string()])
            .unwrap_or_default(),
        created_at_unix_seconds: now,
        updated_at_unix_seconds: Some(now),
    });
    id
}

/// Load → upsert → save, returning the contact id.
pub fn upsert_contact_in_store(
    app: &AppState,
    display_name: &str,
    email: Option<&str>,
) -> Result<String> {
    edit_contacts(app, |file| Ok(upsert_contact(&mut file.contacts, display_name, email, now_unix())))
}

/// Contact ids for a set of attendee display names: matches each name to a
/// Contact by normalized name or by an email equal to the string.
pub fn contact_ids_for_attendee_names(
    names: &[String],
    contacts: &[Contact],
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for name in names {
        let n = norm(name);
        if let Some(c) = contacts
            .iter()
            .find(|c| norm(&c.display_name) == n || c.emails.iter().any(|e| norm(e) == n))
        {
            out.insert(c.id.clone());
        }
    }
    out
}

/// All contact ids a session involves: attendees by name + speaker labels by
/// their stored contact_id.
pub fn session_contact_ids(
    manifest: &recording::manifest::SessionManifest,
    contacts: &[Contact],
) -> std::collections::HashSet<String> {
    let names: Vec<String> = manifest
        .attendees
        .iter()
        .map(|a| a.display_name.clone())
        .collect();
    let mut out = contact_ids_for_attendee_names(&names, contacts);
    for label in &manifest.speaker_map {
        if let Some(cid) = &label.contact_id {
            out.insert(cid.clone());
        }
    }
    out
}

/// Contacts for the people dropdown, sorted by session-count (desc) then name.
pub fn list_contacts_impl(app: &AppState) -> Result<Vec<Contact>> {
    let contacts = load_contacts(app)?;
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let root = app.profile.sessions_dir();
    for p in crate::commands::session_store::session_dirs(&root).unwrap_or_default() {
        let Some(m) = crate::commands::session_store::read_manifest_opt(&p) else {
            continue;
        };
        for id in session_contact_ids(&m, &contacts) {
            *counts.entry(id).or_default() += 1;
        }
    }
    let mut out = contacts;
    out.sort_by(|a, b| {
        counts
            .get(&b.id)
            .unwrap_or(&0)
            .cmp(counts.get(&a.id).unwrap_or(&0))
            .then_with(|| {
                a.display_name
                    .to_lowercase()
                    .cmp(&b.display_name.to_lowercase())
            })
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (AppState::new(profile), tmp)
    }

    #[test]
    fn contact_keeps_keys_it_does_not_declare() {
        let json = r#"{"id":"c1","display_name":"Rowan","emails":[],"created_at_unix_seconds":1,"phone_handle":"+1-555-0100"}"#;
        let c: Contact = serde_json::from_str(json).unwrap();
        let v: serde_json::Value = serde_json::to_value(&c).unwrap();
        assert_eq!(v["phone_handle"], serde_json::json!("+1-555-0100"));
        assert_eq!(v["display_name"], serde_json::json!("Rowan"));
    }

    #[test]
    fn upsert_matches_by_email_then_name_never_merges_distinct_names() {
        let mut cs: Vec<Contact> = vec![];
        let id1 = upsert_contact(&mut cs, "Alice", None, 1);
        assert_eq!(cs.len(), 1);
        let id1b = upsert_contact(&mut cs, "  alice ", None, 2);
        assert_eq!(id1, id1b);
        assert_eq!(cs.len(), 1);
        let id1c = upsert_contact(&mut cs, "Alice", Some("alice@x.com"), 3);
        assert_eq!(id1, id1c);
        assert_eq!(cs[0].emails, vec!["alice@x.com"]);
        let id1d = upsert_contact(&mut cs, "Alice Smith", Some("ALICE@X.COM"), 4);
        assert_eq!(id1, id1d);
        assert_eq!(cs.len(), 1);
        let id2 = upsert_contact(&mut cs, "Bob", None, 5);
        assert_ne!(id1, id2);
        assert_eq!(cs.len(), 2);
    }

    #[test]
    fn resolves_attendees_to_contacts_by_name_and_email() {
        let cs = vec![
            Contact { id: "a".into(), display_name: "Alice".into(), emails: vec!["alice@x.com".into()], created_at_unix_seconds: 1, extra: Default::default(), updated_at_unix_seconds: None },
            Contact { id: "b".into(), display_name: "Bob".into(), emails: vec![], created_at_unix_seconds: 1, extra: Default::default(), updated_at_unix_seconds: None },
        ];
        let names: Vec<String> = ["alice", "BOB", "Carol"].iter().map(|s| s.to_string()).collect();
        let got = contact_ids_for_attendee_names(&names, &cs);
        assert!(got.contains("a"));
        assert!(got.contains("b"));
        assert_eq!(got.len(), 2); // Carol has no contact → contributes nothing
    }

    fn manifest_with(
        attendees: Vec<recording::manifest::Attendee>,
        labels: Vec<recording::manifest::SpeakerLabel>,
    ) -> recording::manifest::SessionManifest {
        use recording::manifest::{AecMode, SessionManifest};
        SessionManifest {
            recording_kind: None,
            origin_device: None,
            extra: Default::default(),
            schema_version: 2, session_id: "s".into(), created_at_unix_seconds: 0,
            sample_rate: 16000, channels: 1, mic_source_id: 1,
            mic_source_node_name: "m".into(), mic_source_description: "m".into(),
            system_source_id: 2, system_source_node_name: "s".into(),
            system_source_description: "s".into(), aec_mode: AecMode::Disabled,
            chunks: vec![], finalized_at_unix_seconds: None, title: None,
            meeting_id: "id".into(), tag_ids: vec![], notes_md_relative: None,
            attendees, calendar: None, recording_segments: vec![],
            speaker_map: labels,
            language: None,
            diarization_unavailable: false,
            single_local_speaker: true,
            expected_speakers: None,
            sent_integration_ids: vec![],
            cluster_sides: vec![],
            mixed_clusters: vec![], cluster_sample_spans: vec![], sample_spans_on_audio_timeline: false, speaker_separation: None, diarize_count: None, diarization_id: None,
            interrupted: false,
            denoise_applied: None,
            dismissed_suggested_tag_ids: vec![],
        }
    }

    #[test]
    fn resolver_includes_speaker_label_contact_ids_and_attendee_names() {
        use recording::manifest::{Attendee, AttendeeRole, SpeakerLabel};
        let cs = vec![
            Contact { id: "z".into(), display_name: "Zed".into(), emails: vec![], created_at_unix_seconds: 1, extra: Default::default(), updated_at_unix_seconds: None },
            Contact { id: "y".into(), display_name: "Yan".into(), emails: vec![], created_at_unix_seconds: 1, extra: Default::default(), updated_at_unix_seconds: None },
        ];
        let m = manifest_with(
            vec![Attendee { display_name: "Yan".into(), role: AttendeeRole::Other }],
            vec![SpeakerLabel {
                extra: Default::default(),
                cluster_id: 0,
                display_name: "Zed".into(),
                email: None,
                voiceprint_id: None,
                match_confidence: None,
                contact_id: Some("z".into()),
            }],
        );
        let got = session_contact_ids(&m, &cs);
        assert!(got.contains("z")); // via speaker-label link
        assert!(got.contains("y")); // via attendee name
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn upsert_in_store_persists_and_dedupes() {
        let (app, _t) = app();
        let id = upsert_contact_in_store(&app, "Nyla", None).unwrap();
        // Same name again returns the same id and doesn't duplicate.
        let id2 = upsert_contact_in_store(&app, "nyla", Some("nyla@x.com")).unwrap();
        assert_eq!(id, id2);
        let cs = load_contacts(&app).unwrap();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].display_name, "Nyla");
        assert_eq!(cs[0].emails, vec!["nyla@x.com"]);
    }

    #[test]
    fn round_trips_contacts_file() {
        let (app, _t) = app();
        assert!(load_contacts(&app).unwrap().is_empty());
        let c = Contact {
            extra: Default::default(), updated_at_unix_seconds: None,
            id: "c1".into(),
            display_name: "Alice".into(),
            emails: vec!["alice@x.com".into()],
            created_at_unix_seconds: 1,
        };
        save_contacts(&app, &[c.clone()]).unwrap();
        assert_eq!(load_contacts(&app).unwrap(), vec![c]);
    }
}
