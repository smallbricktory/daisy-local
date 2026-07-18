//! Contact identity store over a plaintext `<profile>/contacts.json`.
//! A Contact is identity only (name + emails); voice data lives in the vault
//! as a Voiceprint referencing a contact_id. Writes are atomic (`.tmp` +
//! rename). All name/email comparisons are case-insensitive.
use crate::error::{AppError, Result};
use crate::state::AppState;
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
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ContactsFile {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub contacts: Vec<Contact>,
}
fn default_schema() -> u32 {
    1
}

pub fn contacts_path(app: &AppState) -> PathBuf {
    app.profile.root().join("contacts.json")
}

pub fn load_contacts(app: &AppState) -> Result<Vec<Contact>> {
    let p = contacts_path(app);
    if !p.is_file() {
        return Ok(vec![]);
    }
    let bytes = syncsafe::read(&p)?;
    Ok(serde_json::from_slice::<ContactsFile>(&bytes)
        .map_err(|e| AppError::Config(format!("parse contacts.json: {e}")))?
        .contacts)
}

pub fn save_contacts(app: &AppState, contacts: &[Contact]) -> Result<()> {
    let p = contacts_path(app);
    let tmp = p.with_extension("json.tmp");
    let file = ContactsFile {
        schema_version: 1,
        contacts: contacts.to_vec(),
    };
    syncsafe::write(&tmp, serde_json::to_vec_pretty(&file)?)?;
    syncsafe::rename(&tmp, &p)?;
    Ok(())
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
            }
        }
        return c.id.clone();
    }
    // 3. New contact.
    let id = uuid::Uuid::new_v4().to_string();
    contacts.push(Contact {
        id: id.clone(),
        display_name: name.to_string(),
        emails: email
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(|e| vec![e.to_string()])
            .unwrap_or_default(),
        created_at_unix_seconds: now,
    });
    id
}

/// Load → upsert → save, returning the contact id.
pub fn upsert_contact_in_store(
    app: &AppState,
    display_name: &str,
    email: Option<&str>,
) -> Result<String> {
    let mut cs = load_contacts(app)?;
    let id = upsert_contact(&mut cs, display_name, email, now_unix());
    save_contacts(app, &cs)?;
    Ok(id)
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
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.flatten() {
            let Ok(mb) = syncsafe::read(e.path().join("manifest.json")) else {
                continue;
            };
            let Ok(m) = serde_json::from_slice::<recording::manifest::SessionManifest>(&mb) else {
                continue;
            };
            for id in session_contact_ids(&m, &contacts) {
                *counts.entry(id).or_default() += 1;
            }
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
            Contact { id: "a".into(), display_name: "Alice".into(), emails: vec!["alice@x.com".into()], created_at_unix_seconds: 1 },
            Contact { id: "b".into(), display_name: "Bob".into(), emails: vec![], created_at_unix_seconds: 1 },
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
            interrupted: false,
            denoise_applied: None,
        }
    }

    #[test]
    fn resolver_includes_speaker_label_contact_ids_and_attendee_names() {
        use recording::manifest::{Attendee, AttendeeRole, SpeakerLabel};
        let cs = vec![
            Contact { id: "z".into(), display_name: "Zed".into(), emails: vec![], created_at_unix_seconds: 1 },
            Contact { id: "y".into(), display_name: "Yan".into(), emails: vec![], created_at_unix_seconds: 1 },
        ];
        let m = manifest_with(
            vec![Attendee { display_name: "Yan".into(), role: AttendeeRole::Other }],
            vec![SpeakerLabel {
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
        let id = upsert_contact_in_store(&app, "Dana", None).unwrap();
        // Same name again returns the same id and doesn't duplicate.
        let id2 = upsert_contact_in_store(&app, "dana", Some("dana@x.com")).unwrap();
        assert_eq!(id, id2);
        let cs = load_contacts(&app).unwrap();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].display_name, "Dana");
        assert_eq!(cs[0].emails, vec!["dana@x.com"]);
    }

    #[test]
    fn round_trips_contacts_file() {
        let (app, _t) = app();
        assert!(load_contacts(&app).unwrap().is_empty());
        let c = Contact {
            id: "c1".into(),
            display_name: "Alice".into(),
            emails: vec!["alice@x.com".into()],
            created_at_unix_seconds: 1,
        };
        save_contacts(&app, &[c.clone()]).unwrap();
        assert_eq!(load_contacts(&app).unwrap(), vec![c]);
    }
}
