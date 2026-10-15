// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The one-voice gate a diarization run applies to the clusters it just made.
//!
//! After clustering, the run samples each cluster's enrollment audio, measures how far apart two different people sit in this session, and marks the clusters that do not read as one clear voice. Both land in the manifest (`speaker_separation` and `mixed_clusters`) and the Speakers screen reads them. The session is real WAVs through the real speaker encoder.
//!
//! The runs take their speaker turns from the test.
use std::fs;

use recording::manifest::{DiarizeCount, SessionManifest};
use tauri_app_core::commands::voiceprints::{
    diarize_session_impl, diarize_session_with_turns_impl, CountSource, DiarizeResult,
    SuppliedTurns,
};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, Voiceprint};

mod voices;
use voices::{room_tone, rowan, zoe, RATE};

const SESSION: &str = "daisy-1788200000";
const SECONDS: u32 = 90;

/// `DAISY_VOICEPRINT_DIR` is process-wide, so these tests take turns.
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

/// Rowan and Zoe taking ten-second turns twelve seconds apart — clear of the
/// isolation margin, so every turn can be sampled. `(start_ms, is_rowan)`.
const TURNS: &[(u32, bool)] = &[
    (0, true),
    (12_000, false),
    (24_000, true),
    (36_000, false),
    (48_000, true),
    (60_000, false),
];
const TURN_SECONDS: usize = 10;

/// One transcript segment per turn, no word times.
fn turn_segments() -> Vec<serde_json::Value> {
    TURNS
        .iter()
        .map(|&(start_ms, _)| {
            serde_json::json!({
                "start_ms": start_ms,
                "end_ms": start_ms + TURN_SECONDS as u32 * 1_000,
                "text": "the rebate import is the first thing",
                "confidence": null,
                "speaker_id": null,
            })
        })
        .collect()
}

/// The turns as the run receives them, on the mic of chunk 0.
fn supplied() -> SuppliedTurns {
    let spans = TURNS
        .iter()
        .map(|&(start_ms, _)| {
            let s = start_ms as f64 / 1000.0;
            (s, s + TURN_SECONDS as f64)
        })
        .collect();
    SuppliedTurns { mic: [(0, spans)].into_iter().collect(), ..Default::default() }
}

fn diarize(app: &AppState, typed: Option<u32>, gallery: &[Voiceprint]) -> DiarizeResult {
    diarize_session_with_turns_impl(app, SESSION, typed, None, gallery, &supplied()).unwrap()
}

/// A finished mic-only session: every turn carries its own speaker's voice,
/// room tone in between, and no speaker ids at all — the run assigns those.
fn session(app: &AppState) {
    session_with(app, turn_segments(), None);
}

/// [`session`] with these transcript segments and `expected_speakers`.
fn session_with(app: &AppState, segs: Vec<serde_json::Value>, expected: Option<u32>) {
    let root = app.profile.session_path(SESSION);
    let chunk = root.join("chunks/0000");
    fs::create_dir_all(&chunk).unwrap();

    let total = RATE as usize * SECONDS as usize;
    let mut mic = room_tone(total);
    for &(start_ms, is_rowan) in TURNS {
        let from = start_ms as usize * RATE as usize / 1000;
        let len = TURN_SECONDS * RATE as usize;
        let pcm = if is_rowan { rowan(len) } else { zoe(len) };
        mic[from..from + len].copy_from_slice(&pcm);
    }
    wav(&chunk.join("mic.wav"), &mic);

    let manifest = serde_json::json!({
        "schema_version": 4,
        "session_id": SESSION,
        "created_at_unix_seconds": 1_788_200_000i64,
        "sample_rate": RATE,
        "channels": 1,
        "finalized_at_unix_seconds": 1_788_200_000i64 + SECONDS as i64,
        "chunks": [{
            "index": 0,
            "started_at_unix_seconds": 1_788_200_000i64,
            "ended_at_unix_seconds": 1_788_200_000i64 + SECONDS as i64,
            "duration_seconds": SECONDS,
            "mic_wav_relative": "chunks/0000/mic.wav",
            "system_wav_relative": "chunks/0000/system.wav",
            "mic_aec_wav_relative": null,
        }],
        "attendees": [],
        "tag_ids": [],
        "expected_speakers": expected,
    });
    fs::write(root.join("manifest.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();

    let transcript = serde_json::json!({
        "schema_version": 1,
        "session_id": SESSION,
        "provider": "whisper",
        "model": "base.en",
        "transcribed_at_unix_seconds": 1_788_200_000i64 + SECONDS as i64 + 10,
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

fn manifest_of(app: &AppState) -> SessionManifest {
    let p = app.profile.session_path(SESSION).join("manifest.json");
    serde_json::from_slice(&fs::read(p).unwrap()).unwrap()
}

/// A profile holding the session, or `None` when the speaker-encoder weights are not staged in this checkout.
fn app() -> Option<(AppState, tempfile::TempDir)> {
    let (app, dir) = bare_app()?;
    session(&app);
    Some((app, dir))
}

/// A profile with no session yet.
fn bare_app() -> Option<(AppState, tempfile::TempDir)> {
    let models = repo_root().join("models/voiceprints");
    if !models.is_dir() {
        return None;
    }
    std::env::set_var("DAISY_VOICEPRINT_DIR", models);
    let dir = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(dir.path().join("daisy")).unwrap());
    Some((app, dir))
}

fn transcript_of(app: &AppState) -> transcript::SessionTranscript {
    let p = app.profile.session_path(SESSION).join("transcript.json");
    serde_json::from_slice(&fs::read(p).unwrap()).unwrap()
}

#[test]
fn a_mic_only_run_measures_the_session_separation_over_the_clusters_it_made() {
    let _alone = alone();
    let Some((app, _d)) = app() else { return };

    let r = diarize(&app, Some(2), &[]);
    assert_eq!(r.speakers, 2, "two voices, two clusters");
    assert_eq!(r.count_source, Some(CountSource::Typed));

    let m = manifest_of(&app);
    assert_eq!(m.cluster_sides.len(), 2, "sides: {:?}", m.cluster_sides);
    // A mic-only run leaves no surviving system-track clusters, so a gate that
    // samples only the untouched track measures nothing and this stays None.
    let sep = m
        .speaker_separation
        .expect("the run must measure how far apart two people sit in this session");
    assert!(
        (0.0..=1.0).contains(&sep),
        "separation is a cosine between two clusters' windows: {sep}"
    );
}

#[test]
fn a_successful_run_clears_the_voice_grouping_unavailable_flag() {
    let _alone = alone();
    let Some((app, _d)) = app() else { return };
    // An earlier processing pass could not load the model and flagged it.
    let mp = app.profile.session_path(SESSION).join("manifest.json");
    let mut m: serde_json::Value = serde_json::from_slice(&fs::read(&mp).unwrap()).unwrap();
    m["diarization_unavailable"] = serde_json::json!(true);
    fs::write(&mp, serde_json::to_vec_pretty(&m).unwrap()).unwrap();

    diarize(&app, Some(2), &[]);

    assert!(!manifest_of(&app).diarization_unavailable, "the banner goes once grouping succeeds");
}

#[test]
fn a_model_the_disk_cannot_read_is_reported_unreadable_and_an_absent_one_missing() {
    use tauri_app_core::commands::voiceprints::{voice_model_status_impl, voice_model_unusable};
    let _alone = alone();
    let Some((app, d)) = app() else { return };

    // Nothing at the model path.
    let empty = d.path().join("no-model");
    fs::create_dir_all(&empty).unwrap();
    std::env::set_var("DAISY_VOICEPRINT_DIR", &empty);
    let err = diarize_session_impl(&app, SESSION, Some(2), None, &[]).unwrap_err();
    assert!(voice_model_unusable(&err), "{err}");
    assert!(err.to_string().contains("voiceprint model missing"), "{err}");
    assert_eq!(voice_model_status_impl().state, "missing");

    // The speaker encoder alone is not enough: the segmentation network
    // missing reads the same way, naming its own path.
    let half = d.path().join("encoder-only");
    fs::create_dir_all(&half).unwrap();
    fs::write(half.join("model.onnx"), b"x").unwrap();
    std::env::set_var("DAISY_VOICEPRINT_DIR", &half);
    let status = voice_model_status_impl();
    assert_eq!(status.state, "missing");
    assert!(status.path.ends_with("segmentation-3.0.onnx"), "{}", status.path);
    let err = diarize_session_impl(&app, SESSION, Some(2), None, &[]).unwrap_err();
    assert!(voice_model_unusable(&err), "{err}");
    assert!(err.to_string().contains("segmentation-3.0.onnx"), "{err}");

    // A path the stat can't answer: its parent is a regular file (ENOTDIR),
    // which is no proof the model is absent.
    #[cfg(unix)]
    {
        let not_a_dir = d.path().join("plain-file");
        fs::write(&not_a_dir, b"x").unwrap();
        std::env::set_var("DAISY_VOICEPRINT_DIR", &not_a_dir);
        let err = diarize_session_impl(&app, SESSION, Some(2), None, &[]).unwrap_err();
        assert!(voice_model_unusable(&err), "{err}");
        assert!(err.to_string().contains("voiceprint model unreadable"), "{err}");
        assert!(!err.to_string().contains("missing"), "{err}");
        let status = voice_model_status_impl();
        assert_eq!(status.state, "unreadable");
        assert!(status.error.is_some());
    }
    std::env::set_var("DAISY_VOICEPRINT_DIR", repo_root().join("models/voiceprints"));
}

#[test]
fn two_voices_forced_into_one_cluster_do_not_read_as_one_clear_voice() {
    let _alone = alone();
    let Some((app, _d)) = app() else { return };

    // Told to expect one speaker, the run has to put both voices in cluster 0.
    let r = diarize_session_impl(&app, SESSION, Some(1), None, &[]).unwrap();
    assert_eq!(r.speakers, 1, "one cluster was asked for");

    let m = manifest_of(&app);
    assert_eq!(
        m.mixed_clusters,
        vec![0],
        "the cluster holding both voices must carry the chip"
    );
}

/// A segment running from Rowan's first turn into Zoe's, with word times.
fn spanning_segment() -> serde_json::Value {
    let words: Vec<serde_json::Value> = [
        ("we", 6_000), ("ship", 6_800), ("the", 7_600), ("rebate", 8_400), ("friday", 9_200),
        ("sounds", 12_200), ("good", 13_000), ("to", 13_800), ("me", 14_600), ("then", 15_200),
    ]
    .iter()
    .map(|&(t, at)| serde_json::json!({ "text": t, "start_ms": at, "end_ms": at + 600 }))
    .collect();
    serde_json::json!({
        "start_ms": 6_000,
        "end_ms": 16_000,
        "text": "we ship the rebate friday sounds good to me then",
        "confidence": null,
        "speaker_id": null,
        "words": words,
    })
}

#[test]
fn a_segment_two_people_share_splits_at_the_turn_and_a_rerun_does_not_fragment_it() {
    let _alone = alone();
    let Some((app, _d)) = bare_app() else { return };
    let mut segs = turn_segments();
    segs[0]["end_ms"] = serde_json::json!(6_000);
    segs[1]["start_ms"] = serde_json::json!(16_000);
    segs.insert(1, spanning_segment());
    session_with(&app, segs, None);

    let pieces = |st: &transcript::SessionTranscript| -> Vec<(u32, u32, Option<u32>, Option<u32>)> {
        st.chunks[0].tracks[0]
            .segments
            .iter()
            .map(|s| (s.start_ms, s.end_ms, s.speaker_id, s.split_group))
            .collect()
    };
    let r = diarize(&app, Some(2), &[]);
    assert_eq!(r.speakers, 2);
    let first = pieces(&transcript_of(&app));
    assert_eq!(first.len(), 8, "the shared segment is cut in two: {first:?}");
    let (a, b) = (first[1], first[2]);
    assert_eq!((a.0, b.1), (6_000, 16_000), "the pieces keep the segment's outer edges");
    assert!(a.1 <= 12_200 && b.0 >= 9_800, "the cut sits between the speakers: {a:?} {b:?}");
    assert_ne!(a.2, b.2, "each piece goes to its own speaker");
    assert_eq!(a.2, first[0].2, "the first piece is Rowan's, like the turn before it");
    assert_eq!(b.2, first[3].2, "the second piece is Zoe's, like the rest of her turn");
    assert!(a.3.is_some() && a.3 == b.3, "the pieces share one split group");

    // The pieces are joined before the run splits again, so a second run
    // finds the source segment, not two fragments to cut further.
    diarize(&app, Some(2), &[]);
    let second = pieces(&transcript_of(&app));
    assert_eq!(second.len(), 8, "a re-run does not fragment the transcript: {second:?}");
    assert_eq!((second[1].0, second[1].1, second[2].0, second[2].1), (a.0, a.1, b.0, b.1));
}

/// A vault voice enrolled from `pcm`.
fn enrolled(name: &str, pcm: &[i16]) -> Voiceprint {
    use base64::Engine;
    let mut encoder = voiceprints::Encoder::load().unwrap();
    let v = encoder.encode_pcm(pcm).unwrap();
    let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    serde_json::from_value(serde_json::json!({
        "id": format!("vp-{name}"),
        "display_name": name,
        "samples": [{
            "model": "wespeaker-voxceleb-resnet34-LM",
            "v": base64::engine::general_purpose::STANDARD.encode(bytes),
            "at": 1_788_000_000i64,
        }],
        "created_at_unix_seconds": 1_788_000_000i64,
    }))
    .unwrap()
}

/// Marks the session as several people on the mic, so the run groups the mic by voice.
fn several_on_the_mic(app: &AppState) {
    let mp = app.profile.session_path(SESSION).join("manifest.json");
    let mut m: serde_json::Value = serde_json::from_slice(&fs::read(&mp).unwrap()).unwrap();
    m["single_local_speaker"] = serde_json::json!(false);
    fs::write(&mp, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
}

fn diarize_mic(app: &AppState, typed: Option<u32>, gallery: &[Voiceprint]) -> DiarizeResult {
    use tauri_app_core::commands::voiceprints::DiarizeScope;
    diarize_session_with_turns_impl(app, SESSION, typed, Some(DiarizeScope::Mic), gallery, &supplied())
        .unwrap()
}

#[test]
fn enrolled_voices_heard_across_the_speech_set_the_count() {
    let _alone = alone();
    let Some((app, _d)) = app() else { return };
    several_on_the_mic(&app);
    let len = TURN_SECONDS * RATE as usize;
    let gallery = [enrolled("Rowan", &rowan(len)), enrolled("Zoe", &zoe(len))];

    let r = diarize_mic(&app, None, &gallery);
    assert_eq!(r.count_source, Some(CountSource::Voiceprints));
    assert_eq!(r.speakers, 2, "both enrolled voices speak, three turns and 30 s each");
    assert_eq!(
        manifest_of(&app).diarize_count,
        Some(DiarizeCount { source: CountSource::Voiceprints, requested: None, found: 2 }),
        "a count the voices set is no request"
    );

    // One enrolled voice over half the speech is a floor; the session's own
    // expected count sets the total above it.
    let (app2, _d2) = bare_app().unwrap();
    session_with(&app2, turn_segments(), Some(2));
    several_on_the_mic(&app2);
    let r = diarize_mic(&app2, None, &gallery[..1]);
    assert_eq!(r.count_source, Some(CountSource::Expected));
    assert_eq!(r.speakers, 2);
    assert_eq!(
        manifest_of(&app2).diarize_count,
        Some(DiarizeCount { source: CountSource::Expected, requested: Some(2), found: 2 }),
    );
}

#[test]
fn a_typed_count_wins_over_the_enrolled_voices() {
    let _alone = alone();
    let Some((app, _d)) = app() else { return };
    several_on_the_mic(&app);
    let len = TURN_SECONDS * RATE as usize;
    let gallery = [enrolled("Rowan", &rowan(len)), enrolled("Zoe", &zoe(len))];
    let r = diarize_mic(&app, Some(1), &gallery);
    assert_eq!(r.count_source, Some(CountSource::Typed));
    assert_eq!(r.speakers, 1);
}

/// Adds a system track the run leaves alone: two segments an earlier run gave
/// clusters 0 and 1.
fn two_remote_speakers_kept(app: &AppState) {
    let p = app.profile.session_path(SESSION).join("transcript.json");
    let mut st: serde_json::Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
    let seg = |start_ms: u32, id: u32| {
        serde_json::json!({
            "start_ms": start_ms, "end_ms": start_ms + 5_000,
            "text": "the vendor call is on thursday", "confidence": null, "speaker_id": id,
        })
    };
    st["chunks"][0]["tracks"].as_array_mut().unwrap().push(serde_json::json!({
        "track": "system",
        "source_wav_relative": "chunks/0000/system.wav",
        "segments": [seg(72_000, 0), seg(80_000, 1)],
    }));
    fs::write(&p, serde_json::to_vec_pretty(&st).unwrap()).unwrap();
}

#[test]
fn a_room_group_with_no_remote_speech_takes_the_count_on_the_mic() {
    use tauri_app_core::commands::voiceprints::DiarizeScope;
    let _alone = alone();
    let Some((app, _d)) = app() else { return };
    several_on_the_mic(&app);

    // Both tracks are in scope; the system track holds nothing to count. Left
    // to estimate, the mic finds Rowan and Zoe; the typed count holds it to one.
    let r = diarize_session_with_turns_impl(&app, SESSION, Some(1), Some(DiarizeScope::Both), &[], &supplied())
        .unwrap();
    assert_eq!(r.count_source, Some(CountSource::Typed));
    assert_eq!(r.count, Some(1), "the typed count groups the mic");
    assert_eq!(r.clustered, 1);
}

#[test]
fn a_mic_rerun_reports_the_speakers_it_grouped_beside_the_ones_it_kept() {
    let _alone = alone();
    let Some((app, _d)) = app() else { return };
    two_remote_speakers_kept(&app);

    let r = diarize_mic(&app, Some(2), &[]);
    assert_eq!(r.speakers, 4, "fresh ids start above the two kept system clusters");
    assert_eq!(r.clustered, 2);
    assert_eq!(r.count, Some(2));
    assert_eq!(r.count_source, Some(CountSource::Typed));
}

/// Stands in for an edit made while the run is grouping voices.
fn edit_mid_run(root: &std::path::Path) {
    let p = root.join("transcript.json");
    let mut st: serde_json::Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
    st["chunks"][0]["tracks"][0]["segments"][0]["text"] = serde_json::json!("an edit made mid-run");
    fs::write(&p, serde_json::to_vec_pretty(&st).unwrap()).unwrap();
}

#[test]
fn a_transcript_edited_during_the_run_keeps_the_edit() {
    let _alone = alone();
    let Some((app, _d)) = app() else { return };

    let turns = SuppliedTurns { after_turns: Some(edit_mid_run), ..supplied() };
    let err = diarize_session_with_turns_impl(&app, SESSION, Some(2), None, &[], &turns).unwrap_err();
    assert!(err.to_string().contains("changed while voices were being grouped"), "{err}");

    let st = transcript_of(&app);
    let segs = &st.chunks[0].tracks[0].segments;
    assert_eq!(segs[0].text, "an edit made mid-run");
    assert!(segs.iter().all(|s| s.speaker_id.is_none()), "the run wrote nothing over the edit");
    assert!(manifest_of(&app).cluster_sides.is_empty());

    // Run again, the session groups as usual.
    assert_eq!(diarize(&app, Some(2), &[]).clustered, 2);
}

#[test]
fn one_speaker_sessions_group_without_the_segmentation_model() {
    let _alone = alone();
    let Some((app, d)) = app() else { return };
    let encoder_only = d.path().join("encoder-only");
    fs::create_dir_all(&encoder_only).unwrap();
    fs::copy(repo_root().join("models/voiceprints/model.onnx"), encoder_only.join("model.onnx")).unwrap();
    std::env::set_var("DAISY_VOICEPRINT_DIR", &encoder_only);

    // One person on the mic, nothing on the system track.
    let solo_mic = diarize_session_impl(&app, SESSION, None, None, &[]);

    // One remote speaker, as typed.
    let p = app.profile.session_path(SESSION).join("transcript.json");
    let mut st: serde_json::Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
    st["chunks"][0]["tracks"][0]["track"] = serde_json::json!("system");
    fs::write(&p, serde_json::to_vec_pretty(&st).unwrap()).unwrap();
    let one_remote = diarize_session_impl(&app, SESSION, Some(1), None, &[]);

    std::env::set_var("DAISY_VOICEPRINT_DIR", repo_root().join("models/voiceprints"));
    let solo_mic = solo_mic.unwrap();
    assert_eq!((solo_mic.clustered, solo_mic.count_source), (1, None));
    let one_remote = one_remote.unwrap();
    assert_eq!((one_remote.clustered, one_remote.count), (1, Some(1)));
    assert_eq!(one_remote.count_source, Some(CountSource::Typed));
}

#[test]
fn the_count_hint_counts_the_invitees_on_the_track_the_count_applies_to() {
    use tauri_app_core::commands::voiceprints::{attendee_count_hint_impl, DiarizeScope};
    let _alone = alone();
    let dir = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(dir.path().join("daisy")).unwrap());
    session(&app);
    let mp = app.profile.session_path(SESSION).join("manifest.json");
    let mut m: serde_json::Value = serde_json::from_slice(&fs::read(&mp).unwrap()).unwrap();
    m["attendees"] = serde_json::json!([
        { "display_name": "Fictional Owner", "role": "self" },
        { "display_name": "Avery", "role": "other" },
        { "display_name": "Blake", "role": "other" },
    ]);
    fs::write(&mp, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    let hint = |scope| attendee_count_hint_impl(&app, SESSION, scope).unwrap();

    // Speech on the mic only: every count lands on the mic, which holds
    // everyone.
    assert_eq!(hint(None), Some(3));
    assert_eq!(hint(Some(DiarizeScope::Mic)), Some(3));
    assert_eq!(hint(Some(DiarizeScope::Both)), Some(3));
    assert_eq!(hint(Some(DiarizeScope::Others)), Some(2));

    // With remote speech the count lands on the system track: the other side.
    two_remote_speakers_kept(&app);
    assert_eq!(hint(None), Some(2));
    assert_eq!(hint(Some(DiarizeScope::Both)), Some(2));
    assert_eq!(hint(Some(DiarizeScope::Mic)), Some(3));
    several_on_the_mic(&app);
    assert_eq!(hint(None), Some(2));
}

/// A third voice, set apart from Rowan and Zoe.
fn ines(samples: usize) -> Vec<i16> {
    voices::voice(
        samples,
        259.0,
        &[[540.431, 2_066.484, 2_343.883], [297.699, 854.452, 2_982.042], [697.743, 1_288.498, 2_588.342]],
        0x1B87_3593,
    )
}

#[test]
fn a_split_above_the_typed_count_records_both_numbers() {
    let _alone = alone();
    let Some((app, _d)) = app() else { return };
    several_on_the_mic(&app);
    // Rowan, Zoe, Ines, Ines, Rowan, Zoe: three people on the mic. A count of
    // two fuses Rowan with Zoe; the split gate pulls them apart again.
    let len = TURN_SECONDS * RATE as usize;
    let mut mic = room_tone(RATE as usize * SECONDS as usize);
    for (&(start_ms, _), pcm) in TURNS.iter().zip([rowan(len), zoe(len), ines(len), ines(len), rowan(len), zoe(len)]) {
        let from = start_ms as usize * RATE as usize / 1000;
        mic[from..from + len].copy_from_slice(&pcm);
    }
    wav(&app.profile.session_path(SESSION).join("chunks/0000/mic.wav"), &mic);

    let r = diarize_mic(&app, Some(2), &[]);
    assert_eq!(r.count_source, Some(CountSource::Typed));
    assert_eq!(r.count, Some(3), "the split gate found the third voice");
    assert_eq!(
        manifest_of(&app).diarize_count,
        Some(DiarizeCount { source: CountSource::Typed, requested: Some(2), found: 3 }),
        "both numbers are recorded on the session"
    );
}
