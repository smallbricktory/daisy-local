// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
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
/// Dead-air floor sits this far under the learned residue median.
const DEAD_AIR_MARGIN_DB: f32 = 15.0;
/// Floor ceiling: a bad store entry can never push the watchdog into
/// normal room-tone territory.
const DEAD_AIR_MAX_DBFS: f32 = -45.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LevelSource {
    Meeting,
    Calibration,
}

/// `at_unix` is a diagnostic stamp; `residue_dbfs` anchors the live-seed
/// residue floor and the dead-air watchdog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelSample {
    pub at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub source: LevelSource,
    pub speech_dbfs: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residue_dbfs: Option<f32>,
    /// The capture domain this level was measured in. An OS voice processor
    /// (macOS VoiceProcessingIO) applies its own AGC and delivers speech tens
    /// of dB above raw capture, so the two are not comparable and reads
    /// select only their own domain. Absent = raw, which is what every
    /// sample written before the distinction existed was.
    #[serde(default)]
    pub voice_processed: bool,
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
        syncsafe::write_atomic(&path, serde_json::to_vec_pretty(self)?)
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
    pub fn effective(&self, device: &str, voice_processed: bool) -> Option<EffectiveLevel> {
        let d = self.devices.get(device)?;
        if let Some(o) = d.override_dbfs {
            return Some(EffectiveLevel {
                speech_dbfs: o,
                source: EffectiveSource::Override,
            });
        }
        let in_domain: Vec<&LevelSample> = d
            .history
            .iter()
            .filter(|s| s.voice_processed == voice_processed)
            .collect();
        let mut vals: Vec<f32> = in_domain.iter().map(|s| s.speech_dbfs).collect();
        vals.sort_by(|a, b| a.total_cmp(b));
        let median = *vals.get(vals.len() / 2)?;
        let source = match in_domain.last().map(|s| &s.source) {
            Some(LevelSource::Calibration) => EffectiveSource::Calibration,
            _ => EffectiveSource::Learned,
        };
        Some(EffectiveLevel { speech_dbfs: median, source })
    }

    /// Linear SPEECH_ENV_MIN seed for the live AGC guard, or None to use the
    /// built-in default. Going below the static guard requires learned
    /// residue evidence (the seed stays `RESIDUE_CLEARANCE_DB` above the
    /// device's residue median); a manual override skips that requirement.
    pub fn live_speech_env_min(&self, device: &str, voice_processed: bool) -> Option<f32> {
        let e = self.effective(device, voice_processed)?;
        let mut dbfs = (e.speech_dbfs - LIVE_SEED_MARGIN_DB)
            .clamp(LIVE_SEED_MIN_DBFS, LIVE_SEED_MAX_DBFS);
        if !matches!(e.source, EffectiveSource::Override) {
            let floor = self.residue_median_dbfs(device, voice_processed)? + RESIDUE_CLEARANCE_DB;
            if floor > LIVE_SEED_MAX_DBFS {
                return None;
            }
            dbfs = dbfs.max(floor);
        }
        Some(dbfs_to_linear(dbfs))
    }

    /// i16 peak floor for the dead-air watchdog: residue median −
    /// `DEAD_AIR_MARGIN_DB`, capped at `DEAD_AIR_MAX_DBFS`. None without
    /// residue history — a never-recorded device gets no watchdog.
    pub fn dead_air_floor_i16(&self, device: &str, voice_processed: bool) -> Option<i16> {
        let dbfs =
            (self.residue_median_dbfs(device, voice_processed)? - DEAD_AIR_MARGIN_DB).min(DEAD_AIR_MAX_DBFS);
        Some(((dbfs_to_linear(dbfs) * 32767.0) as i16).max(9))
    }

    /// Median of the device's recorded residue levels; None without any.
    fn residue_median_dbfs(&self, device: &str, voice_processed: bool) -> Option<f32> {
        let mut res: Vec<f32> = self
            .devices
            .get(device)?
            .history
            .iter()
            .filter(|s| s.voice_processed == voice_processed)
            .filter_map(|s| s.residue_dbfs)
            .collect();
        if res.is_empty() {
            return None;
        }
        res.sort_by(|a, b| a.total_cmp(b));
        Some(res[res.len() / 2])
    }
}

fn dbfs_to_linear(dbfs: f32) -> f32 {
    10f32.powf(dbfs / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(session: &str, speech: f32) -> LevelSample {
        LevelSample {
            voice_processed: false,
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
        assert!(sl.effective("BRIO", false).is_none());
        sl.record("BRIO", sample("a", -10.0));
        sl.record("BRIO", sample("b", -14.0));
        sl.record("BRIO", sample("c", -18.0));
        let e = sl.effective("BRIO", false).unwrap();
        assert_eq!(e.speech_dbfs, -14.0);
        assert!(matches!(e.source, EffectiveSource::Learned));
        sl.set_override("BRIO", Some(-22.0));
        let e = sl.effective("BRIO", false).unwrap();
        assert_eq!(e.speech_dbfs, -22.0);
        assert!(matches!(e.source, EffectiveSource::Override));
        sl.set_override("BRIO", None);
        assert_eq!(sl.effective("BRIO", false).unwrap().speech_dbfs, -14.0);
    }

    fn sample_with_residue(session: &str, speech: f32, residue: Option<f32>) -> LevelSample {
        LevelSample {
            at_unix: 1_784_000_000,
            session_id: Some(session.into()),
            source: LevelSource::Meeting,
            speech_dbfs: speech,
            residue_dbfs: residue,
            voice_processed: false,
        }
    }

    #[test]
    fn dead_air_floor_is_residue_minus_margin() {
        let mut sl = SpeechLevels::default();
        sl.record("Test Mic", sample_with_residue("a", -25.0, Some(-50.0)));
        // -50 - 15 = -65 dBFS → 10^(-65/20) * 32767 ≈ 18
        let f = sl.dead_air_floor_i16("Test Mic", false).unwrap();
        assert!((15..=21).contains(&f), "floor {f}");
    }

    #[test]
    fn dead_air_floor_clamps_to_minus_45_dbfs_ceiling() {
        let mut sl = SpeechLevels::default();
        // Absurdly hot residue: floor must cap at -45 dBFS ≈ 184.
        sl.record("Hot Mic", sample_with_residue("a", -10.0, Some(-20.0)));
        let f = sl.dead_air_floor_i16("Hot Mic", false).unwrap();
        assert!((180..=188).contains(&f), "floor {f}");
    }

    #[test]
    fn dead_air_floor_none_without_residue_history() {
        let mut sl = SpeechLevels::default();
        sl.record("Fresh Mic", sample_with_residue("a", -25.0, None));
        assert!(sl.dead_air_floor_i16("Fresh Mic", false).is_none());
        assert!(sl.dead_air_floor_i16("Never Seen", false).is_none());
    }

    #[test]
    fn live_seed_never_rises_above_static_guard() {
        // Loud mic (-14 dBFS speech): seed caps at the static 0.02 floor.
        let mut sl = SpeechLevels::default();
        sl.record("BRIO", sample("a", -14.0));
        let lin = sl.live_speech_env_min("BRIO", false).unwrap();
        assert!((lin - 10f32.powf(-34.0 / 20.0)).abs() < 1e-4, "got {lin}");
    }

    #[test]
    fn live_seed_below_static_needs_residue_clearance() {
        // Quiet mic + quiet residue: seed may drop to -48 dBFS.
        let mut sl = SpeechLevels::default();
        sl.record("AirPods", sample_with_residue("a", -40.0, Some(-60.0)));
        let lin = sl.live_speech_env_min("AirPods", false).unwrap();
        assert!((lin - 10f32.powf(-48.0 / 20.0)).abs() < 1e-4, "got {lin}");

        // Quiet mic but residue near speech level: floor pins to residue+6.
        let mut sl = SpeechLevels::default();
        sl.record("HFP", sample_with_residue("a", -40.0, Some(-44.0)));
        let lin = sl.live_speech_env_min("HFP", false).unwrap();
        assert!((lin - 10f32.powf(-38.0 / 20.0)).abs() < 1e-4, "got {lin}");

        // No residue evidence: no learned seed at all.
        let mut sl = SpeechLevels::default();
        sl.record("Mystery", sample_with_residue("a", -40.0, None));
        assert!(sl.live_speech_env_min("Mystery", false).is_none());

        // Residue too hot for any safe lowering: stay static.
        let mut sl = SpeechLevels::default();
        sl.record("HotResidue", sample_with_residue("a", -40.0, Some(-36.0)));
        assert!(sl.live_speech_env_min("HotResidue", false).is_none());

        // Manual override skips the residue requirement.
        let mut sl = SpeechLevels::default();
        sl.set_override("Manual", Some(-40.0));
        let lin = sl.live_speech_env_min("Manual", false).unwrap();
        assert!((lin - 10f32.powf(-48.0 / 20.0)).abs() < 1e-4, "got {lin}");

        assert!(sl.live_speech_env_min("nope", false).is_none());
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

    #[test]
    fn a_reader_never_sees_the_other_capture_domains_levels() {
        let mut sl = SpeechLevels::default();
        sl.record("Fictional USB Mic", sample_with_residue("s1", -14.0, Some(-50.0)));
        // Raw capture learned -14 dBFS; a voice-processed read must not see it.
        assert!(sl.effective("Fictional USB Mic", false).is_some());
        assert!(sl.effective("Fictional USB Mic", true).is_none());
        assert!(sl.dead_air_floor_i16("Fictional USB Mic", true).is_none());

        let mut vp = sample_with_residue("s2", -40.0, Some(-70.0));
        vp.voice_processed = true;
        sl.record("Fictional USB Mic", vp);
        assert_eq!(sl.effective("Fictional USB Mic", false).unwrap().speech_dbfs, -14.0);
        assert_eq!(sl.effective("Fictional USB Mic", true).unwrap().speech_dbfs, -40.0);
    }

    /// A manual override is the user's answer for the device, so it applies
    /// whichever domain is asking.
    #[test]
    fn an_override_applies_in_both_domains() {
        let mut sl = SpeechLevels::default();
        sl.set_override("Fictional USB Mic", Some(-22.0));
        for domain in [false, true] {
            let e = sl.effective("Fictional USB Mic", domain).unwrap();
            assert_eq!(e.speech_dbfs, -22.0);
            assert!(matches!(e.source, EffectiveSource::Override));
        }
    }
}
