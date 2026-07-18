//! Read session artifacts: manifest, transcript markdown.

use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Serialize)]
pub struct SessionView {
    pub session_id: String,
    pub manifest_json: serde_json::Value,
    /// Already-rendered markdown — null if not yet generated.
    pub transcript_md: Option<String>,
    pub has_dedup: bool,
    pub has_transcript: bool,
    pub has_summary: bool,
}

/// Insert or update an entry in `manifest.speaker_map` for a diarized cluster.
/// Passing an empty / whitespace-only `display_name` removes the entry, which
/// reverts the renderer to its "Person A/B/C" auto-label for that cluster.
pub fn set_session_speaker_label_impl(
    app: &AppState,
    session_id: &str,
    cluster_id: u32,
    display_name: String,
    email: Option<String>,
) -> Result<()> {
    use recording::manifest::SpeakerLabel;
    use recording::session::Session;
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    let mut session = Session::load(&root).map_err(|e| AppError::Recording(e.to_string()))?;
    let trimmed = display_name.trim().to_string();
    let cleaned_email = email
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    session
        .update_manifest(|m| {
            m.speaker_map.retain(|s| s.cluster_id != cluster_id);
            if !trimmed.is_empty() {
                m.speaker_map.push(SpeakerLabel {
                    cluster_id,
                    display_name: trimmed.clone(),
                    email: cleaned_email.clone(),
                    voiceprint_id: None,
                    match_confidence: None,
                    contact_id: None,
                });
            }
        })
        .map_err(|e| AppError::Recording(e.to_string()))?;
    Ok(())
}

/// Strip a diarized cluster from the session entirely: clear `speaker_id` on
/// every transcript segment that belongs to the cluster, and drop the
/// `speaker_map` entry. Updates both transcript.json and
/// transcript.dedup.json when present.
pub fn remove_speaker_cluster_impl(
    app: &AppState,
    session_id: &str,
    cluster_id: u32,
) -> Result<()> {
    use recording::session::Session;
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    let mut session = Session::load(&root).map_err(|e| AppError::Recording(e.to_string()))?;
    session
        .update_manifest(|m| {
            m.speaker_map.retain(|s| s.cluster_id != cluster_id);
        })
        .map_err(|e| AppError::Recording(e.to_string()))?;

    for name in ["transcript.dedup.json", "transcript.json"] {
        let path = root.join(name);
        if !path.is_file() { continue; }
        let bytes = syncsafe::read(&path)
            .map_err(|e| AppError::Io(format!("read {}: {e}", path.display())))?;
        let mut st: transcript::SessionTranscript = serde_json::from_slice(&bytes)
            .map_err(|e| AppError::Config(format!("parse {}: {e}", path.display())))?;
        let mut changed = 0u32;
        for ch in st.chunks.iter_mut() {
            for tr in ch.tracks.iter_mut() {
                for seg in tr.segments.iter_mut() {
                    if seg.speaker_id == Some(cluster_id) {
                        seg.speaker_id = None;
                        changed += 1;
                    }
                }
            }
        }
        if changed > 0 {
            let out = serde_json::to_vec_pretty(&st)
                .map_err(|e| AppError::Config(format!("encode {}: {e}", path.display())))?;
            syncsafe::write(&path, out)
                .map_err(|e| AppError::Io(format!("write {}: {e}", path.display())))?;
            log::info!(
                "remove_speaker_cluster: {name} session={session_id} cluster={cluster_id} segments_cleared={changed}"
            );
        }
    }
    Ok(())
}

/// Delete an entire session directory — audio, transcript, summary, notes,
/// manifest. Irreversible. Caller is responsible for confirming with the user.
/// Session ids must be alphanumeric + `-_.`, never `.` or `..`, and the
/// canonicalized path must stay under the profile's sessions root.
pub fn delete_session_impl(app: &AppState, session_id: &str) -> Result<()> {
    fn is_safe_session_id(s: &str) -> bool {
        !s.is_empty()
            && s.len() < 256
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
            && s != "."
            && s != ".."
    }
    if !is_safe_session_id(session_id) {
        return Err(AppError::Config(format!("invalid session id: {session_id}")));
    }
    let sessions_root = app.profile.sessions_dir();
    let target = app.profile.session_path(session_id);
    if !target.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    let canon_target = target
        .canonicalize()
        .map_err(|e| AppError::Config(format!("canonicalize session path: {e}")))?;
    let canon_root = sessions_root
        .canonicalize()
        .map_err(|e| AppError::Config(format!("canonicalize sessions root: {e}")))?;
    if !canon_target.starts_with(&canon_root) {
        return Err(AppError::Config(
            "session path escapes the sessions directory (symlink?)".into(),
        ));
    }
    std::fs::remove_dir_all(&canon_target)
        .map_err(|e| AppError::Io(format!("delete {}: {e}", canon_target.display())))?;
    log::info!("deleted session {} at {}", session_id, canon_target.display());
    Ok(())
}

/// One diarized speaker cluster surfaced in a session. Either the user
/// already labelled it (display_name is the user-supplied name + email)
/// or it gets an auto "Person A/B/C…" assignment in order of appearance.
#[derive(Debug, Serialize)]
pub struct SessionSpeaker {
    pub cluster_id: u32,
    pub display_name: String,
    pub email: Option<String>,
    /// Set when an enrolled voiceprint is currently linked to this cluster.
    pub voiceprint_id: Option<String>,
    /// Cosine score from the auto-match that produced this link. None for
    /// manually-labeled or unmatched clusters.
    pub match_confidence: Option<f32>,
    pub is_user_labeled: bool,
    /// Snippet of the first non-empty segment from this speaker.
    pub sample_text: Option<String>,
    /// Total ms of speech attributed to this cluster across the session.
    pub speech_ms: u32,
    /// Which capture track this cluster came from (Room = mic, Remote = system).
    /// Defaults to Remote when unknown.
    pub side: recording::manifest::SpeakerSide,
}

/// Resolve a cluster's side from the manifest's `cluster_sides` map; unknown
/// clusters default to Remote.
fn side_for(
    sides: &[recording::manifest::ClusterSide],
    cluster_id: u32,
) -> recording::manifest::SpeakerSide {
    sides
        .iter()
        .find(|c| c.cluster_id == cluster_id)
        .map(|c| c.side)
        .unwrap_or_default()
}

/// Re-render `transcript.md` from `transcript.dedup.json` (or `.json` as
/// fallback) using the current `manifest.speaker_map`. Pure string
/// processing; no provider call.
pub fn rerender_session_transcript_impl(app: &AppState, session_id: &str) -> Result<()> {
    use std::collections::HashMap;
    use transcript::render::render_markdown_with_speakers;
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    let st_path = if root.join("transcript.dedup.json").is_file() {
        root.join("transcript.dedup.json")
    } else if root.join("transcript.json").is_file() {
        root.join("transcript.json")
    } else {
        return Err(AppError::Config(format!(
            "session {session_id}: no transcript to render"
        )));
    };
    let st: transcript::SessionTranscript = serde_json::from_slice(&syncsafe::read(&st_path)?)
        .map_err(|e| AppError::Config(format!("parse transcript: {e}")))?;

    let mut offsets: HashMap<u32, u32> = HashMap::new();
    let mut speakers: HashMap<u32, String> = HashMap::new();
    if let Ok(b) = syncsafe::read(root.join("manifest.json")) {
        if let Ok(m) = serde_json::from_slice::<recording::manifest::SessionManifest>(&b) {
            let created = m.created_at_unix_seconds.max(0) as u32;
            for c in &m.chunks {
                let off_secs = (c.started_at_unix_seconds.max(0) as u32).saturating_sub(created);
                offsets.insert(c.index, off_secs.saturating_mul(1000));
            }
            for s in m.speaker_map {
                speakers.insert(s.cluster_id, s.display_name);
            }
        }
    }
    let md = render_markdown_with_speakers(&st, &offsets, &speakers);
    let tmp = root.join("transcript.md.tmp");
    syncsafe::write(&tmp, md.as_bytes())?;
    syncsafe::rename(&tmp, root.join("transcript.md"))?;
    Ok(())
}

/// The transcript text of the segments whose audio
/// `session_speaker_sample_audio_impl` plays for this cluster. Empty string
/// if no audible speech was gathered.
pub fn session_speaker_sample_text_impl(
    app: &AppState,
    session_id: &str,
    cluster_id: u32,
) -> Result<String> {
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    const SAMPLE_CAP_MS: u32 = 5_000;
    let (_pcm, text) = crate::commands::voiceprints::gather_cluster_sample(
        &root,
        cluster_id,
        SAMPLE_CAP_MS,
    )?;
    Ok(text)
}

/// A short (~5 s) clip of the given speaker cluster as 16 kHz mono int16 PCM
/// in a standard WAV envelope.
pub fn session_speaker_sample_audio_impl(
    app: &AppState,
    session_id: &str,
    cluster_id: u32,
) -> Result<Vec<u8>> {
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    const SAMPLE_CAP_MS: u32 = 5_000;
    let pcm = crate::commands::voiceprints::gather_cluster_pcm_capped(
        &root,
        cluster_id,
        SAMPLE_CAP_MS,
    )?;
    if pcm.is_empty() {
        // gather returns empty when the cluster's audio is silence or below
        // the sample floor.
        return Err(AppError::Config(
            "No clear audio for this voice.".to_string(),
        ));
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf: Vec<u8> = Vec::with_capacity(44 + pcm.len() * 2);
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut w = hound::WavWriter::new(cursor, spec)
            .map_err(|e| AppError::Config(format!("wav writer: {e}")))?;
        for s in &pcm {
            w.write_sample(*s)
                .map_err(|e| AppError::Config(format!("wav write: {e}")))?;
        }
        w.finalize()
            .map_err(|e| AppError::Config(format!("wav finalize: {e}")))?;
    }
    Ok(buf)
}

pub fn list_session_speakers_impl(app: &AppState, session_id: &str) -> Result<Vec<SessionSpeaker>> {
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    let st_path = if root.join("transcript.dedup.json").is_file() {
        root.join("transcript.dedup.json")
    } else {
        root.join("transcript.json")
    };
    if !st_path.is_file() {
        return Ok(Vec::new());
    }
    let st: transcript::SessionTranscript = serde_json::from_slice(&syncsafe::read(&st_path)?)
        .map_err(|e| AppError::Config(format!("parse transcript: {e}")))?;

    // Gather order-of-first-appearance, total speech_ms, and sample text from
    // every track. Mic/MicAec segments carry a speaker_id only when the
    // session was diarized with the Local-mic/Both scope; segments without a
    // speaker_id are skipped.
    let mut order: Vec<u32> = Vec::new();
    let mut totals: BTreeMap<u32, u32> = BTreeMap::new();
    let mut samples: BTreeMap<u32, String> = BTreeMap::new();
    for ch in &st.chunks {
        for tr in &ch.tracks {
            for seg in &tr.segments {
                let Some(sid) = seg.speaker_id else { continue };
                if !order.contains(&sid) {
                    order.push(sid);
                }
                let dur = seg.end_ms.saturating_sub(seg.start_ms);
                *totals.entry(sid).or_default() += dur;
                if !samples.contains_key(&sid) && !seg.text.trim().is_empty() {
                    let t = seg.text.trim();
                    let snippet: String = t.chars().take(120).collect();
                    samples.insert(sid, snippet);
                }
            }
        }
    }

    // User labels from manifest (if any), plus voiceprint link + match score.
    type SpeakerMeta = (String, Option<String>, Option<String>, Option<f32>);
    let mut labels: std::collections::HashMap<u32, SpeakerMeta> = Default::default();
    let mut cluster_sides: Vec<recording::manifest::ClusterSide> = Vec::new();
    if let Ok(b) = syncsafe::read(root.join("manifest.json")) {
        if let Ok(m) = serde_json::from_slice::<recording::manifest::SessionManifest>(&b) {
            cluster_sides = m.cluster_sides.clone();
            for s in m.speaker_map {
                labels.insert(
                    s.cluster_id,
                    (s.display_name, s.email, s.voiceprint_id, s.match_confidence),
                );
            }
        }
    }

    let mut out = Vec::with_capacity(order.len());
    for (i, sid) in order.iter().enumerate() {
        let auto = format!("Person {}", char::from(b'A' + (i.min(25) as u8)));
        let (display_name, email, voiceprint_id, match_confidence, is_user_labeled) =
            match labels.get(sid) {
                Some((n, e, vp, mc)) => (n.clone(), e.clone(), vp.clone(), *mc, true),
                None => (auto, None, None, None, false),
            };
        out.push(SessionSpeaker {
            cluster_id: *sid,
            display_name,
            email,
            voiceprint_id,
            match_confidence,
            is_user_labeled,
            sample_text: samples.get(sid).cloned(),
            speech_ms: totals.get(sid).copied().unwrap_or(0),
            side: side_for(&cluster_sides, *sid),
        });
    }

    // speaker_map labels that appear in no segment (manually added or copied
    // from a calendar invite) are appended with speech_ms = 0 and no
    // sample_text.
    let segment_ids: std::collections::HashSet<u32> = order.iter().copied().collect();
    let mut manual_ids: Vec<u32> = labels
        .keys()
        .filter(|id| !segment_ids.contains(id))
        .copied()
        .collect();
    manual_ids.sort();
    for sid in manual_ids {
        if let Some((n, e, vp, mc)) = labels.get(&sid) {
            out.push(SessionSpeaker {
                cluster_id: sid,
                display_name: n.clone(),
                email: e.clone(),
                voiceprint_id: vp.clone(),
                match_confidence: *mc,
                is_user_labeled: true,
                sample_text: None,
                speech_ms: 0,
                side: side_for(&cluster_sides, sid),
            });
        }
    }
    Ok(out)
}

/// Insert a manually-added participant into `manifest.speaker_map`. Returns
/// the synthesized cluster_id, larger than any existing diarization cluster
/// or speaker_map entry. The new entry has no segments and `speech_ms = 0`.
pub fn add_session_speaker_impl(
    app: &AppState,
    session_id: &str,
    display_name: String,
    email: Option<String>,
    voiceprint_id: Option<String>,
) -> Result<u32> {
    use recording::session::Session;
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    let name = display_name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Config("display_name is required".into()));
    }

    let mut session = Session::load(&root).map_err(|e| AppError::Recording(e.to_string()))?;

    // Next-free cluster_id, minimum 10000; scans both speaker_map and
    // existing segment speaker_ids.
    let mut max_id: u32 = 9999;
    for s in &session.manifest().speaker_map {
        max_id = max_id.max(s.cluster_id);
    }
    for name in ["transcript.dedup.json", "transcript.json"] {
        let p = root.join(name);
        let Ok(bytes) = syncsafe::read(&p) else { continue };
        let Ok(st) = serde_json::from_slice::<transcript::SessionTranscript>(&bytes) else { continue };
        for ch in &st.chunks {
            for tr in &ch.tracks {
                for seg in &tr.segments {
                    if let Some(sid) = seg.speaker_id {
                        max_id = max_id.max(sid);
                    }
                }
            }
        }
    }
    let new_cluster_id = max_id + 1;

    let email_for_log = email.clone();
    session
        .update_manifest(|m| {
            m.speaker_map.push(recording::manifest::SpeakerLabel {
                cluster_id: new_cluster_id,
                display_name: name.clone(),
                email: email.clone(),
                voiceprint_id: voiceprint_id.clone(),
                match_confidence: None,
                contact_id: None,
            });
        })
        .map_err(|e| AppError::Recording(e.to_string()))?;
    log::info!(
        "add_session_speaker: session={session_id} cluster={new_cluster_id} name={display_name:?} email={email_for_log:?}"
    );
    Ok(new_cluster_id)
}

pub fn read_session_impl(app: &AppState, session_id: &str) -> Result<SessionView> {
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    let manifest_bytes = syncsafe::read(root.join("manifest.json"))?;
    let manifest_json: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
    let md_path = root.join("transcript.md");
    let transcript_md = if md_path.is_file() {
        Some(syncsafe::read_to_string(&md_path)?)
    } else {
        None
    };
    Ok(SessionView {
        session_id: session_id.to_string(),
        manifest_json,
        transcript_md,
        has_transcript: root.join("transcript.json").is_file(),
        has_dedup: root.join("transcript.dedup.json").is_file(),
        has_summary: root.join("summary.md").is_file(),
    })
}

#[cfg(test)]
mod side_tests {
    use super::*;
    use recording::manifest::{ClusterSide, SpeakerSide};

    #[test]
    fn side_for_reads_cluster_sides_default_remote() {
        let sides = vec![ClusterSide { cluster_id: 3, side: SpeakerSide::Room }];
        assert_eq!(side_for(&sides, 3), SpeakerSide::Room);
        assert_eq!(side_for(&sides, 0), SpeakerSide::Remote, "unknown cluster defaults Remote");
        assert_eq!(side_for(&[], 9), SpeakerSide::Remote, "empty map defaults Remote");
    }
}

#[cfg(test)]
mod speaker_list_tests {
    use super::*;
    use crate::state::AppState;

    fn app() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (AppState::new(profile), tmp)
    }

    fn seg(start_ms: u32, end_ms: u32, text: &str, speaker_id: Option<u32>) -> transcript::Segment {
        transcript::Segment { start_ms, end_ms, text: text.into(), confidence: None, speaker_id }
    }

    #[test]
    fn mic_track_clusters_are_listed() {
        let (app, _tmp) = app();
        let sid = "daisy-test-mic";
        let root = app.profile.session_path(sid);
        syncsafe::create_dir_all(&root).unwrap();

        let st = transcript::SessionTranscript {
            schema_version: transcript::SessionTranscript::SCHEMA,
            session_id: sid.into(),
            provider: "local-whisper".into(),
            model: "base.en".into(),
            transcribed_at_unix_seconds: 0,
            chunks: vec![transcript::ChunkTranscript {
                chunk_index: 1,
                tracks: vec![
                    transcript::TrackTranscript {
                        track: transcript::Track::MicAec,
                        source_wav_relative: "chunks/0001/mic_aec.wav".into(),
                        segments: vec![
                            seg(0, 2000, "hello from alice", Some(0)),
                            seg(2000, 4000, "and bob here", Some(1)),
                            seg(4000, 5000, "undiarized = the local user", None),
                        ],
                    },
                    transcript::TrackTranscript {
                        track: transcript::Track::System,
                        source_wav_relative: "chunks/0001/system.wav".into(),
                        segments: vec![], // no segments on the system side
                    },
                ],
            }],
        };
        syncsafe::write(root.join("transcript.dedup.json"), serde_json::to_vec(&st).unwrap()).unwrap();

        let speakers = list_session_speakers_impl(&app, sid).unwrap();
        assert_eq!(speakers.len(), 2, "both mic clusters must be listed");
        assert_eq!(speakers[0].display_name, "Person A");
        assert_eq!(speakers[1].display_name, "Person B");
        assert_eq!(speakers[0].speech_ms, 2000);
    }
}
