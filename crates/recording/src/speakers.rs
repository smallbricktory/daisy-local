// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! `speakers.json`: the per-cluster table a phone writes beside its
//! transcript (schema 2: id, seconds, segments, embedding, name, name source,
//! match cosine, clip range). The table is the phone's: the desktop reads
//! its run id (`diarization_id`) and rewrites it only to drop retired
//! clusters or record a run it started itself. Names live in `meta.json`
//! (`crate::session_meta`); this module never reads or writes `name`,
//! `name_source` or `match_cosine`, and a rewrite carries those fields
//! through as bytes it does not look at. Every key this build does not
//! declare rides through a rewrite the same way.
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{RecordingError, Result};

pub const FILE_NAME: &str = "speakers.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakersFile {
    pub schema_version: u32,
    /// The diarization run that produced this table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diarization_id: Option<String>,
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
/// not parse: that is logged, and the next write replaces the file.
pub fn read(root: &Path) -> Result<Option<SpeakersFile>> {
    let path = root.join(FILE_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = syncsafe::read(&path).map_err(|e| RecordingError::Io { path: path.clone(), source: e })?;
    match serde_json::from_slice(&bytes) {
        Ok(file) => Ok(Some(file)),
        Err(e) => {
            log::warn!("{}: unreadable, the run id it carried is lost until the phone resends it: {e}", path.display());
            Ok(None)
        }
    }
}

/// Drop every speaker `keep` does not list, after a re-diarization that
/// minted new clusters. `false` when there was no file or nothing to drop.
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

/// Records the run that produced this table. `false` when there was no file
/// or it already named this run.
pub fn set_run(root: &Path, run: &str) -> Result<bool> {
    let Some(mut file) = read(root)? else {
        return Ok(false);
    };
    if file.diarization_id.as_deref() == Some(run) {
        return Ok(false);
    }
    file.diarization_id = Some(run.to_string());
    write(root, &file)
}

fn write(root: &Path, file: &SpeakersFile) -> Result<bool> {
    let bytes = serde_json::to_vec_pretty(file)?;
    let path = root.join(FILE_NAME);
    syncsafe::write_atomic_synced(&path, &bytes)
        .map_err(|e| RecordingError::Io { path, source: e })?;
    Ok(true)
}
