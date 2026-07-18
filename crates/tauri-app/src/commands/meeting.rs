//! Session metadata + notes commands. Operates on <profile>/sessions/<id>/manifest.json
//! (schema v2) and <profile>/sessions/<id>/notes.md.

use crate::error::{AppError, Result};
use crate::state::{AppState, VaultState};
use recording::manifest::{AecMode, Attendee, ChunkManifest, SessionManifest};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::commands::write_wav_mono_16k;

fn session_dir(app: &AppState, sid: &str) -> PathBuf {
    app.profile.session_path(sid)
}
fn manifest_path(app: &AppState, sid: &str) -> PathBuf {
    session_dir(app, sid).join("manifest.json")
}
fn notes_path(app: &AppState, sid: &str) -> PathBuf {
    session_dir(app, sid).join("notes.md")
}

fn load_manifest(app: &AppState, sid: &str) -> Result<SessionManifest> {
    let p = manifest_path(app, sid);
    let bytes =
        syncsafe::read(&p).map_err(|e| AppError::Config(format!("read {}: {e}", p.display())))?;
    let m: SessionManifest = serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Config(format!("parse manifest: {e}")))?;
    if !m.schema_is_supported() {
        return Err(AppError::Config(format!(
            "session {sid}: manifest schema v{} unsupported (expected v{})",
            m.schema_version,
            SessionManifest::SCHEMA
        )));
    }
    Ok(m)
}
fn save_manifest(app: &AppState, sid: &str, m: &SessionManifest) -> Result<()> {
    let p = manifest_path(app, sid);
    let tmp = p.with_extension("json.tmp");
    syncsafe::write(&tmp, serde_json::to_vec_pretty(m)?)?;
    syncsafe::rename(&tmp, &p)?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct SessionMeta {
    pub session_id: String,
    pub title: Option<String>,
    pub tag_ids: Vec<String>,
    pub attendees: Vec<Attendee>,
    pub has_notes: bool,
    pub has_summary: bool,
    pub recording_segments: usize,
    pub sent_integration_ids: Vec<String>,
}

pub fn session_meta_get_impl(app: &AppState, sid: &str) -> Result<SessionMeta> {
    let m = load_manifest(app, sid)?;
    Ok(SessionMeta {
        session_id: sid.to_string(),
        title: m.title.clone(),
        tag_ids: m.tag_ids.clone(),
        attendees: m.attendees.clone(),
        has_notes: notes_path(app, sid).is_file(),
        has_summary: session_dir(app, sid).join("summary.json").is_file(),
        recording_segments: m.recording_segments.len(),
        sent_integration_ids: m.sent_integration_ids.clone(),
    })
}

#[derive(Debug, Deserialize)]
pub struct SessionMetaUpdate {
    pub session_id: String,
    pub title: Option<Option<String>>,
    pub tag_ids: Option<Vec<String>>,
    pub attendees: Option<Vec<Attendee>>,
    /// User-corrected recording date/time; drives Library sort + display.
    /// Chunk/audio timestamps are untouched.
    pub created_at_unix_seconds: Option<i64>,
}

pub fn session_meta_update_impl(app: &AppState, req: SessionMetaUpdate) -> Result<()> {
    let mut m = load_manifest(app, &req.session_id)?;
    if let Some(t) = req.title {
        m.title = t;
    }
    if let Some(tags) = req.tag_ids {
        m.tag_ids = tags;
    }
    if let Some(att) = req.attendees {
        m.attendees = att;
        // Every attendee becomes a Contact (identity only — never a voiceprint).
        for a in &m.attendees {
            let _ = crate::commands::contacts::upsert_contact_in_store(app, &a.display_name, None);
        }
    }
    if let Some(ts) = req.created_at_unix_seconds {
        // Accepted range: 2000-01-01 .. now + 2 days.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if !(946_684_800..=now + 2 * 86_400).contains(&ts) {
            return Err(AppError::Config(
                "That date doesn't look right — pick a date between 2000 and now.".into(),
            ));
        }
        m.created_at_unix_seconds = ts;
    }
    save_manifest(app, &req.session_id, &m)
}

/// Records a successful push to `integration_id` in the session manifest.
pub fn mark_sent_to_integration(app: &AppState, sid: &str, integration_id: &str) -> Result<()> {
    let mut m = load_manifest(app, sid)?;
    if !m.sent_integration_ids.iter().any(|i| i == integration_id) {
        m.sent_integration_ids.push(integration_id.to_string());
        save_manifest(app, sid, &m)?;
    }
    Ok(())
}

pub fn session_notes_load_impl(app: &AppState, sid: &str) -> Result<String> {
    Ok(syncsafe::read_to_string(notes_path(app, sid)).unwrap_or_default())
}
pub fn session_notes_save_impl(app: &AppState, sid: &str, markdown: &str) -> Result<()> {
    let p = notes_path(app, sid);
    let tmp = p.with_extension("md.tmp");
    syncsafe::write(&tmp, markdown.as_bytes())?;
    syncsafe::rename(&tmp, &p)?;
    let mut m = load_manifest(app, sid)?;
    if m.notes_md_relative.is_none() {
        m.notes_md_relative = Some(PathBuf::from("notes.md"));
        save_manifest(app, sid, &m)?;
    }
    Ok(())
}

/// Set the exact set of tag_ids on a session; bump use_count on adds,
/// decrement on removes. `_vs` is unused.
pub fn session_assign_tags_impl(
    app: &AppState,
    _vs: &VaultState,
    sid: &str,
    tag_ids: Vec<String>,
) -> Result<()> {
    let mut m = load_manifest(app, sid)?;
    let old: std::collections::HashSet<String> = m.tag_ids.iter().cloned().collect();
    let new: std::collections::HashSet<String> = tag_ids.iter().cloned().collect();
    let added: Vec<String> = new.difference(&old).cloned().collect();
    let removed: Vec<String> = old.difference(&new).cloned().collect();
    crate::commands::tags::adjust_use_counts(app, &added, 1)?;
    crate::commands::tags::adjust_use_counts(app, &removed, -1)?;
    m.tag_ids = tag_ids;
    save_manifest(app, sid, &m)
}

pub fn sessions_referencing_tag(app: &AppState, tag_id: &str) -> Result<Vec<String>> {
    let root = app.profile.sessions_dir();
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&root) else {
        return Ok(out);
    };
    for e in rd.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(bytes) = syncsafe::read(e.path().join("manifest.json")) else {
            continue;
        };
        let Ok(m) = serde_json::from_slice::<SessionManifest>(&bytes) else {
            continue;
        };
        if m.tag_ids.iter().any(|t| t == tag_id) {
            out.push(name);
        }
    }
    Ok(out)
}
pub fn detach_tag_from_session(app: &AppState, sid: &str, tag_id: &str) -> Result<()> {
    let mut m = load_manifest(app, sid)?;
    m.tag_ids.retain(|t| t != tag_id);
    save_manifest(app, sid, &m)
}

#[derive(Debug, Deserialize)]
pub struct CreateNoteRequest {
    pub title: Option<String>,
    /// Markdown body the user typed/pasted. Stored as notes.md and indexed
    /// for both keyword search and Q&A.
    pub notes_md: String,
    #[serde(default)]
    pub tag_ids: Vec<String>,
}

/// Create a recording-less "note" session: a finalized manifest with no
/// chunks + a notes.md. Returns the new session id.
pub fn create_note_session_impl(app: &AppState, req: CreateNoteRequest) -> Result<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let sid = format!("daisy-note-{now}");
    let dir = session_dir(app, &sid);
    syncsafe::create_dir_all(&dir)
        .map_err(|e| AppError::Io(format!("mkdir session: {e}")))?;

    let manifest = SessionManifest {
        schema_version: SessionManifest::SCHEMA,
        session_id: sid.clone(),
        created_at_unix_seconds: now,
        sample_rate: 16_000,
        channels: 1,
        mic_source_id: 0,
        mic_source_node_name: String::new(),
        mic_source_description: String::new(),
        system_source_id: 0,
        system_source_node_name: String::new(),
        system_source_description: String::new(),
        aec_mode: AecMode::Disabled,
        chunks: vec![],
        finalized_at_unix_seconds: Some(now),
        title: req.title.clone().filter(|t| !t.trim().is_empty()),
        meeting_id: uuid::Uuid::new_v4().to_string(),
        tag_ids: req.tag_ids.clone(),
        notes_md_relative: Some(PathBuf::from("notes.md")),
        attendees: vec![],
        calendar: None,
        recording_segments: vec![],
        speaker_map: vec![],
        language: None,
            diarization_unavailable: false,
            single_local_speaker: true,
            expected_speakers: None,
            sent_integration_ids: vec![],
            cluster_sides: vec![],
            interrupted: false,
            denoise_applied: None,
    };
    save_manifest(app, &sid, &manifest)?;

    let np = notes_path(app, &sid);
    syncsafe::write(&np, req.notes_md.as_bytes())
        .map_err(|e| AppError::Io(format!("write notes.md: {e}")))?;

    Ok(sid)
}

#[derive(Debug, Deserialize)]
pub struct ImportSessionRequest {
    pub title: Option<String>,
    /// Unix seconds the meeting happened; drives the library sort position.
    /// Default now.
    #[serde(default)]
    pub occurred_at: Option<i64>,
    /// Rendered transcript markdown, written to transcript.md. No
    /// transcript.json is created; `has_transcript` stays false.
    #[serde(default)]
    pub transcript_md: Option<String>,
    /// Summary markdown. Stored as a user-edited summary with empty
    /// structured fields; no LLM call.
    #[serde(default)]
    pub summary_md: Option<String>,
    /// Freeform notes markdown → notes.md.
    #[serde(default)]
    pub notes_md: Option<String>,
    #[serde(default)]
    pub tag_ids: Vec<String>,
}

/// Text-only session import: create a finalized, audio-less session from
/// caller-supplied transcript / summary / notes markdown. Returns the new id.
///
/// At least one of the three text fields must be non-empty. Always mints a
/// new id; never touches an existing session.
pub fn import_session_impl(app: &AppState, req: ImportSessionRequest) -> Result<String> {
    let transcript = req.transcript_md.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let summary = req.summary_md.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let notes = req.notes_md.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if transcript.is_none() && summary.is_none() && notes.is_none() {
        return Err(AppError::Config(
            "import_session: need at least one of transcript_md / summary_md / notes_md".into(),
        ));
    }

    let now = crate::now_unix();
    let occurred = req.occurred_at.unwrap_or(now);
    let sid = format!("daisy-import-{now}");
    let dir = session_dir(app, &sid);
    syncsafe::create_dir_all(&dir).map_err(|e| AppError::Io(format!("mkdir session: {e}")))?;

    let manifest = SessionManifest {
        schema_version: SessionManifest::SCHEMA,
        session_id: sid.clone(),
        created_at_unix_seconds: occurred,
        sample_rate: 16_000,
        channels: 1,
        mic_source_id: 0,
        mic_source_node_name: String::new(),
        mic_source_description: String::new(),
        system_source_id: 0,
        system_source_node_name: String::new(),
        system_source_description: String::new(),
        aec_mode: AecMode::Disabled,
        chunks: vec![],
        finalized_at_unix_seconds: Some(now),
        title: req.title.clone().filter(|t| !t.trim().is_empty()),
        meeting_id: uuid::Uuid::new_v4().to_string(),
        tag_ids: req.tag_ids.clone(),
        notes_md_relative: notes.map(|_| PathBuf::from("notes.md")),
        attendees: vec![],
        calendar: None,
        recording_segments: vec![],
        speaker_map: vec![],
        language: None,
        diarization_unavailable: false,
        single_local_speaker: true,
        expected_speakers: None,
            sent_integration_ids: vec![],
        cluster_sides: vec![],
        interrupted: false,
        denoise_applied: None,
    };
    save_manifest(app, &sid, &manifest)?;

    if let Some(t) = transcript {
        write_atomic(&dir.join("transcript.md"), t.as_bytes())?;
    }
    if let Some(n) = notes {
        write_atomic(&notes_path(app, &sid), n.as_bytes())?;
    }
    if let Some(s) = summary {
        write_import_summary(&dir, &sid, s, now)?;
    }

    Ok(sid)
}

/// Write a user-edited summary.json + summary.md for an import, with no
/// provider/model and empty structured fields.
fn write_import_summary(dir: &Path, sid: &str, markdown: &str, now: i64) -> Result<()> {
    use summarize::{SessionSummary, SummaryStructured};
    let summary = SessionSummary {
        schema_version: SessionSummary::SCHEMA,
        session_id: sid.to_string(),
        provider: "import".into(),
        model: String::new(),
        generated_at_unix_seconds: now,
        source_inputs_hash: String::new(),
        structured: SummaryStructured {
            tldr: String::new(),
            action_items: vec![],
            decisions: vec![],
            open_questions: vec![],
            key_topics: vec![],
        },
        markdown: markdown.to_string(),
        user_edited: true,
    };
    let json = serde_json::to_vec_pretty(&summary)
        .map_err(|e| AppError::Config(format!("serialize summary: {e}")))?;
    write_atomic(&dir.join("summary.json"), &json)?;
    write_atomic(&dir.join("summary.md"), markdown.as_bytes())?;
    Ok(())
}

/// Atomic write via tmp + rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    syncsafe::write(&tmp, bytes).map_err(|e| AppError::Io(format!("write {}: {e}", path.display())))?;
    syncsafe::rename(&tmp, path).map_err(|e| AppError::Io(format!("rename {}: {e}", path.display())))?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct ImportAudioRequest {
    pub title: Option<String>,
    #[serde(default)]
    pub notes_md: String,
    #[serde(default)]
    pub tag_ids: Vec<String>,
    /// Absolute path to the user-picked audio file.
    pub audio_path: String,
    /// Known speaker count for diarization. `None`/0 = auto-estimate.
    #[serde(default)]
    pub expected_speakers: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ImportAudioResult {
    pub session_id: String,
    pub quality_ok: bool,
    pub quality_note: String,
    pub duration_secs: f32,
}

/// Create a meeting from a user-uploaded audio file. The decoded audio becomes
/// the session's single (system) track. Returns a quality read of the source
/// recording.
pub fn import_audio_meeting_impl(app: &AppState, req: ImportAudioRequest) -> Result<ImportAudioResult> {
    let imported = recording::audio_import::decode_to_mono_16k(Path::new(&req.audio_path))
        .map_err(|e| AppError::Config(format!("import audio: {e}")))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let sid = format!("daisy-import-{now}");
    let dir = session_dir(app, &sid);
    let chunk_dir = dir.join("chunks/0001");
    syncsafe::create_dir_all(&chunk_dir)
        .map_err(|e| AppError::Io(format!("mkdir chunk: {e}")))?;

    // Only system.wav is written; downstream stages treat the missing mic wav
    // as silence.
    write_wav_mono_16k(&chunk_dir.join("system.wav"), &imported.pcm)?;

    let has_notes = !req.notes_md.trim().is_empty();
    let dur = imported.duration_secs.max(1.0) as u64;
    let manifest = SessionManifest {
        schema_version: SessionManifest::SCHEMA,
        session_id: sid.clone(),
        created_at_unix_seconds: now,
        sample_rate: 16_000,
        channels: 1,
        mic_source_id: 0,
        mic_source_node_name: String::new(),
        mic_source_description: String::new(),
        system_source_id: 0,
        system_source_node_name: String::new(),
        system_source_description: String::new(),
        aec_mode: AecMode::Disabled,
        chunks: vec![ChunkManifest {
            index: 1,
            started_at_unix_seconds: now,
            ended_at_unix_seconds: Some(now + dur as i64),
            duration_seconds: Some(dur),
            mic_wav_relative: PathBuf::from("chunks/0001/mic.wav"),
            system_wav_relative: PathBuf::from("chunks/0001/system.wav"),
            mic_aec_wav_relative: None,
            mic_dn_wav_relative: None,
        }],
        // Not finalized; the caller's cascade stamps finalized_at.
        finalized_at_unix_seconds: None,
        title: req.title.clone().filter(|t| !t.trim().is_empty()),
        meeting_id: uuid::Uuid::new_v4().to_string(),
        tag_ids: req.tag_ids.clone(),
        notes_md_relative: if has_notes { Some(PathBuf::from("notes.md")) } else { None },
        attendees: vec![],
        calendar: None,
        recording_segments: vec![],
        speaker_map: vec![],
        language: None,
            diarization_unavailable: false,
            single_local_speaker: true,
            expected_speakers: req.expected_speakers.filter(|&n| n > 0),
            sent_integration_ids: vec![],
            cluster_sides: vec![],
            interrupted: false,
            denoise_applied: None,
    };
    save_manifest(app, &sid, &manifest)?;
    if has_notes {
        syncsafe::write(notes_path(app, &sid), req.notes_md.as_bytes())
            .map_err(|e| AppError::Io(format!("write notes.md: {e}")))?;
    }

    Ok(ImportAudioResult {
        session_id: sid,
        quality_ok: imported.quality_ok,
        quality_note: imported.quality_note,
        duration_secs: imported.duration_secs,
    })
}

#[cfg(test)]
mod import_tests {
    use super::*;
    use crate::state::AppState;

    fn app() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (AppState::new(profile), tmp)
    }

    #[test]
    fn assigning_attendees_upserts_contacts_never_voiceprints() {
        use recording::manifest::{Attendee, AttendeeRole};
        let (app, _tmp) = app();
        let sid = import_session_impl(
            &app,
            ImportSessionRequest {
                title: Some("Pilot sync".into()),
                occurred_at: Some(1_700_000_000),
                transcript_md: Some("**Mira:** hello".into()),
                summary_md: None,
                notes_md: None,
                tag_ids: vec![],
            },
        )
        .unwrap();
        let assign = |app: &AppState| {
            session_meta_update_impl(
                app,
                SessionMetaUpdate {
                    session_id: sid.clone(),
                    title: None,
                    tag_ids: None,
                    attendees: Some(vec![
                        Attendee { display_name: "Mira".into(), role: AttendeeRole::Other },
                        Attendee { display_name: "Dana".into(), role: AttendeeRole::Other },
                    ]),
                    created_at_unix_seconds: None,
                },
            )
            .unwrap();
        };
        assign(&app);
        let contacts = crate::commands::contacts::load_contacts(&app).unwrap();
        assert!(contacts.iter().any(|c| c.display_name == "Mira"));
        assert!(contacts.iter().any(|c| c.display_name == "Dana"));
        assert_eq!(contacts.len(), 2);
        // Re-assigning the same attendees never duplicates (upsert by name).
        assign(&app);
        assert_eq!(crate::commands::contacts::load_contacts(&app).unwrap().len(), 2);
    }

    #[test]
    fn import_writes_transcript_and_summary_not_notes() {
        let (app, _tmp) = app();
        let sid = import_session_impl(
            &app,
            ImportSessionRequest {
                title: Some("Weekly sync".into()),
                occurred_at: Some(1_700_000_000),
                transcript_md: Some("**Alice:** ship it".into()),
                summary_md: Some("## TL;DR\nShipping.".into()),
                notes_md: None,
                tag_ids: vec![],
            },
        )
        .unwrap();
        let dir = session_dir(&app, &sid);
        assert!(dir.join("manifest.json").is_file());
        assert_eq!(
            syncsafe::read_to_string(dir.join("transcript.md")).unwrap(),
            "**Alice:** ship it"
        );
        assert!(dir.join("summary.json").is_file());
        assert_eq!(
            syncsafe::read_to_string(dir.join("summary.md")).unwrap(),
            "## TL;DR\nShipping."
        );
        assert!(!dir.join("notes.md").exists(), "no notes supplied → no notes.md");

        // Summary round-trips as authoritative (user-edited), no provider run.
        let s = crate::commands::summary::summary_load_impl(&app, &sid)
            .unwrap()
            .unwrap();
        assert!(s.user_edited);
        assert_eq!(s.provider, "import");
        assert_eq!(s.markdown, "## TL;DR\nShipping.");

        // SessionView shows the transcript but flags it as un-structured.
        let view = crate::commands::session::read_session_impl(&app, &sid).unwrap();
        assert_eq!(view.transcript_md.as_deref(), Some("**Alice:** ship it"));
        assert!(view.has_summary);
        assert!(!view.has_transcript, "no transcript.json → flag false");

        // Manifest carries the supplied occurred_at as created_at.
        assert_eq!(view.manifest_json["created_at_unix_seconds"], 1_700_000_000_i64);
    }

    #[test]
    fn import_notes_only_sets_manifest_pointer() {
        let (app, _tmp) = app();
        let sid = import_session_impl(
            &app,
            ImportSessionRequest {
                title: None,
                occurred_at: None,
                transcript_md: None,
                summary_md: None,
                notes_md: Some("just some notes".into()),
                tag_ids: vec![],
            },
        )
        .unwrap();
        let dir = session_dir(&app, &sid);
        assert_eq!(syncsafe::read_to_string(dir.join("notes.md")).unwrap(), "just some notes");
        assert!(!dir.join("transcript.md").exists());
        let m = load_manifest(&app, &sid).unwrap();
        assert_eq!(m.notes_md_relative, Some(PathBuf::from("notes.md")));
    }

    #[test]
    fn import_rejects_all_empty() {
        let (app, _tmp) = app();
        let err = import_session_impl(
            &app,
            ImportSessionRequest {
                title: Some("nothing".into()),
                occurred_at: None,
                transcript_md: Some("   ".into()), // whitespace-only → treated empty
                summary_md: None,
                notes_md: None,
                tag_ids: vec![],
            },
        );
        assert!(err.is_err());
    }

    #[test]
    fn mark_sent_records_once_and_meta_exposes_it() {
        let (app, _tmp) = app();
        let sid = import_session_impl(
            &app,
            ImportSessionRequest {
                title: Some("Vendor sync".into()),
                occurred_at: Some(1_700_000_000),
                transcript_md: Some("**Mira:** hello".into()),
                summary_md: None,
                notes_md: None,
                tag_ids: vec![],
            },
        )
        .unwrap();
        mark_sent_to_integration(&app, &sid, "i-hook").unwrap();
        mark_sent_to_integration(&app, &sid, "i-hook").unwrap(); // idempotent
        mark_sent_to_integration(&app, &sid, "i-crm").unwrap();
        let meta = session_meta_get_impl(&app, &sid).unwrap();
        assert_eq!(meta.sent_integration_ids, vec!["i-hook", "i-crm"]);
    }

    #[test]
    fn import_audio_stores_expected_speakers_in_manifest() {
        let (app, _tmp) = app();
        // 1 s of 440 Hz tone as a decodable source file.
        let src = app.profile.root().join("clip.wav");
        let samples: Vec<i16> = (0..16_000)
            .map(|i| ((i as f32 * 440.0 * 2.0 * std::f32::consts::PI / 16_000.0).sin() * 8000.0) as i16)
            .collect();
        crate::commands::write_wav_mono_16k(&src, &samples).unwrap();

        let r = import_audio_meeting_impl(&app, ImportAudioRequest {
            title: Some("Panel discussion".into()),
            notes_md: String::new(),
            tag_ids: vec![],
            audio_path: src.to_string_lossy().into_owned(),
            expected_speakers: Some(4),
        })
        .unwrap();
        let m = load_manifest(&app, &r.session_id).unwrap();
        assert_eq!(m.expected_speakers, Some(4));

        // 0 = auto-detect: normalized to None.
        let r2 = import_audio_meeting_impl(&app, ImportAudioRequest {
            title: None,
            notes_md: String::new(),
            tag_ids: vec![],
            audio_path: src.to_string_lossy().into_owned(),
            expected_speakers: Some(0),
        })
        .unwrap();
        let m2 = load_manifest(&app, &r2.session_id).unwrap();
        assert_eq!(m2.expected_speakers, None);
    }
}
