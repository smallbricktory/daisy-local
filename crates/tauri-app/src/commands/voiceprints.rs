//! Extract speaker embeddings from a session's audio, match them against the
//! vault's known voiceprints, and expose a CRUD API to the Settings →
//! Voiceprints panel.

use crate::error::{AppError, Result};
use crate::state::{AppState, Voiceprint, VaultState};
use recording::manifest::{ClusterSide, SessionManifest, SpeakerSide};
use serde::{Deserialize, Serialize};
use voiceprints::{
    cluster_speakers, cosine, match_threshold, merge_by_gallery, read_pcm_window, Encoder,
};


/// Diarization parameters resolved per run. `known_count`: `Some` pins the
/// cluster count, `None` auto-estimates.
#[derive(Clone, Copy, Default)]
struct DiarizeParams {
    known_count: Option<usize>,
}

impl DiarizeParams {
    /// Per-call known speaker count from the Participants "# of speakers" field
    /// (None / 0 = auto-estimate). `DAISY_DIARIZE_MAX_SPEAKERS` overrides when set.
    fn for_call(expected: Option<usize>) -> Self {
        let env_usize = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<usize>().ok());
        let known_count = expected
            .or_else(|| env_usize("DAISY_DIARIZE_MAX_SPEAKERS"))
            .filter(|&n| n > 0);
        Self { known_count }
    }
}

const ENROLL_CAP_MS: u32 = 30_000; // max 30s of audio per voiceprint extraction
const GALLERY_CAP: usize = 16; // max embeddings retained per identity

#[derive(Debug, Serialize)]
pub struct VoiceprintView {
    pub id: String,
    pub display_name: String,
    pub email: Option<String>,
    pub created_at_unix_seconds: i64,
    pub session_count: u32,
    pub vector_dim: u32,
    /// Number of samples in this identity's gallery.
    pub sample_count: u32,
}

fn to_view(v: &Voiceprint) -> VoiceprintView {
    let embs = v.embeddings();
    VoiceprintView {
        id: v.id.clone(),
        display_name: v.display_name.clone(),
        email: v.email.clone(),
        created_at_unix_seconds: v.created_at_unix_seconds,
        session_count: v.session_ids.len() as u32,
        vector_dim: embs.first().map(|e| e.len()).unwrap_or(0) as u32,
        sample_count: embs.len() as u32,
    }
}

pub fn list_voiceprints_impl(_app: &AppState, vs: &VaultState) -> Result<Vec<VoiceprintView>> {
    let g = vs.keys.lock().unwrap();
    let keys = g.as_ref().ok_or(AppError::VaultLocked)?;
    Ok(keys.voiceprints.iter().map(to_view).collect())
}

#[derive(Debug, Deserialize)]
pub struct VoiceprintRenameRequest {
    pub id: String,
    pub display_name: String,
    pub email: Option<String>,
}

pub fn rename_voiceprint_impl(
    app: &AppState,
    vs: &VaultState,
    req: VoiceprintRenameRequest,
) -> Result<()> {
    let new_name = req.display_name.trim().to_string();
    if new_name.is_empty() {
        return Err(AppError::Config("display name is required".into()));
    }
    let email = req.email.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    update_vault(app, vs, |keys| {
        if let Some(v) = keys.voiceprints.iter_mut().find(|v| v.id == req.id) {
            v.display_name = new_name;
            v.email = email;
            Ok(())
        } else {
            Err(AppError::Config(format!("no voiceprint {}", req.id)))
        }
    })
}

pub fn delete_voiceprint_impl(app: &AppState, vs: &VaultState, id: &str) -> Result<()> {
    update_vault(app, vs, |keys| {
        let before = keys.voiceprints.len();
        keys.voiceprints.retain(|v| v.id != id);
        if keys.voiceprints.len() == before {
            return Err(AppError::Config(format!("no voiceprint {id}")));
        }
        Ok(())
    })
}

/// Detach the voiceprint link from a session's cluster: clears `voiceprint_id`
/// and `match_confidence` on the matching `SpeakerLabel`. The display name and
/// the vault gallery are unchanged.
pub fn detach_speaker_voiceprint_impl(
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
    let mut changed = false;
    session
        .update_manifest(|m| {
            for s in m.speaker_map.iter_mut() {
                if s.cluster_id == cluster_id && (s.voiceprint_id.is_some() || s.match_confidence.is_some()) {
                    s.voiceprint_id = None;
                    s.match_confidence = None;
                    changed = true;
                }
            }
        })
        .map_err(|e| AppError::Recording(e.to_string()))?;
    if !changed {
        log::debug!(
            "detach_speaker_voiceprint: session {session_id} cluster {cluster_id} had no voiceprint link"
        );
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct EnrollFromSpeakerRequest {
    pub session_id: String,
    pub cluster_id: u32,
    pub display_name: String,
    pub email: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EnrollResult {
    pub voiceprint_id: String,
    pub vector_dim: u32,
    pub samples_ms: u32,
}

/// Extract a voiceprint from a specific session+cluster's audio, save it to
/// the vault, and stamp `manifest.speaker_map[cluster_id]` with the new
/// voiceprint id.
pub fn enroll_voiceprint_from_speaker_impl(
    app: &AppState,
    vs: &VaultState,
    req: EnrollFromSpeakerRequest,
) -> Result<EnrollResult> {
    let display_name = req.display_name.trim().to_string();
    if display_name.is_empty() {
        return Err(AppError::Config("display name is required".into()));
    }
    let email = req.email.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    // Find-or-create the Contact for this person (keyed by email, else name).
    let enrolled_contact_id =
        crate::commands::contacts::upsert_contact_in_store(app, &display_name, email.as_deref())?;
    let root = app.profile.session_path(&req.session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(req.session_id));
    }

    let vec = extract_vector_for_cluster(&root, req.cluster_id)?;
    let voiceprint_id = uuid::Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let samples_ms = ENROLL_CAP_MS;
    let dim = vec.len() as u32;

    // 1. Store the embedding in the vault as a gallery entry. Identities are
    //    keyed by email (case-insensitive) when present, else by display name.
    //    Enrolling the same person again appends this sample to their gallery
    //    and absorbs duplicate entries sharing the key.
    let session_id_for_vp = req.session_id.clone();
    let display_for_vp = display_name.clone();
    let email_for_vp = email.clone();
    let new_id = voiceprint_id.clone();
    let contact_for_vp = enrolled_contact_id.clone();
    update_vault(app, vs, move |keys| {
        let key_email = email_for_vp.as_deref().map(|e| e.to_lowercase());
        let key_name = display_for_vp.to_lowercase();
        let matches: Vec<usize> = keys
            .voiceprints
            .iter()
            .enumerate()
            .filter(|(_, v)| match (&key_email, v.email.as_deref()) {
                (Some(k), Some(ve)) => ve.to_lowercase() == *k,
                _ => v.display_name.to_lowercase() == key_name,
            })
            .map(|(i, _)| i)
            .collect();

        if let Some(&primary) = matches.first() {
            // Remove duplicate entries in descending index order.
            let mut absorbed: Vec<Voiceprint> = Vec::new();
            for &idx in matches.iter().skip(1).rev() {
                absorbed.push(keys.voiceprints.remove(idx));
            }
            let vp = &mut keys.voiceprints[primary];
            vp.contact_id = Some(contact_for_vp.clone());
            if !vp.vector.is_empty() {
                let legacy = std::mem::take(&mut vp.vector);
                vp.vectors.push(legacy);
            }
            for mut other in absorbed {
                if !other.vector.is_empty() {
                    vp.vectors.push(std::mem::take(&mut other.vector));
                }
                vp.vectors.append(&mut other.vectors);
                for sid in other.session_ids {
                    if !vp.session_ids.contains(&sid) {
                        vp.session_ids.push(sid);
                    }
                }
            }
            vp.vectors.push(vec.clone());
            if vp.vectors.len() > GALLERY_CAP {
                let drop = vp.vectors.len() - GALLERY_CAP;
                vp.vectors.drain(0..drop);
            }
            vp.display_name = display_for_vp.clone();
            vp.email = email_for_vp.clone();
            if !vp.session_ids.contains(&session_id_for_vp) {
                vp.session_ids.push(session_id_for_vp.clone());
            }
        } else {
            keys.voiceprints.push(Voiceprint {
                id: new_id.clone(),
                display_name: display_for_vp.clone(),
                email: email_for_vp.clone(),
                vector: Vec::new(),
                vectors: vec![vec.clone()],
                created_at_unix_seconds: now,
                session_ids: vec![session_id_for_vp.clone()],
                contact_id: Some(contact_for_vp.clone()),
            });
        }
        Ok(())
    })?;
    // Re-read the surviving identity's id: the merge target, or the freshly
    // inserted entry.
    let voiceprint_id = {
        let key_email = email.as_deref().map(|e| e.to_lowercase());
        let key_name = display_name.to_lowercase();
        let g = vs.keys.lock().unwrap();
        g.as_ref()
            .and_then(|keys| {
                keys.voiceprints
                    .iter()
                    .find(|v| match (&key_email, v.email.as_deref()) {
                        (Some(k), Some(ve)) => ve.to_lowercase() == *k,
                        _ => v.display_name.to_lowercase() == key_name,
                    })
                    .map(|v| v.id.clone())
            })
            .unwrap_or(voiceprint_id)
    };

    // 2. Write the voiceprint id onto the session's speaker_map entry.
    //    `set_session_speaker_label` clears voiceprint ids; the entry is
    //    written directly via the Session API.
    use recording::manifest::SpeakerLabel;
    use recording::session::Session;
    let mut session =
        Session::load(&root).map_err(|e| AppError::Recording(e.to_string()))?;
    let cluster_id = req.cluster_id;
    let dn = display_name.clone();
    let em = email.clone();
    let vp_id = voiceprint_id.clone();
    session
        .update_manifest(|m| {
            m.speaker_map.retain(|s| s.cluster_id != cluster_id);
            m.speaker_map.push(SpeakerLabel {
                cluster_id,
                display_name: dn.clone(),
                email: em.clone(),
                voiceprint_id: Some(vp_id.clone()),
                // Enrolled clusters record no confidence score.
                match_confidence: None,
                contact_id: Some(enrolled_contact_id.clone()),
            });
        })
        .map_err(|e| AppError::Recording(e.to_string()))?;

    Ok(EnrollResult {
        voiceprint_id,
        vector_dim: dim,
        samples_ms,
    })
}

/// Walk every session in the profile, run `rematch_session_speakers_impl`,
/// and re-render `transcript.md` for any session that gained new labels.
/// Returns `(sessions_scanned, clusters_matched)`. Best-effort: per-session
/// failures are logged and skipped, never aborting the whole sweep.
pub fn rematch_all_sessions_impl(
    app: &AppState,
    vs: &VaultState,
) -> Result<(u32, u32)> {
    let dir = app.profile.sessions_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(it) => it,
        Err(_) => return Ok((0, 0)),
    };
    let mut scanned = 0u32;
    let mut matched_total = 0u32;
    for ent in entries.flatten() {
        let path = ent.path();
        if !path.is_dir() {
            continue;
        }
        let Some(sid) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        scanned += 1;
        match rematch_session_speakers_impl(app, vs, sid) {
            Ok(0) => {}
            Ok(n) => {
                matched_total += n;
                if let Err(e) = crate::commands::session::rerender_session_transcript_impl(app, sid)
                {
                    log::warn!("rerender after rematch for {sid}: {e}");
                }
            }
            Err(e) => log::warn!("rematch sweep: session {sid}: {e}"),
        }
    }
    log::info!("rematch sweep: scanned {scanned} session(s), matched {matched_total} cluster(s)");
    Ok((scanned, matched_total))
}

/// Extract embeddings for a session's un-labeled clusters and auto-fill
/// `manifest.speaker_map` for clusters that match a known voiceprint above
/// `match_threshold()`. Idempotent.
pub fn rematch_session_speakers_impl(
    app: &AppState,
    vs: &VaultState,
    session_id: &str,
) -> Result<u32> {
    let voiceprints = {
        let g = vs.keys.lock().unwrap();
        let keys = g
            .as_ref()
            .ok_or(AppError::VaultLocked)?;
        keys.voiceprints.clone()
    };
    if voiceprints.is_empty() {
        return Ok(0);
    }
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    let clusters = clusters_needing_match(&root)?;
    if clusters.is_empty() {
        return Ok(0);
    }
    let mut matched = 0u32;
    let mut encoder = Encoder::load()
        .map_err(|e| AppError::Config(format!("voiceprint model: {e}")))?;
    for cluster_id in clusters {
        let Ok(samples) = gather_cluster_pcm(&root, cluster_id) else { continue };
        if samples.len() < (voiceprints::SAMPLE_RATE as usize) {
            continue; // under 1s of speech
        }
        let Ok(vec) = encoder.encode_pcm(&samples) else { continue };
        let mut best: Option<(&Voiceprint, f32)> = None;
        for vp in &voiceprints {
            // Match score = max cosine over the identity's gallery.
            let s = vp
                .embeddings()
                .into_iter()
                .map(|e| cosine(&vec, e))
                .fold(f32::MIN, f32::max);
            if best.map(|(_, b)| s > b).unwrap_or(true) {
                best = Some((vp, s));
            }
        }
        let Some((vp, score)) = best else { continue };
        if score >= match_threshold() {
            apply_match(&root, cluster_id, vp, score)?;
            // Record the session id in the vault entry (best-effort).
            let id = vp.id.clone();
            let sid = session_id.to_string();
            let _ = update_vault(app, vs, move |keys| {
                if let Some(v) = keys.voiceprints.iter_mut().find(|v| v.id == id) {
                    if !v.session_ids.contains(&sid) {
                        v.session_ids.push(sid.clone());
                    }
                }
                Ok(())
            });
            matched += 1;
            // Logs carry the voiceprint id, never the person's name.
            log::info!(
                "voiceprint match: session {session_id} cluster {cluster_id} -> vp {} (cosine {:.3})",
                vp.id,
                score
            );
        }
    }

    // Collapse distinct clusters that matched the same vault identity.
    if let Err(e) = merge_clusters_sharing_voiceprint(&root, session_id) {
        log::warn!("vault-merge for {session_id}: {e}");
    }

    Ok(matched)
}

/// Walk `manifest.speaker_map` for clusters that ended up pinned to the same
/// `voiceprint_id` after the rematch loop. For each such voiceprint, merge
/// every duplicate cluster into the lowest-numbered one — rewriting
/// `speaker_id` on segments in transcript.json + transcript.dedup.json and
/// dropping the duplicate entries from `speaker_map`. Best-effort; per-file
/// failures log and continue.
fn merge_clusters_sharing_voiceprint(
    root: &std::path::Path,
    session_id: &str,
) -> Result<()> {
    use std::collections::HashMap;
    let mp_path = root.join("manifest.json");
    let bytes = syncsafe::read(&mp_path)?;
    let mut m: SessionManifest = serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Config(format!("parse manifest: {e}")))?;

    // (voiceprint_id, Vec<cluster_id>) grouped from the just-updated speaker_map.
    let mut groups: HashMap<String, Vec<u32>> = HashMap::new();
    for sl in &m.speaker_map {
        if let Some(vid) = &sl.voiceprint_id {
            groups.entry(vid.clone()).or_default().push(sl.cluster_id);
        }
    }

    let mut any_changed = false;
    for (vid, mut clusters) in groups {
        if clusters.len() < 2 {
            continue;
        }
        clusters.sort();
        let canonical = clusters[0];
        let to_merge: Vec<u32> = clusters[1..].to_vec();
        log::info!(
            "vault-merge: session {session_id} voiceprint_id={vid} clusters {clusters:?} -> {canonical}"
        );

        // Rewrite speaker_id on transcript JSON files. Both files are kept
        // (dedup is the renderer's source of truth, raw is the audit trail).
        for name in ["transcript.dedup.json", "transcript.json"] {
            let p = root.join(name);
            if !p.is_file() {
                continue;
            }
            let tb = match syncsafe::read(&p) {
                Ok(b) => b,
                Err(e) => {
                    log::warn!("vault-merge: read {}: {e}", p.display());
                    continue;
                }
            };
            let mut st: transcript::SessionTranscript = match serde_json::from_slice(&tb) {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("vault-merge: parse {}: {e}", p.display());
                    continue;
                }
            };
            let mut changed = 0u32;
            for ch in st.chunks.iter_mut() {
                for tr in ch.tracks.iter_mut() {
                    for seg in tr.segments.iter_mut() {
                        if let Some(sid) = seg.speaker_id {
                            if to_merge.contains(&sid) {
                                seg.speaker_id = Some(canonical);
                                changed += 1;
                            }
                        }
                    }
                }
            }
            if changed > 0 {
                if let Err(e) = syncsafe::write(&p, serde_json::to_vec_pretty(&st)?) {
                    log::warn!("vault-merge: write {}: {e}", p.display());
                    continue;
                }
                log::info!(
                    "vault-merge: {name} session={session_id} merged {} segment(s) into cluster {canonical}",
                    changed
                );
                any_changed = true;
            }
        }

        let before = m.speaker_map.len();
        m.speaker_map.retain(|sl| !to_merge.contains(&sl.cluster_id));
        if m.speaker_map.len() != before {
            any_changed = true;
        }
    }

    if any_changed {
        let tmp = mp_path.with_extension("json.tmp");
        syncsafe::write(&tmp, serde_json::to_vec_pretty(&m)?)?;
        syncsafe::rename(&tmp, &mp_path)?;
    }
    Ok(())
}

fn apply_match(
    session_dir: &std::path::Path,
    cluster_id: u32,
    vp: &Voiceprint,
    score: f32,
) -> Result<()> {
    use recording::manifest::SpeakerLabel;
    use recording::session::Session;
    let mut session = Session::load(session_dir).map_err(|e| AppError::Recording(e.to_string()))?;
    let dn = vp.display_name.clone();
    let em = vp.email.clone();
    let vp_id = vp.id.clone();
    session
        .update_manifest(|m| {
            m.speaker_map.retain(|s| s.cluster_id != cluster_id);
            m.speaker_map.push(SpeakerLabel {
                cluster_id,
                display_name: dn.clone(),
                email: em.clone(),
                voiceprint_id: Some(vp_id.clone()),
                match_confidence: Some(score),
                contact_id: None,
            });
        })
        .map_err(|e| AppError::Recording(e.to_string()))?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct DiarizeResult {
    /// Number of distinct speakers found in the system track.
    pub speakers: u32,
    /// System segments that got a speaker_id assigned.
    pub segments_labeled: u32,
}

/// True if the session's transcript has speech on some track and no speaker
/// labels on any track.
pub fn transcript_undiarized(app: &AppState, session_id: &str) -> bool {
    let root = app.profile.session_path(session_id);
    let st_path = if root.join("transcript.dedup.json").is_file() {
        root.join("transcript.dedup.json")
    } else if root.join("transcript.json").is_file() {
        root.join("transcript.json")
    } else {
        return false;
    };
    let Ok(bytes) = syncsafe::read(&st_path) else { return false };
    let Ok(st) = serde_json::from_slice::<transcript::SessionTranscript>(&bytes) else {
        return false;
    };
    let mut saw_speech = false;
    for ch in &st.chunks {
        for tr in &ch.tracks {
            for seg in &tr.segments {
                saw_speech = true;
                if seg.speaker_id.is_some() {
                    return false; // already diarized
                }
            }
        }
    }
    saw_speech // undiarized only if there ARE segments and none labeled
}

/// Build the mic-id offset and the full `cluster_sides` list for a hybrid
/// diarization. System clusters are `0..sys_count` (Remote); mic clusters are
/// `offset..offset+mic_count` where `offset = sys_count` (Room), disjoint from
/// the system ids. Returns `(offset, cluster_sides)`.
fn merge_side_ids(sys_count: u32, mic_count: u32) -> (u32, Vec<ClusterSide>) {
    let offset = sys_count;
    let mut sides = Vec::with_capacity((sys_count + mic_count) as usize);
    for id in 0..sys_count {
        sides.push(ClusterSide { cluster_id: id, side: SpeakerSide::Remote });
    }
    for id in 0..mic_count {
        sides.push(ClusterSide { cluster_id: id + offset, side: SpeakerSide::Room });
    }
    (offset, sides)
}

/// L2-normalized mean of a set of unit embeddings (None if empty).
fn centroid_of<'a>(embs: impl Iterator<Item = &'a [f32]>) -> Option<Vec<f32>> {
    let mut acc: Vec<f32> = Vec::new();
    let mut n = 0usize;
    for e in embs {
        if acc.is_empty() {
            acc = vec![0.0; e.len()];
        }
        for (a, v) in acc.iter_mut().zip(e) {
            *a += *v;
        }
        n += 1;
    }
    if n == 0 {
        return None;
    }
    let norm = acc.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 {
        for a in acc.iter_mut() {
            *a /= norm;
        }
    }
    Some(acc)
}

/// Cluster one track's embeddings and spread the labels over all of its
/// segments (short/silent ones inherit the nearest preceding speaker, leading
/// ones inherit the first known speaker). Returns `(per-position assignment,
/// cluster count)`. Cluster ids are `0..count`.
fn assign_for(
    positions: &[(usize, usize, usize)],
    embedded: &[(usize, Vec<f32>)],
    params: DiarizeParams,
    bleed_anchor: Option<&[f32]>,
) -> (Vec<Option<u32>>, u32) {
    if embedded.is_empty() {
        return (vec![None; positions.len()], 0);
    }
    let embs: Vec<Vec<f32>> = embedded.iter().map(|(_, v)| v.clone()).collect();
    let mut ids = cluster_speakers(&embs, params.known_count);
    // When a bleed anchor is supplied, clusters whose centroid matches it are
    // folded together.
    if let Some(anchor) = bleed_anchor {
        let gallery = [(u32::MAX, anchor.to_vec())];
        ids = merge_by_gallery(&ids, &embs, &gallery, match_threshold());
    }
    let count = ids.iter().copied().max().map(|m| m + 1).unwrap_or(0);
    let mut assign: Vec<Option<u32>> = vec![None; positions.len()];
    for ((pidx, _), id) in embedded.iter().zip(ids.iter()) {
        assign[*pidx] = Some(*id);
    }
    // Short/silent segments inherit the nearest preceding labeled segment…
    let mut last: Option<u32> = None;
    for a in assign.iter_mut() {
        if a.is_some() {
            last = *a;
        } else if let Some(l) = last {
            *a = Some(l);
        }
    }
    // …and any leading unlabeled ones inherit the first known speaker.
    if let Some(first) = assign.iter().flatten().next().copied() {
        for a in assign.iter_mut() {
            if a.is_none() {
                *a = Some(first);
            }
        }
    }
    (assign, count)
}

/// Which track(s) to diarize when the user re-runs diarization explicitly.
/// `None` (not this enum) means auto — derive from the manifest flag (the
/// finalize-time default). An explicit choice here overrides that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiarizeScope {
    /// Others (Attendees) — the system/loopback track. Default for remote calls.
    Others,
    /// Local mic — in-person / room-mic recordings where the people are on the mic.
    Mic,
    /// Both tracks (system Remote + mic Room).
    Both,
}

/// Resolve which track(s) to cluster: an explicit user `scope` wins; with no
/// explicit scope the default derives from the manifest flag — group on the
/// local end → Both; solo + empty system → Mic; solo + remote → Others.
fn resolve_scope(scope: Option<DiarizeScope>, single_local: bool, sys_empty: bool) -> DiarizeScope {
    scope.unwrap_or(if !single_local {
        DiarizeScope::Both
    } else if sys_empty {
        DiarizeScope::Mic
    } else {
        DiarizeScope::Others
    })
}

pub fn diarize_session_impl(
    app: &AppState,
    session_id: &str,
    expected_speakers: Option<u32>,
    scope: Option<DiarizeScope>,
) -> Result<DiarizeResult> {
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    let st_path = if root.join("transcript.dedup.json").is_file() {
        root.join("transcript.dedup.json")
    } else if root.join("transcript.json").is_file() {
        root.join("transcript.json")
    } else {
        return Err(AppError::Config("no transcript to diarize".into()));
    };
    let manifest: SessionManifest = serde_json::from_slice(&syncsafe::read(root.join("manifest.json"))?)
        .map_err(|e| AppError::Config(format!("parse manifest: {e}")))?;
    let mut st: transcript::SessionTranscript =
        serde_json::from_slice(&syncsafe::read(&st_path)?)
            .map_err(|e| AppError::Config(format!("parse transcript: {e}")))?;

    let mut encoder = Encoder::load()
        .map_err(|e| AppError::Config(format!("voiceprint model: {e}")))?;

    let params = DiarizeParams::for_call(expected_speakers.map(|n| n as usize));
    log::info!(
        "diarize {session_id}: known_count={:?} (k-means + silhouette estimate)",
        params.known_count
    );

    let mut all_positions: Vec<(usize, usize, usize)> = Vec::new();
    let mut all_assign: Vec<Option<u32>> = Vec::new();
    let mut cluster_sides: Vec<ClusterSide> = Vec::new();

    // On a single-track re-run (scope Mic or Others) the other track's
    // clusters are preserved: fresh ids start above them, and their
    // speaker_map/cluster_sides entries survive the manifest update below.
    let ids_on_track = |want_mic: bool| -> std::collections::BTreeSet<u32> {
        let mut ids = std::collections::BTreeSet::new();
        for ch in &st.chunks {
            for tr in &ch.tracks {
                let is_mic = matches!(tr.track, transcript::Track::Mic | transcript::Track::MicAec);
                if is_mic != want_mic {
                    continue;
                }
                for seg in &tr.segments {
                    if let Some(id) = seg.speaker_id {
                        ids.insert(id);
                    }
                }
            }
        }
        ids
    };
    // Ids that survive this run (set per scope arm below).
    let mut surviving_ids: std::collections::BTreeSet<u32> = Default::default();

    // Fast path: exactly 1 known speaker on a solo-local recording labels every
    // system-track segment as that one person, with no embedding or clustering.
    // Runs only when the (implicit or explicit) scope is Others.
    let fast_ok = matches!(scope, None | Some(DiarizeScope::Others));
    if fast_ok && params.known_count == Some(1) && manifest.single_local_speaker {
        let positions = track_segment_positions(&st, DiarizeTrackSelect::System);
        if !positions.is_empty() {
            log::info!("diarize {session_id}: 1-speaker fast path (no embedding)");
            surviving_ids = ids_on_track(true); // mic clusters untouched
            let base = surviving_ids.iter().next_back().map_or(0, |m| m + 1);
            cluster_sides.push(ClusterSide { cluster_id: base, side: SpeakerSide::Remote });
            all_assign = vec![Some(base); positions.len()];
            all_positions = positions;
        }
    }

    // Mic mirror of the fast path: a solo-local session with no system-track
    // speech is one person on the mic. Every mic segment gets the same Room
    // cluster, with no embedding or clustering.
    let mic_fast_ok = matches!(scope, None | Some(DiarizeScope::Mic))
        && manifest.single_local_speaker
        && matches!(params.known_count, None | Some(1));
    if all_positions.is_empty()
        && mic_fast_ok
        && track_segment_positions(&st, DiarizeTrackSelect::System).is_empty()
    {
        let positions = track_segment_positions(&st, DiarizeTrackSelect::Mic);
        if !positions.is_empty() {
            log::info!("diarize {session_id}: solo mic fast path (no embedding)");
            cluster_sides.push(ClusterSide { cluster_id: 0, side: SpeakerSide::Room });
            all_assign = vec![Some(0); positions.len()];
            all_positions = positions;
        }
    }

    // Diarizer backend = the Settings → Voiceprints choice. speakrs fills the
    // same all_positions/all_assign/cluster_sides the k-means path below
    // produces; that path then skips (all_positions non-empty).
    let use_speakrs = crate::settings::Settings::load_or_default(&app.profile.settings_path())
        .diarizer
        == "speakrs";
    if use_speakrs && all_positions.is_empty() {
        // On any speakrs failure, the k-means path below runs.
        match speakrs_diarize(&st, &manifest, &root, scope, params.known_count) {
            Ok((positions, assign, sides, surviving)) => {
                all_positions = positions;
                all_assign = assign;
                cluster_sides = sides;
                surviving_ids = surviving;
            }
            Err(e) => {
                log::warn!(
                    "diarize {session_id}: speakrs failed ({e}); falling back to k-means"
                );
                // all_positions stays empty → the k-means full path below runs.
            }
        }
    }

    // Full path: embed + cluster. (Skipped when the fast path already assigned.)
    if all_positions.is_empty() {
    // System track clustering. On a solo-local recording the mic centroid is
    // passed as the bleed anchor.
    let (sys_positions, sys_embedded) =
        collect_track_embeddings(&st, &manifest, &root, DiarizeTrackSelect::System, &mut encoder);
    let local_anchor: Option<Vec<f32>> = if manifest.single_local_speaker && !sys_embedded.is_empty() {
        let (_p, mic_embedded) =
            collect_track_embeddings(&st, &manifest, &root, DiarizeTrackSelect::Mic, &mut encoder);
        centroid_of(mic_embedded.iter().map(|(_, v)| v.as_slice()))
    } else {
        None
    };
    let (sys_assign, sys_clusters) =
        assign_for(&sys_positions, &sys_embedded, params, local_anchor.as_deref());

    let effective = resolve_scope(scope, manifest.single_local_speaker, sys_embedded.is_empty());

    match effective {
        DiarizeScope::Both => {
            // Cluster the mic (room) track too, with ids offset above the
            // system clusters.
            let (mic_positions, mic_embedded) =
                collect_track_embeddings(&st, &manifest, &root, DiarizeTrackSelect::Mic, &mut encoder);
            let (mic_assign_local, mic_clusters) =
                assign_for(&mic_positions, &mic_embedded, params, None);

            let (offset, sides) = merge_side_ids(sys_clusters, mic_clusters);
            cluster_sides = sides;
            all_positions.extend_from_slice(&sys_positions);
            all_assign.extend(sys_assign.iter().copied());
            all_positions.extend_from_slice(&mic_positions);
            all_assign.extend(mic_assign_local.iter().map(|a| a.map(|id| id + offset)));
        }
        DiarizeScope::Mic => {
            // Cluster the mic track; clusters are tagged Remote. System-track
            // clusters (if any) survive untouched; fresh ids start above them.
            let (mic_positions, mic_embedded) =
                collect_track_embeddings(&st, &manifest, &root, DiarizeTrackSelect::Mic, &mut encoder);
            let (mic_assign, mic_clusters) = assign_for(&mic_positions, &mic_embedded, params, None);
            surviving_ids = ids_on_track(false); // system clusters untouched
            let base = surviving_ids.iter().next_back().map_or(0, |m| m + 1);
            for id in 0..mic_clusters {
                cluster_sides.push(ClusterSide { cluster_id: base + id, side: SpeakerSide::Remote });
            }
            all_positions = mic_positions;
            all_assign = mic_assign.iter().map(|a| a.map(|id| id + base)).collect();
        }
        DiarizeScope::Others => {
            // Cluster the system track only; mic-track clusters (if any)
            // survive untouched; fresh ids start above them.
            surviving_ids = ids_on_track(true);
            let base = surviving_ids.iter().next_back().map_or(0, |m| m + 1);
            for id in 0..sys_clusters {
                cluster_sides.push(ClusterSide { cluster_id: base + id, side: SpeakerSide::Remote });
            }
            all_positions = sys_positions;
            all_assign = sys_assign.iter().map(|a| a.map(|id| id + base)).collect();
        }
    }
    } // end full-path (fast path already assigned)

    if all_assign.iter().all(|a| a.is_none()) {
        return Ok(DiarizeResult { speakers: 0, segments_labeled: 0 });
    }

    // Persist the per-cluster side tags. Session writes atomically (tmp +
    // rename) and re-reads the current manifest.
    {
        use recording::session::Session;
        let mut session = Session::load(&root).map_err(|e| AppError::Recording(e.to_string()))?;
        session
            .update_manifest(|m| {
                // Side tags + labels for clusters this run did not touch are
                // kept; the rest are dropped. The caller re-applies
                // enrolled-voiceprint matches onto the fresh clusters afterward.
                m.cluster_sides.retain(|c| surviving_ids.contains(&c.cluster_id));
                m.cluster_sides.extend(cluster_sides);
                m.speaker_map.retain(|s| surviving_ids.contains(&s.cluster_id));
                // A successful re-process clears the "Interrupted — recovered"
                // badge.
                m.interrupted = false;
            })
            .map_err(|e| AppError::Recording(e.to_string()))?;
    }

    let speakers = all_assign.iter().flatten().copied().max().map(|m| m + 1).unwrap_or(0);
    let mut segments_labeled = 0u32;
    for (pidx, (ci, ti, si)) in all_positions.iter().enumerate() {
        if let Some(spk) = all_assign[pidx] {
            if let Some(seg) = st.chunks[*ci].tracks[*ti].segments.get_mut(*si) {
                seg.speaker_id = Some(spk);
                segments_labeled += 1;
            }
        }
    }

    // Atomic write (tmp + rename).
    let st_tmp = st_path.with_extension("json.tmp");
    syncsafe::write(&st_tmp, serde_json::to_vec_pretty(&st)?)?;
    syncsafe::rename(&st_tmp, &st_path)?;
    // Re-render transcript.md with the fresh speaker labels. Best-effort.
    if let Err(e) = crate::commands::session::rerender_session_transcript_impl(app, session_id) {
        log::warn!("rerender after diarize for {session_id}: {e}");
    }
    Ok(DiarizeResult { speakers, segments_labeled })
}

/// Which transcript track to harvest for the diarize embedder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiarizeTrackSelect {
    /// The System (loopback) track — meeting participants in remote calls.
    System,
    /// The Mic track (echo-cancelled if present, else raw) — single-source
    /// recordings: lectures, in-person meetings on a room mic.
    Mic,
}

impl DiarizeTrackSelect {
    fn matches(self, t: transcript::Track) -> bool {
        match self {
            DiarizeTrackSelect::System => matches!(t, transcript::Track::System),
            DiarizeTrackSelect::Mic => matches!(t, transcript::Track::Mic | transcript::Track::MicAec),
        }
    }

    /// Resolve the per-chunk WAV path for this track. The mic side uses the
    /// AEC-cleaned file when present, else the raw mic; the DFN3-denoised file
    /// is not used.
    fn wav_path(self, chunk: &recording::manifest::ChunkManifest, root: &std::path::Path) -> std::path::PathBuf {
        match self {
            DiarizeTrackSelect::System => root.join(&chunk.system_wav_relative),
            DiarizeTrackSelect::Mic => match chunk.mic_aec_wav_relative.as_ref() {
                Some(p) => root.join(p),
                None => root.join(&chunk.mic_wav_relative),
            },
        }
    }
}

/// Walk every segment of the requested track, embed those long enough, and
/// return parallel `(position, embedding)` lists. `positions` includes ALL
/// segments; `embedded` carries only the ones fed through WeSpeaker.
fn collect_track_embeddings(
    st: &transcript::SessionTranscript,
    manifest: &SessionManifest,
    root: &std::path::Path,
    select: DiarizeTrackSelect,
    encoder: &mut Encoder,
) -> (Vec<(usize, usize, usize)>, Vec<(usize, Vec<f32>)>) {
    // Segments under MIN_MS are skipped here; the post-cluster smoothing pass
    // attributes them to the nearest preceding labeled speaker.
    const MIN_MS: u32 = 1_200;
    const EMBED_CAP_MS: u32 = 8_000;

    let mut positions: Vec<(usize, usize, usize)> = Vec::new();
    let mut embedded: Vec<(usize, Vec<f32>)> = Vec::new();

    for (ci, ch) in st.chunks.iter().enumerate() {
        let wav = manifest
            .chunks
            .iter()
            .find(|c| c.index == ch.chunk_index)
            .map(|c| select.wav_path(c, root));
        for (ti, tr) in ch.tracks.iter().enumerate() {
            if !select.matches(tr.track) {
                continue;
            }
            for (si, seg) in tr.segments.iter().enumerate() {
                let pidx = positions.len();
                positions.push((ci, ti, si));
                let dur = seg.end_ms.saturating_sub(seg.start_ms);
                if dur < MIN_MS {
                    continue;
                }
                let Some(wav) = wav.as_ref() else { continue };
                if !wav.is_file() {
                    continue;
                }
                let cap = dur.min(EMBED_CAP_MS);
                let Ok(pcm) = read_pcm_window(wav, seg.start_ms, seg.start_ms + cap, cap) else {
                    continue;
                };
                if pcm.len() < (voiceprints::SAMPLE_RATE as usize / 2) {
                    continue;
                }
                // Windows below the -46 dBFS silence floor are skipped.
                if pcm_rms_norm(&pcm) < SAMPLE_SILENCE_FLOOR {
                    continue;
                }
                if let Ok(v) = encoder.encode_pcm(&pcm) {
                    embedded.push((pidx, v));
                }
            }
        }
    }
    (positions, embedded)
}

/// Every segment position `(chunk, track, seg)` for the given track — no audio
/// read, no embedding.
fn track_segment_positions(
    st: &transcript::SessionTranscript,
    select: DiarizeTrackSelect,
) -> Vec<(usize, usize, usize)> {
    let mut positions = Vec::new();
    for (ci, ch) in st.chunks.iter().enumerate() {
        for (ti, tr) in ch.tracks.iter().enumerate() {
            if !select.matches(tr.track) {
                continue;
            }
            for si in 0..tr.segments.len() {
                positions.push((ci, ti, si));
            }
        }
    }
    positions
}


fn speakrs_diarize(
    st: &transcript::SessionTranscript,
    manifest: &SessionManifest,
    root: &std::path::Path,
    scope: Option<DiarizeScope>,
    known_count: Option<usize>,
) -> Result<(
    Vec<(usize, usize, usize)>,
    Vec<Option<u32>>,
    Vec<ClusterSide>,
    std::collections::BTreeSet<u32>,
)> {
    let ids_on_track = |want_mic: bool| -> std::collections::BTreeSet<u32> {
        let mut ids = std::collections::BTreeSet::new();
        for ch in &st.chunks {
            for tr in &ch.tracks {
                let is_mic = matches!(tr.track, transcript::Track::Mic | transcript::Track::MicAec);
                if is_mic != want_mic {
                    continue;
                }
                for seg in &tr.segments {
                    if let Some(id) = seg.speaker_id {
                        ids.insert(id);
                    }
                }
            }
        }
        ids
    };

    let mut cluster_sides: Vec<ClusterSide> = Vec::new();
    let mut surviving_ids: std::collections::BTreeSet<u32> = Default::default();
    let mut all_positions: Vec<(usize, usize, usize)> = Vec::new();
    let mut all_assign: Vec<Option<u32>> = Vec::new();

    // known_count drives a cluster-to-N cosine merge. It applies to the system
    // track, and to the mic only when the mic is the sole scope; in Both the
    // mic is unconstrained.
    let (sys_positions, sys_assign, sys_clusters) =
        speakrs_cluster_track(st, manifest, root, DiarizeTrackSelect::System, known_count)?;
    let effective = resolve_scope(scope, manifest.single_local_speaker, sys_positions.is_empty());
    match effective {
        DiarizeScope::Both => {
            let (mic_positions, mic_assign_local, mic_clusters) =
                speakrs_cluster_track(st, manifest, root, DiarizeTrackSelect::Mic, None)?;
            let (offset, sides) = merge_side_ids(sys_clusters, mic_clusters);
            cluster_sides = sides;
            all_positions.extend_from_slice(&sys_positions);
            all_assign.extend(sys_assign.iter().copied());
            all_positions.extend_from_slice(&mic_positions);
            all_assign.extend(mic_assign_local.iter().map(|a| a.map(|id| id + offset)));
        }
        DiarizeScope::Mic => {
            let (mic_positions, mic_assign, mic_clusters) =
                speakrs_cluster_track(st, manifest, root, DiarizeTrackSelect::Mic, known_count)?;
            surviving_ids = ids_on_track(false);
            let base = surviving_ids.iter().next_back().map_or(0, |m| m + 1);
            for id in 0..mic_clusters {
                cluster_sides.push(ClusterSide { cluster_id: base + id, side: SpeakerSide::Remote });
            }
            all_positions = mic_positions;
            all_assign = mic_assign.iter().map(|a| a.map(|id| id + base)).collect();
        }
        DiarizeScope::Others => {
            surviving_ids = ids_on_track(true);
            let base = surviving_ids.iter().next_back().map_or(0, |m| m + 1);
            for id in 0..sys_clusters {
                cluster_sides.push(ClusterSide { cluster_id: base + id, side: SpeakerSide::Remote });
            }
            all_positions = sys_positions;
            all_assign = sys_assign.iter().map(|a| a.map(|id| id + base)).collect();
        }
    }
    Ok((all_positions, all_assign, cluster_sides, surviving_ids))
}

/// speakrs-backed clustering for one track. Concatenates the track's per-chunk
/// WAVs, runs speakrs once, and assigns each segment the speakrs label with
/// the most time-overlap. Returns the same `(positions, per-position
/// assignment, count)` shape as the k-means `collect_track_embeddings` +
/// `assign_for` pair.
fn speakrs_cluster_track(
    st: &transcript::SessionTranscript,
    manifest: &SessionManifest,
    root: &std::path::Path,
    select: DiarizeTrackSelect,
    known_count: Option<usize>,
) -> Result<(Vec<(usize, usize, usize)>, Vec<Option<u32>>, u32)> {
    let positions = track_segment_positions(st, select);
    if positions.is_empty() {
        return Ok((positions, Vec::new(), 0));
    }
    // Concat the track's audio; record each chunk's start offset (seconds).
    // seg.start_ms is chunk-relative; absolute = offset(chunk) + start_ms.
    let mut audio: Vec<f32> = Vec::new();
    let mut off_s: Vec<f64> = vec![0.0; st.chunks.len()];
    for (ci, ch) in st.chunks.iter().enumerate() {
        off_s[ci] = audio.len() as f64 / voiceprints::SAMPLE_RATE as f64;
        if let Some(cm) = manifest.chunks.iter().find(|c| c.index == ch.chunk_index) {
            let wav = select.wav_path(cm, root);
            if wav.is_file() {
                if let Ok(mut s) = voiceprints::speakrs_diar::read_wav_f32(&wav) {
                    audio.append(&mut s);
                }
            }
        }
    }
    if audio.is_empty() {
        return Ok((positions.clone(), vec![None; positions.len()], 0));
    }
    let turns = voiceprints::speakrs_diar::diarize_audio(&audio)
        .map_err(|e| AppError::Config(format!("speakrs diarize: {e}")))?;

    // Assign each segment the speakrs label with the most time-overlap.
    let mut label_id: std::collections::HashMap<String, u32> = Default::default();
    let mut assign: Vec<Option<u32>> = vec![None; positions.len()];
    for (pi, (ci, ti, si)) in positions.iter().enumerate() {
        let seg = &st.chunks[*ci].tracks[*ti].segments[*si];
        let a0 = off_s[*ci] + seg.start_ms as f64 / 1000.0;
        let a1 = off_s[*ci] + seg.end_ms as f64 / 1000.0;
        let mut best: Option<(&str, f64)> = None;
        for t in &turns {
            let ov = (a1.min(t.end) - a0.max(t.start)).max(0.0);
            if ov > 0.0 && best.map_or(true, |b| ov > b.1) {
                best = Some((t.speaker.as_str(), ov));
            }
        }
        if let Some((lab, _)) = best {
            let next = label_id.len() as u32;
            let id = *label_id.entry(lab.to_string()).or_insert(next);
            assign[pi] = Some(id);
        }
    }
    // Short/unlabeled inherit nearest preceding; leading inherit first known —
    // same smoothing the k-means `assign_for` applies.
    let mut last: Option<u32> = None;
    for a in assign.iter_mut() {
        if a.is_some() {
            last = *a;
        } else if let Some(l) = last {
            *a = Some(l);
        }
    }
    if let Some(first) = assign.iter().flatten().next().copied() {
        for a in assign.iter_mut() {
            if a.is_none() {
                *a = Some(first);
            }
        }
    }
    let count = assign.iter().flatten().copied().max().map(|m| m + 1).unwrap_or(0);

    // Same-voice merge (unconditional): collapse clusters whose quality-ranked
    // centroids read as the same voice (cosine ≥ SAME_VOICE_MERGE). Runs before
    // the count-to-N merge below.
    if count > 1 {
        if let Ok(mut encoder) = Encoder::load() {
            let cents =
                cluster_quality_centroids(st, manifest, root, select, &positions, &assign, &mut encoder);
            if cents.len() > 1 {
                assign = merge_clusters_by_cosine(&cents, &assign, SAME_VOICE_MERGE);
            }
        }
    }
    let count = assign.iter().flatten().copied().max().map(|m| m + 1).unwrap_or(0);

    // Cluster-to-N merge: when known_count is set and the cluster count exceeds
    // it, the nearest clusters are collapsed by WeSpeaker-centroid cosine until
    // the count matches.
    if let Some(target) = known_count {
        if target >= 1 && (count as usize) > target {
            if let Ok(mut encoder) = Encoder::load() {
                let (_pos, embedded) =
                    collect_track_embeddings(st, manifest, root, select, &mut encoder);
                if !embedded.is_empty() {
                    assign = merge_speakrs_to_n(&embedded, &assign, target);
                }
            }
        }
    }

    let count = assign.iter().flatten().copied().max().map(|m| m + 1).unwrap_or(0);
    Ok((positions, assign, count))
}

/// Collapse an over-split speakrs assignment down to `target` clusters. Builds a
/// centroid per current cluster from the supplied `(position_index, embedding)`
/// pairs, then repeatedly merges the two clusters with the highest centroid
/// cosine (weighted-mean recombine) until `target` remain. Returns the
/// assignment with ids remapped + renumbered 0..k (first-appearance order).
/// Positions whose cluster never got an embedding (all segments too short/silent)
/// ride through unchanged.
fn merge_speakrs_to_n(
    embedded: &[(usize, Vec<f32>)],
    assign: &[Option<u32>],
    target: usize,
) -> Vec<Option<u32>> {
    use std::collections::BTreeMap;
    // Accumulate sum + count per cluster → centroid.
    let mut sums: BTreeMap<u32, (Vec<f32>, usize)> = BTreeMap::new();
    for (pidx, emb) in embedded {
        let Some(Some(cid)) = assign.get(*pidx) else { continue };
        let e = sums.entry(*cid).or_insert_with(|| (vec![0.0; emb.len()], 0));
        for (s, v) in e.0.iter_mut().zip(emb) {
            *s += v;
        }
        e.1 += 1;
    }
    // (cluster_id, centroid, weight). Clusters without any embedding are absent
    // here and survive un-merged via the identity fallback below.
    let mut centroids: Vec<(u32, Vec<f32>, usize)> = sums
        .into_iter()
        .map(|(cid, (sum, n))| (cid, sum.iter().map(|v| v / n as f32).collect(), n))
        .collect();

    // remap[original_id] = surviving_id after merges.
    let mut remap: BTreeMap<u32, u32> = centroids.iter().map(|(c, _, _)| (*c, *c)).collect();
    while centroids.len() > target {
        let mut best: Option<(usize, usize, f32)> = None;
        for i in 0..centroids.len() {
            for j in (i + 1)..centroids.len() {
                let s = cosine(&centroids[i].1, &centroids[j].1);
                if best.map_or(true, |b| s > b.2) {
                    best = Some((i, j, s));
                }
            }
        }
        let Some((i, j, _)) = best else { break };
        let cj = centroids[j].0;
        let ci = centroids[i].0;
        let (ni, nj) = (centroids[i].2, centroids[j].2);
        let merged: Vec<f32> = centroids[i]
            .1
            .iter()
            .zip(&centroids[j].1)
            .map(|(a, b)| (a * ni as f32 + b * nj as f32) / (ni + nj) as f32)
            .collect();
        centroids[i].1 = merged;
        centroids[i].2 = ni + nj;
        centroids.remove(j);
        for v in remap.values_mut() {
            if *v == cj {
                *v = ci;
            }
        }
    }

    // Apply remap + renumber survivors to 0..k by first appearance.
    let mut renum: BTreeMap<u32, u32> = BTreeMap::new();
    let mut next = 0u32;
    let mut out = assign.to_vec();
    for a in out.iter_mut() {
        if let Some(id) = a {
            let mapped = *remap.get(id).unwrap_or(id);
            let nid = *renum.entry(mapped).or_insert_with(|| {
                let n = next;
                next += 1;
                n
            });
            *a = Some(nid);
        }
    }
    out
}

/// Cosine ≥ this between two clusters' quality-ranked centroids → treat them as
/// the same voice and merge.
const SAME_VOICE_MERGE: f32 = 0.55;

/// One quality-ranked centroid per cluster: gather the cluster's loudest,
/// sustained ~30s (the `select_quality_segments` ranking), concatenate, embed
/// once. Clusters with no audible speech are omitted (they ride through the
/// merge unchanged).
fn cluster_quality_centroids(
    st: &transcript::SessionTranscript,
    manifest: &SessionManifest,
    root: &std::path::Path,
    select: DiarizeTrackSelect,
    positions: &[(usize, usize, usize)],
    assign: &[Option<u32>],
    encoder: &mut Encoder,
) -> std::collections::BTreeMap<u32, Vec<f32>> {
    use std::collections::BTreeMap;
    let mut by_cluster: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (pi, a) in assign.iter().enumerate() {
        if let Some(c) = a {
            by_cluster.entry(*c).or_default().push(pi);
        }
    }
    let mut out: BTreeMap<u32, Vec<f32>> = BTreeMap::new();
    for (cid, pidxs) in &by_cluster {
        // Candidate windows for this cluster, probed for loudness.
        let mut wavs: Vec<std::path::PathBuf> = Vec::new();
        let mut starts: Vec<u32> = Vec::new();
        let mut dims: Vec<(f32, u32)> = Vec::new();
        for &pi in pidxs {
            let (ci, ti, si) = positions[pi];
            let seg = &st.chunks[ci].tracks[ti].segments[si];
            let dur = seg.end_ms.saturating_sub(seg.start_ms);
            if dur < 200 {
                continue;
            }
            let chunk_idx = st.chunks[ci].chunk_index;
            let Some(cm) = manifest.chunks.iter().find(|c| c.index == chunk_idx) else {
                continue;
            };
            let wav = select.wav_path(cm, root);
            if !wav.is_file() {
                continue;
            }
            let probe = dur.min(SAMPLE_PROBE_MS);
            let pcm =
                read_pcm_window(&wav, seg.start_ms, seg.start_ms + probe, probe).unwrap_or_default();
            if pcm.is_empty() || pcm_rms_norm(&pcm) < SAMPLE_SILENCE_FLOOR {
                continue;
            }
            dims.push((pcm_rms_norm(&pcm), dur));
            wavs.push(wav);
            starts.push(seg.start_ms);
        }
        let mut audio: Vec<i16> = Vec::new();
        for (i, take) in select_quality_segments(&dims, ENROLL_CAP_MS, SAMPLE_MAX_SEG_MS) {
            let pcm =
                read_pcm_window(&wavs[i], starts[i], starts[i] + take, take).unwrap_or_default();
            audio.extend_from_slice(&pcm);
        }
        if audio.len() >= voiceprints::SAMPLE_RATE as usize / 2 {
            if let Ok(v) = encoder.encode_pcm(&audio) {
                out.insert(*cid, v);
            }
        }
    }
    out
}

/// Greedily merge clusters whose centroids are the same voice: repeatedly fuse
/// the closest pair with cosine ≥ `threshold` until none remain, then renumber
/// survivors 0..k by first appearance. Clusters absent from `centroids` (no
/// audible speech) pass through unchanged.
fn merge_clusters_by_cosine(
    centroids: &std::collections::BTreeMap<u32, Vec<f32>>,
    assign: &[Option<u32>],
    threshold: f32,
) -> Vec<Option<u32>> {
    use std::collections::BTreeMap;
    let mut cents: Vec<(u32, Vec<f32>)> =
        centroids.iter().map(|(k, v)| (*k, v.clone())).collect();
    let mut remap: BTreeMap<u32, u32> = cents.iter().map(|(c, _)| (*c, *c)).collect();
    loop {
        let mut best: Option<(usize, usize, f32)> = None;
        for i in 0..cents.len() {
            for j in (i + 1)..cents.len() {
                let s = cosine(&cents[i].1, &cents[j].1);
                if s >= threshold && best.map_or(true, |b| s > b.2) {
                    best = Some((i, j, s));
                }
            }
        }
        let Some((i, j, _)) = best else { break };
        let cj = cents[j].0;
        let ci = cents[i].0;
        let merged: Vec<f32> = cents[i]
            .1
            .iter()
            .zip(&cents[j].1)
            .map(|(a, b)| (a + b) / 2.0)
            .collect();
        cents[i].1 = merged;
        cents.remove(j);
        for v in remap.values_mut() {
            if *v == cj {
                *v = ci;
            }
        }
    }
    let mut renum: BTreeMap<u32, u32> = BTreeMap::new();
    let mut next = 0u32;
    let mut out = assign.to_vec();
    for a in out.iter_mut() {
        if let Some(id) = a {
            let mapped = *remap.get(id).unwrap_or(id);
            let nid = *renum.entry(mapped).or_insert_with(|| {
                let n = next;
                next += 1;
                n
            });
            *a = Some(nid);
        }
    }
    out
}

/// The diarized speaker clusters in this session that have no manual/auto
/// label yet.
pub fn unlabeled_clusters_impl(app: &AppState, session_id: &str) -> Result<Vec<u32>> {
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    clusters_needing_match(&root)
}

fn clusters_needing_match(session_dir: &std::path::Path) -> Result<Vec<u32>> {
    let st_path = if session_dir.join("transcript.dedup.json").is_file() {
        session_dir.join("transcript.dedup.json")
    } else if session_dir.join("transcript.json").is_file() {
        session_dir.join("transcript.json")
    } else {
        return Ok(Vec::new());
    };
    let st: transcript::SessionTranscript = serde_json::from_slice(&syncsafe::read(&st_path)?)
        .map_err(|e| AppError::Config(format!("parse transcript: {e}")))?;
    let mb = syncsafe::read(session_dir.join("manifest.json")).ok();
    let labeled: std::collections::HashSet<u32> = mb
        .and_then(|b| serde_json::from_slice::<SessionManifest>(&b).ok())
        .map(|m| m.speaker_map.iter().map(|s| s.cluster_id).collect())
        .unwrap_or_default();
    let mut seen: Vec<u32> = Vec::new();
    for ch in &st.chunks {
        for tr in &ch.tracks {
            // Diarized clusters can live on the System, Mic, or MicAec track.
            if !matches!(
                tr.track,
                transcript::Track::System | transcript::Track::Mic | transcript::Track::MicAec
            ) {
                continue;
            }
            for seg in &tr.segments {
                if let Some(sid) = seg.speaker_id {
                    if !labeled.contains(&sid) && !seen.contains(&sid) {
                        seen.push(sid);
                    }
                }
            }
        }
    }
    Ok(seen)
}

fn extract_vector_for_cluster(
    session_dir: &std::path::Path,
    cluster_id: u32,
) -> Result<Vec<f32>> {
    let samples = gather_cluster_pcm(session_dir, cluster_id)?;
    if samples.len() < (voiceprints::SAMPLE_RATE as usize) {
        return Err(AppError::Config(format!(
            "cluster {cluster_id}: not enough speech ({} samples) to enroll",
            samples.len()
        )));
    }
    let mut encoder = Encoder::load()
        .map_err(|e| AppError::Config(format!("voiceprint model: {e}")))?;
    encoder
        .encode_pcm(&samples)
        .map_err(|e| AppError::Config(format!("encode: {e}")))
}

/// Concatenate up to ENROLL_CAP_MS of audio for a given cluster.
fn gather_cluster_pcm(
    session_dir: &std::path::Path,
    cluster_id: u32,
) -> Result<Vec<i16>> {
    gather_cluster_pcm_capped(session_dir, cluster_id, ENROLL_CAP_MS)
}

/// RMS floor (normalized 0..1, ≈ -46 dBFS) below which a PCM window is treated
/// as silence and skipped when gathering a cluster's audio.
const SAMPLE_SILENCE_FLOOR: f32 = 0.005;

/// Short window (ms) probed per segment to estimate its loudness for ranking.
/// The full window is re-read only for segments that make the cut.
const SAMPLE_PROBE_MS: u32 = 2_500;
/// Max audio (ms) any single segment contributes to a gathered sample.
const SAMPLE_MAX_SEG_MS: u32 = 8_000;

/// Pick which candidate segments make up a quality-ranked sample. `segs` is
/// `(rms, dur_ms)` per candidate in chronological order. Ranks by
/// `rms × min(dur, max_seg_ms)`, takes the strongest until `cap_ms` of audio
/// is budgeted, then returns the chosen `(index, take_ms)` in chronological
/// order.
/// Margin around a candidate window inside which other-cluster speech marks
/// it as overlapped, for sample selection.
const SAMPLE_ISOLATION_MARGIN_MS: u32 = 400;

/// Split candidate windows `(start_ms, end_ms)` into (isolated, overlapped)
/// index lists. A candidate is isolated when no interval in `others` comes
/// within `margin_ms` of it.
fn partition_isolated(
    cands: &[(u32, u32)],
    others: &[(u32, u32)],
    margin_ms: u32,
) -> (Vec<usize>, Vec<usize>) {
    let mut iso = Vec::new();
    let mut rest = Vec::new();
    for (i, &(s, e)) in cands.iter().enumerate() {
        let lo = s.saturating_sub(margin_ms);
        let hi = e.saturating_add(margin_ms);
        let clash = others.iter().any(|&(os, oe)| os < hi && oe > lo);
        if clash { rest.push(i) } else { iso.push(i) }
    }
    (iso, rest)
}

fn select_quality_segments(segs: &[(f32, u32)], cap_ms: u32, max_seg_ms: u32) -> Vec<(usize, u32)> {
    let quality = |i: usize| segs[i].0 * segs[i].1.min(max_seg_ms) as f32;
    let mut ranked: Vec<usize> = (0..segs.len()).collect();
    ranked.sort_by(|&a, &b| {
        quality(b)
            .partial_cmp(&quality(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    let mut picked: Vec<(usize, u32)> = Vec::new();
    let mut total = 0u32;
    for i in ranked {
        if total >= cap_ms {
            break;
        }
        let take = segs[i].1.min(cap_ms - total).min(max_seg_ms);
        if take < 200 {
            continue;
        }
        picked.push((i, take));
        total += take;
    }
    picked.sort_by_key(|&(i, _)| i); // chronological output
    picked
}

/// Normalized RMS (0..1) of an i16 PCM window.
fn pcm_rms_norm(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f64 = pcm.iter().map(|&s| (s as f64) * (s as f64)).sum();
    ((sum / pcm.len() as f64).sqrt() / 32768.0) as f32
}

pub(crate) fn gather_cluster_pcm_capped(
    session_dir: &std::path::Path,
    cluster_id: u32,
    cap_ms: u32,
) -> Result<Vec<i16>> {
    Ok(gather_cluster_sample(session_dir, cluster_id, cap_ms)?.0)
}

/// Like `gather_cluster_pcm_capped`, but also returns the transcript text of
/// exactly the segments whose audio was included.
pub(crate) fn gather_cluster_sample(
    session_dir: &std::path::Path,
    cluster_id: u32,
    cap_ms: u32,
) -> Result<(Vec<i16>, String)> {
    let mb = syncsafe::read(session_dir.join("manifest.json"))?;
    let manifest: SessionManifest = serde_json::from_slice(&mb)
        .map_err(|e| AppError::Config(format!("parse manifest: {e}")))?;
    let st_path = if session_dir.join("transcript.dedup.json").is_file() {
        session_dir.join("transcript.dedup.json")
    } else {
        session_dir.join("transcript.json")
    };
    let st: transcript::SessionTranscript = serde_json::from_slice(&syncsafe::read(&st_path)?)
        .map_err(|e| AppError::Config(format!("parse transcript: {e}")))?;

    // Pass 1: gather every candidate segment for this cluster (any track), each
    // probed for loudness over a short window. Near-silent windows are dropped.
    struct Cand {
        wav: std::path::PathBuf,
        start_ms: u32,
        dur: u32,
        rms: f32,
        text: String,
    }
    let mut cands: Vec<Cand> = Vec::new();
    // Other clusters' speech windows, for isolation scoring.
    let mut other_speech: Vec<(u32, u32)> = Vec::new();
    for ch in &st.chunks {
        let Some(cm) = manifest.chunks.iter().find(|c| c.index == ch.chunk_index) else {
            continue;
        };
        for tr in &ch.tracks {
            let wav_rel = match tr.track {
                transcript::Track::Mic => Some(&cm.mic_wav_relative),
                transcript::Track::MicAec => cm.mic_aec_wav_relative.as_ref(),
                transcript::Track::System => Some(&cm.system_wav_relative),
            };
            let Some(wav_rel) = wav_rel else { continue };
            let wav = session_dir.join(wav_rel);
            if !wav.is_file() {
                continue;
            }
            for seg in &tr.segments {
                if seg.speaker_id != Some(cluster_id) {
                    if seg.speaker_id.is_some() {
                        other_speech.push((seg.start_ms, seg.end_ms));
                    }
                    continue;
                }
                let dur = seg.end_ms.saturating_sub(seg.start_ms);
                if dur < 200 {
                    continue;
                }
                let probe = dur.min(SAMPLE_PROBE_MS);
                let pcm = read_pcm_window(&wav, seg.start_ms, seg.start_ms + probe, probe)
                    .unwrap_or_default();
                if pcm.is_empty() || pcm_rms_norm(&pcm) < SAMPLE_SILENCE_FLOOR {
                    continue;
                }
                cands.push(Cand {
                    wav: wav.clone(),
                    start_ms: seg.start_ms,
                    dur,
                    rms: pcm_rms_norm(&pcm),
                    text: seg.text.trim().to_string(),
                });
            }
        }
    }

    // Pass 2: prefer candidates with no other-cluster speech within the
    // isolation margin; overlapped ones fill in only when the isolated set
    // can't reach the cap. Within each set, rank by quality and read the
    // chosen windows in chronological order. Audio and the displayed text
    // are built from the same chosen segments, in order.
    let windows: Vec<(u32, u32)> = cands.iter().map(|c| (c.start_ms, c.start_ms + c.dur)).collect();
    let (iso, rest) = partition_isolated(&windows, &other_speech, SAMPLE_ISOLATION_MARGIN_MS);
    let pick = |idxs: &[usize], budget_ms: u32| -> Vec<(usize, u32)> {
        let dims: Vec<(f32, u32)> = idxs.iter().map(|&i| (cands[i].rms, cands[i].dur)).collect();
        select_quality_segments(&dims, budget_ms, SAMPLE_MAX_SEG_MS)
            .into_iter()
            .map(|(k, take)| (idxs[k], take))
            .collect()
    };
    let mut chosen = pick(&iso, cap_ms);
    let taken: u32 = chosen.iter().map(|&(_, t)| t).sum();
    if taken < cap_ms {
        chosen.extend(pick(&rest, cap_ms - taken));
    }
    chosen.sort_by_key(|&(i, _)| i);

    let mut out: Vec<i16> = Vec::new();
    let mut text = String::new();
    for (i, take) in chosen {
        let c = &cands[i];
        let pcm =
            read_pcm_window(&c.wav, c.start_ms, c.start_ms + take, take).unwrap_or_default();
        out.extend_from_slice(&pcm);
        if !c.text.is_empty() && text.chars().count() < 200 {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&c.text);
        }
    }
    let text: String = text.chars().take(200).collect();
    Ok((out, text))
}

// ---- vault rewrite helper (clones + locks + re-encrypts) -------------------

pub(crate) fn update_vault<F>(app: &AppState, vs: &VaultState, mutate: F) -> Result<()>
where
    F: FnOnce(&mut crate::state::DecryptedKeys) -> Result<()>,
{
    use crate::commands::lifecycle::vault_path;
    use vault::encrypt;

    let mut g = vs.keys.lock().unwrap();
    let keys_mut = g
        .as_mut()
        .ok_or(AppError::VaultLocked)?;
    mutate(keys_mut)?;
    let snapshot = keys_mut.clone();
    drop(g);

    // Re-encrypt the new state under the held passphrase + write envelope.
    let p_guard = vs.passphrase.lock().unwrap();
    let p = p_guard
        .as_ref()
        .ok_or(AppError::VaultLocked)?;
    let plaintext = serde_json::to_vec(&snapshot).map_err(|e| AppError::Config(e.to_string()))?;
    let env = encrypt(&plaintext, p.as_str()).map_err(|e| AppError::Config(e.to_string()))?;
    let bytes = serde_json::to_vec_pretty(&env).map_err(|e| AppError::Config(e.to_string()))?;
    let path = vault_path(app);
    let tmp = path.with_extension("json.tmp");
    syncsafe::write(&tmp, &bytes)?;
    syncsafe::rename(&tmp, &path)?;
    Ok(())
}


#[cfg(test)]
mod select_quality_tests {
    use super::{partition_isolated, select_quality_segments};

    #[test]
    fn picks_loudest_first_returns_chronological() {
        // idx: 0 quiet-long, 1 LOUD-long, 2 loud-short, 3 quiet-short
        let segs = [(0.02, 6000), (0.20, 6000), (0.18, 800), (0.03, 800)];
        // cap 5000: the loud-long seg alone fills it.
        assert_eq!(select_quality_segments(&segs, 5000, 8000), vec![(1, 5000)]);
    }

    #[test]
    fn spreads_to_cap_in_chronological_order() {
        // both strong; per-seg cap 8000. cap 10000 -> 8000 from the stronger
        // (idx1) + 2000 from idx0, emitted chronologically (idx0 first).
        let segs = [(0.10, 9000), (0.20, 9000)];
        assert_eq!(
            select_quality_segments(&segs, 10000, 8000),
            vec![(0, 2000), (1, 8000)]
        );
    }

    #[test]
    fn drops_sub_min_take_and_empty() {
        assert!(select_quality_segments(&[(0.2, 150)], 5000, 8000).is_empty());
        assert!(select_quality_segments(&[], 5000, 8000).is_empty());
    }

    #[test]
    fn loud_blip_loses_to_sustained_speech() {
        // a 0.5s very-loud blip vs 6s of clear speech — sustained should win.
        let segs = [(0.30, 500), (0.15, 6000)];
        assert_eq!(select_quality_segments(&segs, 5000, 8000), vec![(1, 5000)]);
    }

    #[test]
    fn isolation_partition_separates_overlapped_candidates() {
        // Candidates: [0..1000], [2000..3000], [5000..6000].
        // Other-cluster speech at [900..1500] and [5900..7000] touches the
        // first and third; only the middle one is isolated.
        let cands = [(0, 1000), (2000, 3000), (5000, 6000)];
        let others = [(900, 1500), (5900, 7000)];
        let (iso, rest) = partition_isolated(&cands, &others, 0);
        assert_eq!(iso, vec![1]);
        assert_eq!(rest, vec![0, 2]);
    }

    #[test]
    fn isolation_margin_widens_the_exclusion() {
        // Gap of 300 ms to the other cluster: isolated at margin 0,
        // contaminated at margin 400.
        let cands = [(1000, 2000)];
        let others = [(2300, 3000)];
        assert_eq!(partition_isolated(&cands, &others, 0).0, vec![0]);
        assert_eq!(partition_isolated(&cands, &others, 400).0, Vec::<usize>::new());
    }

    #[test]
    fn no_other_speech_means_everything_is_isolated() {
        let cands = [(0, 1000), (2000, 3000)];
        let (iso, rest) = partition_isolated(&cands, &[], 400);
        assert_eq!(iso, vec![0, 1]);
        assert!(rest.is_empty());
    }
}

#[cfg(test)]
mod merge_clusters_by_cosine_tests {
    use super::merge_clusters_by_cosine;
    use std::collections::{BTreeMap, HashSet};

    fn distinct(a: &[Option<u32>]) -> usize {
        a.iter().flatten().collect::<HashSet<_>>().len()
    }

    #[test]
    fn merges_same_voice_keeps_distinct() {
        // clusters 0 & 2 nearly identical (the over-split pair); 1 orthogonal.
        let mut c = BTreeMap::new();
        c.insert(0u32, vec![1.0, 0.0, 0.0]);
        c.insert(1u32, vec![0.0, 1.0, 0.0]);
        c.insert(2u32, vec![0.98, 0.02, 0.0]);
        let out = merge_clusters_by_cosine(&c, &[Some(0), Some(1), Some(2), Some(0)], 0.55);
        assert_eq!(distinct(&out), 2);
        assert_eq!(out[0], out[2], "over-split pair merged");
        assert_ne!(out[0], out[1], "distinct voice stays");
    }

    #[test]
    fn no_merge_below_threshold() {
        let mut c = BTreeMap::new();
        c.insert(0u32, vec![1.0, 0.0]);
        c.insert(1u32, vec![0.0, 1.0]);
        let out = merge_clusters_by_cosine(&c, &[Some(0), Some(1)], 0.55);
        assert_eq!(distinct(&out), 2);
    }

    #[test]
    fn cluster_without_centroid_passes_through() {
        // cluster 1 has no centroid (all-silent) → stays its own cluster.
        let mut c = BTreeMap::new();
        c.insert(0u32, vec![1.0, 0.0]);
        c.insert(2u32, vec![0.99, 0.01]);
        let out = merge_clusters_by_cosine(&c, &[Some(0), Some(1), Some(2)], 0.55);
        assert_eq!(distinct(&out), 2);
        assert_eq!(out[0], out[2]);
        assert_ne!(out[0], out[1]);
    }
}

#[cfg(test)]
mod merge_speakrs_to_n_tests {
    use super::merge_speakrs_to_n;
    use std::collections::HashSet;

    fn distinct(a: &[Option<u32>]) -> usize {
        a.iter().flatten().collect::<HashSet<_>>().len()
    }

    #[test]
    fn collapses_nearest_over_split_pair() {
        // clusters 0 & 1 nearly identical; cluster 2 orthogonal. target=2 → 0&1 merge.
        let assign = vec![Some(0u32), Some(1), Some(2), Some(0)];
        let embedded = vec![
            (0, vec![1.0, 0.0, 0.0]),
            (1, vec![0.99, 0.02, 0.0]),
            (2, vec![0.0, 1.0, 0.0]),
            (3, vec![1.0, 0.0, 0.0]),
        ];
        let out = merge_speakrs_to_n(&embedded, &assign, 2);
        assert_eq!(distinct(&out), 2);
        assert_eq!(out[0], out[1], "over-split pair merged");
        assert_eq!(out[0], out[3]);
        assert_ne!(out[0], out[2], "orthogonal cluster stays separate");
    }

    #[test]
    fn noop_when_already_at_target() {
        let assign = vec![Some(0u32), Some(1)];
        let embedded = vec![(0, vec![1.0, 0.0]), (1, vec![0.0, 1.0])];
        let out = merge_speakrs_to_n(&embedded, &assign, 2);
        assert_eq!(distinct(&out), 2);
    }

    #[test]
    fn merge_to_one_collapses_all() {
        let assign = vec![Some(0u32), Some(1), Some(2)];
        let embedded = vec![
            (0, vec![1.0, 0.0]),
            (1, vec![0.0, 1.0]),
            (2, vec![0.5, 0.5]),
        ];
        let out = merge_speakrs_to_n(&embedded, &assign, 1);
        assert_eq!(distinct(&out), 1);
    }

    #[test]
    fn ids_renumbered_zero_based_contiguous() {
        // start ids {0,2,5}; target 2 → result ids must be {0,1}.
        let assign = vec![Some(0u32), Some(2), Some(5), Some(2)];
        let embedded = vec![
            (0, vec![1.0, 0.0]),
            (1, vec![0.95, 0.05]), // id 2 near id 0 → merge
            (2, vec![0.0, 1.0]),
            (3, vec![0.95, 0.05]),
        ];
        let out = merge_speakrs_to_n(&embedded, &assign, 2);
        let ids: HashSet<u32> = out.iter().flatten().copied().collect();
        assert_eq!(ids, HashSet::from([0, 1]));
    }
}

#[cfg(test)]
mod diarize_track_select_tests {
    use super::*;
    use recording::manifest::ChunkManifest;
    use std::path::{Path, PathBuf};

    #[test]
    fn resolve_scope_explicit_overrides_flag() {
        use DiarizeScope::*;
        // Explicit user choice always wins, regardless of flag/system state.
        for &s in &[Others, Mic, Both] {
            assert_eq!(resolve_scope(Some(s), true, true), s);
            assert_eq!(resolve_scope(Some(s), false, false), s);
        }
    }

    #[test]
    fn resolve_scope_auto_matches_legacy_flag_logic() {
        // None = finalize-time auto: group local end -> Both; solo + empty
        // system -> Mic (in-person fallback); solo + remote -> Others.
        assert_eq!(resolve_scope(None, false, false), DiarizeScope::Both);
        assert_eq!(resolve_scope(None, false, true), DiarizeScope::Both);
        assert_eq!(resolve_scope(None, true, true), DiarizeScope::Mic);
        assert_eq!(resolve_scope(None, true, false), DiarizeScope::Others);
    }

    #[test]
    fn pcm_rms_norm_distinguishes_silence_from_speech() {
        assert_eq!(pcm_rms_norm(&[]), 0.0);
        assert_eq!(pcm_rms_norm(&[0i16; 1000]), 0.0);
        // ~-46 dBFS floor: a flatline well below it, a loud tone well above.
        assert!(pcm_rms_norm(&[50i16; 1000]) < SAMPLE_SILENCE_FLOOR); // ~0.0015
        assert!(pcm_rms_norm(&[3000i16; 1000]) > SAMPLE_SILENCE_FLOOR); // ~0.09
    }

    fn chunk(mic: &str, mic_aec: Option<&str>, sys: &str) -> ChunkManifest {
        ChunkManifest {
            index: 1,
            started_at_unix_seconds: 0,
            ended_at_unix_seconds: Some(0),
            duration_seconds: Some(1),
            mic_wav_relative: PathBuf::from(mic),
            system_wav_relative: PathBuf::from(sys),
            mic_aec_wav_relative: mic_aec.map(PathBuf::from),
            mic_dn_wav_relative: None,
        }
    }

    #[test]
    fn select_system_only_matches_system_track() {
        let s = DiarizeTrackSelect::System;
        assert!(s.matches(transcript::Track::System));
        assert!(!s.matches(transcript::Track::Mic));
        assert!(!s.matches(transcript::Track::MicAec));
    }

    #[test]
    fn select_mic_matches_both_mic_variants() {
        let s = DiarizeTrackSelect::Mic;
        assert!(s.matches(transcript::Track::Mic));
        assert!(s.matches(transcript::Track::MicAec));
        assert!(!s.matches(transcript::Track::System));
    }

    #[test]
    fn mic_path_prefers_aec_when_present() {
        let c = chunk("chunks/0001/mic.wav", Some("chunks/0001/mic.aec.wav"), "chunks/0001/system.wav");
        let p = DiarizeTrackSelect::Mic.wav_path(&c, Path::new("/sess"));
        assert_eq!(p, Path::new("/sess/chunks/0001/mic.aec.wav"));
    }

    #[test]
    fn mic_path_falls_back_to_raw_when_no_aec() {
        let c = chunk("chunks/0001/mic.wav", None, "chunks/0001/system.wav");
        let p = DiarizeTrackSelect::Mic.wav_path(&c, Path::new("/sess"));
        assert_eq!(p, Path::new("/sess/chunks/0001/mic.wav"));
    }

    #[test]
    fn system_path_always_uses_system_wav() {
        let c = chunk("chunks/0001/mic.wav", Some("chunks/0001/mic.aec.wav"), "chunks/0001/system.wav");
        let p = DiarizeTrackSelect::System.wav_path(&c, Path::new("/sess"));
        assert_eq!(p, Path::new("/sess/chunks/0001/system.wav"));
    }
}

#[cfg(test)]
mod clusters_needing_match_tests {
    use super::*;
    use std::io::Write;

    // Session with an unlabeled system cluster (id 0) AND a mic cluster
    // (id 5). With no speaker_map, BOTH ids must be returned.
    fn write_session(dir: &std::path::Path) {
        syncsafe::create_dir_all(dir).unwrap();
        let st = serde_json::json!({
            "schema_version": 1,
            "session_id": "s",
            "provider": "local",
            "model": "whisper",
            "transcribed_at_unix_seconds": 0,
            "chunks": [{
                "chunk_index": 1,
                "tracks": [
                    { "track": "system", "source_wav_relative": "chunks/0001/system.wav", "segments": [
                        { "start_ms": 0, "end_ms": 2000, "text": "a", "confidence": null, "speaker_id": 0 }
                    ]},
                    { "track": "mic_aec", "source_wav_relative": "chunks/0001/mic_aec.wav", "segments": [
                        { "start_ms": 0, "end_ms": 2000, "text": "b", "confidence": null, "speaker_id": 5 }
                    ]}
                ]
            }]
        });
        let mut f = std::fs::File::create(dir.join("transcript.json")).unwrap();
        f.write_all(serde_json::to_string(&st).unwrap().as_bytes()).unwrap();
        // No manifest.json → labeled set is empty.
    }

    #[test]
    fn includes_mic_track_clusters() {
        let tmp = std::env::temp_dir().join(format!("daisy-cnm-{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        write_session(&tmp);
        let mut got = clusters_needing_match(&tmp).unwrap();
        got.sort();
        std::fs::remove_dir_all(&tmp).ok();
        assert_eq!(got, vec![0, 5], "must surface clusters from BOTH system and mic tracks");
    }
}

#[cfg(test)]
mod hybrid_tests {
    use super::*;

    fn side_of(sides: &[ClusterSide], id: u32) -> Option<SpeakerSide> {
        sides.iter().find(|c| c.cluster_id == id).map(|c| c.side)
    }

    #[test]
    fn mic_ids_offset_above_system_and_tagged() {
        // System produced 2 clusters (ids 0,1 → Remote). Mic produced 3 local
        // clusters (0,1,2) → offset to 2,3,4 (Room), disjoint from system ids.
        let (offset, sides) = merge_side_ids(2, 3);
        assert_eq!(offset, 2, "offset = system cluster count");
        assert_eq!(side_of(&sides, 0), Some(SpeakerSide::Remote));
        assert_eq!(side_of(&sides, 1), Some(SpeakerSide::Remote));
        assert_eq!(side_of(&sides, 2), Some(SpeakerSide::Room));
        assert_eq!(side_of(&sides, 3), Some(SpeakerSide::Room));
        assert_eq!(side_of(&sides, 4), Some(SpeakerSide::Room));
        assert_eq!(sides.len(), 5, "every id has exactly one side entry");
    }

    #[test]
    fn empty_system_offset_is_zero() {
        let (offset, sides) = merge_side_ids(0, 2);
        assert_eq!(offset, 0);
        assert_eq!(side_of(&sides, 0), Some(SpeakerSide::Room));
        assert_eq!(side_of(&sides, 1), Some(SpeakerSide::Room));
    }

    #[test]
    fn assign_for_smooths_like_before() {
        // 3 positions, only the middle embedded → its label spreads to neighbours.
        let positions = vec![(0usize, 0usize, 0usize), (0, 0, 1), (0, 0, 2)];
        let embedded = vec![(1usize, vec![1.0f32, 0.0, 0.0])];
        let params = DiarizeParams::default();
        let (assign, count) = assign_for(&positions, &embedded, params, None);
        assert_eq!(count, 1);
        assert_eq!(assign, vec![Some(0), Some(0), Some(0)], "single voice spreads to all segments");
    }
}
