// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Read session artifacts: manifest, transcript markdown.

use crate::error::{AppError, Result};
use crate::state::{AppState, VaultState};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
                    extra: Default::default(),
                });
            }
        })
        .map_err(|e| AppError::Recording(e.to_string()))?;
    // A name is authored on either device. The email stays on the manifest
    // row the projection keeps.
    if trimmed.is_empty() {
        recording::session_meta::unname_speaker(&root, cluster_id, crate::now_unix())
    } else {
        recording::session_meta::name_speaker(&root, cluster_id, &trimmed, None, crate::now_unix())
    }
    .map_err(|e| AppError::Recording(e.to_string()))?;
    Ok(())
}

/// Strip a diarized cluster from the session entirely: clear `speaker_id` on
/// every transcript segment that belongs to the cluster, drop the
/// `speaker_map` entry, and clear the recorded diarize count, which no longer
/// matches the speakers left. Updates both transcript.json and
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
            m.diarize_count = None;
        })
        .map_err(|e| AppError::Recording(e.to_string()))?;
    recording::session_meta::unname_speaker(&root, cluster_id, crate::now_unix()).map_err(|e| AppError::Recording(e.to_string()))?;

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
/// Session ids must pass [`crate::commands::session_store::is_safe_session_id`]
/// and the canonicalized path must stay under the profile's sessions root.
pub fn delete_session_impl(app: &AppState, vs: &VaultState, session_id: &str) -> Result<()> {
    if !crate::commands::session_store::is_safe_session_id(session_id) {
        return Err(AppError::Config(format!("invalid session id: {session_id}")));
    }
    let sessions_root = app.profile.sessions_dir();
    let target = app.profile.session_path(session_id);
    if !target.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    // Held across the removal and the tombstone.
    let _writing = crate::sync::import::library_write_lock();
    let canon_target = contained_session_dir(&sessions_root, &target)?;
    // Read before the directory goes: a phone session's origin decides
    // whether a tombstone has to travel, and the id that phone knows the
    // session by is the one it travels under.
    let origin = phone_origin(&canon_target, session_id);
    remove_contained_session_dir(&sessions_root, &target)?;
    log::info!("deleted session {} at {}", session_id, canon_target.display());
    if let Some((phone_id, wire)) = origin {
        entomb_for_phone(app, vs, &wire, session_id, &phone_id);
    }
    Ok(())
}

/// The session directory as the filesystem resolves it, once it is known to sit under the sessions root; a path that leaves the library is refused.
pub fn contained_session_dir(sessions_root: &Path, target: &Path) -> Result<PathBuf> {
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
    Ok(canon_target)
}

/// Deletes a session directory through that check. The person's own delete and a phone's tombstone both go through here.
pub fn remove_contained_session_dir(sessions_root: &Path, target: &Path) -> Result<()> {
    let dir = contained_session_dir(sessions_root, target)?;
    std::fs::remove_dir_all(&dir).map_err(|e| AppError::Io(format!("delete {}: {e}", dir.display())))
}

/// The phone a session came from and the id it knows the session by, or
/// `None` for one this desktop recorded.
pub(crate) fn phone_origin(session_dir: &std::path::Path, local: &str) -> Option<(String, String)> {
    let bytes = syncsafe::read(session_dir.join("manifest.json")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let device = value["origin_device"].as_str().filter(|s| !s.is_empty())?;
    let wire = value["origin_session"].as_str().unwrap_or(local);
    Some((device.to_string(), wire.to_string()))
}

/// Signs the tombstone the phone collects at its next hello. With a locked vault the delete stands here.
fn entomb_for_phone(app: &AppState, vs: &VaultState, wire: &str, local: &str, phone_id: &str) {
    let id = match crate::sync::identity::load_or_create(app, vs) {
        Ok(id) => id,
        Err(e) => {
            log::warn!("sync: no tombstone for {wire}: {e}");
            return;
        }
    };
    let inbox = crate::sync::server::inbox_of(app);
    crate::sync::exchange::entomb_here(&inbox, wire, local, phone_id, &id.device_id, &id.signing_key());
}

/// Chunks either side of a cut, and how far into the straddling chunk the
/// cut falls.
struct CutAt {
    keep: Vec<recording::manifest::ChunkManifest>,
    after: Vec<recording::manifest::ChunkManifest>,
    /// Milliseconds from the start of the last kept chunk. 0 means the cut
    /// landed exactly on its boundary.
    into_last_ms: u64,
}

fn split_chunks_at(m: &recording::manifest::SessionManifest, cut_ms: u64) -> Result<CutAt> {
    let offsets = m.chunk_offsets_ms();
    let start_ms = |c: &recording::manifest::ChunkManifest| {
        u64::from(offsets.get(&c.index).copied().unwrap_or(0))
    };
    let keep: Vec<_> = m.chunks.iter().filter(|c| start_ms(c) <= cut_ms).cloned().collect();
    let after: Vec<_> = m.chunks.iter().filter(|c| start_ms(c) > cut_ms).cloned().collect();
    let Some(last) = keep.last() else {
        return Err(AppError::Config(
            "Cut point is before the first chunk — nothing would be left".into(),
        ));
    };
    let into_last_ms = cut_ms.saturating_sub(start_ms(last));
    let last_len_ms = last.audio_ms(m.sample_rate).unwrap_or(0);
    // Nothing to remove only when the cut is at or past the end of the final chunk.
    if after.is_empty() && (last_len_ms == 0 || into_last_ms >= last_len_ms) {
        return Err(AppError::Config("Nothing recorded after that point".into()));
    }
    Ok(CutAt { keep, after, into_last_ms })
}

/// Cut every track of `chunk` down to `keep_ms`, returning the audio removed
/// from the mic and system tracks so a split can rebuild it.
///
/// WAV here is PCM, so this is a copy of the leading samples — no re-encode
/// and no quality loss.
fn truncate_chunk_audio(
    root: &Path,
    chunk: &recording::manifest::ChunkManifest,
    keep_ms: u64,
    sample_rate: u32,
) -> Result<BTreeMap<PathBuf, Vec<i16>>> {
    let keep_samples = (keep_ms as usize * sample_rate as usize) / 1000;
    let mut tails = BTreeMap::new();
    let tracks = [
        Some(&chunk.mic_wav_relative),
        Some(&chunk.system_wav_relative),
        chunk.mic_aec_wav_relative.as_ref(),
        chunk.mic_dn_wav_relative.as_ref(),
    ];
    for rel in tracks.into_iter().flatten() {
        let p = syncsafe::join_rel(root, rel);
        if !p.is_file() {
            continue;
        }
        let (all, _) = recording::read_wav_mono_i16(&p)
            .map_err(|e| AppError::Io(format!("read {}: {e}", p.display())))?;
        if keep_samples >= all.len() {
            continue;
        }
        tails.insert(rel.clone(), all[keep_samples..].to_vec());
        recording::write_wav_mono_i16(&p, &all[..keep_samples], sample_rate)
            .map_err(|e| AppError::Io(format!("write {}: {e}", p.display())))?;
    }
    Ok(tails)
}

/// Rewrite a session's manifest + transcripts to end at the cut, and drop
/// every artifact derived from what came after. `into_last_ms` is how far
/// into the last kept chunk the cut fell; 0 means it landed on that chunk's
/// boundary.
fn trim_session_to(
    root: &std::path::Path,
    mut manifest: recording::manifest::SessionManifest,
    keep: Vec<recording::manifest::ChunkManifest>,
    into_last_ms: u64,
) -> Result<i64> {
    let last = keep.last().expect("keep is non-empty").clone();
    let last_index = last.index;
    let end_unix = last
        .ended_at_unix_seconds
        .unwrap_or(last.started_at_unix_seconds);
    // The live transcript and the telemetry are stamped on the session
    // timeline, not by chunk index: the cut sits `into_last_ms` into the last
    // kept chunk, or at its end when it landed on the boundary.
    let last_start = u64::from(manifest.chunk_offsets_ms().get(&last_index).copied().unwrap_or(0));
    let cut_from_ms = last_start
        + if into_last_ms > 0 {
            into_last_ms
        } else {
            last.audio_ms(manifest.sample_rate).unwrap_or(0)
        };

    manifest.chunks = keep;
    let kept_seconds = (manifest.audio_ms() / 1000) as i64;
    for seg in manifest.recording_segments.iter_mut() {
        if seg.last_chunk_index.is_none_or(|i| i > last_index) {
            seg.last_chunk_index = Some(last_index);
        }
        if seg.stopped_at_unix_seconds.is_none_or(|t| t > end_unix) {
            seg.stopped_at_unix_seconds = Some(end_unix);
        }
    }
    manifest.recording_segments.retain(|s| s.first_chunk_index <= last_index);
    syncsafe::write_atomic(root.join("manifest.json"), &serde_json::to_vec_pretty(&manifest)?)?;
    crate::live_names::overlay::delete_all(root);

    // Segment times are chunk-relative, so the cut inside the last kept chunk
    // sits at `into_last_ms` on that chunk's own clock.
    let keep_in_last = u32::try_from(into_last_ms).unwrap_or(u32::MAX);
    for name in ["transcript.json", "transcript.dedup.json"] {
        let p = root.join(name);
        let Ok(bytes) = syncsafe::read(&p) else { continue };
        let Ok(mut t) = serde_json::from_slice::<transcript::SessionTranscript>(&bytes) else {
            continue;
        };
        t.chunks.retain(|c| c.chunk_index <= last_index);
        if into_last_ms > 0 {
            for c in t.chunks.iter_mut().filter(|c| c.chunk_index == last_index) {
                for tr in c.tracks.iter_mut() {
                    tr.segments.retain_mut(|s| s.cut_after(keep_in_last));
                }
            }
        }
        syncsafe::write_atomic(&p, &serde_json::to_vec_pretty(&t)?)?;
    }

    recording::telemetry::truncate_words(root, cut_from_ms);
    let lt = root.join("live_transcript.jsonl");
    if let Ok(text) = std::fs::read_to_string(&lt) {
        let kept: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter(|l| {
                serde_json::from_str::<serde_json::Value>(l)
                    .ok()
                    .and_then(|v| v.get("start_ms").and_then(|m| m.as_u64()))
                    .is_none_or(|ms| ms < cut_from_ms)
            })
            .collect();
        let mut out = kept.join("\n");
        out.push('\n');
        let _ = std::fs::write(&lt, out);
    }

    drop_derived(root);
    Ok(kept_seconds)
}

/// Remove every artifact computed from the audio. The caller rebuilds
/// transcript.md and meeting.opus; summary and chapters are regenerated by
/// the user.
fn drop_derived(root: &std::path::Path) {
    for name in [
        "summary.json",
        "summary.md",
        "chapters.json",
        "transcript.md",
        recording::mixdown::MEETING_AUDIO_NAME,
    ] {
        let _ = std::fs::remove_file(root.join(name));
    }
}

/// Outcome of a truncation, for the toast the frontend shows.
#[derive(Debug, Serialize)]
pub struct TruncateOutcome {
    pub chunks_removed: usize,
    pub chunks_kept: usize,
    /// New session length in seconds after the cut.
    pub duration_seconds: i64,
}

/// Drop everything recorded after `cut_ms` (milliseconds from session
/// start): the audio chunks, their transcript, and every derived artifact.
///
/// Cuts to the sample: the chunk containing `cut_ms` keeps only what came
/// before it, and so does its transcript. Irreversible — the audio is
/// removed from disk, not archived.
pub fn truncate_session_after_impl(
    app: &AppState,
    session_id: &str,
    cut_ms: u64,
) -> Result<TruncateOutcome> {
    let root = app.profile.session_path(session_id);
    let manifest = read_manifest(&root, session_id)?;
    let cut = split_chunks_at(&manifest, cut_ms)?;
    let removed = cut.after.len();
    let sample_rate = manifest.sample_rate.max(8000);

    for c in &cut.after {
        remove_chunk_dir(&root, c)?;
    }
    let mut keep = cut.keep;
    if cut.into_last_ms > 0 {
        let last = keep.last_mut().expect("keep is non-empty");
        truncate_chunk_audio(&root, last, cut.into_last_ms, sample_rate)?;
        let secs = cut.into_last_ms.div_ceil(1000);
        last.duration_seconds = Some(secs);
        last.ended_at_unix_seconds = Some(last.started_at_unix_seconds + secs as i64);
        // Cut from the end: the first sample is unchanged, so the stamps
        // stand and only the counts shrink.
        let kept_samples = cut.into_last_ms * sample_rate as u64 / 1000;
        last.mic_samples = last.mic_samples.map(|n| n.min(kept_samples));
        last.system_samples = last.system_samples.map(|n| n.min(kept_samples));
        last.audio_samples = last.audio_samples.map(|n| n.min(kept_samples));
    }
    let kept = keep.len();
    let duration_seconds = trim_session_to(&root, manifest, keep, cut.into_last_ms)?;
    // transcript.md is rendered from the JSON that was just trimmed.
    let _ = rerender_session_transcript_impl(app, session_id);

    log::info!("truncated {session_id} after {cut_ms}ms: kept {kept} chunk(s), removed {removed}");
    Ok(TruncateOutcome { chunks_removed: removed, chunks_kept: kept, duration_seconds })
}

fn read_manifest(
    root: &std::path::Path,
    session_id: &str,
) -> Result<recording::manifest::SessionManifest> {
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    serde_json::from_slice(&syncsafe::read(root.join("manifest.json"))?)
        .map_err(|e| AppError::Config(format!("read manifest: {e}")))
}

fn chunk_dir(root: &std::path::Path, c: &recording::manifest::ChunkManifest) -> Option<std::path::PathBuf> {
    let d = syncsafe::join_rel(root, &c.mic_wav_relative).parent()?.to_path_buf();
    (d.starts_with(root) && d != root).then_some(d)
}

fn remove_chunk_dir(root: &std::path::Path, c: &recording::manifest::ChunkManifest) -> Result<()> {
    if let Some(d) = chunk_dir(root, c) {
        std::fs::remove_dir_all(&d)
            .map_err(|e| AppError::Io(format!("remove {}: {e}", d.display())))?;
    }
    Ok(())
}

/// Result of a split: the tail became its own session.
#[derive(Debug, Serialize)]
pub struct SplitOutcome {
    pub new_session_id: String,
    pub chunks_moved: usize,
    pub chunks_kept: usize,
}

/// Move everything after `cut_ms` into a new session, leaving the original
/// ending at the cut.
///
/// Segment times are chunk-relative and the session timeline is the chunks'
/// audio back to back, so the moved chunks form the new session's timeline
/// from zero; chunk entries, indices and directory names move unchanged. Only
/// the straddling chunk's segments shift, by the head that stayed behind.
///
/// The new session carries chunks, their timestamps and the capture settings.
/// Title, tags, attendees, calendar link, notes and integration history stay
/// with the original.
pub fn split_session_at_impl(
    app: &AppState,
    session_id: &str,
    cut_ms: u64,
) -> Result<SplitOutcome> {
    let root = app.profile.session_path(session_id);
    let manifest = read_manifest(&root, session_id)?;
    let cut = split_chunks_at(&manifest, cut_ms)?;

    let sample_rate = manifest.sample_rate.max(8000);
    // The cut usually lands inside a chunk, so the new session begins with
    // that chunk's tail. Its index is the straddling chunk's, keeping the
    // pairing with the transcript chunk the segments belong to.
    let straddles = cut.into_last_ms > 0;
    let last_kept = cut.keep.last().expect("keep is non-empty").clone();
    let new_created = if straddles {
        last_kept.started_at_unix_seconds + (cut.into_last_ms / 1000) as i64
    } else {
        cut.after
            .first()
            .map(|c| c.started_at_unix_seconds)
            .ok_or_else(|| AppError::Config("Nothing recorded after that point".into()))?
    };
    let first_index = if straddles {
        last_kept.index
    } else {
        cut.after.first().expect("after is non-empty").index
    };
    let new_id = format!("daisy-{new_created}");
    let new_root = app.profile.session_path(&new_id);
    if new_root.exists() {
        return Err(AppError::Config(format!("session {new_id} already exists")));
    }
    std::fs::create_dir_all(new_root.join("chunks"))
        .map_err(|e| AppError::Io(format!("create {}: {e}", new_root.display())))?;

    for c in &cut.after {
        let (Some(from), Some(to)) = (chunk_dir(&root, c), chunk_dir(&new_root, c)) else {
            continue;
        };
        std::fs::rename(&from, &to)
            .map_err(|e| AppError::Io(format!("move {} -> {}: {e}", from.display(), to.display())))?;
    }

    // Tail of the straddling chunk becomes the new session's first chunk.
    let mut new_chunks: Vec<recording::manifest::ChunkManifest> = Vec::new();
    if straddles {
        let tails = truncate_chunk_audio(&root, &last_kept, cut.into_last_ms, sample_rate)?;
        let mut head = last_kept.clone();
        let tail_secs = tails
            .values()
            .map(|v| v.len() as u64 / sample_rate as u64)
            .max()
            .unwrap_or(0);
        for (rel, pcm) in &tails {
            let dest = syncsafe::join_rel(&new_root, rel);
            if let Some(d) = dest.parent() {
                std::fs::create_dir_all(d)
                    .map_err(|e| AppError::Io(format!("create {}: {e}", d.display())))?;
            }
            recording::write_wav_mono_i16(&dest, pcm, sample_rate)
                .map_err(|e| AppError::Io(format!("write {}: {e}", dest.display())))?;
        }
        head.started_at_unix_seconds = new_created;
        head.duration_seconds = Some(tail_secs);
        head.ended_at_unix_seconds = Some(new_created + tail_secs as i64);
        // This chunk keeps only the tail, so its first sample is the one that
        // sat `removed` samples in and the stamps advance with it.
        let removed = cut.into_last_ms * sample_rate as u64 / 1000;
        let advance = removed * 1_000_000_000 / sample_rate.max(1) as u64;
        head.mic_host_ns = head.mic_host_ns.map(|ns| ns + advance);
        head.system_host_ns = head.system_host_ns.map(|ns| ns + advance);
        head.mic_samples = head.mic_samples.map(|n| n.saturating_sub(removed));
        head.system_samples = head.system_samples.map(|n| n.saturating_sub(removed));
        head.audio_samples = head.audio_samples.map(|n| n.saturating_sub(removed));
        new_chunks.push(head);
    }
    new_chunks.extend(cut.after.iter().cloned());
    let last = new_chunks.last().expect("new session has a chunk").clone();
    let new_end = last.ended_at_unix_seconds.unwrap_or(last.started_at_unix_seconds);
    let moved_indices: Vec<u32> = new_chunks.iter().map(|c| c.index).collect();

    let new_manifest = recording::manifest::SessionManifest {
        session_id: new_id.clone(),
        created_at_unix_seconds: new_created,
        chunks: new_chunks.clone(),
        finalized_at_unix_seconds: Some(new_end),
        recording_segments: vec![recording::manifest::RecordingSegment {
            reason: None,
            extra: Default::default(),
            started_at_unix_seconds: new_created,
            stopped_at_unix_seconds: Some(new_end),
            first_chunk_index: first_index,
            last_chunk_index: Some(last.index),
        }],
        title: Some("Split recording".into()),
        meeting_id: uuid::Uuid::new_v4().to_string(),
        tag_ids: vec![],
        attendees: vec![],
        calendar: None,
        notes_md_relative: None,
        sent_integration_ids: vec![],
        // Sample windows are on the old session's timeline, and the split
        // starts a new one: the new session samples segment spans until it is
        // diarized again.
        cluster_sample_spans: vec![], sample_spans_on_audio_timeline: false,
        ..manifest.clone()
    };
    syncsafe::write_atomic(new_root.join("manifest.json"), &serde_json::to_vec_pretty(&new_manifest)?)?;

    // Chunk-relative times stay correct under the new manifest's zero. The
    // straddling chunk moves only the segments after the cut, re-zeroed by
    // the head that stayed behind.
    let dropped_ms = u32::try_from(cut.into_last_ms).unwrap_or(u32::MAX);
    for name in ["transcript.json", "transcript.dedup.json"] {
        let Ok(bytes) = syncsafe::read(root.join(name)) else { continue };
        let Ok(t) = serde_json::from_slice::<transcript::SessionTranscript>(&bytes) else {
            continue;
        };
        let mut moved = t.clone();
        moved.session_id = new_id.clone();
        moved.chunks.retain(|c| moved_indices.contains(&c.chunk_index));
        if straddles {
            for c in moved.chunks.iter_mut().filter(|c| c.chunk_index == last_kept.index) {
                for tr in c.tracks.iter_mut() {
                    tr.segments.retain(|s| s.start_ms >= dropped_ms);
                    for s in tr.segments.iter_mut() {
                        s.shift(-i64::from(dropped_ms));
                    }
                }
            }
        }
        syncsafe::write_atomic(new_root.join(name), &serde_json::to_vec_pretty(&moved)?)?;
    }
    // live_transcript.jsonl is session-global and a finalize-time input; it
    // is not carried over.

    let kept = cut.keep.len();
    let moved = new_chunks.len();
    let mut keep = cut.keep;
    if straddles {
        let head = keep.last_mut().expect("keep is non-empty");
        let secs = cut.into_last_ms.div_ceil(1000);
        head.duration_seconds = Some(secs);
        head.ended_at_unix_seconds = Some(head.started_at_unix_seconds + secs as i64);
        // Keeps its head, so the stamps stand and only the counts shrink.
        let kept_samples = cut.into_last_ms * sample_rate as u64 / 1000;
        head.mic_samples = head.mic_samples.map(|n| n.min(kept_samples));
        head.system_samples = head.system_samples.map(|n| n.min(kept_samples));
        head.audio_samples = head.audio_samples.map(|n| n.min(kept_samples));
    }
    trim_session_to(&root, manifest, keep, cut.into_last_ms)?;
    // Both transcript.md files are rendered from the JSON just written.
    let _ = rerender_session_transcript_impl(app, session_id);
    let _ = rerender_session_transcript_impl(app, &new_id);

    log::info!("split {session_id} at {cut_ms}ms -> {new_id} ({moved} chunk(s) moved)");
    Ok(SplitOutcome { new_session_id: new_id, chunks_moved: moved, chunks_kept: kept })
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
    /// Diarization judged this cluster's enrollment sample to carry two
    /// voices; enrollment refuses it.
    pub mixed_voices: bool,
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
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    rerender_transcript_md_at(&root)
}

/// Re-render `transcript.md` in the session directory `root`.
pub(crate) fn rerender_transcript_md_at(root: &Path) -> Result<()> {
    render_transcript_md_on(root, &crate::commands::pipeline::chunk_offsets_ms(root))
}

/// Re-render `transcript.md` in the session directory `root` with each
/// chunk at `offsets` (`chunk_index -> ms`).
pub(crate) fn render_transcript_md_on(
    root: &Path,
    offsets: &std::collections::HashMap<u32, u32>,
) -> Result<()> {
    use transcript::render::render_markdown_with_speakers;
    let root = root.to_path_buf();
    let st_path = crate::commands::session_store::preferred_transcript_path(&root)
        .ok_or_else(|| {
            AppError::Config(format!("{}: no transcript to render", root.display()))
        })?;
    let st: transcript::SessionTranscript = serde_json::from_slice(&syncsafe::read(&st_path)?)
        .map_err(|e| AppError::Config(format!("parse transcript: {e}")))?;

    // Same speaker derivation as the dedup/polish renders.
    let speakers = crate::commands::pipeline::speaker_map_for(&root);
    let md = render_markdown_with_speakers(&st, offsets, &speakers);
    crate::commands::write_transcript_md(&root, &md)?;
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
    // The caption names what the clip plays, and the clip carries the pool
    // enrollment reads: this cluster's isolated windows.
    let isolated_only = true;
    let (_pcm, text) = crate::commands::voiceprints::gather_cluster_sample(
        &root,
        cluster_id,
        SAMPLE_CAP_MS,
        isolated_only,
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
    let pcm = crate::commands::voiceprints::gather_cluster_isolated_pcm_capped(
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
    let Some(st_path) = crate::commands::session_store::preferred_transcript_path(&root) else {
        return Ok(Vec::new());
    };
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
    let mut mixed_clusters: Vec<u32> = Vec::new();
    if let Some(m) = crate::commands::session_store::read_manifest_opt(&root) {
        cluster_sides = m.cluster_sides.clone();
        mixed_clusters = m.mixed_clusters.clone();
        for s in m.speaker_map {
            labels.insert(
                s.cluster_id,
                (s.display_name, s.email, s.voiceprint_id, s.match_confidence),
            );
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
            mixed_voices: mixed_clusters.contains(sid),
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
                mixed_voices: mixed_clusters.contains(&sid),
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
    // A participant added to a session no run has numbered starts a numbering
    // of this machine's own.
    recording::session_meta::mint_run(&root).map_err(|e| AppError::Recording(e.to_string()))?;
    // A participant added by hand is a name a person wrote, so it is authored
    // on the session's own file and reaches the other device from there.
    recording::session_meta::name_speaker(&root, new_cluster_id, &name, None, crate::now_unix())
        .map_err(|e| AppError::Recording(e.to_string()))?;
    session
        .update_manifest(|m| {
            m.speaker_map.retain(|s| s.cluster_id != new_cluster_id);
            m.speaker_map.push(recording::manifest::SpeakerLabel {
                extra: Default::default(),
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
    let session = recording::session::Session::load(&root).map_err(|e| AppError::Recording(e.to_string()))?;
    let manifest_json = serde_json::to_value(session.manifest())?;
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
        transcript::Segment { start_ms, end_ms, text: text.into(), confidence: None, speaker_id, words: Vec::new(), split_group: None }
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
            provisional: false,
            edited_at_unix_seconds: None,
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

#[cfg(test)]
mod truncate_tests {
    use super::*;
    use crate::state::AppState;
    use recording::manifest::{ChunkManifest, RecordingSegment, SessionManifest};

    /// Three 300s chunks starting at t=0, with audio and derived artifacts.
    fn session(tmp: &tempfile::TempDir) -> (AppState, String) {
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        let app = AppState::new(profile);
        let sid = "daisy-1000000000";
        let root = app.profile.session_path(sid);
        let created = 1_000_000_000i64;
        let mut chunks = Vec::new();
        for i in 1..=3u32 {
            let dir = recording::chunk_dir(&root, i);
            std::fs::create_dir_all(&dir).unwrap();
            // 300 s of real PCM at 16 kHz so a mid-chunk cut has samples to
            // remove and the truncated file can be read back.
            let pcm = vec![0i16; 300 * 16_000];
            recording::write_wav_mono_i16(&dir.join("mic.wav"), &pcm, 16_000).unwrap();
            recording::write_wav_mono_i16(&dir.join("system.wav"), &pcm, 16_000).unwrap();
            let start = created + ((i - 1) as i64) * 300;
            chunks.push(ChunkManifest {
                extra: Default::default(),
                index: i,
                started_at_unix_seconds: start,
                ended_at_unix_seconds: Some(start + 300),
                duration_seconds: Some(300),
                mic_wav_relative: recording::chunk_rel(i, "mic.wav"),
                system_wav_relative: recording::chunk_rel(i, "system.wav"),
                mic_aec_wav_relative: None,
                mic_dn_wav_relative: None,
                mic_host_ns: Some(((i - 1) as u64) * 300 * 1_000_000_000),
                system_host_ns: Some(((i - 1) as u64) * 300 * 1_000_000_000 + 1_000),
                mic_samples: Some(300 * 16_000),
                system_samples: Some(300 * 16_000),
                mic_voice_processed: false,
                mic_zero_samples: None,
                mic_longest_zero_run: None,
                audio_samples: None,
            });
        }
        let m = SessionManifest {
            recording_kind: None,
            origin_device: None,
            extra: Default::default(),
            schema_version: SessionManifest::SCHEMA,
            session_id: sid.into(),
            created_at_unix_seconds: created,
            chunks,
            recording_segments: vec![RecordingSegment {
                reason: None,
                extra: Default::default(),
                started_at_unix_seconds: created,
                stopped_at_unix_seconds: Some(created + 900),
                first_chunk_index: 1,
                last_chunk_index: Some(3),
            }],
            sample_rate: 16_000,
            channels: 1,
            mic_source_id: 0,
            mic_source_node_name: String::new(),
            mic_source_description: String::new(),
            system_source_id: 0,
            system_source_node_name: String::new(),
            system_source_description: String::new(),
            aec_mode: recording::manifest::AecMode::Disabled,
            finalized_at_unix_seconds: Some(created + 900),
            title: None,
            meeting_id: String::new(),
            tag_ids: vec![],
            notes_md_relative: None,
            attendees: vec![],
            calendar: None,
            speaker_map: vec![],
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
        };
        syncsafe::write(root.join("manifest.json"), &serde_json::to_vec(&m).unwrap()).unwrap();
        for f in ["summary.json", "summary.md", "chapters.json", "transcript.md"] {
            std::fs::write(root.join(f), b"x").unwrap();
        }
        (app, sid.to_string())
    }

    #[test]
    fn cuts_inside_the_chunk_and_drops_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, sid) = session(&tmp);
        let root = app.profile.session_path(&sid);

        // 350s falls inside chunk 2 (300..600): chunk 2 is cut 50 s in.
        let out = truncate_session_after_impl(&app, &sid, 350_000).unwrap();
        assert_eq!(out.chunks_kept, 2);
        assert_eq!(out.chunks_removed, 1);
        assert_eq!(out.duration_seconds, 350, "session ends at the cut, not the chunk");

        assert!(root.join("chunks/0002").is_dir(), "the cut's own chunk stays");
        let (kept_pcm, _) = recording::read_wav_mono_i16(&root.join("chunks/0002/mic.wav")).unwrap();
        assert_eq!(kept_pcm.len(), 50 * 16_000, "audio is cut at the cut point");
        assert!(!root.join("chunks/0003").exists(), "later audio is gone from disk");

        let m: SessionManifest =
            serde_json::from_slice(&syncsafe::read(root.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(m.chunks.iter().map(|c| c.index).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(m.recording_segments[0].last_chunk_index, Some(2));
        assert_eq!(m.recording_segments[0].stopped_at_unix_seconds, Some(1_000_000_350));

        // Artifacts derived from the deleted audio are removed.
        for f in ["summary.json", "summary.md", "chapters.json", "transcript.md"] {
            assert!(!root.join(f).exists(), "{f} must be deleted, not left stale");
        }
    }

    #[test]
    fn refuses_a_cut_that_would_empty_the_session_or_change_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, sid) = session(&tmp);
        // Past the end: nothing to remove.
        assert!(truncate_session_after_impl(&app, &sid, 5_000_000).is_err());
        // Still three chunks on disk.
        assert!(app.profile.session_path(&sid).join("chunks/0003").is_dir());
    }

    /// A transcript whose chunk-2 track has one segment before the 350 s cut,
    /// one spanning it, and one after it. Chunk 1 and 3 carry a single
    /// segment each at their own zero.
    fn write_straddling_transcript(root: &std::path::Path, sid: &str) {
        // Words are spread evenly across each segment's span.
        let seg = |start_ms: u32, end_ms: u32, text: &str| {
            let toks: Vec<&str> = text.split(' ').collect();
            let step = (end_ms - start_ms) / toks.len() as u32;
            let mut seg = transcript::Segment::from_words(toks.iter().enumerate().map(|(i, t)| {
                let at = start_ms + step * i as u32;
                (t.to_string(), at as i64, (at + step) as i64)
            }))
            .unwrap();
            seg.end_ms = end_ms;
            seg
        };
        let chunk = |i: u32, segments: Vec<transcript::Segment>| transcript::ChunkTranscript {
            chunk_index: i,
            tracks: vec![transcript::TrackTranscript {
                track: transcript::Track::MicAec,
                source_wav_relative: format!("chunks/{i:04}/mic.wav").into(),
                segments,
            }],
        };
        let st = transcript::SessionTranscript {
            schema_version: 1,
            session_id: sid.into(),
            provider: "t".into(),
            model: "t".into(),
            transcribed_at_unix_seconds: 0,
            provisional: false,
            edited_at_unix_seconds: None,
            chunks: vec![
                chunk(1, vec![seg(0, 1_000, "chunk 1")]),
                chunk(
                    2,
                    vec![
                        seg(0, 1_000, "before the cut"),
                        seg(45_000, 55_000, "across the cut"),
                        seg(60_000, 61_000, "after the cut"),
                    ],
                ),
                chunk(3, vec![seg(0, 1_000, "chunk 3")]),
            ],
        };
        let bytes = serde_json::to_vec(&st).unwrap();
        syncsafe::write(root.join("transcript.json"), &bytes).unwrap();
        syncsafe::write(root.join("transcript.dedup.json"), &bytes).unwrap();
    }

    fn segs_words_of(p: &std::path::Path, chunk_index: u32) -> Vec<transcript::Word> {
        segments_of(p, chunk_index).into_iter().flat_map(|s| s.words).collect()
    }

    fn segments_of(p: &std::path::Path, chunk_index: u32) -> Vec<transcript::Segment> {
        let t: transcript::SessionTranscript =
            serde_json::from_slice(&syncsafe::read(p).unwrap()).unwrap();
        t.chunks
            .iter()
            .find(|c| c.chunk_index == chunk_index)
            .map(|c| c.tracks[0].segments.clone())
            .unwrap_or_default()
    }

    /// The straddling chunk is kept, so its transcript is cut on the same
    /// clock as its audio.
    #[test]
    fn truncate_cuts_the_straddling_chunks_transcript_by_time() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, sid) = session(&tmp);
        let root = app.profile.session_path(&sid);
        write_straddling_transcript(&root, &sid);

        truncate_session_after_impl(&app, &sid, 350_000).unwrap();

        for name in ["transcript.json", "transcript.dedup.json"] {
            let segs = segments_of(&root.join(name), 2);
            let texts: Vec<&str> = segs.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(texts, vec!["before the cut", "across the"], "{name}");
            assert_eq!(segs[1].end_ms, 50_000, "{name}: a segment cannot outlast its audio");
            assert!(segs.iter().all(|s| s.words_consistent()), "{name}: words follow the cut");
            assert_eq!(segs[1].text, "across the", "{name}: words past the cut leave with their text");
            assert_eq!(segs[1].words.len(), 2, "{name}");
            let t: transcript::SessionTranscript =
                serde_json::from_slice(&syncsafe::read(root.join(name)).unwrap()).unwrap();
            assert_eq!(t.chunks.iter().map(|c| c.chunk_index).collect::<Vec<_>>(), vec![1, 2]);
        }

        // transcript.md is regenerated from the trimmed JSON.
        // The renderer capitalises each turn, so compare on lowercase.
        let md = std::fs::read_to_string(root.join("transcript.md")).unwrap().to_lowercase();
        assert!(md.contains("before the cut"), "{md}");
        assert!(!md.contains("after the cut"), "text past the cut must be gone:\n{md}");
        assert!(!md.contains("chunk 3"), "removed chunks must be gone:\n{md}");
    }

    const LIVE_WORDS: &str = "{\"v\":1,\"wall_ms\":1,\"kind\":\"words\",\"track\":\"mic\",\"engine\":\"nemotron\",\"words\":[[\"before\",349000,349200,null],[\"after\",351000,351200,null]]}\n";

    fn telemetry_of(root: &std::path::Path) -> String {
        std::fs::read_to_string(root.join(recording::telemetry::TELEMETRY_FILE)).unwrap()
    }

    #[test]
    fn truncate_drops_the_live_words_past_the_cut() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, sid) = session(&tmp);
        let root = app.profile.session_path(&sid);
        std::fs::write(
            root.join("live_transcript.jsonl"),
            "{\"track\":\"mic\",\"start_ms\":349000,\"end_ms\":349500,\"text\":\"before the cut\",\"final\":true,\"received_at_unix\":0}\n\
             {\"track\":\"mic\",\"start_ms\":351000,\"end_ms\":351500,\"text\":\"after the cut\",\"final\":true,\"received_at_unix\":0}\n",
        )
        .unwrap();
        std::fs::write(root.join(recording::telemetry::TELEMETRY_FILE), LIVE_WORDS).unwrap();

        truncate_session_after_impl(&app, &sid, 350_000).unwrap();

        let live = std::fs::read_to_string(root.join("live_transcript.jsonl")).unwrap();
        assert!(live.contains("before the cut") && !live.contains("after the cut"), "{live}");
        let tel = telemetry_of(&root);
        assert!(tel.contains("\"before\"") && !tel.contains("\"after\""), "{tel}");
    }

    #[test]
    fn split_leaves_no_live_words_past_the_cut_in_the_original() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, sid) = session(&tmp);
        let root = app.profile.session_path(&sid);
        std::fs::write(root.join(recording::telemetry::TELEMETRY_FILE), LIVE_WORDS).unwrap();

        split_session_at_impl(&app, &sid, 350_000).unwrap();

        let tel = telemetry_of(&root);
        assert!(tel.contains("\"before\"") && !tel.contains("\"after\""), "{tel}");
    }

    #[test]
    fn split_divides_the_straddling_chunks_transcript_at_the_cut() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, sid) = session(&tmp);
        let root = app.profile.session_path(&sid);
        write_straddling_transcript(&root, &sid);

        // 350 s straddles chunk 2, so the tail of chunk 2 plus chunk 3 move.
        let out = split_session_at_impl(&app, &sid, 350_000).unwrap();
        assert_eq!(out.chunks_kept, 2);
        assert_eq!(out.chunks_moved, 2);

        let new_root = app.profile.session_path(&out.new_session_id);
        let nm: SessionManifest =
            serde_json::from_slice(&syncsafe::read(new_root.join("manifest.json")).unwrap()).unwrap();

        // The new session starts when its first chunk started; that chunk's
        // render offset is 0.
        assert_eq!(nm.created_at_unix_seconds, nm.chunks[0].started_at_unix_seconds);
        assert_eq!(nm.chunks.iter().map(|c| c.index).collect::<Vec<_>>(), vec![2, 3]);
        // The moved head holds exactly the audio after the cut.
        let (tail, _) = recording::read_wav_mono_i16(&new_root.join("chunks/0002/mic.wav")).unwrap();
        assert_eq!(tail.len(), 250 * 16_000, "chunk 2 was cut 50 s in");
        assert_eq!(nm.title.as_deref(), Some("Split recording"));
        assert!(nm.tag_ids.is_empty(), "user metadata does not follow the audio");

        // Audio moved, not copied.
        assert!(new_root.join("chunks/0003/mic.wav").is_file());
        assert!(!root.join("chunks/0003").exists());

        for name in ["transcript.json", "transcript.dedup.json"] {
            // The tail moves, re-zeroed against the new session's start.
            let moved = segments_of(&new_root.join(name), 2);
            assert_eq!(
                moved.iter().map(|s| (s.start_ms, s.text.as_str())).collect::<Vec<_>>(),
                vec![(10_000, "after the cut")],
                "{name}",
            );
            let moved_words = segs_words_of(&new_root.join(name), 2);
            assert_eq!(moved_words[0].start_ms, 10_000, "{name}: words re-zero with their segment");
            // Chunk 3 moves whole, its times untouched.
            assert_eq!(segments_of(&new_root.join(name), 3)[0].start_ms, 0, "{name}");
            // The head stays behind, and stays only once.
            let kept = segments_of(&root.join(name), 2);
            assert_eq!(
                kept.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
                vec!["before the cut", "across the"],
                "{name}",
            );
            assert!(segments_of(&root.join(name), 3).is_empty(), "{name}");
        }
        let nt: transcript::SessionTranscript =
            serde_json::from_slice(&syncsafe::read(new_root.join("transcript.dedup.json")).unwrap())
                .unwrap();
        assert_eq!(nt.session_id, out.new_session_id);

        // Source keeps only its half.
        let om: SessionManifest =
            serde_json::from_slice(&syncsafe::read(root.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(om.chunks.iter().map(|c| c.index).collect::<Vec<_>>(), vec![1, 2]);
    }

    /// A split session starts at a non-1 chunk index with no chunk 1 on
    /// disk. Manifest and transcript chunks pair by index, never position.
    #[test]
    fn non_contiguous_chunk_indices_render_with_correct_offsets() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, sid) = session(&tmp);
        let root = app.profile.session_path(&sid);

        // What a split leaves: chunks 3 and 4, session starting at chunk 3.
        let created = 1_000_000_600i64; // chunk 3's start in the fixture
        let mut m: SessionManifest =
            serde_json::from_slice(&syncsafe::read(root.join("manifest.json")).unwrap()).unwrap();
        m.created_at_unix_seconds = created;
        m.chunks.retain(|c| c.index == 3);
        let mut c4 = m.chunks[0].clone();
        c4.index = 4;
        c4.started_at_unix_seconds = created + 300;
        c4.ended_at_unix_seconds = Some(created + 600);
        c4.mic_wav_relative = "chunks/0004/mic.wav".into();
        c4.system_wav_relative = "chunks/0004/system.wav".into();
        m.chunks.push(c4);
        syncsafe::write(root.join("manifest.json"), &serde_json::to_vec(&m).unwrap()).unwrap();

        // Both transcript chunks carry chunk-RELATIVE times starting at 0.
        let seg = |t: &str| transcript::Segment {
            start_ms: 0, end_ms: 1000, text: t.into(), confidence: None, speaker_id: None,
            words: Vec::new(), split_group: None,
        };
        let st = transcript::SessionTranscript {
            schema_version: 1,
            session_id: sid.clone(),
            provider: "t".into(),
            model: "t".into(),
            transcribed_at_unix_seconds: 0,
            provisional: false,
            edited_at_unix_seconds: None,
            chunks: vec![3u32, 4]
                .into_iter()
                .map(|i| transcript::ChunkTranscript {
                    chunk_index: i,
                    tracks: vec![transcript::TrackTranscript {
                        track: transcript::Track::MicAec,
                        source_wav_relative: format!("chunks/{i:04}/mic.wav").into(),
                        segments: vec![seg(&format!("chunk {i}"))],
                    }],
                })
                .collect(),
        };
        syncsafe::write(root.join("transcript.dedup.json"), &serde_json::to_vec(&st).unwrap()).unwrap();

        rerender_session_transcript_impl(&app, &sid).unwrap();
        let md = std::fs::read_to_string(root.join("transcript.md")).unwrap();

        // The first chunk is index 3 and renders at zero.
        assert!(md.contains("[00:00:00]"), "first chunk re-zeroes regardless of its index:\n{md}");
        // The second offsets by the audio of the chunk before it.
        assert!(md.contains("[00:05:00]"), "second chunk offsets by the audio before it:\n{md}");
        assert!(!md.contains("[00:15:00]"), "index must not be treated as an offset:\n{md}");
    }

    /// One long chunk, cut in the middle: the cut removes audio inside the chunk.
    #[test]
    fn cuts_a_single_long_chunk() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, sid) = session(&tmp);
        let root = app.profile.session_path(&sid);

        // Collapse the fixture to one chunk, as a real 18-minute recording is.
        let mut m: SessionManifest =
            serde_json::from_slice(&syncsafe::read(root.join("manifest.json")).unwrap()).unwrap();
        m.chunks.retain(|c| c.index == 1);
        m.chunks[0].duration_seconds = Some(300);
        m.recording_segments[0].last_chunk_index = Some(1);
        syncsafe::write(root.join("manifest.json"), &serde_json::to_vec(&m).unwrap()).unwrap();
        std::fs::remove_dir_all(root.join("chunks/0002")).unwrap();
        std::fs::remove_dir_all(root.join("chunks/0003")).unwrap();

        let out = truncate_session_after_impl(&app, &sid, 120_000).unwrap();
        assert_eq!(out.chunks_kept, 1);
        assert_eq!(out.chunks_removed, 0, "no whole chunk goes; the audio is cut");
        assert_eq!(out.duration_seconds, 120);

        let (pcm, _) = recording::read_wav_mono_i16(&root.join("chunks/0001/mic.wav")).unwrap();
        assert_eq!(pcm.len(), 120 * 16_000);

        // A cut past the end still refuses.
        assert!(truncate_session_after_impl(&app, &sid, 900_000).is_err());
    }

    fn manifest_of(app: &AppState, sid: &str) -> SessionManifest {
        let p = app.profile.session_path(sid).join("manifest.json");
        serde_json::from_slice(&syncsafe::read(p).unwrap()).unwrap()
    }

    /// The fixture with a 1000 s pause before chunk 2 and a 213 s sleep
    /// inside it: chunk 2's wall span is 513 s and its files hold 300 s.
    fn with_pause_and_sleep(app: &AppState, sid: &str) {
        let root = app.profile.session_path(sid);
        let mut m = manifest_of(app, sid);
        let created = m.created_at_unix_seconds;
        m.chunks[1].started_at_unix_seconds = created + 1300;
        m.chunks[1].ended_at_unix_seconds = Some(created + 1813);
        m.chunks[1].duration_seconds = Some(513);
        m.chunks[2].started_at_unix_seconds = created + 1813;
        m.chunks[2].ended_at_unix_seconds = Some(created + 2113);
        syncsafe::write(root.join("manifest.json"), &serde_json::to_vec(&m).unwrap()).unwrap();
    }

    #[test]
    fn transcript_times_after_a_pause_and_a_sleep_match_the_audio() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, sid) = session(&tmp);
        let root = app.profile.session_path(&sid);
        with_pause_and_sleep(&app, &sid);
        write_straddling_transcript(&root, &sid);

        rerender_session_transcript_impl(&app, &sid).unwrap();
        let md = std::fs::read_to_string(root.join("transcript.md")).unwrap();
        // Chunk 2 plays from 5:00 in meeting.opus, chunk 3 from 10:00.
        assert!(md.contains("[00:05:00]"), "{md}");
        assert!(md.contains("[00:10:00]"), "{md}");
        assert!(!md.contains("[00:21:40]"), "the pause takes no time:\n{md}");
    }

    #[test]
    fn a_cut_after_a_pause_and_a_sleep_lands_on_the_audio_timeline() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, sid) = session(&tmp);
        let root = app.profile.session_path(&sid);
        with_pause_and_sleep(&app, &sid);

        // 350 s on the transcript is 50 s into chunk 2.
        let out = truncate_session_after_impl(&app, &sid, 350_000).unwrap();
        assert_eq!(out.chunks_kept, 2);
        assert_eq!(out.duration_seconds, 350);
        let (kept_pcm, _) = recording::read_wav_mono_i16(&root.join("chunks/0002/mic.wav")).unwrap();
        assert_eq!(kept_pcm.len(), 50 * 16_000);
    }

    #[test]
    fn truncate_shrinks_the_sample_count_and_keeps_the_stamp() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, sid) = session(&tmp);
        truncate_session_after_impl(&app, &sid, 450_000).unwrap();
        let m = manifest_of(&app, &sid);
        let last = m.chunks.last().unwrap();
        assert_eq!(last.index, 2);
        assert_eq!(last.mic_host_ns, Some(300 * 1_000_000_000));
        assert_eq!(last.system_host_ns, Some(300 * 1_000_000_000 + 1_000));
        assert_eq!(last.mic_samples, Some(150 * 16_000));
        assert_eq!(last.system_samples, Some(150 * 16_000));
    }

    #[test]
    fn split_advances_the_new_sessions_stamp_by_the_removed_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, sid) = session(&tmp);
        let out = split_session_at_impl(&app, &sid, 450_000).unwrap();
        let m = manifest_of(&app, &out.new_session_id);
        let head = &m.chunks[0];
        assert_eq!(head.mic_host_ns, Some(450 * 1_000_000_000));
        assert_eq!(head.system_host_ns, Some(450 * 1_000_000_000 + 1_000));
        assert_eq!(head.mic_samples, Some(150 * 16_000));
    }

    #[test]
    fn split_shrinks_the_original_head_chunk_count_only() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, sid) = session(&tmp);
        split_session_at_impl(&app, &sid, 450_000).unwrap();
        let m = manifest_of(&app, &sid);
        let head = m.chunks.last().unwrap();
        assert_eq!(head.index, 2);
        assert_eq!(head.mic_host_ns, Some(300 * 1_000_000_000));
        assert_eq!(head.mic_samples, Some(150 * 16_000));
    }

    #[test]
    fn whole_chunk_removal_carries_timing_through_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, sid) = session(&tmp);
        truncate_session_after_impl(&app, &sid, 600_000).unwrap();
        let m = manifest_of(&app, &sid);
        assert_eq!(m.chunks[0].mic_host_ns, Some(0));
        assert_eq!(m.chunks[0].mic_samples, Some(300 * 16_000));
        assert_eq!(m.chunks[1].mic_host_ns, Some(300 * 1_000_000_000));
        assert_eq!(m.chunks[1].mic_samples, Some(300 * 16_000));
    }

}

#[cfg(test)]
mod delete_tests {
    use super::*;
    use crate::state::{AppState, VaultState};

    #[test]
    fn deleting_a_phone_session_waits_for_an_import_holding_the_library() {
        let tmp = tempfile::tempdir().unwrap();
        let app = AppState::new(crate::profile::ProfileDir::at(tmp.path()).unwrap());
        let vs = VaultState::default();
        let sid = "daisy-1700000000";
        let dir = app.profile.session_path(sid);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), br#"{"origin_device":"4a68f72f5a22110b2bfbe71b6588833c"}"#).unwrap();

        let importing = crate::sync::import::library_write_lock();
        std::thread::scope(|s| {
            let deleting = s.spawn(|| delete_session_impl(&app, &vs, sid));
            std::thread::sleep(std::time::Duration::from_millis(300));
            assert!(dir.is_dir(), "the delete waits while an import writes");
            drop(importing);
            deleting.join().unwrap().unwrap();
        });
        assert!(!dir.exists());
    }
}
