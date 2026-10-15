// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The four records both devices write, driven the way a paired phone and a
//! person at this desktop drive them together.

use crate::commands::tags::{load_tags_file, save_tags_file, TagsFile};
use crate::profile::ProfileDir;
use crate::state::{AppState, Tag, VaultState};
use crate::sync::import::{import_session, MachineKey};
use crate::sync::inbox::Inbox;
use crate::sync::signing::body_hash;

const PHONE: &str = "4a68f72f5a22110b2bfbe71b6588833c";
const SESSION: &str = "daisy-1700000000";

fn machine_key() -> MachineKey {
    MachineKey { x25519_secret: [5u8; 32], device_id: "mac-1".into() }
}

fn tag(id: &str, name: &str, at: i64) -> Tag {
    Tag {
        id: id.into(),
        name: name.into(),
        color_hex: "#123456".into(),
        prompt_md: None,
        vocab_md: None,
        created_at_unix_seconds: 100,
        use_count: 0,
        updated_at_unix_seconds: Some(at),
        extra: Default::default(),
    }
}

fn desktop() -> (AppState, VaultState, Inbox, tempfile::TempDir) {
    let td = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(td.path().join("daisy")).unwrap());
    let inbox = Inbox::at(td.path().join("inbox"));
    (app, VaultState::new(), inbox, td)
}

fn stage_profile(inbox: &Inbox, file: &str, plain: &[u8]) {
    inbox.stage_record(PHONE, "profile", file, &body_hash(plain), plain).unwrap();
}

#[test]
fn a_tag_deleted_here_stays_deleted_when_the_phone_still_holds_it() {
    let (app, vs, inbox, _td) = desktop();
    save_tags_file(&app, &TagsFile { schema_version: 1, tags: vec![tag("t-north", "Northwind", 100)], ..Default::default() }).unwrap();
    crate::commands::tags::delete_tag_impl(&app, "t-north", true).unwrap();

    let phone = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "tags": [tag("t-north", "Northwind", 100)],
    }))
    .unwrap();
    stage_profile(&inbox, "tags.json", &phone);
    crate::sync::records::merge_staged_profile(&app, &vs, &inbox, PHONE, &machine_key());

    let left: Vec<String> = load_tags_file(&app).unwrap().tags.into_iter().map(|t| t.id).collect();
    assert!(left.is_empty(), "the deleted tag came back from the phone: {left:?}");
}

fn phone_manifest(title: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schema_version": 4,
        "session_id": SESSION,
        "created_at_unix_seconds": 1_700_000_000i64,
        "finalized_at_unix_seconds": 1_700_000_042i64,
        "sample_rate": 16000,
        "channels": 1,
        "title": title,
        "chunks": [{
            "index": 1,
            "started_at_unix_seconds": 1_700_000_000i64,
            "ended_at_unix_seconds": 1_700_000_010i64,
            "duration_seconds": 9.4,
            "mic_samples": 150_393,
            "mic_wav_relative": "chunks/0001/mic.m4a",
        }],
    }))
    .unwrap()
}

fn stage_session(inbox: &Inbox, manifest: &[u8]) {
    let transcript = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1, "session_id": SESSION, "provider": "phone", "model": "on-device",
        "transcribed_at_unix_seconds": 1_700_000_012i64, "chunks": [],
    }))
    .unwrap();
    let aac = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../recording/tests/fixtures/burst-16k.m4a")).unwrap();
    inbox.store(PHONE, SESSION, "manifest.json", &body_hash(manifest), manifest).unwrap();
    inbox.store(PHONE, SESSION, "transcript.json", &body_hash(&transcript), &transcript).unwrap();
    inbox.store(PHONE, SESSION, "chunks/0001/mic.m4a", &body_hash(&aac), &aac).unwrap();
}

#[test]
fn a_title_typed_here_is_what_the_library_shows_after_the_phone_sends_its_manifest_again() {
    let (app, _vs, inbox, _td) = desktop();
    let sessions = app.profile.sessions_dir();
    stage_session(&inbox, &phone_manifest("Standup"));
    let local = import_session(&inbox, &sessions, PHONE, SESSION, &machine_key()).unwrap();

    recording::session_meta::update(&sessions.join(&local), |m| m.set_title("Harbour review", 1_787_300_000)).unwrap();

    stage_session(&inbox, &phone_manifest("Standup "));
    import_session(&inbox, &sessions, PHONE, SESSION, &machine_key()).unwrap();

    crate::commands::library::invalidate_library_cache();
    let listed = crate::commands::library::list_sessions_impl(&app).unwrap();
    let row = listed.iter().find(|s| s.session_id == local).unwrap();
    assert_eq!(row.title.as_deref(), Some("Harbour review"), "the phone's manifest masked the title typed here");
}

#[test]
fn a_tag_made_here_while_the_phones_catalog_merges_is_not_lost() {
    let (app, vs, inbox, _td) = desktop();
    save_tags_file(&app, &TagsFile { schema_version: 1, tags: vec![], ..Default::default() }).unwrap();
    let app = std::sync::Arc::new(app);
    let vs = std::sync::Arc::new(vs);
    let made: Vec<String> = (0..40).map(|i| format!("Made here {i:02}")).collect();
    let phone_tags: Vec<Tag> = (0..40).map(|i| tag(&format!("p-{i:02}"), &format!("From phone {i:02}"), 100)).collect();

    let writer = {
        let app = app.clone();
        let made = made.clone();
        std::thread::spawn(move || {
            for name in made {
                crate::commands::tags::create_tag_impl(
                    &app,
                    crate::commands::tags::CreateTagRequest { name, color_hex: "#123456".into(), prompt_md: None, vocab_md: None },
                )
                .unwrap();
            }
        })
    };
    for i in 0..phone_tags.len() {
        let plain = serde_json::to_vec(&serde_json::json!({ "schema_version": 1, "tags": phone_tags[..=i] })).unwrap();
        stage_profile(&inbox, "tags.json", &plain);
        crate::sync::records::merge_staged_profile(&app, &vs, &inbox, PHONE, &machine_key());
    }
    writer.join().unwrap();

    let names: std::collections::BTreeSet<String> = load_tags_file(&app).unwrap().tags.into_iter().map(|t| t.name).collect();
    let lost: Vec<&String> = made.iter().filter(|n| !names.contains(*n)).collect();
    assert!(lost.is_empty(), "tags made here were overwritten by the merge: {lost:?}");
}

#[test]
fn keys_only_the_phone_writes_survive_every_merge() {
    // meta.json
    let mine = recording::session_meta::SessionMeta::default();
    let theirs: recording::session_meta::SessionMeta =
        serde_json::from_value(serde_json::json!({ "schema_version": 1, "zz_phone": { "kept": true } })).unwrap();
    let merged = serde_json::to_value(crate::sync::session_meta::merge(&mine, &theirs)).unwrap();
    assert_eq!(merged["zz_phone"], serde_json::json!({ "kept": true }), "meta.json dropped the phone's key");
}

#[test]
fn keys_only_the_phone_writes_survive_a_voices_merge() {
    let theirs: crate::sync::voices::VoicesFile =
        serde_json::from_value(serde_json::json!({ "schema_version": 1, "zz_phone": 7 })).unwrap();
    let merged = crate::sync::voices::merge(&crate::sync::voices::VoicesFile::default(), &theirs);
    assert_eq!(merged.file.extra.get("zz_phone"), Some(&serde_json::json!(7)), "voices.json dropped the phone's key");
}

#[test]
fn keys_only_the_phone_writes_survive_a_tags_decode() {
    let theirs: TagsFile = serde_json::from_value(serde_json::json!({ "schema_version": 1, "tags": [], "zz_phone": 7 })).unwrap();
    let bytes = serde_json::to_value(&theirs).unwrap();
    assert_eq!(bytes["zz_phone"], 7, "tags.json dropped the phone's key");
}

// ---- the round a paired phone drives ----------------------------------------

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use crate::commands::contacts::ContactsFile;
use crate::sync::exchange::{hello_reply, HelloReply, PROTOCOL_V};
use crate::sync::import::seal_to;
use crate::sync::records::merge_staged_profile;

const SECRET: [u8; 32] = [5u8; 32];

fn phone_pub() -> String {
    B64.encode(ed25519_dalek::SigningKey::from_bytes(&[11u8; 32]).verifying_key().to_bytes())
}

fn desktop_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[31u8; 32])
}

fn seam(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/sync-seam/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn hello_with(app: &AppState, inbox: &Inbox, profile: serde_json::Value, voices: Option<&[u8]>) -> HelloReply {
    let body = serde_json::to_vec(&serde_json::json!({
        "v": PROTOCOL_V, "device_id": PHONE, "sessions": [], "tombstones": [], "profile": profile,
    }))
    .unwrap();
    hello_reply(inbox, &app.profile.sessions_dir(), PHONE, &phone_pub(), &desktop_key(), &machine_key(), &body, voices)
        .unwrap()
}

fn header(offer: &crate::sync::exchange::Offer) -> serde_json::Value {
    serde_json::from_slice(&B64.decode(&offer.header_b64).unwrap()).unwrap()
}

#[test]
fn the_merged_catalog_is_offered_back_on_the_hello_after_the_upload() {
    let (app, vs, inbox, _td) = desktop();
    save_tags_file(&app, &TagsFile { tags: vec![tag("d0", "Northwind", 100), tag("d1", "Weekly", 100)], ..Default::default() }).unwrap();
    let plain = serde_json::to_vec(&TagsFile { tags: vec![tag("p0", "Harbour survey", 100)], ..Default::default() }).unwrap();
    let sealed = seal_to(&SECRET, "mac-1", &plain);
    let phone_hash = body_hash(&sealed);

    // 1. The phone names a revision this desktop does not hold.
    let first = hello_with(&app, &inbox, serde_json::json!({ "tags": phone_hash }), None);
    assert_eq!(first.profile_wants, ["tags.json"]);
    assert!(first.profile_offers.is_empty(), "nothing to offer before the merge");

    // 2. The phone uploads it, and the desktop merges what it staged.
    inbox.stage_record(PHONE, "profile", "tags.json", &phone_hash, &sealed).unwrap();
    assert!(merge_staged_profile(&app, &vs, &inbox, PHONE, &machine_key()));
    let mut names: Vec<String> = load_tags_file(&app).unwrap().tags.into_iter().map(|t| t.name).collect();
    names.sort();
    assert_eq!(names, ["Harbour survey", "Northwind", "Weekly"]);

    // 3. The next hello names the same phone revision and gets the merged
    //    file back under it.
    let second = hello_with(&app, &inbox, serde_json::json!({ "tags": phone_hash }), None);
    assert!(second.profile_wants.is_empty(), "a revision already merged is not asked for again");
    assert_eq!(second.profile_offers.len(), 1);
    assert_eq!(second.profile_offers[0].name, "tags.json");

    // 4. The phone decodes the header with one strict shape for every artifact.
    let h = header(&second.profile_offers[0]);
    let mut keys: Vec<&str> = h.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["artifact_id", "built_from", "ids", "merged_from", "name", "session", "size", "v"]);
    assert_eq!(h["merged_from"], serde_json::json!(phone_hash));
    assert_eq!(
        crate::sync::exchange::artifact_bytes(&inbox, &app.profile.sessions_dir(), None, &second.profile_offers[0].artifact_id).unwrap(),
        std::fs::read(app.profile.root().join("tags.json")).unwrap(),
        "the offer downloads as the file it names"
    );
}

#[test]
fn a_phone_that_names_no_voices_file_is_given_this_desktops_to_install_whole() {
    let (app, _vs, inbox, _td) = desktop();
    let mine = crate::sync::voices::encode(&crate::sync::voices::VoicesFile {
        deleted: vec![crate::sync::records::Tombstone::delete("v-gone", 1)],
        ..Default::default()
    })
    .unwrap();
    let reply = hello_with(&app, &inbox, serde_json::json!({}), Some(&mine));
    let offer = reply.profile_offers.iter().find(|o| o.name == "voices.json").expect("offered like any record");
    assert!(header(offer)["merged_from"].is_null());
    let none = hello_with(&app, &inbox, serde_json::json!({}), None);
    assert!(none.profile_offers.iter().all(|o| o.name != "voices.json"), "a vault holding nothing has no file");
}

#[test]
fn the_phones_voices_merge_into_the_vault_and_go_back_merged() {
    use crate::sync::voices;
    let (app, vs, inbox, _td) = desktop();
    crate::commands::lifecycle::init_vault_impl(&app, &vs, "correct-horse-battery-staple-extended-22").unwrap();
    let sample = |session: &str, at: i64| voices::Sample {
        key: voices::SampleKey { session: Some(session.into()), diarization_id: Some("run-1".into()), speaker: Some(0), legacy: None },
        model: voices::MODEL.into(),
        v: voices::encode_vector(&[0.6, 0.8]),
        at,
        extra: Default::default(),
    };
    crate::commands::voiceprints::update_vault(&app, &vs, |keys| {
        keys.voiceprints.push(crate::state::Voiceprint {
            id: "vp-desk".into(),
            display_name: "Priya Natarajan".into(),
            email: Some("priya@example.org".into()),
            vector: vec![],
            vectors: vec![],
            samples: vec![sample("daisy-1", 100)],
            created_at_unix_seconds: 100,
            updated_at_unix_seconds: Some(100),
            session_ids: vec!["daisy-1".into()],
            contact_id: Some("c-priya".into()),
            extra: Default::default(),
        });
        Ok(())
    })
    .unwrap();
    let theirs = voices::VoicesFile {
        voices: vec![voices::Voice {
            id: "vp-phone".into(),
            display_name: "Priya N.".into(),
            contact_id: Some("c-priya".into()),
            created_at_unix_seconds: 150,
            updated_at_unix_seconds: Some(200),
            samples: vec![sample("daisy-2", 200)],
            extra: Default::default(),
        }],
        ..Default::default()
    };
    let sealed = seal_to(&SECRET, "mac-1", &voices::encode(&theirs).unwrap());
    let phone_hash = body_hash(&sealed);
    let mine = || voices::bytes(vs.keys.lock().unwrap().as_ref().unwrap());

    let first = hello_with(&app, &inbox, serde_json::json!({ "voices": phone_hash }), mine().as_deref());
    assert_eq!(first.profile_wants, ["voices.json"]);

    inbox.stage_record(PHONE, "profile", "voices.json", &phone_hash, &sealed).unwrap();
    assert!(merge_staged_profile(&app, &vs, &inbox, PHONE, &machine_key()));
    {
        let g = vs.keys.lock().unwrap();
        let keys = g.as_ref().unwrap();
        assert_eq!(keys.voiceprints.len(), 1, "one person, one voice");
        let v = &keys.voiceprints[0];
        assert_eq!((v.id.as_str(), v.display_name.as_str()), ("vp-phone", "Priya N."));
        assert_eq!(v.samples.len(), 2, "both galleries");
        assert_eq!(v.email.as_deref(), Some("priya@example.org"), "this machine's own fields follow the voice");
        assert!(keys.voice_deleted.iter().any(|d| d.id == "vp-desk" && d.into.is_none()));
    }

    let second = hello_with(&app, &inbox, serde_json::json!({ "voices": phone_hash }), mine().as_deref());
    assert!(second.profile_wants.is_empty());
    let offer = second.profile_offers.iter().find(|o| o.name == "voices.json").expect("the merged voices go back");
    assert_eq!(offer.artifact_id, body_hash(&mine().unwrap()));
    assert_eq!(header(offer)["ids"], serde_json::json!({}), "the phone's id stood");
}

/// A desktop holding the seam's desktop contacts, a vault with a voice on
/// the contact the phone deleted and one on the contact that collapsed, and
/// a session naming both on its own run.
fn desktop_with_voices() -> (AppState, VaultState, Inbox, tempfile::TempDir) {
    let (app, vs, inbox, td) = desktop();
    crate::commands::lifecycle::init_vault_impl(&app, &vs, "correct-horse-battery-staple-extended-22").unwrap();
    let file: ContactsFile = serde_json::from_slice(&seam("contacts.desktop-v2.json")).unwrap();
    crate::commands::contacts::save_contacts_file(&app, &file).unwrap();
    let sample = crate::sync::voices::Sample {
        key: crate::sync::voices::SampleKey { session: Some("daisy-1".into()), diarization_id: Some("run-1".into()), speaker: Some(0), legacy: None },
        model: crate::sync::voices::MODEL.into(),
        v: crate::sync::voices::encode_vector(&[0.6, 0.8]),
        at: 1789550000,
        extra: Default::default(),
    };
    let voice = |id: &str, contact: &str| crate::state::Voiceprint {
        id: id.into(),
        display_name: id.into(),
        email: None,
        vector: vec![],
        vectors: vec![],
        samples: vec![sample.clone()],
        created_at_unix_seconds: 1789000000,
        updated_at_unix_seconds: Some(1789000000),
        session_ids: vec![],
        contact_id: Some(contact.into()),
        extra: Default::default(),
    };
    crate::commands::voiceprints::update_vault(&app, &vs, |keys| {
        keys.voiceprints = vec![voice("vp-ana", "c-ana"), voice("vp-kai", "c-desk-kai")];
        Ok(())
    })
    .unwrap();
    let dir = app.profile.sessions_dir().join("daisy-1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 4, "session_id": "daisy-1", "created_at_unix_seconds": 1, "sample_rate": 16000,
            "channels": 1, "chunks": [], "finalized_at_unix_seconds": 2, "diarization_id": "run-1",
            "speaker_map": [
                { "cluster_id": 0, "display_name": "Ana Okafor", "contact_id": "c-ana", "email": "ana@example.com" },
                { "cluster_id": 1, "display_name": "Kai Moreno", "contact_id": "c-desk-kai" },
                { "cluster_id": 2, "display_name": "Bo", "contact_id": "c-bo", "email": "bo@example.com" },
            ],
        }))
        .unwrap(),
    )
    .unwrap();
    (app, vs, inbox, td)
}

fn merge_contacts_from_phone(app: &AppState, vs: &VaultState, inbox: &Inbox, plain: &[u8]) {
    let sealed = seal_to(&SECRET, "mac-1", plain);
    inbox.stage_record(PHONE, "profile", "contacts.json", &body_hash(&sealed), &sealed).unwrap();
    assert!(merge_staged_profile(app, vs, inbox, PHONE, &machine_key()));
}

fn labels(app: &AppState) -> serde_json::Value {
    let dir = app.profile.sessions_dir().join("daisy-1");
    let m: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    m["speaker_map"].clone()
}

#[test]
fn a_contact_the_phone_deleted_takes_its_voice_and_leaves_its_name() {
    let (app, vs, inbox, _td) = desktop_with_voices();
    merge_contacts_from_phone(&app, &vs, &inbox, &seam("contacts.phone-v2.json"));
    {
        let g = vs.keys.lock().unwrap();
        let keys = g.as_ref().unwrap();
        assert!(!keys.voiceprints.iter().any(|v| v.id == "vp-ana"), "the voice outlived its contact");
        let d = keys.voice_deleted.iter().find(|d| d.id == "vp-ana").expect("the voice was not tombstoned");
        assert_eq!(d.at, 1789550000, "no earlier than the voice's newest sample, so every sample goes");
    }
    let l = labels(&app);
    assert_eq!(l[0]["display_name"], "Ana Okafor");
    assert!(l[0]["contact_id"].is_null(), "{l}");
    assert!(l[0]["email"].is_null(), "a deleted contact's email stays on its label: {l}");
    let meta = recording::session_meta::load(&app.profile.sessions_dir().join("daisy-1"));
    assert_eq!(meta.speakers("run-1").unwrap()["0"].contact_id, None, "the session's own file lost the id");
    assert_eq!(
        std::fs::read(app.profile.root().join("contacts.json")).unwrap(),
        seam("contacts.merged-v2.json"),
        "the file written here is the pinned merge"
    );
}

#[test]
fn a_contact_merged_into_another_moves_its_references_and_keeps_its_voice() {
    let (app, vs, inbox, _td) = desktop_with_voices();
    merge_contacts_from_phone(&app, &vs, &inbox, &seam("contacts.phone-v2.json"));
    {
        let g = vs.keys.lock().unwrap();
        let keys = g.as_ref().unwrap();
        let kai = keys.voiceprints.iter().find(|v| v.id == "vp-kai").expect("a collapse deleted a voice");
        assert_eq!(kai.contact_id.as_deref(), Some("c-phone-kai"));
        assert!(!keys.voice_deleted.iter().any(|d| d.id == "vp-kai"));
    }
    assert_eq!(labels(&app)[1]["contact_id"], "c-phone-kai");
}

#[test]
fn a_contact_the_phone_merged_away_moves_its_references_here() {
    let (app, vs, inbox, _td) = desktop_with_voices();
    let phone = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "contacts": [{ "id": "c-zed", "display_name": "Zed Quill", "emails": [], "created_at_unix_seconds": 1799999999 }],
        "deleted": [{ "id": "c-bo", "at": 1799999999, "into": "c-zed" }],
    }))
    .unwrap();
    merge_contacts_from_phone(&app, &vs, &inbox, &phone);
    let l = labels(&app);
    assert_eq!(l[2]["contact_id"], "c-zed", "{l}");
    assert_eq!(l[2]["email"], "bo@example.com", "a merge keeps the label's email");
}

#[test]
fn a_tag_the_phone_merged_into_one_here_follows_it_on_every_session() {
    let (app, vs, inbox, _td) = desktop();
    save_tags_file(&app, &TagsFile { tags: vec![tag("t-desk", "Harbour", 100)], ..Default::default() }).unwrap();
    let dir = app.profile.sessions_dir().join("daisy-1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest.json"), phone_manifest("Standup")).unwrap();
    recording::session_meta::update(&dir, |m| m.set_tag_ids(["t-phone".to_string()], 50)).unwrap();
    let phone = serde_json::to_vec(&TagsFile { tags: vec![tag("t-phone", "harbour", 90)], ..Default::default() }).unwrap();
    stage_profile(&inbox, "tags.json", &phone);
    assert!(merge_staged_profile(&app, &vs, &inbox, PHONE, &machine_key()));
    let meta = recording::session_meta::load(&dir);
    assert_eq!(meta.attached_tag_ids(), ["t-desk"]);
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["tag_ids"], serde_json::json!(["t-desk"]), "the manifest's copy follows");
}

#[test]
fn a_tag_list_naming_retired_tags_is_written_with_the_tags_that_stand() {
    use crate::sync::records::Tombstone;
    let (app, vs, _inbox, _td) = desktop();
    let merged = Tombstone { id: "t-old".into(), at: 100, into: Some("t-new".into()), extra: Default::default() };
    save_tags_file(
        &app,
        &TagsFile { tags: vec![tag("t-new", "Harbour", 100)], deleted: vec![merged, Tombstone::delete("t-gone", 100)], ..Default::default() },
    )
    .unwrap();
    let dir = app.profile.sessions_dir().join("daisy-1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest.json"), phone_manifest("Standup")).unwrap();

    let named = ["t-old", "t-gone", "t-unseen"].map(String::from).to_vec();
    crate::commands::meeting::session_assign_tags_impl(&app, &vs, "daisy-1", named).unwrap();

    let meta = recording::session_meta::load(&dir);
    assert_eq!(meta.attached_tag_ids(), ["t-new", "t-unseen"], "a merged-away tag lands on its successor; one the catalog never saw stays");
    assert!(!meta.tag_ids.contains_key("t-old") && !meta.tag_ids.contains_key("t-gone"), "no cell stays under a retired id");
}

#[test]
fn a_tag_attached_here_while_the_phones_file_merges_is_not_lost() {
    let (app, _vs, inbox, _td) = desktop();
    let sessions = app.profile.sessions_dir();
    let dir = sessions.join(SESSION);
    std::fs::create_dir_all(&dir).unwrap();
    inbox.record_import(PHONE, SESSION, &Default::default()).unwrap();
    let dir_for_writer = dir.clone();
    let writer = std::thread::spawn(move || {
        for i in 0..40 {
            recording::session_meta::update(&dir_for_writer, |m| {
                let mut ids = m.attached_tag_ids();
                ids.push(format!("d-{i:02}"));
                m.set_tag_ids(ids, 1_000 + i);
            })
            .unwrap();
        }
    });
    for i in 0..40 {
        let mut theirs = recording::session_meta::SessionMeta::default();
        theirs.set_title(&format!("From the phone {i}"), 10 + i);
        let bytes = recording::session_meta::encode(&theirs).unwrap();
        inbox.stage_record(PHONE, SESSION, "meta.json", &body_hash(&bytes), &bytes).unwrap();
        crate::sync::records::merge_staged_meta(&inbox, &sessions, PHONE);
    }
    writer.join().unwrap();
    let attached = recording::session_meta::load(&dir).attached_tag_ids();
    let lost: Vec<String> = (0..40).map(|i| format!("d-{i:02}")).filter(|id| !attached.contains(id)).collect();
    assert!(lost.is_empty(), "tags attached here were written over by the merge: {lost:?}");
}

#[test]
fn a_phone_diarized_session_named_here_lands_in_the_phones_run_map() {
    let (app, _vs, inbox, _td) = desktop();
    let sessions = app.profile.sessions_dir();
    stage_session(&inbox, &phone_manifest("Standup"));
    let speakers = seam("speakers.phone-v2.json");
    inbox.store(PHONE, SESSION, "speakers.json", &body_hash(&speakers), &speakers).unwrap();
    let local = import_session(&inbox, &sessions, PHONE, SESSION, &machine_key()).unwrap();
    let dir = sessions.join(&local);
    assert_eq!(recording::session_meta::current_run(&dir).as_deref(), Some("run-phone"), "the table the phone sent names the run");

    crate::commands::session::set_session_speaker_label_impl(&app, &local, 1, "Sam Ortiz".into(), None).unwrap();

    let meta = recording::session_meta::load(&dir);
    assert_eq!(meta.speaker_maps.keys().collect::<Vec<_>>(), ["run-phone"], "no run of this desktop's own was minted");
    assert_eq!(meta.speakers("run-phone").unwrap()["1"].display_name, "Sam Ortiz");
    // The file goes to the phone, which reads that run's map.
    let minted = crate::sync::import::minted_ids(&sessions);
    let (_, offer) = crate::sync::records::meta_exchange(&inbox, &sessions, &minted, PHONE, SESSION, None, &desktop_key());
    let offered: serde_json::Value = serde_json::from_slice(
        &crate::sync::records::artifact(&inbox, &sessions, None, &offer.expect("offered").artifact_id).unwrap(),
    )
    .unwrap();
    assert_eq!(offered["speaker_maps"]["run-phone"]["1"]["display_name"], "Sam Ortiz");
}

#[test]
fn a_legacy_phone_session_named_here_lands_in_the_legacy_map_and_its_sample_names_no_run() {
    use recording::session_meta::LEGACY_RUN;
    let (app, _vs, inbox, _td) = desktop();
    let sessions = app.profile.sessions_dir();
    stage_session(&inbox, &phone_manifest("Standup"));
    let speakers = seam("speakers.phone-legacy.json");
    inbox.store(PHONE, SESSION, "speakers.json", &body_hash(&speakers), &speakers).unwrap();
    let local = import_session(&inbox, &sessions, PHONE, SESSION, &machine_key()).unwrap();
    let dir = sessions.join(&local);
    assert_eq!(recording::session_meta::current_run(&dir).as_deref(), Some(LEGACY_RUN));

    crate::commands::session::set_session_speaker_label_impl(&app, &local, 1, "Sam Ortiz".into(), None).unwrap();

    let meta = recording::session_meta::load(&dir);
    assert_eq!(meta.speaker_maps.keys().collect::<Vec<_>>(), [LEGACY_RUN], "nothing is minted for a phone's table");
    assert_eq!(meta.speakers(LEGACY_RUN).unwrap()["1"].display_name, "Sam Ortiz", "the name lands at once");
    assert!(recording::speakers::read(&dir).unwrap().unwrap().diarization_id.is_none(), "the phone's table is left as it came");
    let key = crate::commands::voiceprints::sample_source(&dir, &local, 1);
    assert_eq!(key.diarization_id, None, "a legacy cluster's sample keeps no run, so samples taken before still match");
    assert_eq!(key.session.as_deref(), Some(SESSION));
}
