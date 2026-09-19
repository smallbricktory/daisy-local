// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
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
    /// (None / 0 = auto-estimate). `DAISY_DIARIZE_SPEAKERS` overrides when set.
    fn for_call(expected: Option<usize>) -> Self {
        let env_usize = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<usize>().ok());
        let known_count = expected
            .or_else(|| env_usize("DAISY_DIARIZE_SPEAKERS"))
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
    let mut kept_name: Option<String> = None;
    session
        .update_manifest(|m| {
            for s in m.speaker_map.iter_mut() {
                if s.cluster_id == cluster_id && (s.voiceprint_id.is_some() || s.match_confidence.is_some()) {
                    s.voiceprint_id = None;
                    s.match_confidence = None;
                    kept_name = Some(s.display_name.clone());
                    changed = true;
                }
            }
        })
        .map_err(|e| AppError::Recording(e.to_string()))?;
    // The name stays as a typed one, so speakers.json drops its match score too.
    if let Some(name) = kept_name.as_deref() {
        recording::speakers::set_name(&root, cluster_id, Some(name))
            .map_err(|e| AppError::Recording(e.to_string()))?;
    }
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
    /// Enroll even when the cluster's audio reads as more than one voice.
    #[serde(default)]
    pub allow_mixed: bool,
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

    let floor = coherence_floor(
        crate::settings::Settings::load_or_default(&app.profile.settings_path())
            .voice_match_tolerance,
    );
    let vec = extract_vector_for_cluster(&root, req.cluster_id, req.allow_mixed, floor)?;
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
                extra: Default::default(),
            });
        })
        .map_err(|e| AppError::Recording(e.to_string()))?;
    recording::speakers::set_name(&root, cluster_id, Some(&display_name))
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
    let mut scanned = 0u32;
    let mut matched_total = 0u32;
    for path in crate::commands::session_store::session_dirs(&app.profile.sessions_dir()).unwrap_or_default() {
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
    // Shares the diarization permit: this loads the same speaker encoder and
    // runs against the same cores.
    let _diarize = diarize_permit();

    let mut encoder = Encoder::load()
        .map_err(|e| AppError::Config(format!("voiceprint model: {e}")))?;

    // Every (cluster, identity) pair that clears the threshold.
    let mut candidates: Vec<(f32, u32, usize)> = Vec::new();
    for cluster_id in clusters {
        let Ok(samples) = gather_cluster_pcm(&root, cluster_id) else { continue };
        if samples.len() < (voiceprints::SAMPLE_RATE as usize) {
            continue; // under 1s of speech
        }
        let Ok(vec) = encoder.encode_pcm(&samples) else { continue };
        for (vi, vp) in voiceprints.iter().enumerate() {
            // Match score = max cosine over the identity's gallery.
            let s = vp
                .embeddings()
                .into_iter()
                .map(|e| cosine(&vec, e))
                .fold(f32::MIN, f32::max);
            if s >= match_threshold() {
                candidates.push((s, cluster_id, vi));
            }
        }
    }
    let (mut awards, yielded) = award_identities(candidates);
    // A cluster that lost an identity to a closer one is the same person only
    // when its windows read as one voice against the winner's — the same spread
    // test the merge uses. Two speakers alike enough to match one voiceprint
    // fail it and stay unlabeled.
    let mut spread_cache: std::collections::BTreeMap<u32, Vec<Vec<f32>>> = Default::default();
    for (score, cluster_id, vi) in yielded {
        let winner = awards.iter().find(|(_, _, w)| *w == vi).map(|(_, c, _)| *c);
        let sibling = winner.filter(|&w| {
            for cid in [w, cluster_id] {
                spread_cache
                    .entry(cid)
                    .or_insert_with(|| cluster_spread_windows(&root, cid, SPREAD_WINDOWS, &mut encoder));
            }
            let (left, right) = (&spread_cache[&w], &spread_cache[&cluster_id]);
            // Either side without a measurable spread leaves nothing to
            // calibrate against, and the cluster stays unlabeled.
            mean_cosine_within(left).is_some()
                && mean_cosine_within(right).is_some()
                && same_voice_by_spread(left, right)
        });
        if let Some(w) = sibling {
            // Logs carry the voiceprint id, never the person's name.
            log::info!(
                "voiceprint match: session {session_id} cluster {cluster_id} reads as the same voice as cluster {w} and takes vp {} too",
                voiceprints[vi].id
            );
            awards.push((score, cluster_id, vi));
        } else {
            log::info!(
                "voiceprint match: session {session_id} cluster {cluster_id} yields vp {} (cosine {:.3}) to a closer cluster",
                voiceprints[vi].id,
                score
            );
        }
    }

    let mut matched = 0u32;
    for (score, cluster_id, vi) in awards {
        let vp = &voiceprints[vi];
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
        log::info!(
            "voiceprint match: session {session_id} cluster {cluster_id} -> vp {} (cosine {:.3})",
            vp.id,
            score
        );
    }

    Ok(matched)
}

/// Pair clusters with vault identities one-to-one. `candidates` holds every
/// `(score, cluster_id, voiceprint_index)` above the match threshold, in any
/// order. Each cluster is resolved by its own best candidate: it takes
/// that identity when no closer cluster has claimed it, and otherwise stays
/// unlabeled. Returns `(awarded, yielded)`, where `yielded` holds the clusters
/// that lost their identity to a closer one.
///
/// A cluster that loses is left for the user to name rather than dropped to a
/// weaker identity. Ordering guarantees any remaining candidate for it scores
/// below the one it just lost, and naming a speaker after the runner-up is a
/// silent error where an unnamed speaker is a visible one.
type IdentityPair = (f32, u32, usize);
fn award_identities(mut candidates: Vec<IdentityPair>) -> (Vec<IdentityPair>, Vec<IdentityPair>) {
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut awarded = Vec::new();
    let mut yielded = Vec::new();
    let mut cluster_done: std::collections::HashSet<u32> = Default::default();
    let mut vp_taken: std::collections::HashSet<usize> = Default::default();
    for (score, cluster_id, vi) in candidates {
        if !cluster_done.insert(cluster_id) {
            continue;
        }
        if vp_taken.contains(&vi) {
            yielded.push((score, cluster_id, vi));
            continue;
        }
        vp_taken.insert(vi);
        awarded.push((score, cluster_id, vi));
    }
    (awarded, yielded)
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
                extra: Default::default(),
            });
        })
        .map_err(|e| AppError::Recording(e.to_string()))?;
    recording::speakers::set_match(session_dir, cluster_id, &vp.display_name, score)
        .map_err(|e| AppError::Recording(e.to_string()))?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct DiarizeResult {
    /// Number of distinct speakers found in the system track.
    pub speakers: u32,
    /// System segments that got a speaker_id assigned.
    pub segments_labeled: u32,
    /// Transcript segments this run had to work with on the track(s) it read.
    /// Zero means the session has nothing transcribed to group.
    pub segments_seen: u32,
}

/// True if the session's transcript has speech on some track and no speaker
/// labels on any track.
pub fn transcript_undiarized(app: &AppState, session_id: &str) -> bool {
    let root = app.profile.session_path(session_id);
    let Some(st_path) = crate::commands::session_store::preferred_transcript_path(&root) else {
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

/// Spread labels over unlabeled positions: each inherits the nearest
/// preceding labeled speaker, and any leading unlabeled ones inherit the
/// first known speaker.
fn smooth_unlabeled(assign: &mut [Option<u32>]) {
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
    smooth_unlabeled(&mut assign);
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

/// The scope a call clusters. A phone recording is one microphone in a
/// room: the mic track is the only audio and every voice on it is a room
/// voice, so it is the mic whatever was asked for.
fn scope_for(manifest: &SessionManifest, asked: Option<DiarizeScope>) -> Option<DiarizeScope> {
    if manifest.recording_kind.as_deref() == Some("phone_room") {
        Some(DiarizeScope::Mic)
    } else {
        asked
    }
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

/// Speaker ids already assigned on one side of the transcript: the mic tracks
/// when `want_mic`, else the system track.
fn ids_on_track(
    st: &transcript::SessionTranscript,
    want_mic: bool,
) -> std::collections::BTreeSet<u32> {
    let mut ids = std::collections::BTreeSet::new();
    for ch in &st.chunks {
        for tr in &ch.tracks {
            if tr.track.is_mic() != want_mic {
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
}

/// Assigned cluster ids are exactly `0..count`: `apply_scope` iterates
/// `0..count` to build `cluster_sides` and offsets fresh ids by prior counts,
/// so a hole or an id at/above `count` mislabels sides silently.
fn debug_assert_dense_ids(assign: &[Option<u32>], count: u32, what: &str) {
    if cfg!(debug_assertions) {
        let ids: std::collections::BTreeSet<u32> =
            assign.iter().flatten().copied().collect();
        let expect: std::collections::BTreeSet<u32> = (0..count).collect();
        debug_assert_eq!(ids, expect, "{what}: cluster ids not dense 0..{count}");
    }
}

/// Assemble one diarize run's combined output from the already-clustered
/// system track plus per-scope mic clustering. `cluster_mic` runs only for
/// the Both and Mic arms and receives the effective scope, so a backend can
/// vary the mic constraints per arm. Returns `(positions, assignments,
/// cluster_sides, surviving_ids)`; `surviving_ids` are the untouched track's
/// existing cluster ids — fresh ids start above them, and their
/// speaker_map/cluster_sides entries survive the manifest update (empty for
/// Both, which re-clusters everything).
fn apply_scope(
    st: &transcript::SessionTranscript,
    effective: DiarizeScope,
    sys: (Vec<(usize, usize, usize)>, Vec<Option<u32>>, u32),
    cluster_mic: impl FnOnce(
        DiarizeScope,
    ) -> Result<(Vec<(usize, usize, usize)>, Vec<Option<u32>>, u32)>,
) -> Result<(
    Vec<(usize, usize, usize)>,
    Vec<Option<u32>>,
    Vec<ClusterSide>,
    std::collections::BTreeSet<u32>,
)> {
    let (sys_positions, sys_assign, sys_clusters) = sys;
    debug_assert_dense_ids(&sys_assign, sys_clusters, "system");
    let mut cluster_sides: Vec<ClusterSide> = Vec::new();
    let mut surviving_ids: std::collections::BTreeSet<u32> = Default::default();
    let mut all_positions: Vec<(usize, usize, usize)>;
    let all_assign: Vec<Option<u32>>;
    match effective {
        DiarizeScope::Both => {
            // Cluster the mic (room) track too, with ids offset above the
            // system clusters.
            let (mic_positions, mic_assign_local, mic_clusters) =
                cluster_mic(DiarizeScope::Both)?;
            debug_assert_dense_ids(&mic_assign_local, mic_clusters, "mic");
            let (offset, sides) = merge_side_ids(sys_clusters, mic_clusters);
            cluster_sides = sides;
            all_positions = sys_positions;
            let mut assign = sys_assign;
            all_positions.extend_from_slice(&mic_positions);
            assign.extend(mic_assign_local.iter().map(|a| a.map(|id| id + offset)));
            all_assign = assign;
        }
        DiarizeScope::Mic => {
            // Cluster the mic track; clusters are tagged Remote. System-track
            // clusters (if any) survive untouched; fresh ids start above them.
            let (mic_positions, mic_assign, mic_clusters) = cluster_mic(DiarizeScope::Mic)?;
            debug_assert_dense_ids(&mic_assign, mic_clusters, "mic");
            surviving_ids = ids_on_track(st, false);
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
            surviving_ids = ids_on_track(st, true);
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

/// Serializes on-device diarization. Poisoning is ignored: a run that
/// panicked left no state behind that the next one reads.
fn diarize_permit() -> std::sync::MutexGuard<'static, ()> {
    static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GATE.lock().unwrap_or_else(|e| e.into_inner())
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
    let st_path = crate::commands::session_store::preferred_transcript_path(&root)
        .ok_or_else(|| AppError::Config("no transcript to diarize".into()))?;
    let manifest: SessionManifest = serde_json::from_slice(&syncsafe::read(root.join("manifest.json"))?)
        .map_err(|e| AppError::Config(format!("parse manifest: {e}")))?;
    let mut st: transcript::SessionTranscript =
        serde_json::from_slice(&syncsafe::read(&st_path)?)
            .map_err(|e| AppError::Config(format!("parse transcript: {e}")))?;

    // One diarization at a time across the process. Every backend here is
    // on-device — the speaker encoder always, the pyannote pipeline when
    // speakrs is the chosen diarizer — and a second run at the same moment
    // holds a second copy of both while contending for the same cores.
    let _diarize = diarize_permit();

    let mut encoder = Encoder::load()
        .map_err(|e| AppError::Config(format!("voiceprint model: {e}")))?;

    let scope = scope_for(&manifest, scope);

    // What the run has to work with, before any embedding: a session whose
    // transcript holds no segments on the track(s) in scope can only come
    // back empty, and the caller says so instead of blaming the track.
    let segments_seen = segments_in_scope(&st, scope, manifest.single_local_speaker) as u32;

    let params = DiarizeParams::for_call(expected_speakers.map(|n| n as usize));
    let floor = coherence_floor(
        crate::settings::Settings::load_or_default(&app.profile.settings_path())
            .voice_match_tolerance,
    );
    // Diarizer backend = the Settings → Voiceprints choice.
    let use_speakrs = app.machine_settings().diarizer == "speakrs";
    log::info!(
        "diarize {session_id}: backend={} known_count={:?}",
        if use_speakrs { "speakrs (pyannote community-1)" } else { "k-means + silhouette estimate" },
        params.known_count
    );

    let mut all_positions: Vec<(usize, usize, usize)> = Vec::new();
    let mut all_assign: Vec<Option<u32>> = Vec::new();
    let mut cluster_sides: Vec<ClusterSide> = Vec::new();

    // Ids that survive this run (set per scope arm below). On a single-track
    // re-run (scope Mic or Others) the other track's clusters are preserved:
    // fresh ids start above them, and their speaker_map/cluster_sides entries
    // survive the manifest update below.
    let mut surviving_ids: std::collections::BTreeSet<u32> = Default::default();

    // Fast path: exactly 1 known speaker on a solo-local recording labels every
    // system-track segment as that one person, with no embedding or clustering.
    // Runs only when the (implicit or explicit) scope is Others.
    let fast_ok = matches!(scope, None | Some(DiarizeScope::Others));
    if fast_ok && params.known_count == Some(1) && manifest.single_local_speaker {
        let positions = track_segment_positions(&st, DiarizeTrackSelect::System);
        if !positions.is_empty() {
            log::info!("diarize {session_id}: 1-speaker fast path (no embedding)");
            surviving_ids = ids_on_track(&st, true); // mic clusters untouched
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

    // speakrs fills the same all_positions/all_assign/cluster_sides the k-means
    // path below produces; that path then skips (all_positions non-empty).
    // Only speakrs knows where the turns are, so only it fills `turn_spans`;
    // the other paths leave the session sampling segment spans.
    let mut turn_spans: TurnSpans = Default::default();
    if use_speakrs && all_positions.is_empty() {
        // On any speakrs failure, the k-means path below runs.
        match speakrs_diarize(
            &st,
            &manifest,
            &root,
            scope,
            params.known_count,
            &mut encoder,
            floor,
            &mut turn_spans,
        ) {
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
                // all_positions stays empty → the k-means full path below runs,
                // and its clusters own no turns.
                turn_spans.clear();
            }
        }
    }

    // Full path: embed + cluster. (Skipped when the fast path already assigned.)
    if all_positions.is_empty() {
    // System track clustering. On a solo-local recording the mic centroid is
    // passed as the bleed anchor.
    let embed_t = std::time::Instant::now();
    let (sys_positions, sys_embedded) =
        collect_track_embeddings(&st, &manifest, &root, DiarizeTrackSelect::System, &mut encoder);
    log::info!(
        "diarize {session_id}: embedded {} system segment(s) in {:.1}s",
        sys_embedded.len(),
        embed_t.elapsed().as_secs_f32()
    );
    let local_anchor: Option<Vec<f32>> = if manifest.single_local_speaker && !sys_embedded.is_empty() {
        let (_p, mic_embedded) =
            collect_track_embeddings(&st, &manifest, &root, DiarizeTrackSelect::Mic, &mut encoder);
        centroid_of(mic_embedded.iter().map(|(_, v)| v.as_slice()))
    } else {
        None
    };
    let cluster_t = std::time::Instant::now();
    let (sys_assign, sys_clusters) =
        assign_for(&sys_positions, &sys_embedded, params, local_anchor.as_deref());
    log::info!(
        "diarize {session_id}: clustered into {sys_clusters} speaker(s) in {:.1}s",
        cluster_t.elapsed().as_secs_f32()
    );

    let effective = resolve_scope(scope, manifest.single_local_speaker, sys_embedded.is_empty());

    let (positions, assign, sides, surviving) = apply_scope(
        &st,
        effective,
        (sys_positions, sys_assign, sys_clusters),
        |_arm| {
            let (mic_positions, mic_embedded) =
                collect_track_embeddings(&st, &manifest, &root, DiarizeTrackSelect::Mic, &mut encoder);
            let (mic_assign, mic_clusters) = assign_for(&mic_positions, &mic_embedded, params, None);
            Ok((mic_positions, mic_assign, mic_clusters))
        },
    )?;
    all_positions = positions;
    all_assign = assign;
    cluster_sides = sides;
    surviving_ids = surviving;
    } // end full-path (fast path already assigned)

    if all_assign.iter().all(|a| a.is_none()) {
        return Ok(DiarizeResult { speakers: 0, segments_labeled: 0, segments_seen });
    }

    // The windows voice sampling reads for each fresh cluster: the stretch of
    // each of its segments that its own speaker turn covers, grouped under the
    // cluster the run finally settled on.
    let sample_spans: Vec<recording::manifest::ClusterSampleSpans> = {
        let mut by_cluster: std::collections::BTreeMap<u32, Vec<(u32, u32)>> = Default::default();
        for (pidx, pos) in all_positions.iter().enumerate() {
            if let (Some(cid), Some(w)) = (all_assign[pidx], turn_spans.get(pos)) {
                by_cluster.entry(cid).or_default().push(*w);
            }
        }
        by_cluster
            .into_iter()
            .map(|(cluster_id, mut windows)| {
                windows.sort_unstable();
                recording::manifest::ClusterSampleSpans { cluster_id, windows }
            })
            .collect()
    };

    // Persist the per-cluster side tags. Session writes atomically (tmp +
    // rename) and re-reads the current manifest.
    let fresh_ids: Vec<u32> = cluster_sides.iter().map(|c| c.cluster_id).collect();
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
                m.cluster_sample_spans.retain(|c| surviving_ids.contains(&c.cluster_id));
                m.cluster_sample_spans.extend(sample_spans);
                m.speaker_map.retain(|s| surviving_ids.contains(&s.cluster_id));
                // A successful re-process clears the "Interrupted — recovered"
                // badge.
                m.interrupted = false;
            })
            .map_err(|e| AppError::Recording(e.to_string()))?;
        recording::speakers::retain(&root, &surviving_ids.iter().copied().collect::<Vec<_>>()).map_err(|e| AppError::Recording(e.to_string()))?;
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

    syncsafe::write_atomic(&st_path, serde_json::to_vec_pretty(&st)?)?;

    // The transcript on disk now carries this run's speaker ids, so each
    // fresh cluster's enrollment sample can be judged for a second voice.
    // Separation is the session's own scale, so it reads every cluster the
    // session now holds — the untouched track's included. Fresh ids start
    // above the surviving ones, so the two sets never overlap.
    let judged: Vec<u32> = surviving_ids.iter().copied().chain(fresh_ids.iter().copied()).collect();
    let sampled: Vec<(u32, Vec<Vec<f32>>)> = judged
        .iter()
        .map(|&cid| (cid, cluster_sample_windows(&root, cid, &mut encoder)))
        .collect();
    let separation = speaker_separation(
        &sampled.iter().map(|(_, w)| w.clone()).collect::<Vec<_>>(),
    );
    let mixed: Vec<u32> = sampled
        .iter()
        .filter(|(cid, w)| fresh_ids.contains(cid) && cluster_reads_mixed(*cid, w, floor, separation))
        .map(|(cid, _)| *cid)
        .collect();
    if !mixed.is_empty() {
        log::info!("diarize {session_id}: cluster(s) {mixed:?} do not read as one clear voice");
    }
    {
        use recording::session::Session;
        let mut session = Session::load(&root).map_err(|e| AppError::Recording(e.to_string()))?;
        session
            .update_manifest(|m| {
                m.mixed_clusters.retain(|c| surviving_ids.contains(c));
                m.mixed_clusters.extend(mixed.iter().copied());
                m.speaker_separation = separation;
            })
            .map_err(|e| AppError::Recording(e.to_string()))?;
    }

    // Re-render transcript.md with the fresh speaker labels. Best-effort.
    if let Err(e) = crate::commands::session::rerender_session_transcript_impl(app, session_id) {
        log::warn!("rerender after diarize for {session_id}: {e}");
    }
    Ok(DiarizeResult { speakers, segments_labeled, segments_seen })
}

/// Transcript segments on the track(s) a run with this scope will read.
fn segments_in_scope(
    st: &transcript::SessionTranscript,
    scope: Option<DiarizeScope>,
    single_local: bool,
) -> usize {
    let sys = track_segment_positions(st, DiarizeTrackSelect::System).len();
    let mic = track_segment_positions(st, DiarizeTrackSelect::Mic).len();
    match resolve_scope(scope, single_local, sys == 0) {
        DiarizeScope::Mic => mic,
        DiarizeScope::Others => sys,
        DiarizeScope::Both => sys + mic,
    }
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
            DiarizeTrackSelect::Mic => t.is_mic(),
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
    encoder: &mut Encoder,
    floor: f32,
    spans: &mut TurnSpans,
) -> Result<(
    Vec<(usize, usize, usize)>,
    Vec<Option<u32>>,
    Vec<ClusterSide>,
    std::collections::BTreeSet<u32>,
)> {
    // known_count forces speakrs to that many clusters. It applies to the system
    // track, and to the mic only when the mic is the sole scope; in Both the
    // mic is unconstrained.
    let sys = speakrs_cluster_track(
        st, manifest, root, DiarizeTrackSelect::System, known_count, encoder, floor, spans,
    )?;
    let effective = resolve_scope(scope, manifest.single_local_speaker, sys.0.is_empty());
    apply_scope(st, effective, sys, |arm| {
        let kc = if arm == DiarizeScope::Mic { known_count } else { None };
        speakrs_cluster_track(
            st, manifest, root, DiarizeTrackSelect::Mic, kc, encoder, floor, spans,
        )
    })
}

/// Session-absolute `(start_ms, end_ms)` of each segment's turn-bounded voice
/// sample window, keyed by its `(chunk, track, segment)` position.
type TurnSpans = std::collections::HashMap<(usize, usize, usize), (u32, u32)>;

/// speakrs-backed clustering for one track. Concatenates the track's per-chunk
/// WAVs, runs speakrs once, and assigns each segment the speakrs label with
/// the most time-overlap. Returns the same `(positions, per-position
/// assignment, count)` shape as the k-means `collect_track_embeddings` +
/// `assign_for` pair, and records each segment's turn-bounded window in
/// `spans`.
fn speakrs_cluster_track(
    st: &transcript::SessionTranscript,
    manifest: &SessionManifest,
    root: &std::path::Path,
    select: DiarizeTrackSelect,
    known_count: Option<usize>,
    encoder: &mut Encoder,
    floor: f32,
    spans: &mut TurnSpans,
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
            match transcript::wav_io::read_wav_mono_f32(&wav) {
                Ok((mut s, _)) => audio.append(&mut s),
                // A missing/undecodable chunk is replaced with silence for
                // its manifest duration so later chunks' offsets stay
                // aligned; with no known duration the chunk is skipped.
                Err(e) => match cm.duration_seconds {
                    Some(secs) => {
                        log::warn!(
                            "speakrs: chunk {} wav unreadable ({e}); padding {secs}s of silence",
                            cm.index
                        );
                        let pad = (secs as usize) * voiceprints::SAMPLE_RATE as usize;
                        audio.resize(audio.len() + pad, 0.0);
                    }
                    None => log::warn!(
                        "speakrs: chunk {} wav unreadable ({e}); skipping (duration unknown)",
                        cm.index
                    ),
                },
            }
        }
    }
    if audio.is_empty() {
        return Ok((positions.clone(), vec![None; positions.len()], 0));
    }
    let turns = voiceprints::speakrs_diar::diarize_audio(&audio, known_count)
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
    smooth_unlabeled(&mut assign);
    let count = assign.iter().flatten().copied().max().map(|m| m + 1).unwrap_or(0);

    // The part of each segment its own speaker holds. Both cluster hygiene and
    // the voice sample read these rather than whole segments.
    let id_label: std::collections::HashMap<u32, &str> =
        label_id.iter().map(|(lab, id)| (*id, lab.as_str())).collect();
    let sample_windows: Vec<Option<(u32, u32, bool)>> = positions
        .iter()
        .zip(&assign)
        .map(|((ci, ti, si), id)| {
            let seg = &st.chunks[*ci].tracks[*ti].segments[*si];
            let label = id_label.get(&(*id)?)?;
            let (own, others): (Vec<_>, Vec<_>) = turns
                .iter()
                .map(|t| (t.speaker == **label, (t.start, t.end)))
                .partition(|(mine, _)| *mine);
            let own: Vec<(f64, f64)> = own.into_iter().map(|(_, span)| span).collect();
            let others: Vec<(f64, f64)> = others.into_iter().map(|(_, span)| span).collect();
            turn_bounded_window(
                off_s[*ci] + seg.start_ms as f64 / 1000.0,
                off_s[*ci] + seg.end_ms as f64 / 1000.0,
                &own,
                &others,
                off_s[*ci],
            )
        })
        .collect();

    // The windows the voice sample will read, in the frame a session on disk
    // uses: chunk-relative from `turn_bounded_window`, re-based on the
    // manifest's own chunk offsets.
    let offsets = manifest.chunk_offsets_ms();
    for (pi, pos) in positions.iter().enumerate() {
        let Some((start_ms, dur_ms, _)) = sample_windows[pi] else { continue };
        let base = offsets.get(&st.chunks[pos.0].chunk_index).copied().unwrap_or(0);
        let abs = base.saturating_add(start_ms);
        spans.insert(*pos, (abs, abs.saturating_add(dur_ms)));
    }

    // Cluster hygiene: pull apart any cluster carrying two voices, then
    // collapse the ones that read as the same voice. A cluster holding two
    // people taints the voiceprint enrolled from it for good, while an
    // over-split speaker is rejoined here, so the split runs first and the
    // merge cannot undo it — a surviving split sits below SAME_VOICE_MERGE by
    // construction. The merge stops at `known_count`; a split that clears the
    // gate may take the session above it, and the count mismatch is reported
    // to the user.
    if count > 1 {
        let by_cluster =
            cluster_windows(st, manifest, root, select, &positions, &assign, &sample_windows);
        let split = split_fused_clusters(&by_cluster, &mut assign, encoder, floor);
        // A split rewrites cluster membership, so the merge reads profiles
        // gathered from the clusters it will actually see.
        let by_cluster = if split > 0 {
            log::info!("speakrs {select:?}: split {split} fused cluster(s) apart");
            cluster_windows(st, manifest, root, select, &positions, &assign, &sample_windows)
        } else {
            by_cluster
        };
        let (cents, spread) = cluster_voice_profiles(&by_cluster, encoder);
        if cents.len() > 1 {
            assign = merge_clusters_by_cosine(
                &cents,
                &spread,
                &assign,
                SAME_VOICE_MERGE,
                known_count.unwrap_or(1),
            );
        }
    }

    let count = assign.iter().flatten().copied().max().map(|m| m + 1).unwrap_or(0);
    if let Some(target) = known_count.filter(|t| *t != count as usize) {
        log::warn!(
            "speakrs {select:?}: asked for {target} speaker(s), grouped into {count} — \
             the audio separates into {count}"
        );
    }
    Ok((positions, assign, count))
}

/// Cosine ≥ this between two clusters' quality-ranked centroids makes the pair
/// a merge candidate.
const SAME_VOICE_MERGE: f32 = 0.55;

/// A candidate pair merges only when its cross-cluster window cosine comes
/// within this margin of the two clusters' own mean window spread.
const SAME_VOICE_SPREAD_MARGIN: f32 = 0.15;

/// Windows per cluster used to measure that spread.
const SPREAD_WINDOWS: usize = 8;

/// Shortest window that carries a usable spread measurement.
const SPREAD_WINDOW_MIN_MS: u32 = 1_000;

/// Pulled off each end of a turn-bounded window, so an approximate turn
/// boundary doesn't leak the neighbouring speaker into the sample.
const TURN_EDGE_GUARD_MS: f64 = 0.2;

/// Shortest turn-bounded window worth sampling.
const TURN_WINDOW_MIN_MS: f64 = 0.4;

/// Another speaker talking over this much of a window marks it overlapped.
/// Turns that overlap in time can't be trimmed apart.
const TURN_OVERLAP_REJECT_MS: f64 = 0.2;

/// The part of a segment its own speaker actually holds, chunk-relative. An ASR
/// segment is a transcription window, not a speaker turn: it can run straight
/// through a speaker change, and sampling the whole of it feeds the other
/// person's voice into this cluster's embedding. Takes the longest stretch of
/// `[seg_start, seg_end]` (session-absolute seconds) covered by one of `own`,
/// pulls both ends in by `TURN_EDGE_GUARD_MS`, and returns it as
/// `(start_ms, dur_ms)` relative to `chunk_offset`. `None` when nothing of
/// usable length survives. The flag is false when a turn in `others` talks over
/// the result — overlapping turns occupy the same seconds and no trim separates
/// them, so such a window is fit for a centroid but not for measuring spread.
fn turn_bounded_window(
    seg_start: f64,
    seg_end: f64,
    own: &[(f64, f64)],
    others: &[(f64, f64)],
    chunk_offset: f64,
) -> Option<(u32, u32, bool)> {
    let (mut start, mut end) = own
        .iter()
        .map(|&(ts, te)| (seg_start.max(ts), seg_end.min(te)))
        .filter(|(s, e)| e > s)
        .max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0)))?;
    if end - start - 2.0 * TURN_EDGE_GUARD_MS >= TURN_WINDOW_MIN_MS {
        start += TURN_EDGE_GUARD_MS;
        end -= TURN_EDGE_GUARD_MS;
    }
    if end - start < TURN_WINDOW_MIN_MS {
        return None;
    }
    let over: f64 = others
        .iter()
        .map(|&(os, oe)| (end.min(oe) - start.max(os)).max(0.0))
        .sum();
    let start_ms = ((start - chunk_offset) * 1000.0).round().max(0.0) as u32;
    let dur_ms = ((end - start) * 1000.0).round() as u32;
    Some((start_ms, dur_ms, over < TURN_OVERLAP_REJECT_MS))
}

/// One candidate window of a cluster: the turn-bounded stretch of a segment
/// its own speaker holds, with the loudness dimensions used to rank it.
struct ClusterWindow {
    /// Index into `positions` — what a split reassigns.
    pos: usize,
    wav: std::path::PathBuf,
    start_ms: u32,
    dur_ms: u32,
    /// False when another turn talks over the window.
    clean: bool,
    rms: f32,
    voiced_ms: u32,
    /// Speech floor of the wav this window came from.
    floor: f32,
}

/// Every cluster's candidate windows, loudest first, probed for speech and
/// stripped of sparse ones. Shared by the split pass and the same-voice merge
/// so both read the same audio.
fn cluster_windows(
    st: &transcript::SessionTranscript,
    manifest: &SessionManifest,
    root: &std::path::Path,
    select: DiarizeTrackSelect,
    positions: &[(usize, usize, usize)],
    assign: &[Option<u32>],
    sample_windows: &[Option<(u32, u32, bool)>],
) -> std::collections::BTreeMap<u32, Vec<ClusterWindow>> {
    use std::collections::BTreeMap;
    let mut by_cluster: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (pi, a) in assign.iter().enumerate() {
        if let Some(c) = a {
            by_cluster.entry(*c).or_default().push(pi);
        }
    }
    let mut out: BTreeMap<u32, Vec<ClusterWindow>> = BTreeMap::new();
    let mut floors: std::collections::HashMap<std::path::PathBuf, f32> = Default::default();
    for (cid, pidxs) in &by_cluster {
        let mut cands: Vec<ClusterWindow> = Vec::new();
        for &pi in pidxs {
            let (ci, _, _) = positions[pi];
            let Some((start_ms, dur_ms, clean)) = sample_windows[pi] else {
                continue;
            };
            if dur_ms < 200 {
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
            // Same discipline as the sample clip: sparse windows are
            // rejected and ranking runs on the compacted speech, so a
            // centroid is never diluted by dead air.
            let floor = *floors.entry(wav.clone()).or_insert_with(|| voiced_floor(&wav));
            let Some((voiced, rms)) = voiced_sample(&wav, start_ms, dur_ms, floor) else {
                continue;
            };
            let voiced_ms = pcm_len_ms(&voiced);
            if voiced_ms < 200 {
                continue;
            }
            cands.push(ClusterWindow { pos: pi, wav, start_ms, dur_ms, clean, rms, voiced_ms, floor });
        }
        out.insert(*cid, cands);
    }
    out
}

/// Per cluster, the two things the same-voice merge reads: one quality-ranked
/// centroid (the cluster's loudest, sustained ~30s under the
/// `select_quality_segments` ranking, concatenated and embedded once) and up to
/// `SPREAD_WINDOWS` separately-embedded loud windows, whose mutual cosine is
/// that voice's own spread under this session's capture conditions. Clusters
/// with no audible speech are omitted from both (they ride through the merge
/// unchanged).
fn cluster_voice_profiles(
    windows_by_cluster: &std::collections::BTreeMap<u32, Vec<ClusterWindow>>,
    encoder: &mut Encoder,
) -> (
    std::collections::BTreeMap<u32, Vec<f32>>,
    std::collections::BTreeMap<u32, Vec<Vec<f32>>>,
) {
    use std::collections::BTreeMap;
    let mut out: BTreeMap<u32, Vec<f32>> = BTreeMap::new();
    let mut spread: BTreeMap<u32, Vec<Vec<f32>>> = BTreeMap::new();
    for (cid, cands) in windows_by_cluster {
        let dims: Vec<(f32, u32)> = cands.iter().map(|c| (c.rms, c.voiced_ms)).collect();
        // The centroid takes uncontested windows first and fills the remaining
        // budget from overlapped ones, so a cluster with little clean speech
        // still gets a centroid rather than dropping out of the merge.
        let pick = |idxs: &[usize], budget: u32| -> Vec<(usize, u32)> {
            let sub: Vec<(f32, u32)> = idxs.iter().map(|&i| dims[i]).collect();
            select_quality_segments(&sub, budget, SAMPLE_MAX_SEG_MS)
                .into_iter()
                .map(|(k, take)| (idxs[k], take))
                .collect()
        };
        let uncontested: Vec<usize> = (0..cands.len()).filter(|&i| cands[i].clean).collect();
        let overlapped: Vec<usize> = (0..cands.len()).filter(|&i| !cands[i].clean).collect();
        let mut chosen = pick(&uncontested, ENROLL_CAP_MS);
        let taken: u32 = chosen.iter().map(|&(_, t)| t).sum();
        if taken < ENROLL_CAP_MS {
            chosen.extend(pick(&overlapped, ENROLL_CAP_MS - taken));
        }
        chosen.sort_by_key(|&(i, _)| i);
        let mut audio: Vec<i16> = Vec::new();
        for (i, take) in chosen {
            let Some((voiced, _)) = voiced_sample(&cands[i].wav, cands[i].start_ms, cands[i].dur_ms, cands[i].floor)
            else {
                continue;
            };
            let n = (take as usize * voiceprints::SAMPLE_RATE as usize / 1000).min(voiced.len());
            audio.extend_from_slice(&voiced[..n]);
        }
        if audio.len() >= voiceprints::SAMPLE_RATE as usize / 2 {
            if let Ok(v) = encoder.encode_pcm(&audio) {
                out.insert(*cid, v);
            }
        }
        // The spread windows: the loudest sustained windows nobody talks over,
        // embedded one by one rather than concatenated. Fewer than two leaves
        // the spread unmeasurable and the centroid cosine decides alone.
        let mut windows: Vec<Vec<f32>> = Vec::new();
        for i in loudest_clean(cands, SPREAD_WINDOWS) {
            let Some((voiced, _)) = voiced_sample(&cands[i].wav, cands[i].start_ms, cands[i].dur_ms, cands[i].floor)
            else {
                continue;
            };
            if let Ok(v) = encoder.encode_pcm(&voiced) {
                windows.push(v);
            }
        }
        if windows.len() > 1 {
            spread.insert(*cid, windows);
        }
    }
    (out, spread)
}

/// The `cap` loudest sustained windows nobody talks over, as indices into
/// `cands`, loudest first.
fn loudest_clean(cands: &[ClusterWindow], cap: usize) -> Vec<usize> {
    let mut ranked: Vec<(f32, usize)> = (0..cands.len())
        .filter(|&i| cands[i].clean && cands[i].voiced_ms >= SPREAD_WINDOW_MIN_MS)
        .map(|i| (cands[i].rms * cands[i].voiced_ms as f32, i))
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked.into_iter().take(cap).map(|(_, i)| i).collect()
}

/// Greedily merge clusters that read as the same voice: repeatedly fuse the
/// closest pair whose centroid cosine is ≥ `threshold` **and** whose
/// cross-cluster window cosine reaches the clusters' own window spread
/// (`same_voice_by_spread`), until none remain or `min_clusters` are left, then
/// renumber survivors 0..k by first appearance. Clusters absent from
/// `centroids` (no audible speech) pass through unchanged; clusters absent from
/// `windows` are decided by the centroid cosine alone.
fn merge_clusters_by_cosine(
    centroids: &std::collections::BTreeMap<u32, Vec<f32>>,
    windows: &std::collections::BTreeMap<u32, Vec<Vec<f32>>>,
    assign: &[Option<u32>],
    threshold: f32,
    min_clusters: usize,
) -> Vec<Option<u32>> {
    use std::collections::BTreeMap;
    let mut cents: Vec<(u32, Vec<f32>, Vec<Vec<f32>>)> = centroids
        .iter()
        .map(|(k, v)| (*k, v.clone(), windows.get(k).cloned().unwrap_or_default()))
        .collect();
    let mut remap: BTreeMap<u32, u32> = cents.iter().map(|(c, _, _)| (*c, *c)).collect();
    while cents.len() > min_clusters.max(1) {
        let mut best: Option<(usize, usize, f32)> = None;
        for i in 0..cents.len() {
            for j in (i + 1)..cents.len() {
                let s = cosine(&cents[i].1, &cents[j].1);
                if s >= threshold
                    && best.map_or(true, |b| s > b.2)
                    && same_voice_by_spread(&cents[i].2, &cents[j].2)
                {
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
        let moved = std::mem::take(&mut cents[j].2);
        cents[i].2.extend(moved);
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

/// A split-test window is cut into pieces of about this length. A speaker
/// fused into a cluster can hold a single long segment, and one window
/// carries no spread of its own to judge.
const SPLIT_SUBWINDOW_MS: u32 = 3_000;

/// Ceiling on split-test embeddings per cluster, after sub-windowing.
const SPLIT_EMBED_CAP: usize = 32;

/// Windows embedded per cluster for the split test. Above the merge's
/// `SPREAD_WINDOWS` because a speaker fused into a cluster can hold a small
/// share of it.
const SPLIT_WINDOWS: usize = 24;

/// Pull every cluster carrying two voices apart, in place. Embeds each
/// cluster's loudest uncontested windows, splits on `split_two_voices`, and
/// re-tests each side until nothing more separates; positions with no window of
/// their own stay with the side that keeps the cluster id. Returns the number
/// of new clusters created.
fn split_fused_clusters(
    windows_by_cluster: &std::collections::BTreeMap<u32, Vec<ClusterWindow>>,
    assign: &mut [Option<u32>],
    encoder: &mut Encoder,
    floor: f32,
) -> usize {
    let mut next_id = assign.iter().flatten().copied().max().map_or(0, |m| m + 1);
    let created_from = next_id;
    let mut by_cluster: Vec<(u32, Vec<(usize, Vec<f32>)>)> = Vec::new();
    for (cid, cands) in windows_by_cluster {
        let mut embedded: Vec<(usize, Vec<f32>)> = Vec::new();
        for i in loudest_clean(cands, SPLIT_WINDOWS) {
            let Some((voiced, _)) = voiced_sample(&cands[i].wav, cands[i].start_ms, cands[i].dur_ms, cands[i].floor)
            else {
                continue;
            };
            for piece in subwindows(&voiced, SPLIT_SUBWINDOW_MS) {
                if embedded.len() >= SPLIT_EMBED_CAP {
                    break;
                }
                if let Ok(v) = encoder.encode_pcm(piece) {
                    embedded.push((cands[i].pos, v));
                }
            }
        }
        by_cluster.push((*cid, embedded));
    }
    let sets: Vec<Vec<Vec<f32>>> = by_cluster
        .iter()
        .map(|(_, e)| e.iter().map(|(_, v)| v.clone()).collect())
        .collect();
    let separation = speaker_separation(&sets);
    for (cid, embedded) in by_cluster {
        // Each pass peels one voice off; what is left is re-tested until it
        // reads as one person.
        let mut rest = embedded;
        loop {
            let vecs: Vec<Vec<f32>> = rest.iter().map(|(_, v)| v.clone()).collect();
            let Some((left, right)) = split_two_voices(&vecs, floor, separation) else { break };
            let peeled: Vec<usize> = right.iter().map(|&i| rest[i].0).collect();
            for pos in &peeled {
                assign[*pos] = Some(next_id);
            }
            log::info!(
                "diarize: cluster {cid} carried a second voice; {} window(s) moved to cluster {next_id}",
                peeled.len()
            );
            next_id += 1;
            rest = left.into_iter().map(|i| rest[i].clone()).collect();
        }
    }
    (next_id - created_from) as usize
}

/// A split side holding fewer windows than this carries no spread of its own
/// to judge, so it cannot be told from noise.
/// The coherence a set of ~3s sub-windows must reach to read as one person,
/// from `Settings::voice_match_tolerance`. 0 tolerance demands identical
/// windows; 100 accepts any.
pub fn coherence_floor(tolerance: u8) -> f32 {
    (100.0 - tolerance.min(100) as f32) / 100.0
}

/// The floor at the default tolerance, so a test states a window set's verdict
/// without restating the mapping.
#[cfg(test)]
const ENROLLABLE_COHERENCE: f32 = 0.35;

const SPLIT_MIN_WINDOWS: usize = 2;

/// 2-means passes over a cluster's windows. The seeded assignment settles well
/// inside this.
const SPLIT_ITERATIONS: usize = 10;

/// Cut a window into equal pieces of about `target_ms`, dropping anything that
/// would fall under `SPREAD_WINDOW_MIN_MS`. A window at or under the target is
/// returned whole.
fn subwindows(pcm: &[i16], target_ms: u32) -> Vec<&[i16]> {
    let per_ms = voiceprints::SAMPLE_RATE as usize / 1000;
    let min = SPREAD_WINDOW_MIN_MS as usize * per_ms;
    if pcm.len() < min {
        return Vec::new();
    }
    let n = (pcm.len() / (target_ms as usize * per_ms)).max(1);
    let step = pcm.len() / n;
    if step < min {
        return vec![pcm];
    }
    (0..n)
        .map(|k| {
            let end = if k + 1 == n { pcm.len() } else { (k + 1) * step };
            &pcm[k * step..end]
        })
        .collect()
}

/// How far apart two different people sit in this session, as the largest mean
/// window cosine between any two clusters, or `None` below two clusters that
/// each carry windows. Capture conditions set this scale: a phone on a table
/// across a room puts every voice closer together than a headset does.
fn speaker_separation(sets: &[Vec<Vec<f32>>]) -> Option<f32> {
    let mut widest: Option<f32> = None;
    for i in 0..sets.len() {
        for j in (i + 1)..sets.len() {
            if let Some(c) = mean_cosine_across(&sets[i], &sets[j]) {
                widest = Some(widest.map_or(c, |w: f32| w.max(c)));
            }
        }
    }
    widest
}

/// A split's two sides may sit this much closer together than the session's
/// two most alike speakers and still read as two people.
const SPEAKER_SEPARATION_SLACK: f32 = 0.05;

/// Two voices inside one cluster, as `(left, right)` window indices, or `None`
/// when the windows read as one person. Seeds 2-means on the least-similar
/// pair, then keeps the split only when each side holds at least
/// `SPLIT_MIN_WINDOWS` windows, each side is itself as coherent as a merge
/// candidate must be, the two sides sit no closer than two different people do
/// in this session, and their centroids sit below the merge threshold — a
/// split the merge would undo is not a split. A cluster carrying two speakers
/// is unrecoverable damage — its enrolled voiceprint averages both — while an
/// over-split speaker is rejoined by the merge, so the gate leans toward
/// splitting.
fn split_two_voices(
    windows: &[Vec<f32>],
    floor: f32,
    separation: Option<f32>,
) -> Option<(Vec<usize>, Vec<usize>)> {
    let (left, right) = two_means_partition(windows)?;
    let vecs = |idx: &[usize]| -> Vec<Vec<f32>> { idx.iter().map(|&i| windows[i].clone()).collect() };
    let (lv, rv) = (vecs(&left), vecs(&right));
    let coherent = |v: &[Vec<f32>]| mean_cosine_within(v).is_some_and(|c| c >= floor);
    if !coherent(&lv) || !coherent(&rv) {
        return None;
    }
    if let Some(sep) = separation {
        let across = mean_cosine_across(&lv, &rv)?;
        if across > sep + SPEAKER_SEPARATION_SLACK {
            return None;
        }
    }
    let (Some(lc), Some(rc)) = (
        centroid_of(lv.iter().map(|v| v.as_slice())),
        centroid_of(rv.iter().map(|v| v.as_slice())),
    ) else {
        return None;
    };
    if cosine(&lc, &rc) >= SAME_VOICE_MERGE {
        return None;
    }
    Some((left, right))
}

/// Seeded 2-means over `windows`, as `(left, right)` indices. `None` when
/// there are too few windows to judge or a side falls under
/// `SPLIT_MIN_WINDOWS`. Says nothing about whether the two sides are different
/// voices — that is `split_two_voices`'s gates.
fn two_means_partition(windows: &[Vec<f32>]) -> Option<(Vec<usize>, Vec<usize>)> {
    if windows.len() < SPLIT_MIN_WINDOWS * 2 {
        return None;
    }
    let mut seed = (0usize, 1usize, f32::MAX);
    for i in 0..windows.len() {
        for j in (i + 1)..windows.len() {
            let c = cosine(&windows[i], &windows[j]);
            if c < seed.2 {
                seed = (i, j, c);
            }
        }
    }
    let mut left_of = windows[seed.0].clone();
    let mut right_of = windows[seed.1].clone();
    let mut sides: Vec<bool> = Vec::new();
    for _ in 0..SPLIT_ITERATIONS {
        let next: Vec<bool> = windows
            .iter()
            .map(|w| cosine(w, &right_of) > cosine(w, &left_of))
            .collect();
        if next == sides {
            break;
        }
        sides = next;
        let pick = |right: bool| -> Option<Vec<f32>> {
            centroid_of(
                windows
                    .iter()
                    .zip(&sides)
                    .filter(|(_, s)| **s == right)
                    .map(|(w, _)| w.as_slice()),
            )
        };
        let (Some(l), Some(r)) = (pick(false), pick(true)) else {
            return None;
        };
        left_of = l;
        right_of = r;
    }
    let left: Vec<usize> = (0..windows.len()).filter(|&i| !sides[i]).collect();
    let right: Vec<usize> = (0..windows.len()).filter(|&i| sides[i]).collect();
    if left.len() < SPLIT_MIN_WINDOWS || right.len() < SPLIT_MIN_WINDOWS {
        return None;
    }
    Some((left, right))
}

/// Embeddings of a gathered sample's ~3s pieces — the windows the one-voice
/// check judges.
fn sample_subwindow_embeddings(samples: &[i16], encoder: &mut Encoder) -> Vec<Vec<f32>> {
    subwindows(samples, SPLIT_SUBWINDOW_MS)
        .into_iter()
        .filter_map(|piece| encoder.encode_pcm(piece).ok())
        .collect()
}

/// This cluster's enrollment sample, cut into the ~3s pieces the one-voice
/// check judges. Empty when there is too little audio to judge.
fn cluster_sample_windows(
    session_dir: &std::path::Path,
    cluster_id: u32,
    encoder: &mut Encoder,
) -> Vec<Vec<f32>> {
    let Ok((samples, _)) = gather_cluster_sample(session_dir, cluster_id, ENROLL_CAP_MS, true)
    else {
        return Vec::new();
    };
    if samples.len() < (voiceprints::SAMPLE_RATE as usize) {
        return Vec::new();
    }
    sample_subwindow_embeddings(&samples, encoder)
}

/// Does this cluster's enrollment sample carry a second voice? The same
/// verdict enrollment gives: a cluster flagged here is one enrollment refuses.
/// Too little audio to judge reads as one voice.
fn cluster_reads_mixed(
    cluster_id: u32,
    windows: &[Vec<f32>],
    floor: f32,
    separation: Option<f32>,
) -> bool {
    let mixed = !one_enrollable_voice(windows, floor, separation);
    if mixed {
        let split = split_two_voices(windows, floor, separation).map(|(l, r)| (l.len(), r.len()));
        log::info!(
            "diarize: cluster {cluster_id} reads mixed — {} window(s), within cosine {}, floor {floor:.2}, separation {}, split {split:?}",
            windows.len(),
            mean_cosine_within(windows).map(|c| format!("{c:.3}")).unwrap_or_else(|| "n/a".into()),
            separation.map(|c| format!("{c:.3}")).unwrap_or_else(|| "n/a".into()),
        );
    }
    mixed
}

/// The windows about to be averaged into one voiceprint read as one clear
/// voice: no separable second voice, and enough agreement across the whole set
/// to tell. Fewer than two windows carry nothing to judge and pass.
fn one_enrollable_voice(windows: &[Vec<f32>], floor: f32, separation: Option<f32>) -> bool {
    split_two_voices(windows, floor, separation).is_none()
        && mean_cosine_within(windows).is_none_or(|c| c >= floor)
}

/// Do two clusters' loud windows read as one voice? A voice's own windows sit
/// at some cosine spread that depends on the session's capture conditions and
/// on how varied that person's speech is, so the pair is one voice when its
/// cross-cluster cosine reaches that spread (less `SAME_VOICE_SPREAD_MARGIN`)
/// and two voices when it sits below. With fewer than two windows on either
/// side there is nothing to calibrate against and the centroid cosine decides
/// alone.
fn same_voice_by_spread(left: &[Vec<f32>], right: &[Vec<f32>]) -> bool {
    let (Some(within_left), Some(within_right), Some(across)) = (
        mean_cosine_within(left),
        mean_cosine_within(right),
        mean_cosine_across(left, right),
    ) else {
        return true;
    };
    across >= (within_left + within_right) / 2.0 - SAME_VOICE_SPREAD_MARGIN
}

/// Mean cosine over a set's distinct pairs; `None` below two vectors.
fn mean_cosine_within(vs: &[Vec<f32>]) -> Option<f32> {
    let mut total = 0.0;
    let mut count = 0u32;
    for (i, a) in vs.iter().enumerate() {
        for b in &vs[i + 1..] {
            total += cosine(a, b);
            count += 1;
        }
    }
    (count > 0).then(|| total / count as f32)
}

/// Mean cosine over every cross pair of two sets; `None` if either is empty.
fn mean_cosine_across(left: &[Vec<f32>], right: &[Vec<f32>]) -> Option<f32> {
    let mut total = 0.0;
    let mut count = 0u32;
    for a in left {
        for b in right {
            total += cosine(a, b);
            count += 1;
        }
    }
    (count > 0).then(|| total / count as f32)
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
    let Some(st_path) = crate::commands::session_store::preferred_transcript_path(session_dir)
    else {
        return Ok(Vec::new());
    };
    let st: transcript::SessionTranscript = serde_json::from_slice(&syncsafe::read(&st_path)?)
        .map_err(|e| AppError::Config(format!("parse transcript: {e}")))?;
    let labeled: std::collections::HashSet<u32> =
        crate::commands::session_store::read_manifest_opt(session_dir)
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
    allow_mixed: bool,
    floor: f32,
) -> Result<Vec<f32>> {
    let samples = gather_enrollment_pcm(session_dir, cluster_id)?;
    if samples.len() < (voiceprints::SAMPLE_RATE as usize) {
        return Err(AppError::Config(format!(
            "cluster {cluster_id}: not enough speech ({} samples) to enroll",
            samples.len()
        )));
    }
    // Shares the diarization permit: this loads the same speaker encoder as a
    // diarization or a rematch sweep, and a second copy of it is a second set
    // of weights and a second run at the same cores.
    let _diarize = diarize_permit();
    let mut encoder = Encoder::load()
        .map_err(|e| AppError::Config(format!("voiceprint model: {e}")))?;
    // A vector averaged over two people is matched against forever and cannot
    // be told apart from a real one afterward, so the exact audio about to be
    // embedded must read as one voice. The scale that judges it was measured
    // over every cluster when the session was diarized; without it the
    // centroid gate decides alone.
    let separation = syncsafe::read(session_dir.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<SessionManifest>(&b).ok())
        .and_then(|m| m.speaker_separation);
    if !allow_mixed
        && !one_enrollable_voice(
            &sample_subwindow_embeddings(&samples, &mut encoder),
            floor,
            separation,
        )
    {
        return Err(AppError::Config(format!(
            "cluster {cluster_id} does not read as one clear voice — re-diarize this session, \
             or save it anyway if you know this is one speaker"
        )));
    }
    encoder
        .encode_pcm(&samples)
        .map_err(|e| AppError::Config(format!("encode: {e}")))
}

/// Concatenate up to ENROLL_CAP_MS of audio for a given cluster. Windows
/// somebody else talks over are included once the isolated ones run out: a
/// match this feeds is re-decided every run, so thin evidence costs a match,
/// never a wrong recording. Enrollment gathers with `isolated_only`.
fn gather_cluster_pcm(
    session_dir: &std::path::Path,
    cluster_id: u32,
) -> Result<Vec<i16>> {
    Ok(gather_cluster_sample(session_dir, cluster_id, ENROLL_CAP_MS, false)?.0)
}

/// The audio an enrollment is allowed to keep: this cluster's isolated windows
/// only. A voiceprint is written to the vault once and matched against forever,
/// so a window carrying two voices is never worth the extra seconds.
fn gather_enrollment_pcm(
    session_dir: &std::path::Path,
    cluster_id: u32,
) -> Result<Vec<i16>> {
    Ok(gather_cluster_sample(session_dir, cluster_id, ENROLL_CAP_MS, true)?.0)
}

/// RMS floor (normalized 0..1, ≈ -46 dBFS) below which a PCM window is treated
/// as silence and skipped when gathering a cluster's audio. The absolute
/// bottom of `voiced_floor`.
const SAMPLE_SILENCE_FLOOR: f32 = 0.005;

/// A frame counts as speech at this multiple of the recording's own noise
/// floor — 6 dB above it.
const VOICED_OVER_NOISE: f32 = 2.0;

/// Head of a chunk read to estimate its noise floor.
const NOISE_FLOOR_PROBE_MS: u32 = 60_000;

/// The RMS a frame of this recording has to reach to be speech: 6 dB over the
/// chunk's own noise floor, never below the absolute silence floor. Room tone
/// on a far-field recording sits tens of dB above `SAMPLE_SILENCE_FLOOR`, and
/// against the absolute floor alone every pause and every other voice in a
/// window reads as speech. A probe with no frame 6 dB over its quietest tenth
/// has no noise bed to measure, and keeps the absolute floor.
fn voiced_floor(wav: &std::path::Path) -> f32 {
    let Ok(pcm) = read_pcm_window(wav, 0, NOISE_FLOOR_PROBE_MS, NOISE_FLOOR_PROBE_MS) else {
        return SAMPLE_SILENCE_FLOOR;
    };
    let mut frames: Vec<f32> =
        pcm.chunks(VOICED_FRAME_SAMPLES).map(pcm_rms_norm).collect();
    if frames.is_empty() {
        return SAMPLE_SILENCE_FLOOR;
    }
    frames.sort_by(f32::total_cmp);
    let relative = frames[frames.len() / 10] * VOICED_OVER_NOISE;
    if frames.last().is_some_and(|&loudest| loudest < relative) {
        return SAMPLE_SILENCE_FLOOR;
    }
    relative.max(SAMPLE_SILENCE_FLOOR)
}

/// Max audio (ms) any single segment contributes to a gathered sample. A take
/// this long stays inside one speaker turn on conversational speech.
const SAMPLE_MAX_SEG_MS: u32 = 6_000;

/// Frame used for voiced-run analysis: 20 ms @ 16 kHz.
const VOICED_FRAME_SAMPLES: usize = 320;
/// Interior silence longer than this is collapsed down to it: a sample keeps
/// its natural pauses without carrying dead air into the embedding.
const VOICED_MAX_GAP_MS: u32 = 300;
/// A candidate window whose voiced fraction falls below this is a brief
/// utterance adrift in silence, not a usable voice sample.
const SAMPLE_MIN_VOICED: f32 = 0.25;

/// Strip a window down to its speech: drop leading/trailing silence and
/// collapse interior gaps to `VOICED_MAX_GAP_MS`. `floor` is the RMS a frame
/// has to reach to count as speech — `voiced_floor` for the wav the window
/// came from. Returns the compacted audio and the voiced fraction of the
/// ORIGINAL window (the density used to reject sparse candidates). Ranking on
/// the compacted audio is what keeps a long, mostly-silent segment from
/// out-scoring real speech: loudness is measured on what is actually taken,
/// not on a probe window.
fn compact_voiced(pcm: &[i16], floor: f32) -> (Vec<i16>, f32) {
    if pcm.is_empty() {
        return (Vec::new(), 0.0);
    }
    let frames: Vec<bool> = pcm
        .chunks(VOICED_FRAME_SAMPLES)
        .map(|f| pcm_rms_norm(f) >= floor)
        .collect();
    let voiced = frames.iter().filter(|v| **v).count();
    if voiced == 0 {
        return (Vec::new(), 0.0);
    }
    let density = voiced as f32 / frames.len() as f32;
    let first = frames.iter().position(|v| *v).unwrap_or(0);
    let last = frames.iter().rposition(|v| *v).unwrap_or(0);
    let max_gap_frames =
        (VOICED_MAX_GAP_MS as usize * voiceprints::SAMPLE_RATE as usize / 1000)
            / VOICED_FRAME_SAMPLES;
    let mut out: Vec<i16> = Vec::with_capacity(pcm.len());
    let mut gap = 0usize;
    for i in first..=last {
        if frames[i] {
            gap = 0;
        } else {
            gap += 1;
            if gap > max_gap_frames {
                continue;
            }
        }
        let s = i * VOICED_FRAME_SAMPLES;
        let e = ((i + 1) * VOICED_FRAME_SAMPLES).min(pcm.len());
        out.extend_from_slice(&pcm[s..e]);
    }
    (out, density)
}

/// The speech content of one segment window: read at most `SAMPLE_MAX_SEG_MS`
/// from `start_ms` and compact it against `floor`, the wav's `voiced_floor`.
/// `None` when the window is unreadable, too quiet, or too sparse to represent
/// the voice.
fn voiced_sample(
    wav: &std::path::Path,
    start_ms: u32,
    dur_ms: u32,
    floor: f32,
) -> Option<(Vec<i16>, f32)> {
    let want = dur_ms.min(SAMPLE_MAX_SEG_MS);
    let pcm = read_pcm_window(wav, start_ms, start_ms + want, want).ok()?;
    let (audio, density) = compact_voiced(&pcm, floor);
    if density < SAMPLE_MIN_VOICED || audio.is_empty() {
        return None;
    }
    let rms = pcm_rms_norm(&audio);
    if rms < floor {
        return None;
    }
    Some((audio, rms))
}

/// Length of a PCM buffer in ms at the voiceprint sample rate.
fn pcm_len_ms(pcm: &[i16]) -> u32 {
    (pcm.len() * 1000 / voiceprints::SAMPLE_RATE as usize) as u32
}

/// Margins (ms) around a candidate window inside which another cluster's
/// speaker change marks it as overlapped. Tried widest first: a sample takes
/// the widest margin that still fills what the caller asked for, and relaxes
/// only far enough to fill it.
const SAMPLE_ISOLATION_MARGINS_MS: [u32; 3] = [2_000, 1_000, 400];

/// The margin a sample falls back to when no width fills the cap.
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

/// The isolated pool at the widest margin `enough` accepts, walking
/// `SAMPLE_ISOLATION_MARGINS_MS` from widest to narrowest. Returns
/// `(isolated, overlapped, margin_ms)`; the narrowest margin is used when no
/// width satisfies `enough`.
fn isolated_pool(
    cands: &[(u32, u32)],
    others: &[(u32, u32)],
    enough: impl Fn(&[usize]) -> bool,
) -> (Vec<usize>, Vec<usize>, u32) {
    let mut last = (Vec::new(), (0..cands.len()).collect::<Vec<usize>>(), SAMPLE_ISOLATION_MARGIN_MS);
    for margin in SAMPLE_ISOLATION_MARGINS_MS {
        let (iso, rest) = partition_isolated(cands, others, margin);
        let ok = enough(&iso);
        last = (iso, rest, margin);
        if ok {
            break;
        }
    }
    last
}

/// Pick which candidates make up a quality-ranked sample. `segs` is
/// `(rms, dur_ms)` per candidate in chronological order. Ranks by
/// `rms × min(dur, max_seg_ms)`, takes the strongest until `cap_ms` of audio
/// is budgeted, then returns the chosen `(index, take_ms)` in chronological
/// order.
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

/// Up to `cap_ms` of a cluster's isolated audio — the pool enrollment reads,
/// so a preview clip plays what a voiceprint would be built from.
pub(crate) fn gather_cluster_isolated_pcm_capped(
    session_dir: &std::path::Path,
    cluster_id: u32,
    cap_ms: u32,
) -> Result<Vec<i16>> {
    Ok(gather_cluster_sample(session_dir, cluster_id, cap_ms, true)?.0)
}

/// One candidate window of a cluster, probed for loudness.
struct Cand {
    wav: std::path::PathBuf,
    /// Chunk-relative start of the window taken from this segment.
    start_ms: u32,
    /// Session-absolute start (chunk offset applied), for the isolation
    /// test — segment times are chunk-relative and collide across chunks.
    abs_start_ms: u32,
    /// Length of that window, for the isolation test.
    dur: u32,
    /// Length of the window's compacted speech — what it can contribute.
    voiced_ms: u32,
    /// Loudness of that compacted speech, not of a probe window.
    rms: f32,
    /// Speech floor of the wav this window came from.
    floor: f32,
    text: String,
}

/// The stretch of `[seg_start, seg_end]` (session-absolute ms) this cluster's
/// own speaker turn covers, from the windows diarization wrote for it. The
/// span with the most overlap wins; `None` when no window of this cluster
/// lands inside the segment, which is a segment its speaker does not hold.
fn span_within(spans: &[(u32, u32)], seg_start: u32, seg_end: u32) -> Option<(u32, u32)> {
    spans
        .iter()
        .filter(|(s, e)| *e > seg_start && *s < seg_end)
        .map(|&(s, e)| (s.max(seg_start), e.min(seg_end)))
        .filter(|(s, e)| e.saturating_sub(*s) >= 200)
        .max_by_key(|(s, e)| e.saturating_sub(*s))
}

/// Every candidate window of one cluster (any track), plus the speech windows
/// of the session's other clusters for the isolation test. A candidate is the
/// stretch of a transcript segment the cluster's own speaker turn covers —
/// an ASR segment can run straight through a speaker change. Sessions
/// diarized before those windows were written fall back to the segment span.
/// Near-silent and sparse windows are dropped.
fn cluster_candidates(
    session_dir: &std::path::Path,
    cluster_id: u32,
) -> Result<(Vec<Cand>, Vec<(u32, u32)>)> {
    let mb = syncsafe::read(session_dir.join("manifest.json"))?;
    let manifest: SessionManifest = serde_json::from_slice(&mb)
        .map_err(|e| AppError::Config(format!("parse manifest: {e}")))?;
    let st_path = crate::commands::session_store::preferred_transcript_path(session_dir)
        .unwrap_or_else(|| session_dir.join("transcript.json"));
    let st: transcript::SessionTranscript = serde_json::from_slice(&syncsafe::read(&st_path)?)
        .map_err(|e| AppError::Config(format!("parse transcript: {e}")))?;
    let mut cands: Vec<Cand> = Vec::new();
    let mut other_speech: Vec<(u32, u32)> = Vec::new();
    let offsets = manifest.chunk_offsets_ms();
    let spans: Option<&[(u32, u32)]> = manifest
        .cluster_sample_spans
        .iter()
        .find(|c| c.cluster_id == cluster_id)
        .map(|c| c.windows.as_slice())
        .filter(|w| !w.is_empty());
    for ch in &st.chunks {
        let Some(cm) = manifest.chunks.iter().find(|c| c.index == ch.chunk_index) else {
            continue;
        };
        let base = offsets.get(&ch.chunk_index).copied().unwrap_or(0);
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
            let floor = voiced_floor(&wav);
            for seg in &tr.segments {
                if seg.speaker_id != Some(cluster_id) {
                    if seg.speaker_id.is_some() {
                        // The whole segment: a speaker change can sit
                        // anywhere inside one, so the isolation test keeps
                        // clear of all of it.
                        other_speech.push((
                            base.saturating_add(seg.start_ms),
                            base.saturating_add(seg.end_ms),
                        ));
                    }
                    continue;
                }
                let abs_start = base.saturating_add(seg.start_ms);
                let abs_end = base.saturating_add(seg.end_ms);
                let (abs_start, abs_end) = match spans {
                    Some(spans) => match span_within(spans, abs_start, abs_end) {
                        Some(w) => w,
                        None => continue,
                    },
                    None => (abs_start, abs_end),
                };
                let dur = abs_end.saturating_sub(abs_start);
                if dur < 200 {
                    continue;
                }
                let start_ms = abs_start.saturating_sub(base);
                // Rank on the window's actual speech: one that is mostly
                // silence is rejected outright, and what survives is scored
                // by the loudness/length of its compacted audio.
                let Some((audio, rms)) = voiced_sample(&wav, start_ms, dur, floor) else {
                    continue;
                };
                let voiced_ms = pcm_len_ms(&audio);
                if voiced_ms < 200 {
                    continue;
                }
                cands.push(Cand {
                    wav: wav.clone(),
                    start_ms,
                    abs_start_ms: abs_start,
                    dur,
                    voiced_ms,
                    rms,
                    floor,
                    text: seg.text.trim().to_string(),
                });
            }
        }
    }
    Ok((cands, other_speech))
}

/// This cluster's isolated windows, embedded one by one — the spread the
/// same-voice test reads, gathered from a session on disk. Ordered loudest
/// first and capped at `cap`.
fn cluster_spread_windows(
    session_dir: &std::path::Path,
    cluster_id: u32,
    cap: usize,
    encoder: &mut Encoder,
) -> Vec<Vec<f32>> {
    let Ok((cands, other_speech)) = cluster_candidates(session_dir, cluster_id) else {
        return Vec::new();
    };
    let windows: Vec<(u32, u32)> = cands
        .iter()
        .map(|c| (c.abs_start_ms, c.abs_start_ms.saturating_add(c.dur)))
        .collect();
    let usable = |i: usize| cands[i].voiced_ms >= SPREAD_WINDOW_MIN_MS;
    let (iso, _rest, _margin) = isolated_pool(&windows, &other_speech, |iso| {
        iso.iter().filter(|&&i| usable(i)).count() >= cap
    });
    let mut ranked: Vec<(f32, usize)> = iso
        .into_iter()
        .filter(|&i| usable(i))
        .map(|i| (cands[i].rms * cands[i].voiced_ms as f32, i))
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut out: Vec<Vec<f32>> = Vec::new();
    for (_, i) in ranked.into_iter().take(cap) {
        let c = &cands[i];
        let Some((voiced, _)) = voiced_sample(&c.wav, c.start_ms, c.dur, c.floor) else {
            continue;
        };
        if let Ok(v) = encoder.encode_pcm(&voiced) {
            out.push(v);
        }
    }
    out
}

/// Which candidates a cluster's sample is built from, and the isolation margin
/// they were taken at. Candidates with no other-cluster speaker change within
/// the margin come first, at the widest margin that still fills `cap_ms`;
/// overlapped ones fill in only when the isolated set can't reach the cap and
/// the caller allows them. Within each set, rank by quality; the result is
/// chronological.
fn choose_sample_segments(
    cands: &[Cand],
    other_speech: &[(u32, u32)],
    cap_ms: u32,
    isolated_only: bool,
) -> (Vec<(usize, u32)>, u32) {
    let windows: Vec<(u32, u32)> = cands
        .iter()
        .map(|c| (c.abs_start_ms, c.abs_start_ms.saturating_add(c.dur)))
        .collect();
    let pick = |idxs: &[usize], budget_ms: u32| -> Vec<(usize, u32)> {
        let dims: Vec<(f32, u32)> =
            idxs.iter().map(|&i| (cands[i].rms, cands[i].voiced_ms)).collect();
        select_quality_segments(&dims, budget_ms, SAMPLE_MAX_SEG_MS)
            .into_iter()
            .map(|(k, take)| (idxs[k], take))
            .collect()
    };
    let filled = |idxs: &[usize]| -> bool {
        pick(idxs, cap_ms).iter().map(|&(_, t)| t).sum::<u32>() >= cap_ms
    };
    let (iso, rest, margin) = isolated_pool(&windows, other_speech, filled);
    let mut chosen = pick(&iso, cap_ms);
    let taken: u32 = chosen.iter().map(|&(_, t)| t).sum();
    if taken < cap_ms && !isolated_only {
        chosen.extend(pick(&rest, cap_ms - taken));
    }
    chosen.sort_by_key(|&(i, _)| i);
    (chosen, margin)
}

/// Up to `cap_ms` of one cluster's speech, plus the transcript text of exactly
/// the windows whose audio was included.
pub(crate) fn gather_cluster_sample(
    session_dir: &std::path::Path,
    cluster_id: u32,
    cap_ms: u32,
    isolated_only: bool,
) -> Result<(Vec<i16>, String)> {
    let (cands, other_speech) = cluster_candidates(session_dir, cluster_id)?;
    // Audio and the displayed text are built from the same chosen windows, in
    // order.
    let (chosen, margin) = choose_sample_segments(&cands, &other_speech, cap_ms, isolated_only);
    log::info!(
        "sample: cluster {cluster_id} took {} window(s) at a {margin} ms isolation margin",
        chosen.len()
    );

    let mut out: Vec<i16> = Vec::new();
    let mut text = String::new();
    for (i, take) in chosen {
        let c = &cands[i];
        // Re-derive the compacted audio (pass 1 kept only its dimensions) and
        // emit the budgeted prefix of the SPEECH, not of the raw window.
        let Some((audio, _)) = voiced_sample(&c.wav, c.start_ms, c.dur, c.floor) else {
            continue;
        };
        let n = (take as usize * voiceprints::SAMPLE_RATE as usize / 1000).min(audio.len());
        out.extend_from_slice(&audio[..n]);
        if !c.text.is_empty() && text.chars().count() < 200 {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&c.text);
            // The clip carries only part of this segment; say so rather than
            // promising a phrase the audio does not reach.
            if n < audio.len() {
                text.push('…');
            }
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
    syncsafe::write_atomic(&path, &bytes)?;
    Ok(())
}

#[cfg(test)]
mod voiced_sample_tests {
    use super::*;

    /// 16 kHz: 16 samples per ms.
    fn tone(ms: usize) -> Vec<i16> {
        vec![3000; ms * 16]
    }
    fn hush(ms: usize) -> Vec<i16> {
        vec![0; ms * 16]
    }

    #[test]
    fn silence_yields_nothing() {
        let (audio, density) = compact_voiced(&hush(1000), SAMPLE_SILENCE_FLOOR);
        assert!(audio.is_empty());
        assert_eq!(density, 0.0);
        assert_eq!(compact_voiced(&[], SAMPLE_SILENCE_FLOOR).0.len(), 0);
    }

    #[test]
    fn trims_leading_and_trailing_silence() {
        // 0.5 s hush + 1 s speech + 2 s hush -> just the speech.
        let pcm = [hush(500), tone(1000), hush(2000)].concat();
        let (audio, density) = compact_voiced(&pcm, SAMPLE_SILENCE_FLOOR);
        assert_eq!(pcm_len_ms(&audio), 1000);
        // Density reports the ORIGINAL window: 1 s voiced of 3.5 s.
        assert!((density - 1000.0 / 3500.0).abs() < 0.01, "density {density}");
    }

    #[test]
    fn collapses_interior_gap_to_the_pause_cap() {
        // 1 s speech, 1 s gap, 1 s speech -> the gap shrinks to 300 ms.
        let pcm = [tone(1000), hush(1000), tone(1000)].concat();
        let (audio, density) = compact_voiced(&pcm, SAMPLE_SILENCE_FLOOR);
        assert_eq!(pcm_len_ms(&audio), 1000 + VOICED_MAX_GAP_MS + 1000);
        assert!((density - 2.0 / 3.0).abs() < 0.01, "density {density}");
    }

    #[test]
    fn short_gaps_are_left_alone() {
        let pcm = [tone(1000), hush(100), tone(1000)].concat();
        assert_eq!(pcm_len_ms(&compact_voiced(&pcm, SAMPLE_SILENCE_FLOOR).0), 2100);
    }

    #[test]
    fn sparse_window_falls_under_the_voiced_floor() {
        // A brief utterance adrift in silence. Density must land below the
        // rejection floor.
        let pcm = [tone(500), hush(7500)].concat();
        let (_, density) = compact_voiced(&pcm, SAMPLE_SILENCE_FLOOR);
        assert!(density < SAMPLE_MIN_VOICED, "density {density}");
    }

    #[test]
    fn sparse_window_no_longer_outranks_dense_speech() {
        // Ranking runs on compacted speech, so length credit tracks actual
        // voice: a long, mostly-silent segment must not outrank dense speech,
        // fill the clip on its own, and embed a centroid of mostly room tone.
        let sparse = [tone(2000), hush(6000)].concat();
        let dense = tone(3000);
        let (sparse_c, _) = compact_voiced(&sparse, SAMPLE_SILENCE_FLOOR);
        let (dense_c, _) = compact_voiced(&dense, SAMPLE_SILENCE_FLOOR);
        let dims = [
            (pcm_rms_norm(&sparse_c), pcm_len_ms(&sparse_c)),
            (pcm_rms_norm(&dense_c), pcm_len_ms(&dense_c)),
        ];
        // Budget fits one: the dense segment is the one taken.
        assert_eq!(
            select_quality_segments(&dims, 3000, SAMPLE_MAX_SEG_MS),
            vec![(1, 3000)]
        );
    }
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
mod gather_cluster_sample_tests {
    use super::gather_cluster_sample;

    /// 16 kHz mono WAV, `secs` long: 800 ms at `amp` then 200 ms of room tone
    /// at `amp / 40`, repeating. The quiet fifth is what a noise floor is
    /// estimated from.
    fn write_wav(path: &std::path::Path, amp: i16, secs: u32) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..(16_000 * secs) {
            let bed = (i % 16_000) >= 12_800;
            w.write_sample(if bed { amp / 40 } else { amp }).unwrap();
        }
        w.finalize().unwrap();
    }

    /// Two chunks. Chunk 1: another cluster speaks at 0–1 s, then a LOUD
    /// cluster-0 segment starts 200 ms later (inside the isolation margin →
    /// overlapped). Chunk 2 (6 minutes later): a quieter cluster-0 segment at
    /// chunk-relative 0–8 s with no other speech anywhere near it in wall
    /// time — but whose chunk-RELATIVE window coincides with chunk 1's
    /// other-cluster speech.
    fn write_session(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        write_wav(&dir.join("chunks/0001/system.wav"), 8000, 10);
        write_wav(&dir.join("chunks/0002/system.wav"), 2000, 8);
        let chunk = |index: u32, started: i64, wav: &str| {
            serde_json::json!({
                "index": index,
                "started_at_unix_seconds": started,
                "ended_at_unix_seconds": null,
                "duration_seconds": null,
                "mic_wav_relative": format!("chunks/{index:04}/mic.wav"),
                "system_wav_relative": wav,
                "mic_aec_wav_relative": null,
            })
        };
        let manifest = serde_json::json!({
            "schema_version": 3,
            "session_id": "s",
            "created_at_unix_seconds": 0,
            "sample_rate": 16000,
            "channels": 1,
            "mic_source_id": 0,
            "mic_source_node_name": "",
            "mic_source_description": "",
            "system_source_id": 0,
            "system_source_node_name": "",
            "system_source_description": "",
            "aec_mode": "always",
            "chunks": [
                chunk(1, 0, "chunks/0001/system.wav"),
                chunk(2, 360, "chunks/0002/system.wav"),
            ],
            "finalized_at_unix_seconds": null,
        });
        let seg = |s: u32, e: u32, spk: u32, text: &str| {
            serde_json::json!({
                "start_ms": s, "end_ms": e, "text": text,
                "confidence": null, "speaker_id": spk,
            })
        };
        let transcript = serde_json::json!({
            "schema_version": 1,
            "session_id": "s",
            "provider": "local",
            "model": "whisper",
            "transcribed_at_unix_seconds": 0,
            "chunks": [
                { "chunk_index": 1, "tracks": [
                    { "track": "system", "source_wav_relative": "chunks/0001/system.wav", "segments": [
                        seg(0, 1000, 1, "other voice"),
                        seg(1200, 9200, 0, "loud overlapped"),
                    ]},
                ]},
                { "chunk_index": 2, "tracks": [
                    { "track": "system", "source_wav_relative": "chunks/0002/system.wav", "segments": [
                        seg(0, 8000, 0, "quiet isolated"),
                    ]},
                ]},
            ],
        });
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::write(dir.join("transcript.json"), serde_json::to_vec(&transcript).unwrap()).unwrap();
    }

    #[test]
    fn isolation_compares_wall_time_not_chunk_relative_time() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp = tmp.path();
        write_session(tmp);
        let (audio, text) = gather_cluster_sample(tmp, 0, 5000, false).unwrap();
        assert!(!audio.is_empty());
        assert!(
            text.contains("quiet isolated"),
            "must prefer the isolated chunk-2 segment, got: {text:?}"
        );
        assert!(
            !text.contains("loud overlapped"),
            "louder-but-overlapped segment must lose to an isolated one, got: {text:?}"
        );
    }
}

#[cfg(test)]
mod turn_bounded_candidate_tests {
    use super::{cluster_candidates, Cand};

    /// 16 kHz mono WAV, `secs` long, 800 ms of speech to 200 ms of room tone.
    fn write_wav(path: &std::path::Path, secs: u32) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..(16_000 * secs) {
            w.write_sample(if (i % 16_000) >= 12_800 { 200i16 } else { 8000 }).unwrap();
        }
        w.finalize().unwrap();
    }

    /// One chunk. Cluster 0 speaks 2–12 s, but its own turn covers only
    /// 2–8 s; cluster 1 answers at 12–14 s. Cluster 0 has a second segment at
    /// 15–17 s that no turn of its own covers.
    fn write_session(dir: &std::path::Path, spans: Option<serde_json::Value>) {
        std::fs::create_dir_all(dir).unwrap();
        write_wav(&dir.join("chunks/0001/system.wav"), 20);
        let mut manifest = serde_json::json!({
            "schema_version": 3,
            "session_id": "s",
            "created_at_unix_seconds": 0,
            "sample_rate": 16000,
            "channels": 1,
            "mic_source_id": 0,
            "mic_source_node_name": "",
            "mic_source_description": "",
            "system_source_id": 0,
            "system_source_node_name": "",
            "system_source_description": "",
            "aec_mode": "always",
            "chunks": [{
                "index": 1,
                "started_at_unix_seconds": 0,
                "ended_at_unix_seconds": null,
                "duration_seconds": null,
                "mic_wav_relative": "chunks/0001/mic.wav",
                "system_wav_relative": "chunks/0001/system.wav",
                "mic_aec_wav_relative": null,
            }],
            "finalized_at_unix_seconds": null,
        });
        if let Some(spans) = spans {
            manifest["cluster_sample_spans"] = spans;
        }
        let seg = |s: u32, e: u32, spk: u32, text: &str| {
            serde_json::json!({
                "start_ms": s, "end_ms": e, "text": text,
                "confidence": null, "speaker_id": spk,
            })
        };
        let transcript = serde_json::json!({
            "schema_version": 1,
            "session_id": "s",
            "provider": "local",
            "model": "whisper",
            "transcribed_at_unix_seconds": 0,
            "chunks": [{ "chunk_index": 1, "tracks": [
                { "track": "system", "source_wav_relative": "chunks/0001/system.wav", "segments": [
                    seg(2000, 12000, 0, "straddles the change"),
                    seg(12000, 14000, 1, "other voice"),
                    seg(15000, 17000, 0, "no turn of its own"),
                ]},
            ]}],
        });
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::write(dir.join("transcript.json"), serde_json::to_vec(&transcript).unwrap())
            .unwrap();
    }

    fn spans_of(cands: &[Cand]) -> Vec<(u32, u32)> {
        cands.iter().map(|c| (c.abs_start_ms, c.abs_start_ms + c.dur)).collect()
    }

    #[test]
    fn a_candidate_is_the_turn_bounded_window_not_the_segment() {
        let tmp = tempfile::tempdir().unwrap();
        write_session(
            tmp.path(),
            Some(serde_json::json!([{ "cluster_id": 0, "windows": [[2200, 7800]] }])),
        );
        let (cands, _) = cluster_candidates(tmp.path(), 0).unwrap();
        assert_eq!(spans_of(&cands), vec![(2200, 7800)]);
    }

    #[test]
    fn a_session_without_windows_still_samples_the_segment() {
        let tmp = tempfile::tempdir().unwrap();
        write_session(tmp.path(), None);
        let (cands, other) = cluster_candidates(tmp.path(), 0).unwrap();
        assert_eq!(spans_of(&cands), vec![(2000, 12000), (15000, 17000)]);
        assert_eq!(other, vec![(12000, 14000)]);
    }
}

#[cfg(test)]
mod isolation_margin_tests {
    use super::*;

    fn cand(abs_start_ms: u32, dur: u32) -> Cand {
        Cand {
            wav: std::path::PathBuf::new(),
            start_ms: abs_start_ms,
            abs_start_ms,
            dur,
            voiced_ms: dur,
            rms: 0.1,
            floor: SAMPLE_SILENCE_FLOOR,
            text: String::new(),
        }
    }

    fn taken(chosen: &[(usize, u32)]) -> u32 {
        chosen.iter().map(|&(_, t)| t).sum()
    }

    #[test]
    fn room_to_spare_keeps_the_widest_margin() {
        let cands: Vec<Cand> = (0..10).map(|k| cand(30_000 * k, 6_000)).collect();
        let (chosen, margin) = choose_sample_segments(&cands, &[], ENROLL_CAP_MS, true);
        assert_eq!(margin, SAMPLE_ISOLATION_MARGINS_MS[0]);
        assert_eq!(taken(&chosen), ENROLL_CAP_MS);
    }

    #[test]
    fn rapid_turns_relax_the_margin_until_the_cap_fills() {
        // Ten 4 s windows, each with another cluster stopping 600 ms before it
        // and starting 600 ms after: nothing clears 2000 or 1000 ms, and the
        // sample still has to come from somewhere.
        let mut cands = Vec::new();
        let mut others = Vec::new();
        for k in 0..10u32 {
            let s = 10_000 + k * 20_000;
            cands.push(cand(s, 4_000));
            others.push((s - 1_000, s - 600));
            others.push((s + 4_600, s + 5_000));
        }
        let (chosen, margin) = choose_sample_segments(&cands, &others, ENROLL_CAP_MS, true);
        assert_eq!(margin, 400);
        assert_eq!(taken(&chosen), ENROLL_CAP_MS);
    }

    #[test]
    fn a_wide_margin_that_fills_the_cap_is_never_relaxed() {
        // The same windows, but only two of them have a neighbour close by:
        // eight × 4 s still reaches the 30 s cap at 2000 ms.
        let mut cands = Vec::new();
        let mut others = Vec::new();
        for k in 0..10u32 {
            let s = 10_000 + k * 20_000;
            cands.push(cand(s, 4_000));
            if k < 2 {
                others.push((s - 1_000, s - 600));
            }
        }
        let (chosen, margin) = choose_sample_segments(&cands, &others, ENROLL_CAP_MS, true);
        assert_eq!(margin, SAMPLE_ISOLATION_MARGINS_MS[0]);
        assert_eq!(taken(&chosen), ENROLL_CAP_MS);
        assert!(chosen.iter().all(|&(i, _)| i >= 2), "chosen {chosen:?}");
    }
}

#[cfg(test)]
mod voiced_floor_tests {
    use super::*;

    /// 20 s: 15 s of −34.6 dBFS room tone, then 5 s of speech.
    fn write_bed_wav(path: &std::path::Path) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..(16_000 * 20) {
            w.write_sample(if i < 16_000 * 15 { 610i16 } else { 8000 }).unwrap();
        }
        w.finalize().unwrap();
    }

    #[test]
    fn a_far_field_noise_bed_no_longer_reads_as_voiced() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("bed.wav");
        write_bed_wav(&wav);
        let bed = vec![610i16; 16_000];
        // The bed sits ~3.7x over the absolute floor, so that floor alone
        // reads every frame of it as speech.
        assert!(pcm_rms_norm(&bed) > SAMPLE_SILENCE_FLOOR);
        assert!(!compact_voiced(&bed, SAMPLE_SILENCE_FLOOR).0.is_empty());

        let floor = voiced_floor(&wav);
        assert!(floor > pcm_rms_norm(&bed), "floor {floor}");
        assert!(compact_voiced(&bed, floor).0.is_empty());
        // Speech over the same bed still is.
        let speech = [8000i16; 16_000];
        assert!(!compact_voiced(&speech, floor).0.is_empty());
    }

    #[test]
    fn a_clean_recording_keeps_the_absolute_floor() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("quiet.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        for i in 0..(16_000 * 20) {
            w.write_sample(if i < 16_000 * 15 { 0i16 } else { 8000 }).unwrap();
        }
        w.finalize().unwrap();
        assert_eq!(voiced_floor(&wav), SAMPLE_SILENCE_FLOOR);
    }

    #[test]
    fn a_recording_with_no_quiet_stretch_keeps_the_absolute_floor() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("steady.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        for i in 0..(16_000 * 20) {
            w.write_sample(if i % 2 == 0 { 3000i16 } else { -3000 }).unwrap();
        }
        w.finalize().unwrap();
        let floor = voiced_floor(&wav);
        assert_eq!(floor, SAMPLE_SILENCE_FLOOR);
        assert!(!compact_voiced(&[3000i16; 16_000], floor).0.is_empty());
    }
}

#[cfg(test)]
mod turn_bounded_window_tests {
    use super::turn_bounded_window;

    #[test]
    fn keeps_the_segment_pulled_in_from_both_ends() {
        // Segment 10.0-14.0s sits inside one turn: 200 ms off each end.
        let w = turn_bounded_window(10.0, 14.0, &[(9.0, 20.0)], &[], 0.0);
        assert_eq!(w, Some((10_200, 3_600, true)));
    }

    #[test]
    fn a_segment_spanning_a_speaker_change_keeps_only_its_own_part() {
        // The speaker holds 10.0-12.0; the rest of the segment is someone else.
        let w = turn_bounded_window(10.0, 14.0, &[(5.0, 12.0)], &[], 0.0);
        assert_eq!(w, Some((10_200, 1_600, true)));
    }

    #[test]
    fn picks_the_longest_own_stretch() {
        // Two of the speaker's turns fall inside: 0.5s and 2.0s.
        let w = turn_bounded_window(10.0, 15.0, &[(10.0, 10.5), (12.0, 14.0)], &[], 0.0);
        assert_eq!(w, Some((12_200, 1_600, true)));
    }

    #[test]
    fn short_stretches_skip_the_guard_then_fail_the_floor() {
        // 0.5s survives without the guard; 0.3s is below the floor entirely.
        assert_eq!(turn_bounded_window(10.0, 14.0, &[(10.0, 10.5)], &[], 0.0), Some((10_000, 500, true)));
        assert_eq!(turn_bounded_window(10.0, 14.0, &[(10.0, 10.3)], &[], 0.0), None);
    }

    #[test]
    fn a_window_someone_talks_over_is_marked_overlapped() {
        // The speaker holds 10.0-14.0, but another turn covers 11.0-11.5 of it:
        // still usable for a centroid, no longer usable for measuring spread.
        assert_eq!(
            turn_bounded_window(10.0, 14.0, &[(9.0, 20.0)], &[(11.0, 11.5)], 0.0),
            Some((10_200, 3_600, false))
        );
        // A 100 ms clip of the neighbour is below the reject threshold.
        assert_eq!(
            turn_bounded_window(10.0, 14.0, &[(9.0, 20.0)], &[(11.0, 11.1)], 0.0),
            Some((10_200, 3_600, true))
        );
    }

    #[test]
    fn no_own_turn_inside_the_segment_yields_nothing() {
        assert_eq!(turn_bounded_window(10.0, 14.0, &[(20.0, 30.0)], &[], 0.0), None);
        assert_eq!(turn_bounded_window(10.0, 14.0, &[], &[], 0.0), None);
    }

    #[test]
    fn the_window_comes_back_chunk_relative() {
        // Chunk 2 starts 600s in; the same absolute turn is 10.2s into it.
        let w = turn_bounded_window(610.0, 614.0, &[(600.0, 700.0)], &[], 600.0);
        assert_eq!(w, Some((10_200, 3_600, true)));
    }
}

#[cfg(test)]
mod merge_clusters_by_cosine_tests {
    use super::merge_clusters_by_cosine;
    use std::collections::{BTreeMap, HashSet};

    fn distinct(a: &[Option<u32>]) -> usize {
        a.iter().flatten().collect::<HashSet<_>>().len()
    }

    fn no_windows() -> BTreeMap<u32, Vec<Vec<f32>>> {
        BTreeMap::new()
    }

    /// Two unit vectors `angle` radians apart in the x-y plane, centred on
    /// `centre`, so a set's mean pairwise cosine can be dialled directly.
    fn spread(centre: f32, angle: f32) -> Vec<Vec<f32>> {
        vec![
            vec![(centre - angle / 2.0).cos(), (centre - angle / 2.0).sin()],
            vec![(centre + angle / 2.0).cos(), (centre + angle / 2.0).sin()],
        ]
    }

    #[test]
    fn merges_same_voice_keeps_distinct() {
        // clusters 0 & 2 nearly identical (the over-split pair); 1 orthogonal.
        let mut c = BTreeMap::new();
        c.insert(0u32, vec![1.0, 0.0, 0.0]);
        c.insert(1u32, vec![0.0, 1.0, 0.0]);
        c.insert(2u32, vec![0.98, 0.02, 0.0]);
        let out = merge_clusters_by_cosine(
            &c,
            &no_windows(),
            &[Some(0), Some(1), Some(2), Some(0)],
            0.55,
            1,
        );
        assert_eq!(distinct(&out), 2);
        assert_eq!(out[0], out[2], "over-split pair merged");
        assert_ne!(out[0], out[1], "distinct voice stays");
    }

    #[test]
    fn no_merge_below_threshold() {
        let mut c = BTreeMap::new();
        c.insert(0u32, vec![1.0, 0.0]);
        c.insert(1u32, vec![0.0, 1.0]);
        let out = merge_clusters_by_cosine(&c, &no_windows(), &[Some(0), Some(1)], 0.55, 1);
        assert_eq!(distinct(&out), 2);
    }

    #[test]
    fn min_clusters_floor_holds_the_requested_count() {
        // All three centroids read as the same voice; a floor of 3 keeps them.
        let mut c = BTreeMap::new();
        c.insert(0u32, vec![1.0, 0.0]);
        c.insert(1u32, vec![0.99, 0.01]);
        c.insert(2u32, vec![0.98, 0.02]);
        let assign = [Some(0u32), Some(1), Some(2)];
        let w = no_windows();
        assert_eq!(distinct(&merge_clusters_by_cosine(&c, &w, &assign, 0.55, 3)), 3);
        assert_eq!(distinct(&merge_clusters_by_cosine(&c, &w, &assign, 0.55, 2)), 2);
        assert_eq!(distinct(&merge_clusters_by_cosine(&c, &w, &assign, 0.55, 1)), 1);
    }

    #[test]
    fn cluster_without_centroid_passes_through() {
        // cluster 1 has no centroid (all-silent) → stays its own cluster.
        let mut c = BTreeMap::new();
        c.insert(0u32, vec![1.0, 0.0]);
        c.insert(2u32, vec![0.99, 0.01]);
        let out = merge_clusters_by_cosine(&c, &no_windows(), &[Some(0), Some(1), Some(2)], 0.55, 1);
        assert_eq!(distinct(&out), 2);
        assert_eq!(out[0], out[2]);
        assert_ne!(out[0], out[1]);
    }

    #[test]
    fn tight_clusters_far_apart_stay_split() {
        // Both clusters' own windows sit at cosine ~0.99, and the two clusters
        // sit 0.75 rad apart (cross ~0.73) — alike enough for the centroid
        // threshold, far outside either voice's own spread.
        let mut c = BTreeMap::new();
        c.insert(0u32, vec![1.0, 0.0]);
        c.insert(1u32, vec![0.75f32.cos(), 0.75f32.sin()]);
        let mut w = BTreeMap::new();
        w.insert(0u32, spread(0.0, 0.15));
        w.insert(1u32, spread(0.75, 0.15));
        let out = merge_clusters_by_cosine(&c, &w, &[Some(0), Some(1)], 0.55, 1);
        assert_eq!(distinct(&out), 2);
    }

    #[test]
    fn loose_clusters_inside_their_own_spread_merge() {
        // Same 0.6 rad between the clusters, but each voice's own windows are
        // this varied (0.9 rad) — the cross similarity is the spread, one voice.
        let mut c = BTreeMap::new();
        c.insert(0u32, vec![1.0, 0.0]);
        c.insert(1u32, vec![0.6f32.cos(), 0.6f32.sin()]);
        let mut w = BTreeMap::new();
        w.insert(0u32, spread(0.0, 0.9));
        w.insert(1u32, spread(0.6, 0.9));
        let out = merge_clusters_by_cosine(&c, &w, &[Some(0), Some(1)], 0.55, 1);
        assert_eq!(distinct(&out), 1);
    }

    #[test]
    fn spread_veto_leaves_the_next_best_pair_mergeable() {
        // 0+1 is the closest pair but both are tight and sit outside each
        // other's spread; 0+2 is further from 0 yet inside cluster 2's own
        // spread, so the merge takes 0+2 instead of stopping. 1+2 is below the
        // centroid threshold and never a candidate.
        let unit = |a: f32, axis: usize| {
            let mut v = vec![a.cos(), 0.0, 0.0];
            v[axis] = a.sin();
            v
        };
        let pair = |centre: f32, angle: f32, axis: usize| {
            vec![unit(centre - angle / 2.0, axis), unit(centre + angle / 2.0, axis)]
        };
        let mut c = BTreeMap::new();
        c.insert(0u32, unit(0.0, 1));
        c.insert(1u32, unit(0.75, 1));
        c.insert(2u32, unit(0.9, 2));
        let mut w = BTreeMap::new();
        w.insert(0u32, pair(0.0, 0.1, 1));
        w.insert(1u32, pair(0.75, 0.1, 1));
        w.insert(2u32, pair(0.9, 1.6, 2));
        let out = merge_clusters_by_cosine(&c, &w, &[Some(0), Some(1), Some(2)], 0.55, 2);
        assert_eq!(distinct(&out), 2);
        assert_eq!(out[0], out[2], "0+2 merged");
        assert_ne!(out[0], out[1], "0+1 vetoed against both clusters' own spread");
    }

    #[test]
    fn one_window_side_falls_back_to_the_centroid() {
        // A single window has no spread to calibrate against, so the pair is
        // decided by the centroid cosine alone.
        let mut c = BTreeMap::new();
        c.insert(0u32, vec![1.0, 0.0]);
        c.insert(1u32, vec![0.6f32.cos(), 0.6f32.sin()]);
        let mut w = BTreeMap::new();
        w.insert(0u32, spread(0.0, 0.15));
        w.insert(1u32, vec![vec![0.6f32.cos(), 0.6f32.sin()]]);
        let out = merge_clusters_by_cosine(&c, &w, &[Some(0), Some(1)], 0.55, 1);
        assert_eq!(distinct(&out), 1);
    }
}

#[cfg(test)]
mod diarize_track_select_tests {
    use super::*;
    use recording::manifest::ChunkManifest;
    use std::path::{Path, PathBuf};

    #[test]
    fn a_phone_recording_clusters_the_mic_whatever_was_asked_for() {
        let mut m: SessionManifest = serde_json::from_value(serde_json::json!({
            "schema_version": 4, "session_id": "s", "created_at_unix_seconds": 0,
            "sample_rate": 16000, "channels": 1, "chunks": [],
        }))
        .unwrap();
        for asked in [None, Some(DiarizeScope::Others), Some(DiarizeScope::Both)] {
            assert_eq!(scope_for(&m, asked), asked);
        }
        m.recording_kind = Some("phone_room".into());
        for asked in [None, Some(DiarizeScope::Others), Some(DiarizeScope::Both)] {
            assert_eq!(scope_for(&m, asked), Some(DiarizeScope::Mic));
        }
    }

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
            extra: Default::default(),
            index: 1,
            started_at_unix_seconds: 0,
            ended_at_unix_seconds: Some(0),
            duration_seconds: Some(1),
            mic_wav_relative: PathBuf::from(mic),
            system_wav_relative: PathBuf::from(sys),
            mic_aec_wav_relative: mic_aec.map(PathBuf::from),
            mic_dn_wav_relative: None,
            mic_host_ns: None,
            system_host_ns: None,
            mic_samples: None,
            system_samples: None,
            mic_voice_processed: false,
            mic_zero_samples: None,
            mic_longest_zero_run: None,
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
        let tmp = tempfile::tempdir().unwrap();
        let tmp = tmp.path();
        write_session(tmp);
        let mut got = clusters_needing_match(tmp).unwrap();
        got.sort();
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

#[cfg(test)]
mod subwindow_tests {
    use super::*;

    fn pcm(ms: usize) -> Vec<i16> {
        vec![1000; ms * (voiceprints::SAMPLE_RATE as usize / 1000)]
    }

    #[test]
    fn a_long_window_yields_several_pieces() {
        let long = pcm(13_400);
        let parts = subwindows(&long, SPLIT_SUBWINDOW_MS);
        assert_eq!(parts.len(), 4);
        assert!(parts.iter().all(|p| pcm_len_ms(p) >= SPREAD_WINDOW_MIN_MS));
    }

    #[test]
    fn a_short_window_stays_whole() {
        let short = pcm(2_000);
        let parts = subwindows(&short, SPLIT_SUBWINDOW_MS);
        assert_eq!(parts.len(), 1);
        assert_eq!(pcm_len_ms(parts[0]), 2_000);
    }

    #[test]
    fn a_window_under_the_minimum_yields_nothing() {
        let tiny = pcm(700);
        assert!(subwindows(&tiny, SPLIT_SUBWINDOW_MS).is_empty());
    }
}

#[cfg(test)]
mod split_two_voices_tests {
    use super::*;

    /// Unit vector in the plane of two axes, `t` in [0,1] leaning from `a` to `b`.
    pub(super) fn v(dim: usize, a: usize, b: usize, t: f32) -> Vec<f32> {
        let mut out = vec![0.0; dim];
        out[a] = 1.0 - t;
        out[b] = t;
        let n = out.iter().map(|x| x * x).sum::<f32>().sqrt();
        out.iter().map(|x| x / n).collect()
    }

    /// Two tight groups far apart — the shape of a fused speaker's windows
    /// against the rest of their cluster.
    #[test]
    fn two_coherent_voices_split() {
        let windows = vec![
            v(4, 0, 1, 0.05),
            v(4, 0, 1, 0.10),
            v(4, 0, 1, 0.15),
            v(4, 2, 3, 0.05),
            v(4, 2, 3, 0.12),
        ];
        let (left, right) = split_two_voices(&windows, ENROLLABLE_COHERENCE, None).expect("splits");
        assert_eq!(left, vec![0, 1, 2]);
        assert_eq!(right, vec![3, 4]);
    }

    #[test]
    fn one_voice_does_not_split() {
        let windows =
            vec![v(4, 0, 1, 0.02), v(4, 0, 1, 0.06), v(4, 0, 1, 0.10), v(4, 0, 1, 0.14)];
        assert!(split_two_voices(&windows, ENROLLABLE_COHERENCE, None).is_none());
    }

    /// A side that is itself incoherent is noise, not a second speaker.
    #[test]
    fn an_incoherent_side_does_not_split() {
        let windows = vec![
            v(6, 0, 1, 0.05),
            v(6, 0, 1, 0.08),
            v(6, 0, 1, 0.11),
            v(6, 2, 3, 0.0),
            v(6, 4, 5, 0.0),
        ];
        assert!(split_two_voices(&windows, ENROLLABLE_COHERENCE, None).is_none());
    }

    /// One window on a side carries no spread of its own to judge.
    #[test]
    fn a_single_window_side_does_not_split() {
        let windows = vec![v(4, 0, 1, 0.05), v(4, 0, 1, 0.09), v(4, 0, 1, 0.13), v(4, 2, 3, 0.0)];
        assert!(split_two_voices(&windows, ENROLLABLE_COHERENCE, None).is_none());
    }

    #[test]
    fn too_few_windows_does_not_split() {
        let windows = vec![v(4, 0, 1, 0.05), v(4, 2, 3, 0.05), v(4, 2, 3, 0.09)];
        assert!(split_two_voices(&windows, ENROLLABLE_COHERENCE, None).is_none());
    }

    /// Sides closer together than any two people in this session are one
    /// person recorded under two conditions.
    #[test]
    fn sides_closer_than_two_speakers_do_not_split() {
        let windows = vec![
            v(4, 0, 1, 0.05),
            v(4, 0, 1, 0.10),
            v(4, 2, 3, 0.05),
            v(4, 2, 3, 0.12),
        ];
        let (lv, rv) = (windows[..2].to_vec(), windows[2..].to_vec());
        let across = mean_cosine_across(&lv, &rv).unwrap();
        assert!(split_two_voices(&windows, ENROLLABLE_COHERENCE, Some(across - 0.06)).is_none());
        assert!(split_two_voices(&windows, ENROLLABLE_COHERENCE, Some(across)).is_some());
    }

    /// A split the merge would undo is not a split.
    #[test]
    fn sides_the_merge_would_rejoin_do_not_split() {
        let windows = vec![
            v(4, 0, 1, 0.02),
            v(4, 0, 1, 0.05),
            v(4, 0, 1, 0.30),
            v(4, 0, 1, 0.33),
        ];
        let (lv, rv) = (windows[..2].to_vec(), windows[2..].to_vec());
        let (lc, rc) = (
            centroid_of(lv.iter().map(|v| v.as_slice())).unwrap(),
            centroid_of(rv.iter().map(|v| v.as_slice())).unwrap(),
        );
        assert!(cosine(&lc, &rc) >= SAME_VOICE_MERGE, "fixture drifted");
        assert!(split_two_voices(&windows, ENROLLABLE_COHERENCE, Some(1.0)).is_none());
    }

    /// The session's own scale: the widest gap between any two clusters.
    #[test]
    fn separation_is_the_widest_cluster_pair() {
        let near = vec![v(4, 0, 1, 0.30), v(4, 0, 1, 0.34)];
        let mid = vec![v(4, 0, 1, 0.55), v(4, 0, 1, 0.60)];
        let far = vec![v(4, 2, 3, 0.05), v(4, 2, 3, 0.10)];
        let sets = vec![near.clone(), mid.clone(), far.clone()];
        let widest = speaker_separation(&sets).expect("two sets");
        assert_eq!(widest, mean_cosine_across(&near, &mid).unwrap());
        assert!(speaker_separation(&[near]).is_none());
        assert!(speaker_separation(&[]).is_none());
    }
}

#[cfg(test)]
mod one_enrollable_voice_tests {
    use super::*;
    use super::split_two_voices_tests::v;

    /// The Settings slider reads as tolerance; the gate reads as a floor.
    #[test]
    fn tolerance_maps_to_the_floor_it_replaces() {
        assert_eq!(coherence_floor(65), ENROLLABLE_COHERENCE);
        assert_eq!(coherence_floor(0), 1.0);
        assert_eq!(coherence_floor(100), 0.0);
        // Out-of-range values clamp rather than wrap into a nonsense floor.
        assert_eq!(coherence_floor(255), 0.0);
    }

    #[test]
    fn a_coherent_voice_enrolls() {
        let windows =
            vec![v(4, 0, 1, 0.02), v(4, 0, 1, 0.06), v(4, 0, 1, 0.10), v(4, 0, 1, 0.14)];
        assert!(one_enrollable_voice(&windows, ENROLLABLE_COHERENCE, None));
    }

    #[test]
    fn a_separable_second_voice_refuses() {
        let windows = vec![
            v(4, 0, 1, 0.05),
            v(4, 0, 1, 0.10),
            v(4, 0, 1, 0.15),
            v(4, 2, 3, 0.05),
            v(4, 2, 3, 0.12),
        ];
        assert!(!one_enrollable_voice(&windows, ENROLLABLE_COHERENCE, None));
    }

    /// A mix that 2-means cannot separate (its stray windows agree with
    /// nothing, including each other) still refuses: the sample does not read
    /// as one person even though no clean split exists.
    #[test]
    fn an_inseparable_mix_refuses() {
        let windows = vec![
            v(6, 0, 1, 0.05),
            v(6, 0, 1, 0.08),
            v(6, 0, 1, 0.11),
            v(6, 2, 3, 0.0),
            v(6, 4, 5, 0.0),
        ];
        assert!(!one_enrollable_voice(&windows, ENROLLABLE_COHERENCE, None));
    }

    /// One speaker's own ~3s sub-windows spread out over a meeting: a shared
    /// voice direction plus a per-window component of its own. They are a
    /// continuum, not two groups, and must enroll.
    #[test]
    fn one_speakers_spread_subwindows_enroll() {
        // Shared component on axis 0, private component on one axis each, at
        // the ratio that puts mutual cosine in the measured band.
        let windows: Vec<Vec<f32>> = (0..10)
            .map(|k| {
                let mut w = vec![0.0f32; 12];
                w[0] = 0.98;
                w[1 + k] = 1.0;
                let n = w.iter().map(|x| x * x).sum::<f32>().sqrt();
                w.iter().map(|x| x / n).collect()
            })
            .collect();
        let spread = mean_cosine_within(&windows).unwrap();
        assert!(
            (0.45..=0.55).contains(&spread),
            "fixture drifted off the measured band: {spread}"
        );
        assert!(one_enrollable_voice(&windows, ENROLLABLE_COHERENCE, None));
    }

    #[test]
    fn a_single_window_carries_nothing_to_judge_and_passes() {
        assert!(one_enrollable_voice(&[v(4, 0, 1, 0.5)], ENROLLABLE_COHERENCE, None));
        assert!(one_enrollable_voice(&[], ENROLLABLE_COHERENCE, None));
    }
}

#[cfg(test)]
mod identity_award_tests {
    use super::*;

    /// Two clusters clearing the threshold against one enrolled identity: the
    /// exact self-match and a near-twin voice. The closer cluster is named and
    /// the other is left for the user, instead of both collapsing onto one
    /// speaker.
    #[test]
    fn one_identity_names_only_its_closest_cluster() {
        let candidates = vec![(1.000, 5u32, 0usize), (0.802, 3u32, 0usize)];
        let (awarded, yielded) = award_identities(candidates);
        assert_eq!(awarded, vec![(1.000, 5, 0)]);
        assert_eq!(yielded, vec![(0.802, 3, 0)]);
    }

    /// The caller builds candidates cluster by cluster, in vault order. The
    /// closest cluster still takes the identity.
    #[test]
    fn candidate_order_does_not_decide_the_award() {
        let candidates = vec![(0.802, 3u32, 0usize), (1.000, 5u32, 0usize)];
        let (awarded, yielded) = award_identities(candidates);
        assert_eq!(awarded, vec![(1.000, 5, 0)]);
        assert_eq!(yielded, vec![(0.802, 3, 0)]);
    }

    #[test]
    fn a_cluster_that_loses_its_best_identity_stays_unlabeled() {
        // Cluster 3 is closest to identity 0, but cluster 5 is closer still.
        // Identity 1 is a weaker match for cluster 3 than the one it lost, so
        // cluster 3 takes no name.
        let candidates = vec![
            (0.98, 5u32, 0usize),
            (0.80, 3u32, 0usize),
            (0.71, 3u32, 1usize),
        ];
        let (awarded, yielded) = award_identities(candidates);
        assert_eq!(awarded, vec![(0.98, 5, 0)]);
        assert_eq!(yielded, vec![(0.80, 3, 0)]);
    }

    #[test]
    fn a_clusters_own_best_identity_wins_over_a_weaker_pairing() {
        // Cluster 3 matches identity 1 more closely than identity 0, so it is
        // named identity 1 and never contests identity 0 with cluster 5.
        let candidates = vec![
            (0.91, 3u32, 1usize),
            (0.88, 5u32, 0usize),
            (0.64, 3u32, 0usize),
        ];
        let (awarded, yielded) = award_identities(candidates);
        assert_eq!(awarded, vec![(0.91, 3, 1), (0.88, 5, 0)]);
        assert!(yielded.is_empty());
    }

    #[test]
    fn each_cluster_takes_at_most_one_identity() {
        let candidates = vec![(0.90, 1u32, 0usize), (0.85, 1u32, 1usize)];
        let (awarded, _) = award_identities(candidates);
        assert_eq!(awarded, vec![(0.90, 1, 0)]);
    }
}

#[cfg(test)]
mod mixed_gate_probe {
    use super::*;

    /// Every number the one-voice gate reads, for one session on disk, plus
    /// each split side as a wav file to listen to. Runs the shipped path: the
    /// session's own separation, measured over every cluster.
    ///
    /// DAISY_PROBE_SESSION=/path/to/session DAISY_PROBE_OUT=/path/to/dir \
    ///   cargo test -p tauri-app dump_mixed_gate -- --ignored --nocapture
    #[test]
    #[ignore]
    fn dump_mixed_gate() {
        let dir = std::path::PathBuf::from(std::env::var("DAISY_PROBE_SESSION").unwrap());
        let out = std::path::PathBuf::from(
            std::env::var("DAISY_PROBE_OUT").unwrap_or_else(|_| "/tmp".into()),
        );
        std::fs::create_dir_all(&out).unwrap();
        let mb = syncsafe::read(dir.join("manifest.json")).unwrap();
        let manifest: SessionManifest = serde_json::from_slice(&mb).unwrap();
        let mut encoder = Encoder::load().unwrap();
        let floor = ENROLLABLE_COHERENCE;

        let ids: Vec<u32> = manifest.cluster_sides.iter().map(|c| c.cluster_id).collect();
        let sampled: Vec<(u32, Vec<Vec<f32>>)> = ids
            .iter()
            .map(|&cid| (cid, cluster_sample_windows(&dir, cid, &mut encoder)))
            .collect();
        let separation =
            speaker_separation(&sampled.iter().map(|(_, w)| w.clone()).collect::<Vec<_>>());
        println!(
            "session {} clusters {ids:?} on disk mixed={:?} separation={:?}",
            dir.display(),
            manifest.mixed_clusters,
            separation
        );

        for (cid, windows) in &sampled {
            let name = manifest
                .speaker_map
                .iter()
                .find(|s| s.cluster_id == *cid)
                .map(|s| s.display_name.clone())
                .unwrap_or_default();
            let split = split_two_voices(windows, floor, separation);
            let (across, centroid) = match &split {
                Some((l, r)) => {
                    let take = |idx: &[usize]| -> Vec<Vec<f32>> {
                        idx.iter().map(|&i| windows[i].clone()).collect()
                    };
                    let (lv, rv) = (take(l), take(r));
                    let c = match (
                        centroid_of(lv.iter().map(|v| v.as_slice())),
                        centroid_of(rv.iter().map(|v| v.as_slice())),
                    ) {
                        (Some(a), Some(b)) => cosine(&a, &b),
                        _ => 0.0,
                    };
                    (mean_cosine_across(&lv, &rv).unwrap_or(0.0), c)
                }
                None => (0.0, 0.0),
            };
            println!(
                "--- cluster {cid} ({name}): {} window(s) within={:.3} split={:?} across={across:.3} centroid={centroid:.3} MIXED={} ---",
                windows.len(),
                mean_cosine_within(windows).unwrap_or(0.0),
                split.as_ref().map(|(l, r)| (l.len(), r.len())),
                cluster_reads_mixed(*cid, windows, floor, separation)
            );
            // What 2-means would cut, whatever the gates then decide, with the
            // audio behind each side.
            dump_sides(&dir, *cid, &out, &mut encoder);
        }

        println!("--- between-cluster mean pairwise cosine (what two people measure here) ---");
        for (a, wa) in &sampled {
            let row: Vec<String> = sampled
                .iter()
                .map(|(b, wb)| {
                    if a == b {
                        format!("{b}:  -  ")
                    } else {
                        format!("{b}:{:.3}", mean_cosine_across(wa, wb).unwrap_or(0.0))
                    }
                })
                .collect();
            println!("  cluster {a}: {}", row.join("  "));
        }

        // Positive control: clusters that really do hold two people, built by
        // pooling two clusters. Whole pools and a 2-window intrusion, the shape
        // a fused speaker actually takes.
        println!("--- pooled pairs (a two-voice cluster, by construction) ---");
        for a in 0..sampled.len() {
            for b in (a + 1)..sampled.len() {
                for (tag, take_b) in [("whole", usize::MAX), ("intrusion", 2)] {
                    let mut pool = sampled[a].1.clone();
                    pool.extend(sampled[b].1.iter().take(take_b).cloned());
                    println!(
                        "  {}+{} {tag:9}: n={} within={:.3} caught={}",
                        sampled[a].0,
                        sampled[b].0,
                        pool.len(),
                        mean_cosine_within(&pool).unwrap_or(0.0),
                        !one_enrollable_voice(&pool, floor, separation)
                    );
                }
            }
        }
    }

    /// The two sides 2-means cuts a cluster into, each piece named by the
    /// segment it came from and written out as a wav to listen to.
    fn dump_sides(
        dir: &std::path::Path,
        cid: u32,
        out: &std::path::Path,
        encoder: &mut Encoder,
    ) {
        let Ok((all, other)) = cluster_candidates(dir, cid) else { return };
        let (chosen, margin) = choose_sample_segments(&all, &other, ENROLL_CAP_MS, true);
        println!(
            "  cluster {cid}: {} of {} candidate(s) at a {margin} ms margin",
            chosen.len(),
            all.len()
        );
        for &(i, take) in &chosen {
            let c = &all[i];
            println!(
                "    window abs {}–{} dur {} take {take} rms {:.3} floor {:.4}",
                c.abs_start_ms,
                c.abs_start_ms + c.dur,
                c.dur,
                c.rms,
                c.floor
            );
        }

        let mut pieces: Vec<(usize, Vec<i16>)> = Vec::new();
        for (i, take) in chosen {
            let c = &all[i];
            let Some((audio, _)) = voiced_sample(&c.wav, c.start_ms, c.dur, c.floor) else {
                continue;
            };
            let n = (take as usize * voiceprints::SAMPLE_RATE as usize / 1000).min(audio.len());
            for piece in subwindows(&audio[..n], SPLIT_SUBWINDOW_MS) {
                if pieces.len() >= SPLIT_EMBED_CAP {
                    break;
                }
                pieces.push((i, piece.to_vec()));
            }
        }
        let vecs: Vec<Vec<f32>> =
            pieces.iter().filter_map(|(_, p)| encoder.encode_pcm(p).ok()).collect();
        let Some((left, right)) = two_means_partition(&vecs) else { return };
        for k in 0..vecs.len() {
            let c = &all[pieces[k].0];
            println!(
                "  piece {k:2} side {} | seg t={:6.1}s rms={:.3} | {}",
                if right.contains(&k) { "R" } else { "L" },
                c.abs_start_ms as f32 / 1000.0,
                c.rms,
                c.text.chars().take(56).collect::<String>()
            );
        }
        for (name, idx) in [("L", &left), ("R", &right)] {
            let mut buf: Vec<i16> = Vec::new();
            for &k in idx {
                buf.extend_from_slice(&pieces[k].1);
            }
            let path = out.join(format!("cluster{cid}-side{name}.wav"));
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: voiceprints::SAMPLE_RATE,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::create(&path, spec).unwrap();
            for v in &buf {
                w.write_sample(*v).unwrap();
            }
            w.finalize().unwrap();
            println!(
                "  wrote {} ({:.1}s)",
                path.display(),
                buf.len() as f32 / voiceprints::SAMPLE_RATE as f32
            );
        }
    }
}
