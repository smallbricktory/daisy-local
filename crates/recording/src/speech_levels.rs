//! Per-input-device speech/residue level store, `<profile>/speech_levels.json`.
//! Written by finalize (meeting anchors) and the Settings calibration flow;
//! read by the live pipeline (AGC guard seed) and the Settings UI. Keyed by
//! the device description string from the session manifest.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const SPEECH_LEVELS_FILE: &str = "speech_levels.json";
const HISTORY_CAP: usize = 10;
/// Live-guard seed sits this far under the learned speech level.
const LIVE_SEED_MARGIN_DB: f32 = 12.0;
const LIVE_SEED_MIN_DBFS: f32 = -48.0;
/// Ceiling = the static SPEECH_ENV_MIN (0.02): the seed only ever lowers
/// the guard for quiet mics, never raises it above the static floor.
const LIVE_SEED_MAX_DBFS: f32 = -34.0;
/// The seed keeps this much clearance above the device's learned residue
/// level, so a lowered guard can never re-admit echo residue.
const RESIDUE_CLEARANCE_DB: f32 = 6.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LevelSource {
    Meeting,
    Calibration,
}

/// `at_unix` and `residue_dbfs` are on-disk diagnostics today; the residue
/// anchor is the input for a future adaptive live threshold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelSample {
    pub at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub source: LevelSource,
    pub speech_dbfs: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residue_dbfs: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceLevels {
    #[serde(default)]
    pub history: Vec<LevelSample>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_dbfs: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpeechLevels {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceLevels>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveSource {
    Override,
    Calibration,
    Learned,
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectiveLevel {
    pub speech_dbfs: f32,
    pub source: EffectiveSource,
}

impl SpeechLevels {
    pub const SCHEMA: u32 = 1;

    pub fn load(profile_root: &Path) -> Self {
        syncsafe::read(profile_root.join(SPEECH_LEVELS_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, profile_root: &Path) -> std::io::Result<()> {
        let path = profile_root.join(SPEECH_LEVELS_FILE);
        let tmp = path.with_extension("json.tmp");
        syncsafe::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        syncsafe::rename(&tmp, &path)
    }

    /// Append a sample for `device`, replacing any prior entry from the same
    /// session and keeping the newest `HISTORY_CAP` entries.
    pub fn record(&mut self, device: &str, sample: LevelSample) {
        self.schema_version = Self::SCHEMA;
        let d = self.devices.entry(device.to_string()).or_default();
        if let Some(sid) = &sample.session_id {
            d.history.retain(|s| s.session_id.as_deref() != Some(sid));
        }
        d.history.push(sample);
        if d.history.len() > HISTORY_CAP {
            let excess = d.history.len() - HISTORY_CAP;
            d.history.drain(..excess);
        }
    }

    pub fn set_override(&mut self, device: &str, dbfs: Option<f32>) {
        self.schema_version = Self::SCHEMA;
        self.devices.entry(device.to_string()).or_default().override_dbfs = dbfs;
    }

    /// The level the app should treat as this device's speech level:
    /// override > history median. None with no data.
    pub fn effective(&self, device: &str) -> Option<EffectiveLevel> {
        let d = self.devices.get(device)?;
        if let Some(o) = d.override_dbfs {
            return Some(EffectiveLevel {
                speech_dbfs: o,
                source: EffectiveSource::Override,
            });
        }
        if d.history.is_empty() {
            return None;
        }
        let mut vals: Vec<f32> = d.history.iter().map(|s| s.speech_dbfs).collect();
        vals.sort_by(|a, b| a.total_cmp(b));
        let median = vals[vals.len() / 2];
        let source = match d.history.last().map(|s| &s.source) {
            Some(LevelSource::Calibration) => EffectiveSource::Calibration,
            _ => EffectiveSource::Learned,
        };
        Some(EffectiveLevel { speech_dbfs: median, source })
    }

    /// Linear SPEECH_ENV_MIN seed for the live AGC guard, or None to use the
    /// built-in default. Going below the static guard requires learned
    /// residue evidence (the seed stays `RESIDUE_CLEARANCE_DB` above the
    /// device's residue median); a manual override skips that requirement.
    pub fn live_speech_env_min(&self, device: &str) -> Option<f32> {
        let e = self.effective(device)?;
        let mut dbfs = (e.speech_dbfs - LIVE_SEED_MARGIN_DB)
            .clamp(LIVE_SEED_MIN_DBFS, LIVE_SEED_MAX_DBFS);
        if !matches!(e.source, EffectiveSource::Override) {
            let mut res: Vec<f32> = self
                .devices
                .get(device)?
                .history
                .iter()
                .filter_map(|s| s.residue_dbfs)
                .collect();
            if res.is_empty() {
                return None;
            }
            res.sort_by(|a, b| a.total_cmp(b));
            let floor = res[res.len() / 2] + RESIDUE_CLEARANCE_DB;
            if floor > LIVE_SEED_MAX_DBFS {
                return None;
            }
            dbfs = dbfs.max(floor);
        }
        Some(10f32.powf(dbfs / 20.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(session: &str, speech: f32) -> LevelSample {
        LevelSample {
            at_unix: 1_784_000_000,
            session_id: Some(session.into()),
            source: LevelSource::Meeting,
            speech_dbfs: speech,
            residue_dbfs: Some(-40.0),
        }
    }

    #[test]
    fn record_caps_history_and_replaces_same_session() {
        let mut sl = SpeechLevels::default();
        for i in 0..12 {
            sl.record("BRIO", sample(&format!("s{i}"), -14.0));
        }
        assert_eq!(sl.devices["BRIO"].history.len(), 10);
        sl.record("BRIO", sample("s11", -20.0));
        assert_eq!(sl.devices["BRIO"].history.len(), 10);
        assert_eq!(sl.devices["BRIO"].history.last().unwrap().speech_dbfs, -20.0);
    }

    #[test]
    fn effective_prefers_override_then_history_median() {
        let mut sl = SpeechLevels::default();
        assert!(sl.effective("BRIO").is_none());
        sl.record("BRIO", sample("a", -10.0));
        sl.record("BRIO", sample("b", -14.0));
        sl.record("BRIO", sample("c", -18.0));
        let e = sl.effective("BRIO").unwrap();
        assert_eq!(e.speech_dbfs, -14.0);
        assert!(matches!(e.source, EffectiveSource::Learned));
        sl.set_override("BRIO", Some(-22.0));
        let e = sl.effective("BRIO").unwrap();
        assert_eq!(e.speech_dbfs, -22.0);
        assert!(matches!(e.source, EffectiveSource::Override));
        sl.set_override("BRIO", None);
        assert_eq!(sl.effective("BRIO").unwrap().speech_dbfs, -14.0);
    }

    fn sample_with_residue(session: &str, speech: f32, residue: Option<f32>) -> LevelSample {
        LevelSample {
            at_unix: 1_784_000_000,
            session_id: Some(session.into()),
            source: LevelSource::Meeting,
            speech_dbfs: speech,
            residue_dbfs: residue,
        }
    }

    #[test]
    fn live_seed_never_rises_above_static_guard() {
        // Loud mic (-14 dBFS speech): seed caps at the static 0.02 floor.
        let mut sl = SpeechLevels::default();
        sl.record("BRIO", sample("a", -14.0));
        let lin = sl.live_speech_env_min("BRIO").unwrap();
        assert!((lin - 10f32.powf(-34.0 / 20.0)).abs() < 1e-4, "got {lin}");
    }

    #[test]
    fn live_seed_below_static_needs_residue_clearance() {
        // Quiet mic + quiet residue: seed may drop to -48 dBFS.
        let mut sl = SpeechLevels::default();
        sl.record("AirPods", sample_with_residue("a", -40.0, Some(-60.0)));
        let lin = sl.live_speech_env_min("AirPods").unwrap();
        assert!((lin - 10f32.powf(-48.0 / 20.0)).abs() < 1e-4, "got {lin}");

        // Quiet mic but residue near speech level: floor pins to residue+6.
        let mut sl = SpeechLevels::default();
        sl.record("HFP", sample_with_residue("a", -40.0, Some(-44.0)));
        let lin = sl.live_speech_env_min("HFP").unwrap();
        assert!((lin - 10f32.powf(-38.0 / 20.0)).abs() < 1e-4, "got {lin}");

        // No residue evidence: no learned seed at all.
        let mut sl = SpeechLevels::default();
        sl.record("Mystery", sample_with_residue("a", -40.0, None));
        assert!(sl.live_speech_env_min("Mystery").is_none());

        // Residue too hot for any safe lowering: stay static.
        let mut sl = SpeechLevels::default();
        sl.record("HotResidue", sample_with_residue("a", -40.0, Some(-36.0)));
        assert!(sl.live_speech_env_min("HotResidue").is_none());

        // Manual override skips the residue requirement.
        let mut sl = SpeechLevels::default();
        sl.set_override("Manual", Some(-40.0));
        let lin = sl.live_speech_env_min("Manual").unwrap();
        assert!((lin - 10f32.powf(-48.0 / 20.0)).abs() < 1e-4, "got {lin}");

        assert!(sl.live_speech_env_min("nope").is_none());
    }

    #[test]
    fn load_save_roundtrip_and_corrupt_tolerance() {
        let dir = std::env::temp_dir().join(format!("daisy-sl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        syncsafe::create_dir_all(&dir).unwrap();
        let mut sl = SpeechLevels::default();
        sl.record("BRIO", sample("a", -14.0));
        sl.save(&dir).unwrap();
        let back = SpeechLevels::load(&dir);
        assert_eq!(back.devices["BRIO"].history.len(), 1);
        syncsafe::write(dir.join(SPEECH_LEVELS_FILE), b"{garbage").unwrap();
        assert!(SpeechLevels::load(&dir).devices.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
