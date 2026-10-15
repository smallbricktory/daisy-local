// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Extract speaker embeddings from a session's audio, match them against the
//! vault's known voiceprints, and expose a CRUD API to the Settings →
//! Voiceprints panel.

use crate::error::{AppError, Result};
use crate::state::{AppState, Voiceprint, VaultState};
use recording::manifest::{ClusterSide, SessionManifest, SpeakerSide};
use serde::{Deserialize, Serialize};
use voiceprints::segmentation::{SegmentationModel, StreamSegmenter};
use voiceprints::turns::{label_short_turns, Turns, MIN_EMBED_SECONDS};
use voiceprints::{
    cluster_speakers, cosine, estimate_speaker_count, match_threshold, merge_by_gallery, MIN_EMB_FOR_SPLIT,
    read_pcm_window, Encoder,
};

const ENROLL_CAP_MS: u32 = 30_000; // max 30s of audio per voiceprint extraction

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
        vector_dim: embs.first().map(Vec::len).unwrap_or(0) as u32,
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
    let now = crate::now_unix();
    let mut contact = None;
    update_vault(app, vs, |keys| {
        if let Some(v) = keys.voiceprints.iter_mut().find(|v| v.id == req.id) {
            v.display_name = new_name.clone();
            v.email = email.clone();
            v.updated_at_unix_seconds = Some(now);
            contact = v.contact_id.clone();
            Ok(())
        } else {
            Err(AppError::Config(format!("no voiceprint {}", req.id)))
        }
    })?;
    match contact {
        Some(c) => crate::commands::contacts::rename_contact(app, &c, &new_name, email.as_deref(), now),
        None => Ok(()),
    }
}

/// Deletes a person: every voice of theirs, and their contact with a
/// tombstone, so neither comes back from the phone. Speaker labels keep the
/// name they show.
pub fn delete_voiceprint_impl(app: &AppState, vs: &VaultState, id: &str) -> Result<()> {
    let now = crate::now_unix();
    let contact = {
        let g = vs.keys.lock().unwrap();
        let keys = g.as_ref().ok_or(AppError::VaultLocked)?;
        let vp = keys.voiceprints.iter().find(|v| v.id == id).ok_or_else(|| AppError::Config(format!("no voiceprint {id}")))?;
        vp.contact_id.clone()
    };
    match contact {
        // The contact's delete takes every voice under it.
        Some(c) => crate::commands::contacts::delete_contact(app, vs, &c, now),
        None => update_vault(app, vs, |keys| {
            keys.voiceprints.retain(|v| v.id != id);
            crate::sync::voices::retire(keys, id, now);
            Ok(())
        }),
    }
}

/// Where a sample taken from one cluster of a session came from: the session by the id the phone knows it under, the diarization run that numbered the cluster, and the cluster.
pub(crate) fn sample_source(root: &std::path::Path, session_id: &str, cluster_id: u32) -> crate::sync::voices::SampleKey {
    crate::sync::voices::SampleKey {
        session: Some(
            crate::commands::session::phone_origin(root, session_id)
                .map(|(_, wire)| wire)
                .unwrap_or_else(|| session_id.to_string()),
        ),
        diarization_id: recording::session_meta::current_run(root)
            .filter(|run| run != recording::session_meta::LEGACY_RUN),
        speaker: Some(cluster_id),
        legacy: None,
    }
}

/// Detach the voiceprint link from a session's cluster: clears `voiceprint_id`
/// and `match_confidence` on the matching `SpeakerLabel`, and takes back the
/// sample that cluster gave the voice, if it gave one. The display name stays.
pub fn detach_speaker_voiceprint_impl(
    app: &AppState,
    vs: &VaultState,
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
    let mut voice: Option<String> = None;
    session
        .update_manifest(|m| {
            for s in m.speaker_map.iter_mut() {
                if s.cluster_id == cluster_id && (s.voiceprint_id.is_some() || s.match_confidence.is_some()) {
                    voice = s.voiceprint_id.take().or(voice.take());
                    s.match_confidence = None;
                    changed = true;
                }
            }
        })
        .map_err(|e| AppError::Recording(e.to_string()))?;
    if let Some(voice) = voice {
        let key = sample_source(&root, session_id, cluster_id);
        update_vault(app, vs, |keys| {
            crate::sync::voices::take_back(keys, &voice, &key, crate::now_unix());
            Ok(())
        })?;
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
    let source = sample_source(&root, &req.session_id, req.cluster_id);
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

        let sample = crate::sync::voices::Sample {
            key: source.clone(),
            model: crate::sync::voices::MODEL.to_string(),
            v: crate::sync::voices::encode_vector(&vec),
            at: now,
            extra: Default::default(),
        };
        if let Some(&primary) = matches.first() {
            // Remove duplicate entries in descending index order.
            let mut absorbed: Vec<Voiceprint> = Vec::new();
            for &idx in matches.iter().skip(1).rev() {
                absorbed.push(keys.voiceprints.remove(idx));
            }
            for other in &absorbed {
                crate::sync::voices::retire(keys, &other.id, now);
            }
            let vp = &mut keys.voiceprints[primary];
            vp.contact_id = Some(contact_for_vp.clone());
            for other in absorbed {
                for s in other.samples {
                    crate::sync::voices::add_sample(&mut vp.samples, s);
                }
                for sid in other.session_ids {
                    if !vp.session_ids.contains(&sid) {
                        vp.session_ids.push(sid);
                    }
                }
            }
            crate::sync::voices::add_sample(&mut vp.samples, sample);
            vp.display_name = display_for_vp.clone();
            vp.email = email_for_vp.clone();
            vp.updated_at_unix_seconds = Some(now);
            if !vp.session_ids.contains(&session_id_for_vp) {
                vp.session_ids.push(session_id_for_vp.clone());
            }
        } else {
            keys.voiceprints.push(Voiceprint {
                id: new_id.clone(),
                display_name: display_for_vp.clone(),
                email: email_for_vp.clone(),
                vector: Vec::new(),
                vectors: Vec::new(),
                samples: vec![sample],
                created_at_unix_seconds: now,
                updated_at_unix_seconds: Some(now),
                session_ids: vec![session_id_for_vp.clone()],
                contact_id: Some(contact_for_vp.clone()),
                extra: Default::default(),
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
    // The name and the person travel; the voiceprint behind them is this
    // machine's own and stays on the manifest row.
    recording::session_meta::name_speaker(&root, cluster_id, &display_name, Some(enrolled_contact_id.clone()), crate::now_unix())
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
/// `match_threshold()`. Idempotent. Runs at background priority.
pub fn rematch_session_speakers_impl(
    app: &AppState,
    vs: &VaultState,
    session_id: &str,
) -> Result<u32> {
    transcript::cpu::run_in_background(|| run_rematch(app, vs, session_id))
}

fn run_rematch(
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
    // Shares the diarization permit.
    let _diarize = diarize_permit();

    let mut encoder = Encoder::load_background()
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
                .iter()
                .map(|e| cosine(&vec, e))
                .fold(f32::MIN, f32::max);
            if s >= match_threshold() {
                candidates.push((s, cluster_id, vi));
            }
        }
    }
    let (mut awards, yielded) = award_identities(candidates);
    // A cluster that lost an identity to a closer one stays unlabeled unless its windows read as one voice against the winner's.
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
            // Either side without a measurable spread leaves the cluster unlabeled.
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
    // The name travels and the score does not.
    recording::session_meta::name_speaker(session_dir, cluster_id, &vp.display_name, None, crate::now_unix())
        .map_err(|e| AppError::Recording(e.to_string()))?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct DiarizeResult {
    /// One above the highest cluster id this run assigned.
    pub speakers: u32,
    /// Speakers this run grouped, across the tracks it read. Clusters kept
    /// from an earlier run on the other track are outside it.
    pub clustered: u32,
    /// Speakers this run grouped on the track the count applied to. `None`
    /// exactly when `count_source` is.
    pub count: Option<u32>,
    /// Segments that carry one of this run's speaker ids, after splitting.
    pub segments_labeled: u32,
    /// Transcript segments this run had to work with on the track(s) it read.
    /// Zero means the session has nothing transcribed to group.
    pub segments_seen: u32,
    /// What set the speaker count. `None` when nothing was grouped, or a
    /// solo-mic session took one speaker from the recording setup.
    pub count_source: Option<CountSource>,
}

pub use recording::manifest::CountSource;

/// An enrolled voice counts as present when it is the best match for at
/// least this many turns...
const VOICE_PRESENT_TURNS: usize = 3;
/// ...holding at least this much speech.
const VOICE_PRESENT_MS: u32 = 10_000;
/// Present voices covering at least this share of the embedded speech set
/// the count; below it they are a floor under the next source.
const VOICE_FULL_COVERAGE: f32 = 0.8;

/// Enrolled voices heard in one track's turns.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct VoicePresence {
    /// Identities that pass `VOICE_PRESENT_TURNS` and `VOICE_PRESENT_MS`.
    present: usize,
    /// Share of the embedded turns' speech whose best match is one of them.
    coverage: f32,
}

/// The gallery identity (index) whose closest sample is nearest `emb`, when
/// that cosine reaches `threshold`.
fn best_identity(emb: &[f32], gallery: &[Vec<Vec<f32>>], threshold: f32) -> Option<usize> {
    gallery
        .iter()
        .enumerate()
        .map(|(i, samples)| (i, samples.iter().map(|s| cosine(emb, s)).fold(f32::MIN, f32::max)))
        .filter(|&(_, score)| score >= threshold)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Presence from each embedded turn's `(duration_ms, best identity)`.
fn voice_presence(turns: &[(u32, Option<usize>)]) -> VoicePresence {
    let total: u64 = turns.iter().map(|&(ms, _)| ms as u64).sum();
    let mut by_identity: std::collections::BTreeMap<usize, (usize, u64)> = Default::default();
    for &(ms, who) in turns {
        if let Some(who) = who {
            let e = by_identity.entry(who).or_default();
            e.0 += 1;
            e.1 += ms as u64;
        }
    }
    let present: Vec<u64> = by_identity
        .values()
        .filter(|(n, ms)| *n >= VOICE_PRESENT_TURNS && *ms >= VOICE_PRESENT_MS as u64)
        .map(|(_, ms)| *ms)
        .collect();
    let covered: u64 = present.iter().sum();
    VoicePresence {
        present: present.len(),
        coverage: if total == 0 { 0.0 } else { covered as f32 / total as f32 },
    }
}

/// Everything the speaker count can come from, highest priority first.
#[derive(Debug, Clone, Copy, Default)]
struct CountInputs {
    typed: Option<usize>,
    voices: VoicePresence,
    expected: Option<usize>,
    attendees: Option<usize>,
}

/// The speaker count and its source. A typed number wins outright. Enrolled
/// voices covering `VOICE_FULL_COVERAGE` of the speech set the count;
/// fewer make their number a floor, and the next source (expected
/// speakers, attendees, then `auto`) sets the total above it.
fn resolve_count(inputs: &CountInputs, auto: impl FnOnce() -> usize) -> (usize, CountSource) {
    if let Some(n) = inputs.typed {
        return (n, CountSource::Typed);
    }
    let floor = inputs.voices.present;
    if floor > 0 && inputs.voices.coverage >= VOICE_FULL_COVERAGE {
        return (floor, CountSource::Voiceprints);
    }
    if let Some(n) = inputs.expected {
        return (n.max(floor), CountSource::Expected);
    }
    if let Some(n) = inputs.attendees {
        return (n.max(floor), CountSource::Attendees);
    }
    (auto().max(floor).max(1), CountSource::Auto)
}

/// The number a person stated, when `source` is one they set (`Typed` or
/// `Expected`). `resolve_count` may return more than this for `Expected`, when
/// enrolled voices floor the count above it.
fn stated_count(inputs: &CountInputs, source: CountSource) -> Option<u32> {
    match source {
        CountSource::Typed => inputs.typed,
        CountSource::Expected => inputs.expected,
        _ => None,
    }
    .map(|n| n as u32)
}

/// Fewest clusters the same-voice merge may leave: the count itself when
/// someone stated it or the voiceprints set it; otherwise the enrolled voices
/// heard, and at least one.
fn merge_floor(k: usize, source: CountSource, voices_present: usize) -> usize {
    match source {
        CountSource::Typed | CountSource::Expected | CountSource::Voiceprints => k,
        CountSource::Attendees | CountSource::Auto => voices_present.max(1),
    }
}

/// Attendees a count can rest on: everyone when the count applies to the
/// mic, those with role Other when it applies to the system track. `None`
/// when there are none.
fn attendee_count(attendees: &[recording::manifest::Attendee], on_mic: bool) -> Option<usize> {
    let n = attendees
        .iter()
        .filter(|a| on_mic || a.role == recording::manifest::AttendeeRole::Other)
        .count();
    (n > 0).then_some(n)
}

/// True when the speaker count applies to the mic: on a mic-only run, and on
/// a both-tracks run whose system track holds no speech. Otherwise it applies
/// to the system track, and the mic estimates its own.
fn count_on_mic(effective: DiarizeScope, sys_has_speech: bool) -> bool {
    match effective {
        DiarizeScope::Mic => true,
        DiarizeScope::Others => false,
        DiarizeScope::Both => !sys_has_speech,
    }
}

/// The attendee count a run with `scope` would rest on, for the count
/// field's placeholder. Whether the system track holds speech is read from
/// the transcript; the run itself decides from the turns it embeds.
pub fn attendee_count_hint_impl(
    app: &AppState,
    session_id: &str,
    scope: Option<DiarizeScope>,
) -> Result<Option<u32>> {
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    let manifest: SessionManifest = serde_json::from_slice(&syncsafe::read(root.join("manifest.json"))?)
        .map_err(|e| AppError::Config(format!("parse manifest: {e}")))?;
    let sys_has_speech = crate::commands::session_store::preferred_transcript_path(&root)
        .and_then(|p| syncsafe::read(p).ok())
        .and_then(|b| serde_json::from_slice::<transcript::SessionTranscript>(&b).ok())
        .is_some_and(|st| !track_segment_positions(&st, DiarizeTrackSelect::System).is_empty());
    let effective = resolve_scope(scope_for(&manifest, scope), manifest.single_local_speaker, !sys_has_speech);
    Ok(attendee_count(&manifest.attendees, count_on_mic(effective, sys_has_speech)).map(|n| n as u32))
}

/// How the session's last diarization run settled its speaker count; `None`
/// when no run recorded one.
pub fn diarize_count_impl(
    app: &AppState,
    session_id: &str,
) -> Result<Option<recording::manifest::DiarizeCount>> {
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    let manifest: SessionManifest = serde_json::from_slice(&syncsafe::read(root.join("manifest.json"))?)
        .map_err(|e| AppError::Config(format!("parse manifest: {e}")))?;
    Ok(manifest.diarize_count)
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

/// The scope a call clusters. A phone recording is one microphone in a room, so it is the mic.
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

/// Assigned cluster ids are exactly `0..count`.
fn debug_assert_dense_ids(assign: &[Option<u32>], count: u32, what: &str) {
    if cfg!(debug_assertions) {
        let ids: std::collections::BTreeSet<u32> =
            assign.iter().flatten().copied().collect();
        let expect: std::collections::BTreeSet<u32> = (0..count).collect();
        debug_assert_eq!(ids, expect, "{what}: cluster ids not dense 0..{count}");
    }
}

/// One track's clustering: the positions clustered, each one's cluster id
/// (`None` for unassigned), and the cluster count.
type Clustered<P> = (Vec<P>, Vec<Option<u32>>, u32);

/// Assemble one diarize run's combined output from the already-clustered
/// system track plus the mic clustering. `cluster_mic` runs only for the
/// Both and Mic arms. Returns `(positions, assignments,
/// cluster_sides, surviving_ids)`; `surviving_ids` are the untouched track's
/// existing cluster ids — fresh ids start above them, and their
/// speaker_map/cluster_sides entries survive the manifest update (empty for
/// Both, which re-clusters everything).
fn apply_scope<P>(
    st: &transcript::SessionTranscript,
    effective: DiarizeScope,
    sys: Clustered<P>,
    cluster_mic: impl FnOnce() -> Result<Clustered<P>>,
) -> Result<(Vec<P>, Vec<Option<u32>>, Vec<ClusterSide>, std::collections::BTreeSet<u32>)> {
    let (sys_positions, sys_assign, sys_clusters) = sys;
    debug_assert_dense_ids(&sys_assign, sys_clusters, "system");
    let mut cluster_sides: Vec<ClusterSide> = Vec::new();
    let mut surviving_ids: std::collections::BTreeSet<u32> = Default::default();
    let mut all_positions: Vec<P>;
    let all_assign: Vec<Option<u32>>;
    match effective {
        DiarizeScope::Both => {
            // Cluster the mic (room) track too, with ids offset above the
            // system clusters.
            let (mic_positions, mic_assign_local, mic_clusters) =
                cluster_mic()?;
            debug_assert_dense_ids(&mic_assign_local, mic_clusters, "mic");
            let (offset, sides) = merge_side_ids(sys_clusters, mic_clusters);
            cluster_sides = sides;
            all_positions = sys_positions;
            let mut assign = sys_assign;
            all_positions.extend(mic_positions);
            assign.extend(mic_assign_local.iter().map(|a| a.map(|id| id + offset)));
            all_assign = assign;
        }
        DiarizeScope::Mic => {
            // Cluster the mic track; clusters are tagged Remote. System-track
            // clusters (if any) survive untouched; fresh ids start above them.
            let (mic_positions, mic_assign, mic_clusters) = cluster_mic()?;
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

/// Serializes on-device diarization.
fn diarize_permit() -> std::sync::MutexGuard<'static, ()> {
    static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Diarize a session. Runs at background priority within the finalize CPU
/// budget. `typed_speakers` is a count the user typed (0 or `None`: none);
/// `gallery` is the vault's voiceprints, which can set the count.
pub fn diarize_session_impl(
    app: &AppState,
    session_id: &str,
    typed_speakers: Option<u32>,
    scope: Option<DiarizeScope>,
    gallery: &[Voiceprint],
) -> Result<DiarizeResult> {
    diarize_in_background(app, session_id, typed_speakers, scope, gallery, None)
}

/// Speaker turns a caller supplies in place of the segmentation model's: per
/// side, transcript chunk index to chunk-relative `(start_s, end_s)` spans in
/// time order. A chunk with no entry has no turns.
#[derive(Debug, Clone, Default)]
pub struct SuppliedTurns {
    pub system: std::collections::BTreeMap<u32, Vec<(f64, f64)>>,
    pub mic: std::collections::BTreeMap<u32, Vec<(f64, f64)>>,
    /// Stretches where several people talk at once, in the same shape.
    pub system_overlaps: std::collections::BTreeMap<u32, Vec<(f64, f64)>>,
    pub mic_overlaps: std::collections::BTreeMap<u32, Vec<(f64, f64)>>,
    /// Called with the session folder once the turns are taken, before
    /// clustering.
    pub after_turns: Option<fn(&std::path::Path)>,
    /// The mic side's turns come from the segmentation model; `mic` and
    /// `mic_overlaps` are ignored.
    pub segment_mic: bool,
    /// Embeddings of system-track turns already computed, by
    /// [`TurnKey`]; turns found here are not embedded again.
    pub system_embeddings: std::collections::HashMap<TurnKey, Vec<f32>>,
}

/// Identifies one turn's embedding: chunk index, turn start (ms, chunk
/// relative) and the milliseconds embedded from there.
pub type TurnKey = (u32, u32, u32);

/// Milliseconds a turn of `(start_s, end_s)` spans in its chunk, as the
/// diarizer rounds them.
pub fn turn_ms(start_s: f64, end_s: f64) -> (u32, u32) {
    let ms = |s: f64| (s * 1000.0).round().max(0.0) as u32;
    (ms(start_s), ms(end_s))
}

/// The key of the stretch embedded for a turn of chunk `chunk`, or `None`
/// when the turn is too short to embed.
pub fn turn_embed_key(chunk: u32, start_ms: u32, end_ms: u32) -> Option<TurnKey> {
    let dur = end_ms.checked_sub(start_ms)?;
    ((dur as f64) >= MIN_EMBED_SECONDS * 1000.0).then(|| (chunk, start_ms, dur.min(TURN_EMBED_CAP_MS)))
}

/// The embedding of `len_ms` of `wav` from `start_ms`; `None` when the
/// stretch is unreadable, shorter than half a second or below the silence
/// floor.
pub fn embed_turn_audio(wav: &std::path::Path, start_ms: u32, len_ms: u32, encoder: &mut Encoder) -> Option<Vec<f32>> {
    let pcm = read_pcm_window(wav, start_ms, start_ms + len_ms, len_ms).ok()?;
    if pcm.len() < (voiceprints::SAMPLE_RATE as usize / 2) || pcm_rms_norm(&pcm) < SAMPLE_SILENCE_FLOOR {
        return None;
    }
    encoder.encode_pcm(&pcm).ok()
}

/// [`diarize_session_impl`] on `turns` instead of the segmentation model's;
/// everything after turn detection runs as shipped.
pub fn diarize_session_with_turns_impl(
    app: &AppState,
    session_id: &str,
    typed_speakers: Option<u32>,
    scope: Option<DiarizeScope>,
    gallery: &[Voiceprint],
    turns: &SuppliedTurns,
) -> Result<DiarizeResult> {
    diarize_in_background(app, session_id, typed_speakers, scope, gallery, Some(turns))
}

fn diarize_in_background(
    app: &AppState,
    session_id: &str,
    typed_speakers: Option<u32>,
    scope: Option<DiarizeScope>,
    gallery: &[Voiceprint],
    supplied: Option<&SuppliedTurns>,
) -> Result<DiarizeResult> {
    let r = transcript::cpu::run_in_background(|| {
        let _blas_cap = crate::openblas::finalize_cap();
        run_diarize(app, session_id, typed_speakers, scope, gallery, supplied)
    });
    if r.is_ok() {
        clear_diarization_unavailable(&app.profile.session_path(session_id));
    }
    r
}

/// Clears `diarization_unavailable` on the session at `root` when set.
fn clear_diarization_unavailable(root: &std::path::Path) {
    let Ok(mut session) = recording::session::Session::load(root) else { return };
    if !session.manifest().diarization_unavailable {
        return;
    }
    if let Err(e) = session.update_manifest(|m| m.diarization_unavailable = false) {
        log::warn!("clearing diarization_unavailable for {}: {e}", root.display());
    }
}

/// True for a diarization error that comes from the speaker-encoder model
/// itself being missing or unreadable (see `voiceprints::Encoder::load`).
pub fn voice_model_unusable(e: &AppError) -> bool {
    match e {
        AppError::Config(m) => {
            m.starts_with("voiceprint model: voiceprint model missing")
                || m.starts_with("voiceprint model: voiceprint model unreadable")
        }
        _ => false,
    }
}

/// The speaker-encoder and segmentation models' state, for the voice-grouping banner:
/// `state` is `ready`, `missing` or `unreadable`; `error` carries the OS
/// error of an unreadable one.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct VoiceModelStatus {
    pub state: &'static str,
    pub path: String,
    pub error: Option<String>,
}

pub fn voice_model_status_impl() -> VoiceModelStatus {
    let (path, state) = voiceprints::model_state();
    let path = path.display().to_string();
    match state {
        syncsafe::FileState::Present => VoiceModelStatus { state: "ready", path, error: None },
        syncsafe::FileState::Missing => VoiceModelStatus { state: "missing", path, error: None },
        syncsafe::FileState::Unreadable(e) => {
            VoiceModelStatus { state: "unreadable", path, error: Some(e.to_string()) }
        }
    }
}

/// The speaker-encoder or segmentation model failed to load; the message
/// `voice_model_unusable` recognises.
fn voice_model_error(e: voiceprints::VpError) -> AppError {
    AppError::Config(format!("voiceprint model: {e}"))
}

/// What one run decided for one side of the transcript.
enum SideLabels {
    /// Every segment takes this cluster.
    Whole(u32),
    /// Labelled speaker turns per transcript chunk (index into `st.chunks`),
    /// chunk-relative, in time order.
    Turns(std::collections::HashMap<usize, Vec<transcript::split::Turn>>),
}

fn run_diarize(
    app: &AppState,
    session_id: &str,
    typed_speakers: Option<u32>,
    scope: Option<DiarizeScope>,
    gallery: &[Voiceprint],
    supplied: Option<&SuppliedTurns>,
) -> Result<DiarizeResult> {
    use std::time::{Duration, Instant};
    let root = app.profile.session_path(session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(session_id.into()));
    }
    // One diarization at a time across the process.
    let _diarize = diarize_permit();

    let st_path = crate::commands::session_store::preferred_transcript_path(&root)
        .ok_or_else(|| AppError::Config("no transcript to diarize".into()))?;
    let manifest: SessionManifest = serde_json::from_slice(&syncsafe::read(root.join("manifest.json"))?)
        .map_err(|e| AppError::Config(format!("parse manifest: {e}")))?;
    let st_read = syncsafe::read(&st_path)?;
    let mut st: transcript::SessionTranscript = serde_json::from_slice(&st_read)
        .map_err(|e| AppError::Config(format!("parse transcript: {e}")))?;

    // Every path loads the speaker encoder, so it is checked before any work.
    voiceprints::require_model_file(&voiceprints::model_path()).map_err(voice_model_error)?;

    let scope = scope_for(&manifest, scope);

    // What the run has to work with, before any embedding.
    let segments_seen = segments_in_scope(&st, scope, manifest.single_local_speaker) as u32;

    let typed = typed_speakers.map(|n| n as usize).filter(|&n| n > 0).or_else(|| {
        std::env::var("DAISY_DIARIZE_SPEAKERS").ok().and_then(|v| v.parse::<usize>().ok()).filter(|&n| n > 0)
    });
    let expected = manifest.expected_speakers.map(|n| n as usize).filter(|&n| n > 0);
    let gallery: Vec<Vec<Vec<f32>>> =
        gallery.iter().map(Voiceprint::embeddings).filter(|e| !e.is_empty()).collect();
    let floor = coherence_floor(
        crate::settings::Settings::load_or_default(&app.profile.settings_path())
            .voice_match_tolerance,
    );

    let mut sides: Vec<(DiarizeTrackSelect, SideLabels)> = Vec::new();
    let mut cluster_sides: Vec<ClusterSide> = Vec::new();
    let mut count_source: Option<CountSource> = None;
    // The number a person stated, when the count came from one.
    let mut requested: Option<u32> = None;
    // The track the count applied to; `None` on the fast paths, which group
    // one side whole.
    let mut count_select: Option<DiarizeTrackSelect> = None;
    // Turns and their cluster ids per re-clustered side; the voice-sample
    // windows come from these.
    let mut turn_sides: Vec<(TrackTurns, Vec<Option<u32>>)> = Vec::new();
    let mut encoder: Option<Encoder> = None;

    // Ids that survive this run (set per scope arm below). On a single-track
    // re-run (scope Mic or Others) the other track's clusters are preserved:
    // fresh ids start above them, and their speaker_map/cluster_sides entries
    // survive the manifest update below.
    let mut surviving_ids: std::collections::BTreeSet<u32> = Default::default();

    // Fast path: a count of exactly one on a solo-local recording labels every
    // system-track segment as that one person, with no audio read. Runs only
    // when the (implicit or explicit) scope is Others, and only when no
    // higher-priority source can raise the count.
    let pinned_one = match typed {
        Some(n) => n == 1,
        None => gallery.is_empty() && expected == Some(1),
    };
    if matches!(scope, None | Some(DiarizeScope::Others))
        && pinned_one
        && manifest.single_local_speaker
        && !track_segment_positions(&st, DiarizeTrackSelect::System).is_empty()
    {
        log::info!("diarize {session_id}: 1-speaker fast path (no embedding)");
        surviving_ids = ids_on_track(&st, true); // mic clusters untouched
        let base = surviving_ids.iter().next_back().map_or(0, |m| m + 1);
        cluster_sides.push(ClusterSide { cluster_id: base, side: SpeakerSide::Remote });
        sides.push((DiarizeTrackSelect::System, SideLabels::Whole(base)));
        count_source = Some(if typed.is_some() { CountSource::Typed } else { CountSource::Expected });
    }

    // Mic mirror of the fast path: a solo-local session with no system-track
    // speech is one person on the mic. Every mic segment gets the same Room
    // cluster, with no audio read.
    let mic_fast_ok = matches!(scope, None | Some(DiarizeScope::Mic))
        && manifest.single_local_speaker
        && matches!(typed.or(expected), None | Some(1));
    if sides.is_empty()
        && mic_fast_ok
        && track_segment_positions(&st, DiarizeTrackSelect::System).is_empty()
        && !track_segment_positions(&st, DiarizeTrackSelect::Mic).is_empty()
    {
        log::info!("diarize {session_id}: solo mic fast path (no embedding)");
        cluster_sides.push(ClusterSide { cluster_id: 0, side: SpeakerSide::Room });
        sides.push((DiarizeTrackSelect::Mic, SideLabels::Whole(0)));
        count_source = typed.map(|_| CountSource::Typed).or(expected.map(|_| CountSource::Expected));
    }

    // Full path: speaker turns from the segmentation model, one embedding per
    // turn, k-means at the resolved count, then split/merge hygiene.
    if sides.is_empty() {
        let from_model = |select: DiarizeTrackSelect| match supplied {
            None => true,
            Some(s) => select == DiarizeTrackSelect::Mic && s.segment_mic,
        };
        if from_model(DiarizeTrackSelect::System) || from_model(DiarizeTrackSelect::Mic) {
            voiceprints::require_model_file(&voiceprints::segmentation::segmentation_model_path())
                .map_err(voice_model_error)?;
        }
        let seg_t = Instant::now();
        let mut seg_model: Option<SegmentationModel> = None;
        let mut turns_of = |select: DiarizeTrackSelect| -> Result<TrackTurns> {
            match supplied {
                Some(s) if !from_model(select) => Ok(supplied_track(&st, &manifest, &root, select, s)),
                _ => {
                    let model = match seg_model.as_mut() {
                        Some(m) => m,
                        None => seg_model.insert(SegmentationModel::load_background().map_err(voice_model_error)?),
                    };
                    segment_track(&st, &manifest, &root, select, model)
                }
            }
        };
        let sys = if scope == Some(DiarizeScope::Mic) {
            TrackTurns::new(DiarizeTrackSelect::System)
        } else {
            turns_of(DiarizeTrackSelect::System)?
        };
        let sys_empty = !sys.turns.iter().any(TrackTurn::embeddable);
        let effective = resolve_scope(scope, manifest.single_local_speaker, sys_empty);
        let mic = if matches!(effective, DiarizeScope::Mic | DiarizeScope::Both) {
            turns_of(DiarizeTrackSelect::Mic)?
        } else {
            TrackTurns::new(DiarizeTrackSelect::Mic)
        };
        drop(seg_model);
        let seg_s = seg_t.elapsed();
        if let Some(hook) = supplied.and_then(|s| s.after_turns) {
            hook(&root);
        }

        let embed_t = Instant::now();
        let mut enc = Encoder::load_background().map_err(voice_model_error)?;
        let (mut sys, mut mic) = (sys, mic);
        let done = supplied.map(|s| &s.system_embeddings);
        let reused = embed_turns(&mut sys, &mut enc, done);
        embed_turns(&mut mic, &mut enc, None);
        if supplied.is_some() {
            log::info!(
                "diarize {session_id}: {reused} of {} system turn embedding(s) came from the live pass",
                sys.embedded_count()
            );
        }
        let embed_s = embed_t.elapsed();

        // The count is resolved on the track it applies to, and the other
        // track estimates its own.
        let on_mic = count_on_mic(effective, sys.embedded_count() > 0);
        count_select = Some(if on_mic { DiarizeTrackSelect::Mic } else { DiarizeTrackSelect::System });
        let k_track = if on_mic { &mic } else { &sys };
        let voices = voice_presence(
            &k_track
                .turns
                .iter()
                .filter_map(|t| {
                    let emb = t.emb.as_ref()?;
                    Some((t.end_ms - t.start_ms, best_identity(emb, &gallery, match_threshold())))
                })
                .collect::<Vec<_>>(),
        );
        let inputs = CountInputs {
            typed,
            voices,
            expected,
            attendees: attendee_count(&manifest.attendees, on_mic),
        };
        let (k, source) = resolve_count(&inputs, || k_track.estimate_count());
        let merge_min = merge_floor(k, source, voices.present);
        count_source = Some(source);
        requested = stated_count(&inputs, source);
        log::info!(
            "diarize {session_id}: {effective:?} scope, {k} speaker(s) on the {} from {source:?} \
             ({} enrolled voice(s) over {:.0}% of the speech), merge floor {merge_min}",
            if on_mic { "mic" } else { "system track" },
            voices.present,
            voices.coverage * 100.0
        );

        // On a solo-local recording the mic voice is the bleed anchor: system
        // clusters that read as it are folded together.
        let anchor: Option<Vec<f32>> = if manifest.single_local_speaker
            && effective != DiarizeScope::Mic
            && sys.turns.iter().any(|t| t.emb.is_some())
        {
            let (_p, mic_embedded) =
                collect_track_embeddings(&st, &manifest, &root, DiarizeTrackSelect::Mic, &mut enc);
            centroid_of(mic_embedded.iter().map(|(_, v)| v.as_slice()))
        } else {
            None
        };

        let cluster_t = Instant::now();
        let mut hygiene = Duration::ZERO;
        // The counted track clusters at the count; the other estimates its own.
        let (sys_k, sys_floor, mic_k, mic_floor) =
            if on_mic { (None, 1, Some(k), merge_min) } else { (Some(k), merge_min, None, 1) };
        let sys_clustered: Clustered<(usize, usize)> = {
            let (assign, count) = if effective == DiarizeScope::Mic {
                (vec![None; sys.turns.len()], 0)
            } else {
                cluster_track(&sys, sys_k, sys_floor, anchor.as_deref(), &mut enc, floor, &mut hygiene)
            };
            ((0..sys.turns.len()).map(|i| (0, i)).collect(), assign, count)
        };
        let (positions, assign, sides_out, surviving) =
            apply_scope(&st, effective, sys_clustered, || {
                let (assign, count) = cluster_track(&mic, mic_k, mic_floor, None, &mut enc, floor, &mut hygiene);
                Ok(((0..mic.turns.len()).map(|i| (1, i)).collect(), assign, count))
            })?;
        let cluster_s = cluster_t.elapsed().saturating_sub(hygiene);
        log::info!(
            "diarize {session_id}: segmentation {:.1}s, embedding {:.1}s ({} turn(s)), \
             clustering {:.1}s, split/merge {:.1}s",
            seg_s.as_secs_f32(),
            embed_s.as_secs_f32(),
            sys.embedded_count() + mic.embedded_count(),
            cluster_s.as_secs_f32(),
            hygiene.as_secs_f32()
        );
        cluster_sides = sides_out;
        surviving_ids = surviving;

        let mut labels = [vec![None; sys.turns.len()], vec![None; mic.turns.len()]];
        for (&(side, i), a) in positions.iter().zip(&assign) {
            labels[side][i] = *a;
        }
        let [sys_labels, mic_labels] = labels;
        if effective != DiarizeScope::Mic {
            turn_sides.push((sys, sys_labels));
        }
        if effective != DiarizeScope::Others {
            turn_sides.push((mic, mic_labels));
        }
        for (tt, labels) in &turn_sides {
            sides.push((tt.select, SideLabels::Turns(turn_map(tt, labels))));
        }
        encoder = Some(enc);
    }

    let no_labels = sides.is_empty()
        || turn_sides.len() == sides.len() && turn_sides.iter().all(|(_, l)| l.iter().all(Option::is_none));
    let nothing = DiarizeResult { speakers: 0, clustered: 0, count: None, segments_labeled: 0, segments_seen, count_source: None };
    if no_labels {
        return Ok(nothing);
    }

    relabel_transcript(&mut st, &sides);

    // A cluster no segment ended up carrying (its turns held only words that
    // went to a neighbour, or no transcribed speech) is dropped, and the
    // fresh ids are renumbered densely in their original order.
    let used: std::collections::BTreeSet<u32> = side_segments(&st, &sides)
        .filter_map(|seg| seg.speaker_id)
        .collect();
    let first_fresh = cluster_sides.iter().map(|c| c.cluster_id).min().unwrap_or(0);
    let mut remap: std::collections::HashMap<u32, u32> = Default::default();
    for c in &cluster_sides {
        if used.contains(&c.cluster_id) {
            let next = first_fresh + remap.len() as u32;
            remap.insert(c.cluster_id, next);
        }
    }
    cluster_sides.retain(|c| remap.contains_key(&c.cluster_id));
    for c in cluster_sides.iter_mut() {
        c.cluster_id = remap[&c.cluster_id];
    }
    let mut segments_labeled = 0u32;
    for_side_segments(&mut st, &sides, |seg| {
        seg.speaker_id = seg.speaker_id.and_then(|id| remap.get(&id).copied());
        segments_labeled += seg.speaker_id.is_some() as u32;
    });
    #[cfg(test)]
    diarize_gate::record_turns(&st, &turn_sides, &remap);
    if segments_labeled == 0 {
        return Ok(nothing);
    }

    // The windows voice sampling reads for each fresh cluster: its turns,
    // pulled in from both ends, on the session's audio timeline.
    let sample_spans: Vec<recording::manifest::ClusterSampleSpans> = {
        let mut by_cluster: std::collections::BTreeMap<u32, Vec<(u32, u32)>> = Default::default();
        for (tt, labels) in &turn_sides {
            for (turn, label) in tt.turns.iter().zip(labels) {
                let (Some(id), Some((start_ms, dur_ms, _))) = (label.and_then(|l| remap.get(&l)), turn.window)
                else {
                    continue;
                };
                let abs = turn.offset_ms.saturating_add(start_ms);
                by_cluster.entry(*id).or_default().push((abs, abs.saturating_add(dur_ms)));
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

    // Speakers grouped on the counted track: on a fast path, the one side
    // grouped; otherwise the fresh ids its turns carry.
    let count = count_source.map(|_| match count_select {
        None => cluster_sides.len() as u32,
        Some(sel) => turn_sides
            .iter()
            .filter(|(tt, _)| tt.select == sel)
            .flat_map(|(_, labels)| labels.iter().flatten())
            .filter_map(|id| remap.get(id))
            .collect::<std::collections::BTreeSet<_>>()
            .len() as u32,
    });

    // The transcript is written back whole, so a run whose transcript changed
    // on disk since it was read stops here, before writing anything.
    transcript_unchanged(&root, &st_path, &st_read)?;

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
                // Windows kept from a run on the wall-clock timeline would
                // read the wrong audio next to these; those clusters sample
                // their segment spans instead.
                if !m.sample_spans_on_audio_timeline {
                    m.cluster_sample_spans.clear();
                }
                m.cluster_sample_spans.retain(|c| surviving_ids.contains(&c.cluster_id));
                m.cluster_sample_spans.extend(sample_spans);
                m.sample_spans_on_audio_timeline = true;
                m.speaker_map.retain(|s| surviving_ids.contains(&s.cluster_id));
                // A successful re-process clears the "Interrupted — recovered"
                // badge.
                m.interrupted = false;
                m.diarize_count = count_source.zip(count).map(|(source, found)| {
                    recording::manifest::DiarizeCount { source, requested, found }
                });
            })
            .map_err(|e| AppError::Recording(e.to_string()))?;
        recording::speakers::retain(&root, &surviving_ids.iter().copied().collect::<Vec<_>>()).map_err(|e| AppError::Recording(e.to_string()))?;
        // A fresh run id is written; the names of the clusters this run kept come over to it.
        let kept: Vec<u32> = surviving_ids.iter().copied().collect();
        recording::session_meta::start_run(&root, &kept).map_err(|e| AppError::Recording(e.to_string()))?;
    }

    let speakers = fresh_ids.iter().copied().max().map_or(0, |m| m + 1);
    syncsafe::write_atomic(&st_path, serde_json::to_vec_pretty(&st)?)?;

    let mut encoder = match encoder {
        Some(e) => e,
        None => Encoder::load_background().map_err(voice_model_error)?,
    };
    // Each fresh cluster's enrollment sample is judged for a second voice.
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
    Ok(DiarizeResult {
        speakers,
        clustered: fresh_ids.len() as u32,
        count,
        segments_labeled,
        segments_seen,
        count_source,
    })
}

/// What a diarize run reports when the transcript it read changed on disk
/// before the run wrote its labels.
const TRANSCRIPT_CHANGED: &str =
    "the transcript changed while voices were being grouped; group the voices again";

/// Errors with [`TRANSCRIPT_CHANGED`] unless `st_path` is still the session's
/// preferred transcript and holds exactly the bytes `read`.
fn transcript_unchanged(root: &std::path::Path, st_path: &std::path::Path, read: &[u8]) -> Result<()> {
    let same_file = crate::commands::session_store::preferred_transcript_path(root).as_deref() == Some(st_path);
    if same_file && syncsafe::read(st_path).is_ok_and(|now| now[..] == read[..]) {
        Ok(())
    } else {
        Err(AppError::Transcript(TRANSCRIPT_CHANGED.into()))
    }
}

/// Rewrites the speaker ids on every segment of the sides in `sides`. Split
/// groups an earlier run left are rejoined first, so a re-run starts from the
/// source segments; each segment is then split at the side's turns (or takes
/// the side's single cluster). A segment no turn reaches takes the speaker of
/// the nearest labelled segment before it on the same side (the first
/// labelled one when none precedes it).
fn relabel_transcript(st: &mut transcript::SessionTranscript, sides: &[(DiarizeTrackSelect, SideLabels)]) {
    use transcript::split::{rejoin_split_groups, split_by_turns};
    let side_of = |t: transcript::Track| sides.iter().position(|(sel, _)| sel.matches(t));
    for ch in st.chunks.iter_mut() {
        for tr in ch.tracks.iter_mut() {
            if side_of(tr.track).is_some() {
                tr.segments = rejoin_split_groups(std::mem::take(&mut tr.segments));
                for seg in tr.segments.iter_mut() {
                    seg.speaker_id = None;
                }
            }
        }
    }
    // One split-group counter for the whole transcript, above any group the untouched side still carries.
    transcript::split::normalize_split_groups(st);
    let mut group = st
        .chunks
        .iter()
        .flat_map(|c| &c.tracks)
        .flat_map(|t| &t.segments)
        .filter_map(|s| s.split_group)
        .max()
        .map_or(0, |m| m + 1);
    for (ci, ch) in st.chunks.iter_mut().enumerate() {
        for tr in ch.tracks.iter_mut() {
            let Some(side) = side_of(tr.track) else { continue };
            match &sides[side].1 {
                SideLabels::Whole(id) => {
                    for seg in tr.segments.iter_mut() {
                        seg.speaker_id = Some(*id);
                    }
                }
                SideLabels::Turns(by_chunk) => {
                    let turns = by_chunk.get(&ci).map(Vec::as_slice).unwrap_or(&[]);
                    let mut out = Vec::with_capacity(tr.segments.len());
                    for seg in &tr.segments {
                        let pieces = split_by_turns(seg, turns, group);
                        if pieces.len() > 1 {
                            group += 1;
                        }
                        out.extend(pieces);
                    }
                    tr.segments = out;
                }
            }
        }
    }
    for side in 0..sides.len() {
        let one = &sides[side..=side];
        let mut assign: Vec<Option<u32>> = side_segments(st, one).map(|s| s.speaker_id).collect();
        smooth_unlabeled(&mut assign);
        let mut it = assign.into_iter();
        for_side_segments(st, one, |seg| seg.speaker_id = it.next().flatten());
    }
}

/// Every segment on the sides in `sides`, in transcript order.
fn side_segments<'a>(
    st: &'a transcript::SessionTranscript,
    sides: &'a [(DiarizeTrackSelect, SideLabels)],
) -> impl Iterator<Item = &'a transcript::Segment> {
    st.chunks
        .iter()
        .flat_map(|c| &c.tracks)
        .filter(|t| sides.iter().any(|(sel, _)| sel.matches(t.track)))
        .flat_map(|t| &t.segments)
}

/// Runs `f` on every segment on the sides in `sides`, in transcript order.
fn for_side_segments(
    st: &mut transcript::SessionTranscript,
    sides: &[(DiarizeTrackSelect, SideLabels)],
    mut f: impl FnMut(&mut transcript::Segment),
) {
    for ch in st.chunks.iter_mut() {
        for tr in ch.tracks.iter_mut() {
            if sides.iter().any(|(sel, _)| sel.matches(tr.track)) {
                tr.segments.iter_mut().for_each(&mut f);
            }
        }
    }
}

/// Window step the segmentation model runs at, in seconds.
const SEG_STEP_S: f32 = 1.0;

/// Longest stretch at the start of a turn embedded for clustering.
const TURN_EMBED_CAP_MS: u32 = 8_000;

/// Shortest turn whose embedding the automatic speaker count reads. Shorter
/// turns are still clustered and labelled.
const COUNT_TURN_MIN_MS: u32 = 1_000;

/// One track's audio for one transcript chunk.
struct ChunkAudio {
    /// Index into `st.chunks`.
    ci: usize,
    /// The chunk's own index.
    index: u32,
    wav: std::path::PathBuf,
}

/// One speaker turn of a track.
struct TrackTurn {
    /// Index into the track's `chunks`.
    chunk: usize,
    /// Chunk-relative.
    start_ms: u32,
    end_ms: u32,
    /// Where the chunk starts on the session's audio timeline.
    offset_ms: u32,
    /// Chunk-relative `(start_ms, dur_ms, clean)` voice-sample window.
    window: Option<(u32, u32, bool)>,
    emb: Option<Vec<f32>>,
}

impl TrackTurn {
    fn embeddable(&self) -> bool {
        (self.end_ms - self.start_ms) as f64 >= MIN_EMBED_SECONDS * 1000.0
    }
}

/// Every speaker turn on one side of a session, chunk by chunk.
struct TrackTurns {
    select: DiarizeTrackSelect,
    chunks: Vec<ChunkAudio>,
    turns: Vec<TrackTurn>,
}

impl TrackTurns {
    fn new(select: DiarizeTrackSelect) -> Self {
        Self { select, chunks: Vec::new(), turns: Vec::new() }
    }

    fn push_chunk(&mut self, ci: usize, index: u32, wav: std::path::PathBuf, offset_ms: u32, turns: &Turns) {
        let chunk = self.chunks.len();
        self.chunks.push(ChunkAudio { ci, index, wav });
        for &(s, e) in &turns.spans {
            let (start_ms, end_ms) = turn_ms(s, e);
            if end_ms <= start_ms {
                continue;
            }
            self.turns.push(TrackTurn {
                chunk,
                start_ms,
                end_ms,
                offset_ms,
                window: turn_window(s, e, &turns.overlap_spans),
                emb: None,
            });
        }
    }

    /// The embeddings the speaker count is estimated over: those of turns
    /// lasting at least `COUNT_TURN_MIN_MS`, or every embedding when fewer than
    /// `MIN_EMB_FOR_SPLIT` turns last that long.
    fn counting_embeddings(&self) -> Vec<Vec<f32>> {
        let long: Vec<Vec<f32>> = self
            .turns
            .iter()
            .filter(|t| t.end_ms - t.start_ms >= COUNT_TURN_MIN_MS)
            .filter_map(|t| t.emb.clone())
            .collect();
        if long.len() >= MIN_EMB_FOR_SPLIT {
            return long;
        }
        self.turns.iter().filter_map(|t| t.emb.clone()).collect()
    }

    /// The automatic speaker count over `counting_embeddings`.
    fn estimate_count(&self) -> usize {
        estimate_speaker_count(&self.counting_embeddings())
    }

    fn embedded_count(&self) -> usize {
        self.turns.iter().filter(|t| t.emb.is_some()).count()
    }
}

/// The chunks whose `select` track holds segments: `(index into st.chunks,
/// chunk index, wav, offset on the audio timeline)`. Chunks whose wav is
/// gone are skipped.
fn track_chunks(
    st: &transcript::SessionTranscript,
    manifest: &SessionManifest,
    root: &std::path::Path,
    select: DiarizeTrackSelect,
) -> Vec<(usize, u32, std::path::PathBuf, u32)> {
    let offsets = manifest.chunk_offsets_ms();
    st.chunks
        .iter()
        .enumerate()
        .filter(|(_, ch)| ch.tracks.iter().any(|t| select.matches(t.track) && !t.segments.is_empty()))
        .filter_map(|(ci, ch)| {
            let cm = manifest.chunks.iter().find(|c| c.index == ch.chunk_index)?;
            let wav = select.wav_path(cm, root);
            wav.is_file().then(|| {
                (ci, ch.chunk_index, wav, offsets.get(&ch.chunk_index).copied().unwrap_or(0))
            })
        })
        .collect()
}

/// Speaker turns of one side from the segmentation model, chunk by chunk.
/// A chunk whose wav cannot be read is skipped; a model failure fails the run.
fn segment_track(
    st: &transcript::SessionTranscript,
    manifest: &SessionManifest,
    root: &std::path::Path,
    select: DiarizeTrackSelect,
    model: &mut SegmentationModel,
) -> Result<TrackTurns> {
    let mut out = TrackTurns::new(select);
    for (ci, index, wav, offset_ms) in track_chunks(st, manifest, root, select) {
        if let Some(turns) = segment_wav(&wav, model)? {
            out.push_chunk(ci, index, wav, offset_ms, &turns);
        }
    }
    Ok(out)
}

/// One side's turns and overlap spans from `supplied`.
fn supplied_track(
    st: &transcript::SessionTranscript,
    manifest: &SessionManifest,
    root: &std::path::Path,
    select: DiarizeTrackSelect,
    supplied: &SuppliedTurns,
) -> TrackTurns {
    let (by_chunk, overlaps) = match select {
        DiarizeTrackSelect::System => (&supplied.system, &supplied.system_overlaps),
        DiarizeTrackSelect::Mic => (&supplied.mic, &supplied.mic_overlaps),
    };
    let mut out = TrackTurns::new(select);
    for (ci, index, wav, offset_ms) in track_chunks(st, manifest, root, select) {
        let spans = by_chunk.get(&index).cloned().unwrap_or_default();
        let overlap_spans = overlaps.get(&index).cloned().unwrap_or_default();
        out.push_chunk(ci, index, wav, offset_ms, &Turns { spans, overlap_spans });
    }
    out
}

/// Runs the segmentation model over one wav, reading it window by window,
/// and resolves the windows into turns. `None` when the wav cannot be read
/// or is not 16 kHz.
pub(crate) fn segment_wav(wav: &std::path::Path, model: &mut SegmentationModel) -> Result<Option<Turns>> {
    let mut reader = match crate::live_audio::MonoReader::open(wav) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("diarize: {} unreadable ({e}); its turns are skipped", wav.display());
            return Ok(None);
        }
    };
    let audio_s = reader.frames() as f64 / voiceprints::segmentation::SAMPLE_RATE as f64;
    let mut seg = StreamSegmenter::new(SEG_STEP_S);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // A truncated tail ends the stream.
        if reader.read(voiceprints::segmentation::WINDOW_SAMPLES as u64, &mut buf).unwrap_or(0) == 0 {
            break;
        }
        seg.push(model, &buf).map_err(voice_model_error)?;
    }
    Ok(Some(seg.finish(model, audio_s).map_err(voice_model_error)?))
}

/// Embeds every turn of at least `MIN_EMBED_SECONDS` from its first
/// `TURN_EMBED_CAP_MS`. Turns under the silence floor or unreadable stay
/// unembedded. A turn whose key is in `done` takes that embedding; returns
/// how many did.
fn embed_turns(
    track: &mut TrackTurns,
    encoder: &mut Encoder,
    done: Option<&std::collections::HashMap<TurnKey, Vec<f32>>>,
) -> usize {
    let chunks = &track.chunks;
    let mut reused = 0;
    for turn in track.turns.iter_mut() {
        let ca = &chunks[turn.chunk];
        let Some(key) = turn_embed_key(ca.index, turn.start_ms, turn.end_ms) else {
            continue;
        };
        if let Some(e) = done.and_then(|d| d.get(&key)) {
            turn.emb = Some(e.clone());
            reused += 1;
            continue;
        }
        turn.emb = embed_turn_audio(&ca.wav, turn.start_ms, key.2, encoder);
    }
    reused
}

/// Clusters one side's turns: k-means over the embedded turns at `k`
/// (`None`: `TrackTurns::estimate_count`), clusters matching `bleed_anchor` folded together,
/// unembedded turns labelled from their nearest embedded neighbour, then
/// fused clusters split apart and same-voice clusters merged down to no fewer
/// than `merge_min`. Returns `(per-turn cluster id, cluster count)`; ids are
/// `0..count`. Time spent splitting and merging is added to `hygiene`.
fn cluster_track(
    track: &TrackTurns,
    k: Option<usize>,
    merge_min: usize,
    bleed_anchor: Option<&[f32]>,
    encoder: &mut Encoder,
    floor: f32,
    hygiene: &mut std::time::Duration,
) -> (Vec<Option<u32>>, u32) {
    let n = track.turns.len();
    let idx: Vec<usize> = (0..n).filter(|&i| track.turns[i].emb.is_some()).collect();
    if idx.is_empty() {
        return (vec![None; n], 0);
    }
    let embs: Vec<Vec<f32>> = idx.iter().filter_map(|&i| track.turns[i].emb.clone()).collect();
    let ids = cluster_speakers(&embs, Some(k.unwrap_or_else(|| track.estimate_count())));
    let anchor: Vec<(u32, Vec<f32>)> =
        bleed_anchor.map(|a| vec![(u32::MAX, a.to_vec())]).unwrap_or_default();
    let ids = merge_by_gallery(&ids, &embs, &anchor, match_threshold());
    let mut embedded = vec![None; n];
    for (&i, &id) in idx.iter().zip(&ids) {
        embedded[i] = Some(id);
    }
    let spans: Vec<(f64, f64)> = track
        .turns
        .iter()
        .map(|t| {
            let at = |ms: u32| t.offset_ms.saturating_add(ms) as f64 / 1000.0;
            (at(t.start_ms), at(t.end_ms))
        })
        .collect();
    let mut assign = label_short_turns(&spans, &embedded);
    let count = |a: &[Option<u32>]| a.iter().flatten().copied().max().map_or(0, |m| m + 1);

    // Cluster hygiene: pull apart any cluster carrying two voices, then collapse the ones that read as the same voice.
    if count(&assign) > 1 {
        let t = std::time::Instant::now();
        let by_cluster = cluster_windows(track, &assign);
        let split = split_fused_clusters(&by_cluster, &mut assign, encoder, floor);
        let by_cluster = if split > 0 {
            log::info!("diarize {:?}: split {split} fused cluster(s) apart", track.select);
            cluster_windows(track, &assign)
        } else {
            by_cluster
        };
        let (cents, spread) = cluster_voice_profiles(&by_cluster, encoder);
        if cents.len() > 1 {
            assign = merge_clusters_by_cosine(&cents, &spread, &assign, SAME_VOICE_MERGE, merge_min);
        }
        *hygiene += t.elapsed();
    }
    let total = count(&assign);
    if let Some(target) = k.filter(|t| *t != total as usize) {
        log::info!(
            "diarize {:?}: asked for {target} speaker(s), grouped into {total}",
            track.select
        );
    }
    (assign, total)
}

/// Labelled turns per transcript chunk, in the shape the transcript split
/// reads.
fn turn_map(
    track: &TrackTurns,
    labels: &[Option<u32>],
) -> std::collections::HashMap<usize, Vec<transcript::split::Turn>> {
    let mut out: std::collections::HashMap<usize, Vec<transcript::split::Turn>> = Default::default();
    for (t, label) in track.turns.iter().zip(labels) {
        if let Some(id) = label {
            out.entry(track.chunks[t.chunk].ci).or_default().push((t.start_ms, t.end_ms, *id));
        }
    }
    for turns in out.values_mut() {
        turns.sort_unstable();
    }
    out
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
            DiarizeTrackSelect::System => syncsafe::join_rel(root, &chunk.system_wav_relative),
            DiarizeTrackSelect::Mic => match chunk.mic_aec_wav_relative.as_ref() {
                Some(p) => syncsafe::join_rel(root, p),
                None => syncsafe::join_rel(root, &chunk.mic_wav_relative),
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
                // Windows below the silence floor are skipped.
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

/// Pulled off each end of a turn's sample window, so an approximate turn
/// boundary doesn't leak the neighbouring speaker into the sample.
const TURN_EDGE_GUARD_MS: f64 = 0.2;

/// Shortest turn sample window worth taking, in seconds.
const TURN_WINDOW_MIN_MS: f64 = 0.4;

/// Another speaker talking over this much of a window marks it overlapped.
/// Turns that overlap in time can't be trimmed apart.
const TURN_OVERLAP_REJECT_MS: f64 = 0.2;

/// The voice-sample window of a turn `[start, end]` (chunk-relative seconds), pulled in by `TURN_EDGE_GUARD_MS` at both ends, as `(start_ms, dur_ms, clean)`. `None` when nothing of usable length survives; `clean` is false when overlaps cover `TURN_OVERLAP_REJECT_MS` of the window.
fn turn_window(start: f64, end: f64, overlaps: &[(f64, f64)]) -> Option<(u32, u32, bool)> {
    let (mut start, mut end) = (start, end);
    if end - start - 2.0 * TURN_EDGE_GUARD_MS >= TURN_WINDOW_MIN_MS {
        start += TURN_EDGE_GUARD_MS;
        end -= TURN_EDGE_GUARD_MS;
    }
    if end - start < TURN_WINDOW_MIN_MS {
        return None;
    }
    let over: f64 = overlaps
        .iter()
        .map(|&(os, oe)| (end.min(oe) - start.max(os)).max(0.0))
        .sum();
    let start_ms = (start * 1000.0).round().max(0.0) as u32;
    let dur_ms = ((end - start) * 1000.0).round() as u32;
    Some((start_ms, dur_ms, over < TURN_OVERLAP_REJECT_MS))
}

/// One candidate window of a cluster: one of its turns' sample window, with
/// the loudness dimensions used to rank it.
struct ClusterWindow {
    /// Index into the track's turns — what a split reassigns.
    pos: usize,
    wav: std::path::PathBuf,
    start_ms: u32,
    dur_ms: u32,
    /// False when other speech talks over the window.
    clean: bool,
    rms: f32,
    voiced_ms: u32,
    /// Speech floor of the wav this window came from.
    floor: f32,
}

/// Every cluster's candidate windows, probed for speech and stripped of
/// sparse ones. Shared by the split pass and the same-voice merge so both read
/// the same audio.
fn cluster_windows(
    track: &TrackTurns,
    assign: &[Option<u32>],
) -> std::collections::BTreeMap<u32, Vec<ClusterWindow>> {
    use std::collections::BTreeMap;
    let mut out: BTreeMap<u32, Vec<ClusterWindow>> = BTreeMap::new();
    let mut floors: std::collections::HashMap<usize, f32> = Default::default();
    for (pi, (turn, a)) in track.turns.iter().zip(assign).enumerate() {
        let Some(cid) = a else { continue };
        let cands = out.entry(*cid).or_default();
        let Some((start_ms, dur_ms, clean)) = turn.window else { continue };
        if dur_ms < 200 {
            continue;
        }
        let wav = &track.chunks[turn.chunk].wav;
        // Sparse windows are rejected and ranking runs on the compacted speech.
        let floor = *floors.entry(turn.chunk).or_insert_with(|| voiced_floor(wav));
        let Some((voiced, rms)) = voiced_sample(wav, start_ms, dur_ms, floor) else {
            continue;
        };
        let voiced_ms = pcm_len_ms(&voiced);
        if voiced_ms < 200 {
            continue;
        }
        cands.push(ClusterWindow { pos: pi, wav: wav.clone(), start_ms, dur_ms, clean, rms, voiced_ms, floor });
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
        // The centroid takes uncontested windows first and fills the remaining budget from overlapped ones.
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
        // The spread windows: the loudest sustained windows nobody talks over, embedded one by one.
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

/// A split-test window is cut into pieces of about this length.
const SPLIT_SUBWINDOW_MS: u32 = 3_000;

/// Ceiling on split-test embeddings per cluster, after sub-windowing.
const SPLIT_EMBED_CAP: usize = 32;

/// Windows embedded per cluster for the split test.
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

/// 2-means passes over a cluster's windows.
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

/// How far apart two different people sit in this session, as the largest mean window cosine between any two clusters, or `None` below two clusters that each carry windows.
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

/// Two voices inside one cluster, as `(left, right)` window indices, or `None` when the windows read as one person. Seeds 2-means on the least-similar pair, then keeps the split only when each side holds at least `SPLIT_MIN_WINDOWS` windows, each side is coherent, the sides sit no closer than two different people in this session, and their centroids sit below the merge threshold.
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

/// Do two clusters' loud windows read as one voice? The pair is one voice when its cross-cluster cosine reaches the clusters' own window spread (less `SAME_VOICE_SPREAD_MARGIN`); with fewer than two windows on either side the centroid cosine decides alone.
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
    // Shares the diarization permit.
    let _diarize = diarize_permit();
    let mut encoder = Encoder::load()
        .map_err(|e| AppError::Config(format!("voiceprint model: {e}")))?;
    // The audio about to be embedded must read as one voice.
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

/// Concatenate up to ENROLL_CAP_MS of audio for a given cluster, including windows somebody else talks over once the isolated ones run out. Enrollment gathers with `isolated_only`.
fn gather_cluster_pcm(
    session_dir: &std::path::Path,
    cluster_id: u32,
) -> Result<Vec<i16>> {
    Ok(gather_cluster_sample(session_dir, cluster_id, ENROLL_CAP_MS, false)?.0)
}

/// The audio an enrollment is allowed to keep: this cluster's isolated windows only.
fn gather_enrollment_pcm(
    session_dir: &std::path::Path,
    cluster_id: u32,
) -> Result<Vec<i16>> {
    Ok(gather_cluster_sample(session_dir, cluster_id, ENROLL_CAP_MS, true)?.0)
}

/// RMS floor (normalized 0..1) below which a PCM window is treated as silence when gathering a cluster's audio.
const SAMPLE_SILENCE_FLOOR: f32 = 0.005;

/// A frame counts as speech at a multiple of the recording's own noise floor.
const VOICED_OVER_NOISE: f32 = 2.0;

/// Head of a chunk read to estimate its noise floor.
const NOISE_FLOOR_PROBE_MS: u32 = 60_000;

/// The RMS a frame of this recording has to reach to be speech: over the chunk's own noise floor, never below the absolute silence floor. A probe with no noise bed keeps the absolute floor.
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

/// Max audio (ms) any single segment contributes to a gathered sample.
const SAMPLE_MAX_SEG_MS: u32 = 6_000;

/// Frame used for voiced-run analysis: 20 ms @ 16 kHz.
const VOICED_FRAME_SAMPLES: usize = 320;
/// Interior silence longer than this is collapsed down to it: a sample keeps
/// its natural pauses without carrying dead air into the embedding.
const VOICED_MAX_GAP_MS: u32 = 300;
/// A candidate window whose voiced fraction falls below this is a brief
/// utterance adrift in silence, not a usable voice sample.
const SAMPLE_MIN_VOICED: f32 = 0.25;

/// Strip a window down to its speech: drop leading/trailing silence and collapse interior gaps to `VOICED_MAX_GAP_MS`. `floor` is the RMS a frame has to reach to count as speech. Returns the compacted audio and the voiced fraction of the original window.
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

/// Margins (ms) around a candidate window inside which another cluster's speaker change marks it as overlapped. Tried widest first.
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

/// Up to `cap_ms` of a cluster's isolated audio, the pool enrollment reads.
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
    /// Session-absolute start (chunk offset applied), for the isolation test.
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
            let wav = syncsafe::join_rel(session_dir, wav_rel);
            if !wav.is_file() {
                continue;
            }
            let floor = voiced_floor(&wav);
            for seg in &tr.segments {
                if seg.speaker_id != Some(cluster_id) {
                    if seg.speaker_id.is_some() {
                        // The whole segment.
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
            // The clip carries only part of this segment.
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
        // Ranking runs on compacted speech, so length credit tracks actual voice.
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
        // The bed alone reads every frame as speech against the absolute floor.
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
mod turn_window_tests {
    use super::turn_window;

    #[test]
    fn a_turn_is_pulled_in_from_both_ends() {
        assert_eq!(turn_window(10.0, 14.0, &[]), Some((10_200, 3_600, true)));
    }

    #[test]
    fn short_turns_skip_the_guard_then_fail_the_floor() {
        // 0.5s survives without the guard; 0.3s is below the floor entirely.
        assert_eq!(turn_window(10.0, 10.5, &[]), Some((10_000, 500, true)));
        assert_eq!(turn_window(10.0, 10.3, &[]), None);
    }

    #[test]
    fn a_window_others_talk_over_is_marked_overlapped() {
        // Overlapped speech covers 11.0-11.5: still usable for a centroid, no
        // longer usable for measuring spread.
        assert_eq!(turn_window(10.0, 14.0, &[(11.0, 11.5)]), Some((10_200, 3_600, false)));
        // A 100 ms overlap is below the reject threshold.
        assert_eq!(turn_window(10.0, 14.0, &[(11.0, 11.1)]), Some((10_200, 3_600, true)));
        // Overlap outside the window does not count.
        assert_eq!(turn_window(10.0, 14.0, &[(20.0, 21.0)]), Some((10_200, 3_600, true)));
    }
}

#[cfg(test)]
mod counting_embeddings_tests {
    use super::*;

    /// A track of embedded turns lasting `durations_ms`; turn `i` carries
    /// embedding `[i]`.
    fn track(durations_ms: &[u32]) -> TrackTurns {
        let mut tt = TrackTurns::new(DiarizeTrackSelect::System);
        let mut at = 0;
        for (i, &d) in durations_ms.iter().enumerate() {
            tt.turns.push(TrackTurn {
                chunk: 0,
                start_ms: at,
                end_ms: at + d,
                offset_ms: 0,
                window: None,
                emb: Some(vec![i as f32]),
            });
            at += d;
        }
        tt
    }

    #[test]
    fn the_count_reads_turns_of_count_turn_min_ms_or_more() {
        let mut durations = vec![COUNT_TURN_MIN_MS; MIN_EMB_FOR_SPLIT];
        durations.extend([COUNT_TURN_MIN_MS - 1, 600]);
        let got = track(&durations).counting_embeddings();
        let want: Vec<Vec<f32>> = (0..MIN_EMB_FOR_SPLIT).map(|i| vec![i as f32]).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn too_few_long_turns_count_every_embedding() {
        let mut durations = vec![COUNT_TURN_MIN_MS; MIN_EMB_FOR_SPLIT - 1];
        durations.extend([600, 700, 800]);
        assert_eq!(track(&durations).counting_embeddings().len(), durations.len());
    }

    #[test]
    fn unembedded_turns_are_not_counted() {
        let mut tt = track(&[COUNT_TURN_MIN_MS; MIN_EMB_FOR_SPLIT + 1]);
        tt.turns[0].emb = None;
        assert_eq!(tt.counting_embeddings().len(), MIN_EMB_FOR_SPLIT);
    }
}

#[cfg(test)]
mod supplied_overlap_tests {
    use super::*;

    /// A mic-only session folder with one 12 s chunk of steady speech-level
    /// tone and one transcript segment per supplied turn.
    fn session(root: &std::path::Path) -> (transcript::SessionTranscript, SessionManifest) {
        let chunk = root.join("chunks/0000");
        std::fs::create_dir_all(&chunk).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(chunk.join("mic.wav"), spec).unwrap();
        for _ in 0..16_000 * 12 {
            w.write_sample(8000i16).unwrap();
        }
        w.finalize().unwrap();
        let manifest: SessionManifest = serde_json::from_value(serde_json::json!({
            "schema_version": 4, "session_id": "s", "created_at_unix_seconds": 1_788_200_000i64,
            "sample_rate": 16_000, "channels": 1,
            "chunks": [{
                "index": 0, "started_at_unix_seconds": 1_788_200_000i64, "duration_seconds": 12,
                "mic_wav_relative": "chunks/0000/mic.wav", "system_wav_relative": "chunks/0000/system.wav",
                "mic_aec_wav_relative": null,
            }],
            "attendees": [], "tag_ids": [],
        }))
        .unwrap();
        let segs: Vec<_> = [0, 4_000, 8_000]
            .iter()
            .map(|&s| serde_json::json!({ "start_ms": s, "end_ms": s + 3_000, "text": "fictional words",
                "confidence": null, "speaker_id": null }))
            .collect();
        let st: transcript::SessionTranscript = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "session_id": "s", "provider": "p", "model": "m",
            "transcribed_at_unix_seconds": 0,
            "chunks": [{ "chunk_index": 0, "tracks": [{
                "track": "mic", "source_wav_relative": "chunks/0000/mic.wav", "segments": segs,
            }] }],
        }))
        .unwrap();
        (st, manifest)
    }

    #[test]
    fn supplied_overlaps_keep_their_windows_out_of_the_split_and_spread_input() {
        let tmp = tempfile::tempdir().unwrap();
        let (st, manifest) = session(tmp.path());
        let supplied = SuppliedTurns {
            mic: [(0, vec![(0.0, 3.0), (4.0, 7.0), (8.0, 11.0)])].into_iter().collect(),
            mic_overlaps: [(0, vec![(4.5, 6.5)])].into_iter().collect(),
            ..Default::default()
        };
        let track = supplied_track(&st, &manifest, tmp.path(), DiarizeTrackSelect::Mic, &supplied);
        let clean: Vec<_> = track.turns.iter().map(|t| t.window.map(|w| w.2)).collect();
        assert_eq!(clean, vec![Some(true), Some(false), Some(true)]);

        let windows = cluster_windows(&track, &[Some(0), Some(0), Some(0)]);
        let cands = &windows[&0];
        assert_eq!(cands.len(), 3, "an overlapped window still feeds the centroid");
        let picked: Vec<usize> = loudest_clean(cands, SPLIT_WINDOWS).iter().map(|&i| cands[i].pos).collect();
        assert_eq!(picked.len(), 2);
        assert!(!picked.contains(&1), "the overlapped turn is left out: {picked:?}");
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
        // A flatline well below the silence floor, a loud tone well above.
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
            audio_samples: None,
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
mod count_tests {
    use super::*;
    use recording::manifest::{Attendee, AttendeeRole};

    fn inputs() -> CountInputs {
        CountInputs::default()
    }

    fn heard(present: usize, coverage: f32) -> VoicePresence {
        VoicePresence { present, coverage }
    }

    fn never() -> usize {
        panic!("auto estimation must not run when a higher source decides")
    }

    #[test]
    fn a_typed_number_wins_over_every_other_source() {
        let i = CountInputs { typed: Some(4), voices: heard(2, 1.0), expected: Some(3), attendees: Some(6) };
        assert_eq!(resolve_count(&i, never), (4, CountSource::Typed));
        assert_eq!(merge_floor(4, CountSource::Typed, 2), 4);
    }

    #[test]
    fn enrolled_voices_covering_the_speech_set_the_count() {
        let i = CountInputs { voices: heard(3, 0.85), expected: Some(5), ..inputs() };
        assert_eq!(resolve_count(&i, never), (3, CountSource::Voiceprints));
        assert_eq!(merge_floor(3, CountSource::Voiceprints, 3), 3);
    }

    #[test]
    fn partial_coverage_makes_the_voices_a_floor_under_the_next_source() {
        let i = CountInputs { voices: heard(2, 0.5), expected: Some(4), ..inputs() };
        assert_eq!(resolve_count(&i, never), (4, CountSource::Expected));
        let i = CountInputs { voices: heard(2, 0.5), expected: Some(1), ..inputs() };
        assert_eq!(resolve_count(&i, never), (2, CountSource::Expected));
        let i = CountInputs { voices: heard(3, 0.5), attendees: Some(2), ..inputs() };
        assert_eq!(resolve_count(&i, never), (3, CountSource::Attendees));
        assert_eq!(merge_floor(3, CountSource::Attendees, 3), 3, "the merge keeps the voices heard");
        let i = CountInputs { voices: heard(2, 0.5), ..inputs() };
        assert_eq!(resolve_count(&i, || 1), (2, CountSource::Auto));
        assert_eq!(resolve_count(&i, || 5), (5, CountSource::Auto));
    }

    #[test]
    fn expected_speakers_come_before_attendees() {
        let i = CountInputs { expected: Some(3), attendees: Some(7), ..inputs() };
        assert_eq!(resolve_count(&i, never), (3, CountSource::Expected));
        assert_eq!(merge_floor(3, CountSource::Expected, 0), 3);
    }

    #[test]
    fn only_a_stated_number_is_a_request() {
        let stated = |i: &CountInputs| {
            let (_, source) = resolve_count(i, || 3);
            stated_count(i, source)
        };
        let i = CountInputs { typed: Some(9), expected: Some(4), ..inputs() };
        assert_eq!(stated(&i), Some(9));
        // The floor lifts the count to 4; the person stated 2.
        let i = CountInputs { voices: heard(4, 0.5), expected: Some(2), ..inputs() };
        assert_eq!(resolve_count(&i, never), (4, CountSource::Expected));
        assert_eq!(stated(&i), Some(2));
        let i = CountInputs { voices: heard(3, 0.9), expected: Some(5), ..inputs() };
        assert_eq!(stated(&i), None, "voiceprints");
        let i = CountInputs { attendees: Some(5), ..inputs() };
        assert_eq!(stated(&i), None, "attendees");
        assert_eq!(stated(&inputs()), None, "auto");
    }

    #[test]
    fn attendees_come_before_the_estimate_and_leave_the_merge_free() {
        let i = CountInputs { attendees: Some(5), ..inputs() };
        assert_eq!(resolve_count(&i, never), (5, CountSource::Attendees));
        assert_eq!(merge_floor(5, CountSource::Attendees, 0), 1);
    }

    #[test]
    fn with_nothing_else_the_voices_are_estimated() {
        assert_eq!(resolve_count(&inputs(), || 3), (3, CountSource::Auto));
        assert_eq!(resolve_count(&inputs(), || 0), (1, CountSource::Auto));
        assert_eq!(merge_floor(3, CountSource::Auto, 0), 1);
    }

    #[test]
    fn a_voice_is_present_after_three_turns_and_ten_seconds() {
        // Identity 0: three turns, 12 s. Identity 1: two long turns. Identity
        // 2: four turns, 6 s. Unmatched: 6 s.
        let turns = [
            (4_000, Some(0)), (4_000, Some(0)), (4_000, Some(0)),
            (9_000, Some(1)), (9_000, Some(1)),
            (1_500, Some(2)), (1_500, Some(2)), (1_500, Some(2)), (1_500, Some(2)),
            (6_000, None),
        ];
        let p = voice_presence(&turns);
        assert_eq!(p.present, 1);
        assert!((p.coverage - 12.0 / 42.0).abs() < 1e-6, "{p:?}");
        assert_eq!(voice_presence(&[]), VoicePresence::default());
    }

    #[test]
    fn a_turn_matches_the_identity_with_the_closest_sample() {
        let g = vec![
            vec![vec![1.0, 0.0, 0.0], vec![0.0, 0.0, 1.0]],
            vec![vec![0.8, 0.6, 0.0]],
        ];
        assert_eq!(best_identity(&[1.0, 0.0, 0.0], &g, 0.62), Some(0));
        assert_eq!(best_identity(&[0.6, 0.8, 0.0], &g, 0.62), Some(1));
        assert_eq!(best_identity(&[0.0, 1.0, 0.0], &g, 0.62), None, "below the threshold");
        assert_eq!(best_identity(&[1.0, 0.0, 0.0], &[], 0.62), None);
    }

    #[test]
    fn attendees_counted_by_the_track_the_count_applies_to() {
        let a = |n: &str, role| Attendee { display_name: n.into(), role };
        let list = [a("Fictional Owner", AttendeeRole::Self_), a("Avery", AttendeeRole::Other), a("Blake", AttendeeRole::Other)];
        assert_eq!(attendee_count(&list, false), Some(2), "the system track holds the other side");
        assert_eq!(attendee_count(&list, true), Some(3), "the mic holds everyone");
        assert_eq!(attendee_count(&list[..1], false), None);
        assert_eq!(attendee_count(&[], true), None);
    }

    #[test]
    fn the_count_applies_to_the_system_track_only_when_it_holds_speech() {
        assert!(count_on_mic(DiarizeScope::Mic, true));
        assert!(count_on_mic(DiarizeScope::Mic, false));
        assert!(!count_on_mic(DiarizeScope::Others, true));
        assert!(!count_on_mic(DiarizeScope::Others, false));
        assert!(!count_on_mic(DiarizeScope::Both, true), "remote speech: the count is the other side");
        assert!(count_on_mic(DiarizeScope::Both, false), "a room group alone: the count is the room");
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

    fn seg(start_ms: u32, end_ms: u32, words: &[(&str, u32, u32)]) -> transcript::Segment {
        let mut s = transcript::Segment::from_words(
            words.iter().map(|(t, a, b)| (t.to_string(), *a as i64, *b as i64)),
        )
        .unwrap_or_else(|| transcript::Segment {
            start_ms,
            end_ms,
            text: "fictional words".into(),
            confidence: None,
            speaker_id: None,
            words: Vec::new(),
            split_group: None,
        });
        s.set_span(start_ms, end_ms);
        s
    }

    fn transcript(tracks: Vec<(transcript::Track, Vec<transcript::Segment>)>) -> transcript::SessionTranscript {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1, "session_id": "s", "provider": "p", "model": "m",
            "transcribed_at_unix_seconds": 0,
            "chunks": [{ "chunk_index": 0, "tracks": tracks.iter().map(|(t, segs)| serde_json::json!({
                "track": t, "source_wav_relative": "x.wav", "segments": segs,
            })).collect::<Vec<_>>() }],
        }))
        .unwrap()
    }

    fn turns(t: &[(u32, u32, u32)]) -> SideLabels {
        SideLabels::Turns([(0usize, t.to_vec())].into_iter().collect())
    }

    #[test]
    fn a_segment_spanning_two_turns_splits_and_a_rerun_rejoins_it_first() {
        let words = [("so", 0, 400), ("we", 400, 800), ("ship", 800, 1_400), ("friday", 1_400, 2_000),
            ("sounds", 2_100, 2_600), ("good", 2_600, 3_000), ("to", 3_000, 3_300), ("me", 3_300, 3_900)];
        let mut st = transcript(vec![(transcript::Track::System, vec![seg(0, 4_000, &words)])]);
        let sides = [(DiarizeTrackSelect::System, turns(&[(0, 2_050, 0), (2_050, 4_000, 1)]))];
        relabel_transcript(&mut st, &sides);
        let segs = &st.chunks[0].tracks[0].segments;
        assert_eq!(segs.len(), 2, "{segs:?}");
        assert_eq!((segs[0].speaker_id, segs[1].speaker_id), (Some(0), Some(1)));
        assert_eq!(segs[0].split_group, segs[1].split_group);
        assert!(segs[0].split_group.is_some());

        // A re-run that hears one voice puts the source segment back whole.
        let sides = [(DiarizeTrackSelect::System, turns(&[(0, 4_000, 3)]))];
        relabel_transcript(&mut st, &sides);
        let segs = &st.chunks[0].tracks[0].segments;
        assert_eq!(segs.len(), 1, "{segs:?}");
        assert_eq!(segs[0].speaker_id, Some(3));
        assert_eq!((segs[0].start_ms, segs[0].end_ms, segs[0].split_group), (0, 4_000, None));
        assert_eq!(segs[0].words.len(), words.len());
    }

    #[test]
    fn segments_no_turn_reaches_take_the_preceding_speaker() {
        // Turns cover only the middle segment; the first takes the first known
        // speaker and the last the one before it.
        let mut st = transcript(vec![(
            transcript::Track::System,
            vec![seg(0, 1_000, &[]), seg(5_000, 6_000, &[]), seg(20_000, 21_000, &[])],
        )]);
        relabel_transcript(&mut st, &[(DiarizeTrackSelect::System, turns(&[(5_000, 6_000, 2)]))]);
        let ids: Vec<_> = st.chunks[0].tracks[0].segments.iter().map(|s| s.speaker_id).collect();
        assert_eq!(ids, vec![Some(2), Some(2), Some(2)]);
    }

    #[test]
    fn only_the_relabelled_side_changes() {
        let mut mic = seg(0, 1_000, &[]);
        mic.speaker_id = Some(7);
        let mut st = transcript(vec![
            (transcript::Track::Mic, vec![mic]),
            (transcript::Track::System, vec![seg(0, 1_000, &[])]),
        ]);
        relabel_transcript(&mut st, &[(DiarizeTrackSelect::System, SideLabels::Whole(4))]);
        assert_eq!(st.chunks[0].tracks[0].segments[0].speaker_id, Some(7));
        assert_eq!(st.chunks[0].tracks[1].segments[0].speaker_id, Some(4));
    }

    #[test]
    fn a_split_group_at_the_top_of_its_range_leaves_room_for_fresh_ones() {
        let words = [("so", 0, 400), ("we", 400, 800), ("ship", 800, 1_400), ("friday", 1_400, 2_000),
            ("sounds", 2_100, 2_600), ("good", 2_600, 3_000), ("to", 3_000, 3_300), ("me", 3_300, 3_900)];
        // The mic side arrived from another device carrying u32::MAX groups.
        let mut kept = transcript::split::split_by_turns(
            &seg(0, 4_000, &words),
            &[(0, 2_050, 0), (2_050, 4_000, 1)],
            u32::MAX,
        );
        assert_eq!(kept.len(), 2);
        kept.extend(transcript::split::split_by_turns(
            &seg(5_000, 9_000, &words.map(|(t, a, b)| (t, a + 5_000, b + 5_000))),
            &[(5_000, 7_050, 0), (7_050, 9_000, 1)],
            u32::MAX - 1,
        ));
        let mut st = transcript(vec![
            (transcript::Track::Mic, kept),
            (transcript::Track::System, vec![seg(0, 4_000, &words), seg(5_000, 9_000, &words.map(|(t, a, b)| (t, a + 5_000, b + 5_000)))]),
        ]);
        let sides = [(DiarizeTrackSelect::System, turns(&[(0, 2_050, 2), (2_050, 4_000, 3), (5_000, 7_050, 2), (7_050, 9_000, 3)]))];
        relabel_transcript(&mut st, &sides);
        let groups = |t: usize| -> Vec<Option<u32>> {
            st.chunks[0].tracks[t].segments.iter().map(|s| s.split_group).collect()
        };
        assert_eq!(groups(0), vec![Some(0), Some(0), Some(1), Some(1)], "kept groups renumbered densely");
        assert_eq!(groups(1), vec![Some(2), Some(2), Some(3), Some(3)], "fresh groups follow them");
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
        // Shared component on axis 0, private component on one axis each.
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

    /// Two clusters clearing the threshold against one enrolled identity: the exact self-match and a near-twin voice. The closer cluster is named and the other is left for the user.
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

#[cfg(test)]
mod diarize_gate {
    //! Quality gate: the shipped diarization on a scratch copy of one session,
    //! dumped as time spans for scoring against labels.
    use super::*;
    use std::sync::Mutex;

    /// One labelled turn of the most recent run, on the transcript's ids.
    #[derive(Debug, Clone, Serialize)]
    pub(super) struct GateTurn {
        track: String,
        chunk_index: u32,
        /// Chunk-relative.
        start_ms: u32,
        end_ms: u32,
        embedded: bool,
        speaker_id: Option<u32>,
        /// The turn's embedding, when `DAISY_GATE_EMB` is set.
        #[serde(skip_serializing_if = "Option::is_none")]
        emb: Option<Vec<f32>>,
    }

    static TURNS: Mutex<Vec<GateTurn>> = Mutex::new(Vec::new());

    fn turns_slot() -> std::sync::MutexGuard<'static, Vec<GateTurn>> {
        TURNS.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Keeps the turns of the run in progress, labelled with the ids its
    /// segments carry after the dense renumbering.
    pub(super) fn record_turns(
        st: &transcript::SessionTranscript,
        turn_sides: &[(TrackTurns, Vec<Option<u32>>)],
        remap: &std::collections::HashMap<u32, u32>,
    ) {
        let with_emb = std::env::var_os("DAISY_GATE_EMB").is_some();
        let mut out = Vec::new();
        for (tt, labels) in turn_sides {
            for (t, label) in tt.turns.iter().zip(labels) {
                out.push(GateTurn {
                    track: format!("{:?}", tt.select).to_lowercase(),
                    chunk_index: st.chunks[tt.chunks[t.chunk].ci].chunk_index,
                    start_ms: t.start_ms,
                    end_ms: t.end_ms,
                    embedded: t.emb.is_some(),
                    speaker_id: label.and_then(|l| remap.get(&l).copied()),
                    emb: t.emb.clone().filter(|_| with_emb),
                });
            }
        }
        *turns_slot() = out;
    }

    /// One transcript segment, chunk-relative.
    #[derive(Debug, Clone, Serialize)]
    struct GateSegment {
        chunk_index: u32,
        track: transcript::Track,
        start_ms: u32,
        end_ms: u32,
        speaker_id: Option<u32>,
        split_group: Option<u32>,
        words: usize,
        text: String,
    }

    fn segments_of(path: &std::path::Path) -> Vec<GateSegment> {
        let st: transcript::SessionTranscript =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        st.chunks
            .iter()
            .flat_map(|c| {
                c.tracks.iter().flat_map(move |t| {
                    t.segments.iter().map(move |s| GateSegment {
                        chunk_index: c.chunk_index,
                        track: t.track,
                        start_ms: s.start_ms,
                        end_ms: s.end_ms,
                        speaker_id: s.speaker_id,
                        split_group: s.split_group,
                        words: s.words.len(),
                        text: s.text.clone(),
                    })
                })
            })
            .collect()
    }

    /// User plus system CPU seconds this process has used.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn cpu_seconds() -> Option<f64> {
        // SAFETY: getrusage fills the zeroed struct it is handed.
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } == 0;
        let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        ok.then(|| secs(ru.ru_utime) + secs(ru.ru_stime))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn cpu_seconds() -> Option<f64> {
        None
    }

    #[derive(Debug, Serialize)]
    struct GateRun {
        result: DiarizeResult,
        wall_s: f64,
        cpu_s: Option<f64>,
        segments: Vec<GateSegment>,
        turns: Vec<GateTurn>,
    }

    fn run_once(app: &AppState, sid: &str, k: u32, st_path: &std::path::Path) -> GateRun {
        turns_slot().clear();
        let cpu0 = cpu_seconds();
        let t = std::time::Instant::now();
        let result = diarize_session_impl(app, sid, Some(k).filter(|&k| k > 0), None, &[]).expect("diarize");
        let wall_s = t.elapsed().as_secs_f64();
        let cpu_s = cpu_seconds().zip(cpu0).map(|(b, a)| b - a);
        eprintln!("diarize_gate: {sid} k={k} wall {wall_s:.1}s cpu {cpu_s:?}s -> {result:?}");
        let turns = std::mem::take(&mut *turns_slot());
        GateRun { result, wall_s, cpu_s, segments: segments_of(st_path), turns }
    }

    fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
        std::fs::create_dir_all(dst).unwrap();
        for e in std::fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            let to = dst.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy_dir(&e.path(), &to);
            } else {
                std::fs::copy(e.path(), &to).unwrap();
            }
        }
    }

    /// Runs the shipped `diarize_session_impl` at a typed count on a scratch
    /// copy of one session and writes, as JSON: the segments as found, the
    /// segments and labelled turns after the run, wall and CPU time, and a
    /// second run over the first one's output (re-diarize). `DAISY_GATE_K=0`
    /// resolves the count as an untyped run does; `DAISY_GATE_EMB=1` adds each
    /// turn's embedding. The copy is
    /// deleted when the test ends, also on failure. Fails when the re-run
    /// changes a segment or a speaker.
    ///
    /// DAISY_GATE_SESSION=/path/to/session DAISY_GATE_K=3 DAISY_GATE_OUT=/path/to/out.json \
    ///   [DAISY_GATE_WORK=/scratch/dir] [DAISY_GATE_RETRANSCRIBE=active|/path/to/ggml.bin] \
    ///   [DAISY_GATE_REDIARIZE=0] DAISY_VOICEPRINT_DIR=/dir/with/model.onnx+segmentation-3.0.onnx \
    ///   cargo test --release -p tauri-app --lib diarize_gate -- --ignored --nocapture
    #[test]
    #[ignore]
    fn diarize_gate() {
        let _ = env_logger::builder().is_test(true).try_init();
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let src = std::path::PathBuf::from(var("DAISY_GATE_SESSION").expect("DAISY_GATE_SESSION"));
        let k: u32 = var("DAISY_GATE_K").expect("DAISY_GATE_K").parse().expect("DAISY_GATE_K is a count");
        let out = std::path::PathBuf::from(var("DAISY_GATE_OUT").expect("DAISY_GATE_OUT"));
        let work = var("DAISY_GATE_WORK").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let sid = src.file_name().and_then(|s| s.to_str()).expect("session dir name").to_string();

        let scratch = tempfile::Builder::new().prefix("diarize-gate-").tempdir_in(&work).unwrap();
        let root = scratch.path().join("sessions").join(&sid);
        copy_dir(&src, &root);
        let app = AppState::new(crate::profile::ProfileDir::at(scratch.path()).unwrap());
        let found = crate::commands::session_store::preferred_transcript_path(&root).expect("a transcript");
        let original = segments_of(&found);

        if let Some(engine) = var("DAISY_GATE_RETRANSCRIBE") {
            use crate::commands::pipeline::{dedup_impl, transcribe_impl, DedupRequest, TranscribeRequest};
            let model = (engine != "active").then_some(engine);
            let t = std::time::Instant::now();
            transcribe_impl(&app, TranscribeRequest { session_id: sid.clone(), model, resume: false }, None).expect("transcribe");
            dedup_impl(&app, DedupRequest { session_id: sid.clone() }).expect("dedup");
            eprintln!("diarize_gate: re-transcribed in {:.1}s", t.elapsed().as_secs_f64());
        }
        let st_path = crate::commands::session_store::preferred_transcript_path(&root).unwrap();
        let input = segments_of(&st_path);

        let first = run_once(&app, &sid, k, &st_path);
        let second = (var("DAISY_GATE_REDIARIZE").as_deref() != Some("0"))
            .then(|| run_once(&app, &sid, k, &st_path));
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
        let report = serde_json::json!({
            "session": sid,
            "k": k,
            "transcript": st_path.file_name().and_then(|s| s.to_str()),
            "chunks": manifest["chunks"],
            "created_at_unix_seconds": manifest["created_at_unix_seconds"],
            "original": original,
            "input": input,
            "first": first,
            "second": second,
        });
        std::fs::write(&out, serde_json::to_vec(&report).unwrap()).unwrap();
        eprintln!("diarize_gate: wrote {}", out.display());

        if let Some(second) = &second {
            let spans = |r: &GateRun| -> Vec<(u32, transcript::Track, u32, u32, Option<u32>)> {
                r.segments.iter().map(|s| (s.chunk_index, s.track, s.start_ms, s.end_ms, s.speaker_id)).collect()
            };
            assert!(
                second.segments.len() <= first.segments.len(),
                "re-diarize grew the transcript: {} -> {} segments",
                first.segments.len(),
                second.segments.len()
            );
            assert_eq!(spans(&first), spans(second), "re-diarize changed a segment or a speaker");
        }
    }
}
