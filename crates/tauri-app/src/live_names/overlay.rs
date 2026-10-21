// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! `live_speakers.<track>.json`: the live timeline and names of one diarized
//! track, session seconds on the transcript's clock. It stays on this machine.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Which track an instance listens to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    System,
    Mic,
}

impl Track {
    pub fn as_str(self) -> &'static str {
        match self {
            Track::System => "system",
            Track::Mic => "mic",
        }
    }
}

/// Every overlay file a session can hold.
pub const FILES: [&str; 2] = ["live_speakers.system.json", "live_speakers.mic.json"];

pub fn file_name(track: Track) -> String {
    format!("live_speakers.{}.json", track.as_str())
}

pub fn path(session_root: &Path, track: Track) -> PathBuf {
    session_root.join(file_name(track))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayTurn {
    pub speaker: u32,
    pub start_s: f64,
    pub end_s: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayName {
    pub slot: u32,
    pub ordinal: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Overlay {
    pub diarizer: String,
    pub model: String,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_seconds: Option<f64>,
    pub audio_seconds: f64,
    pub origin_s: f64,
    pub updates: u32,
    pub speakers: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_label_latency_mean_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default)]
    pub turns: Vec<OverlayTurn>,
    #[serde(default)]
    pub first_turns: Vec<OverlayTurn>,
    #[serde(default)]
    pub names: Vec<OverlayName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embeddings: Option<u32>,
}

impl Overlay {
    /// An overlay for an instance that never ran, with the reason.
    pub fn failed(model: &str, failure: &str) -> Self {
        Self {
            diarizer: "nemotron".into(),
            model: model.into(),
            mode: "streaming".into(),
            load_seconds: None,
            audio_seconds: 0.0,
            origin_s: 0.0,
            updates: 0,
            speakers: 0,
            first_label_latency_mean_s: None,
            failure: Some(failure.into()),
            turns: vec![],
            first_turns: vec![],
            names: vec![],
            embeddings: None,
        }
    }
}

pub fn write(session_root: &Path, track: Track, overlay: &Overlay) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(overlay).map_err(std::io::Error::other)?;
    syncsafe::write_atomic(path(session_root, track), bytes)
}

pub fn read(session_root: &Path, track: Track) -> Option<Overlay> {
    serde_json::from_slice(&syncsafe::read(path(session_root, track)).ok()?).ok()
}

/// Deletes every overlay file in a session; absent files are fine.
pub fn delete_all(session_root: &Path) {
    for name in FILES {
        let p = session_root.join(name);
        if let Err(e) = syncsafe::remove_file(&p) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("live speakers: remove {}: {e}", p.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_example_round_trips() {
        let json = r#"{"diarizer":"nemotron","model":"nemotron3/low","mode":"streaming",
 "load_seconds":0.31,"audio_seconds":1832.4,"origin_s":0,"updates":1830,"speakers":3,
 "first_label_latency_mean_s":0.74,
 "turns":[{"speaker":0,"start_s":1.2,"end_s":6.8},{"speaker":1,"start_s":7.1,"end_s":9.9}],
 "first_turns":[{"speaker":0,"start_s":1.2,"end_s":6.7}],
 "names":[{"slot":0,"ordinal":1,"voice_id":"8d1e3a40-0000-4000-8000-00000000000b"},{"slot":1,"ordinal":2}],
 "embeddings":41}"#;
        let o: Overlay = serde_json::from_str(json).unwrap();
        assert_eq!(o.names[1].voice_id, None);
        let back: Overlay = serde_json::from_slice(&serde_json::to_vec(&o).unwrap()).unwrap();
        assert_eq!(o, back);
    }

    #[test]
    fn unset_optional_keys_are_absent() {
        let v = serde_json::to_value(Overlay::failed("nemotron3/fast32", "not_downloaded")).unwrap();
        for k in ["load_seconds", "first_label_latency_mean_s", "embeddings"] {
            assert!(v.get(k).is_none(), "{k}");
        }
        assert_eq!(v["failure"], "not_downloaded");
    }

    #[test]
    fn write_read_delete() {
        let td = tempfile::tempdir().unwrap();
        let o = Overlay::failed("nemotron3/fast32", "load_failed");
        write(td.path(), Track::System, &o).unwrap();
        write(td.path(), Track::Mic, &o).unwrap();
        assert_eq!(read(td.path(), Track::System), Some(o));
        delete_all(td.path());
        assert!(read(td.path(), Track::System).is_none() && read(td.path(), Track::Mic).is_none());
        delete_all(td.path());
    }
}
