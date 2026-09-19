// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! `speakers.json`: the per-cluster table a phone writes beside its
//! transcript (schema 2: id, seconds, segments, embedding, name, name source,
//! match cosine, clip range). Names here are canonical; the manifest's
//! `speaker_map` is the view derived from them, and a desktop rename writes
//! both. Every key this build does not declare rides through a rewrite.
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{RecordingError, Result};
use crate::manifest::{SessionManifest, SpeakerLabel};

pub const FILE_NAME: &str = "speakers.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakersFile {
    pub schema_version: u32,
    #[serde(default)]
    pub speakers: Vec<Speaker>,
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Speaker {
    /// The `speaker_id` transcript segments carry.
    pub id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `"user"` for a typed name, `"match"` for one a known voice supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_cosine: Option<f32>,
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// `None` when the session has no `speakers.json`, and when the file does
/// not parse: that is logged, the manifest's own labels stand, and the next
/// write replaces the file.
pub fn read(root: &Path) -> Result<Option<SpeakersFile>> {
    let path = root.join(FILE_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = syncsafe::read(&path).map_err(|e| RecordingError::Io { path: path.clone(), source: e })?;
    match serde_json::from_slice(&bytes) {
        Ok(file) => Ok(Some(file)),
        Err(e) => {
            log::warn!("{}: unreadable, the manifest's speaker labels stand: {e}", path.display());
            Ok(None)
        }
    }
}

/// Make `map` the view of `file`: a named speaker's label takes the name
/// (other label fields kept), a named speaker with no label gets one, an
/// unnamed speaker's label goes. Clusters the file does not list are left
/// alone.
pub fn derive_speaker_map(file: &SpeakersFile, map: &mut Vec<SpeakerLabel>) {
    for sp in &file.speakers {
        match sp.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(name) => {
                let confidence = match sp.name_source.as_deref() {
                    Some("match") => sp.match_cosine,
                    _ => None,
                };
                if let Some(label) = map.iter_mut().find(|l| l.cluster_id == sp.id) {
                    label.display_name = name.to_string();
                    label.match_confidence = confidence;
                } else {
                    map.push(SpeakerLabel {
                        cluster_id: sp.id,
                        display_name: name.to_string(),
                        email: None,
                        voiceprint_id: None,
                        match_confidence: confidence,
                        contact_id: None,
                        extra: Map::new(),
                    });
                }
            }
            None => map.retain(|l| l.cluster_id != sp.id),
        }
    }
}

/// Derive `manifest.speaker_map` from the session's `speakers.json` when it
/// has one.
pub fn apply_to_manifest(root: &Path, manifest: &mut SessionManifest) -> Result<()> {
    if let Some(file) = read(root)? {
        derive_speaker_map(&file, &mut manifest.speaker_map);
    }
    Ok(())
}

/// Set or clear (`None` / blank) a speaker's name in `speakers.json`. A name
/// set here is a typed one: `name_source` becomes `"user"` and any match
/// score goes. Returns whether the file changed; no file or no such speaker
/// changes nothing.
pub fn set_name(root: &Path, cluster_id: u32, name: Option<&str>) -> Result<bool> {
    write_name(root, cluster_id, name, "user", None)
}

/// Name a speaker after the known voice that matched it: `name_source`
/// becomes `"match"` and `match_cosine` the score. Same return as `set_name`.
pub fn set_match(root: &Path, cluster_id: u32, name: &str, cosine: f32) -> Result<bool> {
    write_name(root, cluster_id, Some(name), "match", Some(cosine))
}

fn write_name(root: &Path, cluster_id: u32, name: Option<&str>, source: &str, cosine: Option<f32>) -> Result<bool> {
    let Some(mut file) = read(root)? else {
        return Ok(false);
    };
    let Some(sp) = file.speakers.iter_mut().find(|s| s.id == cluster_id) else {
        return Ok(false);
    };
    sp.name = name.map(str::trim).filter(|n| !n.is_empty()).map(String::from);
    sp.name_source = sp.name.as_ref().map(|_| source.to_string());
    sp.match_cosine = sp.name.as_ref().and(cosine);
    write(root, &file)
}

/// Drop every speaker `keep` does not list, after a re-diarization that
/// minted new clusters. Same return as `set_name`.
pub fn retain(root: &Path, keep: &[u32]) -> Result<bool> {
    let Some(mut file) = read(root)? else {
        return Ok(false);
    };
    let before = file.speakers.len();
    file.speakers.retain(|s| keep.contains(&s.id));
    if file.speakers.len() == before {
        return Ok(false);
    }
    write(root, &file)
}

fn write(root: &Path, file: &SpeakersFile) -> Result<bool> {
    let bytes = serde_json::to_vec_pretty(file)?;
    let path = root.join(FILE_NAME);
    syncsafe::write_atomic_synced(&path, &bytes)
        .map_err(|e| RecordingError::Io { path, source: e })?;
    Ok(true)
}
