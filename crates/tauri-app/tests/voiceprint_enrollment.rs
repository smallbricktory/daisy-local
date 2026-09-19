// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Teaching Daisy a voice, and finding that voice again in other meetings.
//!
//! Enrollment writes a vector into the vault that is matched against forever,
//! so what the encoder is handed matters: these sessions carry real audio with
//! two distinguishable voices on their own windows, and the assertions are
//! about what lands in the vault, in `manifest.speaker_map`, and in the
//! contact book after a sweep.
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tauri_app_core::commands::contacts::list_contacts_impl;
use tauri_app_core::commands::lifecycle::{init_vault_impl, lock_vault_impl};
use tauri_app_core::commands::session::read_session_impl;
use tauri_app_core::commands::voiceprints::{
    enroll_voiceprint_from_speaker_impl, list_voiceprints_impl, rematch_all_sessions_impl,
    rematch_session_speakers_impl, EnrollFromSpeakerRequest,
};
use tauri_app_core::error::AppError;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, VaultState};

const PASS: &str = "correct-horse-battery-staple-extended-22";

/// The residency counter and `DAISY_VOICEPRINT_DIR` are process-wide, so
/// these tests take turns.
fn alone() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// Points the encoder at the checked-out weights. `None` when they are not
/// here, which is the signal to skip: every test below that needs an
/// embedding returns early rather than asserting against a missing model.
fn models() -> Option<()> {
    let dir = repo_root().join("models/voiceprints");
    dir.is_dir().then(|| {
        std::env::set_var("DAISY_VOICEPRINT_DIR", &dir);
    })
}

// ---- two voices ------------------------------------------------------------

mod voices;
use voices::{room_tone, rowan, zoe, RATE};

// ---- sessions --------------------------------------------------------------

/// A turn in a meeting: when it starts, how long it runs, whose cluster it is,
/// and which voice is actually speaking.
#[derive(Clone, Copy)]
struct Turn {
    start_ms: u32,
    seconds: u32,
    cluster: u32,
    /// `true` for Rowan, `false` for Zoe.
    is_rowan: bool,
    text: &'static str,
}

const SECONDS: u32 = 90;

/// Rowan and Zoe taking turns, each on their own cluster.
const CLEAN: &[Turn] = &[
    Turn { start_ms: 0, seconds: 10, cluster: 0, is_rowan: true, text: "the rebate import is first" },
    Turn { start_ms: 12_000, seconds: 10, cluster: 1, is_rowan: false, text: "the bank sends a fixed width file" },
    Turn { start_ms: 24_000, seconds: 10, cluster: 0, is_rowan: true, text: "and the portal certificate is second" },
    Turn { start_ms: 36_000, seconds: 10, cluster: 1, is_rowan: false, text: "the rotation takes about a day" },
    Turn { start_ms: 48_000, seconds: 10, cluster: 0, is_rowan: true, text: "then we escalate rather than fall back" },
    Turn { start_ms: 60_000, seconds: 10, cluster: 1, is_rowan: false, text: "northwind wants the summary by friday" },
];

/// One cluster holding both people — a diarization that ran two speakers
/// together.
const MIXED: &[Turn] = &[
    Turn { start_ms: 0, seconds: 10, cluster: 0, is_rowan: true, text: "the rebate import is first" },
    Turn { start_ms: 12_000, seconds: 10, cluster: 0, is_rowan: false, text: "the bank sends a fixed width file" },
    Turn { start_ms: 24_000, seconds: 10, cluster: 0, is_rowan: true, text: "and the portal certificate is second" },
    Turn { start_ms: 36_000, seconds: 10, cluster: 0, is_rowan: false, text: "the rotation takes about a day" },
];

/// A single turn far too short to enroll from.
const SLIVER: &[Turn] = &[Turn {
    start_ms: 0,
    seconds: 0,
    cluster: 0,
    is_rowan: true,
    text: "morning",
}];

fn wav(path: &std::path::Path, pcm: &[i16]) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for s in pcm {
        w.write_sample(*s).unwrap();
    }
    w.finalize().unwrap();
}

/// A finished session whose mic track carries each turn's own voice at that
/// turn's own offset, with room tone in between.
fn session(app: &AppState, id: &str, turns: &[Turn]) {
    let root = app.profile.session_path(id);
    let chunk = root.join("chunks/0000");
    fs::create_dir_all(&chunk).unwrap();

    let total = RATE as usize * SECONDS as usize;
    let mut mic = room_tone(total);
    for t in turns {
        let from = (t.start_ms as usize) * RATE as usize / 1000;
        // A 200 ms turn is written as 200 ms: the sliver fixture depends on it.
        let len = ((t.seconds as usize) * RATE as usize).max(RATE as usize / 5);
        let pcm = if t.is_rowan { rowan(len) } else { zoe(len) };
        for (i, s) in pcm.iter().enumerate() {
            if from + i < total {
                mic[from + i] = *s;
            }
        }
    }
    wav(&chunk.join("mic.wav"), &mic);
    wav(&chunk.join("system.wav"), &room_tone(total));

    let manifest = serde_json::json!({
        "schema_version": 4,
        "session_id": id,
        "created_at_unix_seconds": 1_788_300_000i64,
        "sample_rate": RATE,
        "channels": 1,
        "finalized_at_unix_seconds": 1_788_300_000i64 + SECONDS as i64,
        "chunks": [{
            "index": 0,
            "started_at_unix_seconds": 1_788_300_000i64,
            "ended_at_unix_seconds": 1_788_300_000i64 + SECONDS as i64,
            "duration_seconds": SECONDS,
            "mic_wav_relative": "chunks/0000/mic.wav",
            "system_wav_relative": "chunks/0000/system.wav",
            "mic_aec_wav_relative": null,
        }],
        "attendees": [],
        "tag_ids": [],
        "speaker_map": [],
    });
    fs::write(root.join("manifest.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();

    let segs: Vec<serde_json::Value> = turns
        .iter()
        .map(|t| {
            serde_json::json!({
                "start_ms": t.start_ms,
                "end_ms": t.start_ms + (t.seconds * 1_000).max(200),
                "text": t.text,
                "confidence": null,
                "speaker_id": t.cluster,
            })
        })
        .collect();
    let transcript = serde_json::json!({
        "schema_version": 1,
        "session_id": id,
        "provider": "whisper",
        "model": "base.en",
        "transcribed_at_unix_seconds": 1_788_300_100i64,
        "chunks": [{
            "chunk_index": 0,
            "tracks": [{
                "track": "mic",
                "source_wav_relative": "chunks/0000/mic.wav",
                "segments": segs,
            }],
        }],
    });
    fs::write(root.join("transcript.json"), serde_json::to_vec_pretty(&transcript).unwrap())
        .unwrap();
}

fn unlocked() -> (AppState, VaultState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let app = AppState::new(profile);
    let vs = VaultState::new();
    init_vault_impl(&app, &vs, PASS).unwrap();
    (app, vs, dir)
}

fn enroll(
    app: &AppState,
    vs: &VaultState,
    session_id: &str,
    cluster_id: u32,
    name: &str,
    email: Option<&str>,
) -> tauri_app_core::error::Result<tauri_app_core::commands::voiceprints::EnrollResult> {
    enroll_voiceprint_from_speaker_impl(
        app,
        vs,
        EnrollFromSpeakerRequest {
            session_id: session_id.into(),
            cluster_id,
            display_name: name.into(),
            email: email.map(str::to_string),
            allow_mixed: false,
        },
    )
}

/// The `speaker_map` entry for one cluster, as the Speakers screen reads it.
fn label(app: &AppState, sid: &str, cluster: u32) -> Option<serde_json::Value> {
    read_session_impl(app, sid).unwrap().manifest_json["speaker_map"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["cluster_id"] == cluster)
        .cloned()
}

// ---- the guards, which need no model ---------------------------------------

#[test]
fn enrolling_without_a_name_is_refused() {
    let _alone = alone();
    let (app, vs, _d) = unlocked();
    session(&app, "daisy-1788300001", CLEAN);
    let e = enroll(&app, &vs, "daisy-1788300001", 0, "   ", None).unwrap_err();
    assert!(
        matches!(&e, AppError::Config(m) if m.contains("display name")),
        "an unnamed enrollment must say what is missing: {e:?}"
    );
    assert!(
        list_contacts_impl(&app).unwrap().is_empty(),
        "and it must not have made a contact on the way out"
    );
}

#[test]
fn enrolling_from_a_session_that_is_not_there_names_it() {
    let _alone = alone();
    let (app, vs, _d) = unlocked();
    let e = enroll(&app, &vs, "daisy-nope", 0, "Rowan Hale", None).unwrap_err();
    assert!(
        matches!(&e, AppError::SessionNotFound(s) if s == "daisy-nope"),
        "{e:?}"
    );
}

// ---- enrollment ------------------------------------------------------------

#[test]
fn enrolling_writes_the_vault_entry_the_contact_and_the_speaker_label() {
    let _alone = alone();
    if models().is_none() {
        return;
    }
    let (app, vs, _d) = unlocked();
    let sid = "daisy-1788300002";
    session(&app, sid, CLEAN);

    let res = enroll(&app, &vs, sid, 0, "Rowan Hale", Some("rowan@northwind.example"))
        .expect("Rowan's cluster enrolls");

    assert!(res.vector_dim > 0, "an embedding with no dimensions is not one");
    let vps = list_voiceprints_impl(&app, &vs).unwrap();
    assert_eq!(vps.len(), 1, "one person enrolled, one identity");
    assert_eq!(vps[0].display_name, "Rowan Hale");
    assert_eq!(vps[0].email.as_deref(), Some("rowan@northwind.example"));
    assert_eq!(vps[0].sample_count, 1);
    assert_eq!(vps[0].session_count, 1, "the session it came from is recorded");

    let contacts = list_contacts_impl(&app).unwrap();
    assert_eq!(contacts.len(), 1, "enrolling makes the person a contact");
    assert_eq!(contacts[0].display_name, "Rowan Hale");

    let l = label(&app, sid, 0).expect("cluster 0 is labeled");
    assert_eq!(l["display_name"], "Rowan Hale");
    assert_eq!(
        l["voiceprint_id"], res.voiceprint_id,
        "the cluster points at the entry that was written"
    );
    assert!(
        l["match_confidence"].is_null(),
        "an enrollment is certain, so it carries no match score: {l}"
    );
    assert!(
        label(&app, sid, 1).is_none(),
        "the other speaker was not touched"
    );
}

#[test]
fn enrolling_the_same_person_twice_grows_one_gallery() {
    let _alone = alone();
    if models().is_none() {
        return;
    }
    let (app, vs, _d) = unlocked();
    session(&app, "daisy-1788300003", CLEAN);
    session(&app, "daisy-1788300004", CLEAN);

    let first = enroll(&app, &vs, "daisy-1788300003", 0, "Rowan Hale", Some("rowan@northwind.example"))
        .unwrap();
    // The same person, typed differently the second time.
    let second = enroll(&app, &vs, "daisy-1788300004", 0, "rowan hale", Some("Rowan@Northwind.Example"))
        .unwrap();

    let vps = list_voiceprints_impl(&app, &vs).unwrap();
    assert_eq!(vps.len(), 1, "the same email is the same person: {vps:?}");
    assert_eq!(vps[0].sample_count, 2, "the second sample joined the gallery");
    assert_eq!(vps[0].session_count, 2, "both meetings are recorded");
    assert_eq!(
        second.voiceprint_id, first.voiceprint_id,
        "and the id the UI holds did not change under it"
    );
    assert_eq!(
        list_contacts_impl(&app).unwrap().len(),
        1,
        "one contact, not one per enrollment"
    );
}

#[test]
fn a_gallery_never_grows_past_its_cap() {
    let _alone = alone();
    if models().is_none() {
        return;
    }
    let (app, vs, _d) = unlocked();
    let sid = "daisy-1788300005";
    session(&app, sid, CLEAN);
    // GALLERY_CAP is 16.
    for _ in 0..18 {
        enroll(&app, &vs, sid, 0, "Rowan Hale", Some("rowan@northwind.example")).unwrap();
    }
    let vps = list_voiceprints_impl(&app, &vs).unwrap();
    assert_eq!(vps.len(), 1);
    assert_eq!(
        vps[0].sample_count, 16,
        "eighteen enrollments must not keep eighteen vectors"
    );
}

#[test]
fn a_cluster_carrying_two_voices_is_refused_until_the_owner_insists() {
    let _alone = alone();
    if models().is_none() {
        return;
    }
    let (app, vs, _d) = unlocked();
    let sid = "daisy-1788300006";
    session(&app, sid, MIXED);

    let e = enroll(&app, &vs, sid, 0, "Rowan Hale", None).unwrap_err();
    assert!(
        matches!(&e, AppError::Config(m) if m.contains("one clear voice")),
        "a cluster holding two people must say so: {e:?}"
    );
    assert!(
        list_voiceprints_impl(&app, &vs).unwrap().is_empty(),
        "and nothing may reach the vault on the way to that refusal"
    );

    enroll_voiceprint_from_speaker_impl(
        &app,
        &vs,
        EnrollFromSpeakerRequest {
            session_id: sid.into(),
            cluster_id: 0,
            display_name: "Rowan Hale".into(),
            email: None,
            allow_mixed: true,
        },
    )
    .expect("the owner can hear what the gate cannot");
    assert_eq!(
        list_voiceprints_impl(&app, &vs).unwrap().len(),
        1,
        "saying it anyway enrolls it"
    );
}

#[test]
fn a_cluster_with_almost_no_speech_is_refused() {
    let _alone = alone();
    if models().is_none() {
        return;
    }
    let (app, vs, _d) = unlocked();
    let sid = "daisy-1788300007";
    session(&app, sid, SLIVER);
    let e = enroll(&app, &vs, sid, 0, "Rowan Hale", None).unwrap_err();
    assert!(
        matches!(&e, AppError::Config(m) if m.contains("not enough speech")),
        "{e:?}"
    );
}

#[test]
fn enrolling_into_a_locked_vault_is_refused() {
    let _alone = alone();
    if models().is_none() {
        return;
    }
    let (app, vs, _d) = unlocked();
    let sid = "daisy-1788300008";
    session(&app, sid, CLEAN);
    lock_vault_impl(&app, &vs).unwrap();
    let e = enroll(&app, &vs, sid, 0, "Rowan Hale", None).unwrap_err();
    assert!(matches!(e, AppError::VaultLocked), "{e:?}");
    assert!(
        label(&app, sid, 0).is_none(),
        "a refused enrollment must not leave the cluster named"
    );
}

// ---- finding the voice again -----------------------------------------------

#[test]
fn a_sweep_finds_the_enrolled_voice_in_another_meeting() {
    let _alone = alone();
    if models().is_none() {
        return;
    }
    let (app, vs, _d) = unlocked();
    let taught = "daisy-1788300010";
    let unseen = "daisy-1788300011";
    session(&app, taught, CLEAN);
    session(&app, unseen, CLEAN);

    enroll(&app, &vs, taught, 0, "Rowan Hale", Some("rowan@northwind.example")).unwrap();

    let (scanned, matched) = rematch_all_sessions_impl(&app, &vs).unwrap();
    assert_eq!(scanned, 2, "both sessions were walked");
    assert!(matched >= 1, "Rowan was never found again: {matched} matched");

    let l = label(&app, unseen, 0).expect("the untaught meeting gained a label");
    assert_eq!(l["display_name"], "Rowan Hale");
    assert!(
        l["match_confidence"].as_f64().unwrap_or(0.0) > 0.0,
        "a match carries the score it was made on: {l}"
    );
    assert!(
        label(&app, unseen, 1).is_none(),
        "Zoe is not in the gallery, so her cluster must stay unnamed"
    );
}

#[test]
fn a_second_sweep_finds_nothing_left_to_do() {
    let _alone = alone();
    if models().is_none() {
        return;
    }
    let (app, vs, _d) = unlocked();
    let taught = "daisy-1788300012";
    let unseen = "daisy-1788300013";
    session(&app, taught, CLEAN);
    session(&app, unseen, CLEAN);
    enroll(&app, &vs, taught, 0, "Rowan Hale", Some("rowan@northwind.example")).unwrap();

    let (_, first) = rematch_all_sessions_impl(&app, &vs).unwrap();
    assert!(first >= 1, "the first sweep matched nothing to repeat");
    let before = label(&app, unseen, 0);

    let (scanned, second) = rematch_all_sessions_impl(&app, &vs).unwrap();
    assert_eq!(scanned, 2, "the second sweep still walks everything");
    assert_eq!(second, 0, "already-labeled clusters are not matched again");
    assert_eq!(before, label(&app, unseen, 0), "and the label is unchanged");
}

#[test]
fn a_sweep_with_an_empty_gallery_changes_nothing() {
    let _alone = alone();
    let (app, vs, _d) = unlocked();
    session(&app, "daisy-1788300014", CLEAN);

    let (scanned, matched) = rematch_all_sessions_impl(&app, &vs).unwrap();
    assert_eq!(scanned, 1, "the session is still walked");
    assert_eq!(matched, 0, "with nobody enrolled there is nothing to match");
    assert!(label(&app, "daisy-1788300014", 0).is_none());
}

#[test]
fn a_sweep_over_a_locked_vault_is_refused_rather_than_reported_as_a_clean_run() {
    let _alone = alone();
    let (app, vs, _d) = unlocked();
    let sid = "daisy-1788300015";
    session(&app, sid, CLEAN);
    lock_vault_impl(&app, &vs).unwrap();
    let e = rematch_session_speakers_impl(&app, &vs, sid).unwrap_err();
    assert!(matches!(e, AppError::VaultLocked), "{e:?}");
}

#[test]
fn matching_one_session_names_the_one_that_is_not_there() {
    let _alone = alone();
    let (app, vs, _d) = unlocked();
    // A gallery entry, so the sweep gets past its empty-gallery shortcut.
    session(&app, "daisy-1788300016", CLEAN);
    if models().is_some() {
        enroll(&app, &vs, "daisy-1788300016", 0, "Rowan Hale", None).unwrap();
        let e = rematch_session_speakers_impl(&app, &vs, "daisy-nope").unwrap_err();
        assert!(
            matches!(&e, AppError::SessionNotFound(s) if s == "daisy-nope"),
            "{e:?}"
        );
    }
}

// ---- what the two of them cost together ------------------------------------

#[test]
fn enrolling_while_a_sweep_runs_holds_one_speaker_encoder_between_them() {
    let _alone = alone();
    if models().is_none() {
        return;
    }
    let (app, vs, _d) = unlocked();
    let app = Arc::new(app);
    let vs = Arc::new(vs);
    let taught = "daisy-1788300020";
    let unseen = "daisy-1788300021";
    session(&app, taught, CLEAN);
    session(&app, unseen, CLEAN);
    // Something in the gallery, so the sweep actually loads the encoder.
    enroll(&app, &vs, taught, 1, "Zoe Winters", Some("zoe@northwind.example")).unwrap();
    assert_eq!(voiceprints::resident(), 0, "nothing is loaded yet");

    let gate = Arc::new(std::sync::Barrier::new(3));
    let peak = Arc::new(AtomicU64::new(0));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let watcher = {
        let (peak, done, gate) = (Arc::clone(&peak), Arc::clone(&done), Arc::clone(&gate));
        std::thread::spawn(move || {
            gate.wait();
            while !done.load(Ordering::Relaxed) {
                peak.fetch_max(voiceprints::resident(), Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };

    let sweeper = {
        let (app, vs, gate) = (Arc::clone(&app), Arc::clone(&vs), Arc::clone(&gate));
        std::thread::spawn(move || {
            gate.wait();
            rematch_all_sessions_impl(&app, &vs)
        })
    };
    let enroller = {
        let (app, vs, gate) = (Arc::clone(&app), Arc::clone(&vs), Arc::clone(&gate));
        std::thread::spawn(move || {
            gate.wait();
            enroll(&app, &vs, unseen, 0, "Rowan Hale", Some("rowan@northwind.example"))
        })
    };

    let _ = sweeper.join().unwrap();
    let _ = enroller.join().unwrap();
    done.store(true, Ordering::Relaxed);
    watcher.join().unwrap();

    let peak = peak.load(Ordering::Relaxed);
    assert_eq!(
        peak, 1,
        "an enrollment and a sweep must not each load the speaker model: \
         {peak} were resident at once"
    );
    assert_eq!(
        voiceprints::resident(),
        0,
        "and both are freed when the work ends"
    );
}

// ---- the fixture's own premise ---------------------------------------------

/// Everything above rests on Rowan and Zoe reading as two people. This states
/// the number, so a fixture that stopped separating them fails here rather
/// than as a puzzling result somewhere downstream.
#[test]
fn the_two_fixture_voices_are_two_people() {
    let _alone = alone();
    if models().is_none() {
        return;
    }
    let mut encoder = voiceprints::Encoder::load().expect("the speaker model loads");
    let a = encoder.encode_pcm(&rowan(RATE as usize * 10)).unwrap();
    let b = encoder.encode_pcm(&zoe(RATE as usize * 10)).unwrap();
    let a2 = encoder.encode_pcm(&rowan(RATE as usize * 8)).unwrap();
    let across = voiceprints::cosine(&a, &b);
    let within = voiceprints::cosine(&a, &a2);
    println!("[fixture] rowan-to-zoe {across:.3}, rowan-to-rowan {within:.3}");
    assert!(
        within > across,
        "a voice must sit closer to itself ({within:.3}) than to the other one ({across:.3})"
    );
    assert!(
        across < voiceprints::match_threshold(),
        "Rowan and Zoe sit {across:.3} apart, over the {:.2} at which a match is awarded — \
         every test here that expects them told apart is resting on nothing",
        voiceprints::match_threshold()
    );
}
