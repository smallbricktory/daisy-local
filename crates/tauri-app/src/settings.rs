//! Settings file reader/writer (plaintext, preferences only — secrets
//! live in the encrypted vault at <profile>/keys.vault.json).

use crate::state::ProviderId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Local ASR engine selection + tunables. Per-machine: engine choice must
/// never leak across machines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AsrSettings {
    /// Active model id: a whisper ggml filename (`ggml-base.en.bin`) or a
    /// parakeet model dir name (`parakeet-tdt-0.6b-v2-int8`). `None` = the
    /// legacy whisper resolution chain.
    pub active: Option<String>,
    pub parakeet: providers_local::parakeet::realtime::ParakeetTune,
    pub whisper: providers_local::streaming::transcriber::WhisperTune,
    /// Annotate energy-positive windows the engine decoded to zero words
    /// with a "possible missed speech" span marker.
    pub faint_audio_marker: bool,

    /// Newer-build fields, carried through untouched (same contract as
    /// [`MachineSettings::unknown`]).
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

impl Default for AsrSettings {
    fn default() -> Self {
        Self {
            active: None,
            parakeet: Default::default(),
            whisper: Default::default(),
            faint_audio_marker: true,
            unknown: BTreeMap::new(),
        }
    }
}

/// Settings that describe ONE machine — its devices, paths, environment,
/// and performance — keyed by machine name in [`Settings::machines`].
/// The profile syncs across machines; each machine reads and writes only
/// its own entry — a per-hardware choice stored globally would run on
/// hardware it was never meant for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineSettings {
    /// Preferred capture device, by stable device key (survives audio-daemon
    /// restarts and reboots). A missing/foreign key falls back to the system
    /// default at record time.
    #[serde(default)]
    pub default_mic_key: Option<String>,
    /// Resolved runtime id for `default_mic_key` — a fresh view filled by
    /// the machine_settings commands for the frontend, stored as None on
    /// disk. On pre-migration files it carries the legacy persisted id
    /// until the mic-id-to-key migration converts it.
    #[serde(default)]
    pub default_mic_source_id: Option<u32>,
    #[serde(default)]
    pub aec_mode_override: AecModeOverride,
    /// Show the live-captions panel during recording. Display-only: the
    /// live decode always runs (single-pass transcript).
    #[serde(default = "default_true")]
    pub show_live_captions: bool,
    /// DFN3 noise suppression at finalize, applied to the saved audio only.
    /// Diarization always uses the echo-cancelled track, never the denoised
    /// one. The ASR feed is never denoised.
    #[serde(default = "default_false")]
    pub denoise_enabled: bool,
    /// Diagnostic logging level (Settings → Recordings). Read at app
    /// startup; changing it requires a restart.
    #[serde(default)]
    pub debug_level: DebugLevel,
    /// `"kmeans"` (WeSpeaker + k-means) or `"speakrs"` (pyannote
    /// community-1). Platform default: speakrs on macOS (CoreML), k-means
    /// elsewhere (speakrs on CPU is hour-scale on long sessions). speakrs
    /// failures fall back to k-means.
    #[serde(default = "default_diarizer")]
    pub diarizer: String,
    /// Local ASR engine selection + tunables.
    #[serde(default)]
    pub asr: AsrSettings,
    /// Whole-UI zoom factor (1.0 = 100%), clamped 0.5..=2.0 on the
    /// frontend. Display property of this machine's monitor.
    #[serde(default = "default_zoom")]
    pub ui_zoom: f32,
    /// Theme override: stem of a file in `<profile>/css` injected after the
    /// built-in stylesheet. `None` = built-in look. The css files sync in
    /// the profile; which one a machine uses is this machine's choice.
    #[serde(default)]
    pub css_theme: Option<String>,
    /// Light, dark, or follow the OS. Screens differ per machine, so this
    /// stays with the machine like zoom does.
    #[serde(default = "default_appearance")]
    pub appearance: String,
    /// Read-only loopback MCP server (binds 127.0.0.1; refuses queries
    /// while the vault is locked). See docs/MCP.md.
    #[serde(default)]
    pub mcp_enabled: bool,
    #[serde(default = "default_mcp_port")]
    pub mcp_port: u16,
    /// MCP write tools (text-only session import); separate opt-in, read
    /// live per request.
    #[serde(default)]
    pub mcp_allow_write: bool,

    /// Properties this build does not know, carried through untouched. A
    /// machine that runs a newer build and then an older one would otherwise
    /// have the newer build's settings deleted the first time the older one
    /// saved.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

impl Default for MachineSettings {
    fn default() -> Self {
        Self {
            unknown: BTreeMap::new(),
            default_mic_key: None,
            default_mic_source_id: None,
            aec_mode_override: AecModeOverride::Auto,
            show_live_captions: true,
            denoise_enabled: false,
            debug_level: DebugLevel::Off,
            diarizer: default_diarizer(),
            asr: AsrSettings::default(),
            ui_zoom: 1.0,
            css_theme: None,
            appearance: default_appearance(),
            mcp_enabled: false,
            mcp_port: default_mcp_port(),
            mcp_allow_write: false,
        }
    }
}

/// The per-machine settings file next to settings.json:
/// `settings.<MACHINE>.json` (name sanitized). Single writer — only the
/// machine itself — so shared-profile sync can never conflict on it.
pub fn machine_settings_path(settings_path: &Path) -> std::path::PathBuf {
    let name = crate::hardware::sanitized_machine_name();
    settings_path.with_file_name(format!("settings.{name}.json"))
}

impl MachineSettings {
    /// This machine's settings: its own file, else a seed from whatever
    /// legacy data settings.json still carries (not persisted until
    /// [`MachineSettings::save`]).
    pub fn load(settings_path: &Path) -> Self {
        let own = machine_settings_path(settings_path);
        if let Ok(b) = syncsafe::read(&own) {
            if let Ok(m) = serde_json::from_slice::<Self>(&b) {
                return m.seeded_asr().seeded_live_captions();
            }
        }
        let shared = Settings::load_or_default(settings_path);
        shared
            .seed_machine(&crate::hardware::machine_name())
            .seeded_asr()
            .seeded_live_captions()
    }

    /// One-shot seed for the deleted legacy fields, read from the raw-JSON
    /// `unknown` map: an unset `asr.active` inherits the model the legacy
    /// side-load path pointed at, and a legacy hop ladder becomes the
    /// whisper tunable. The legacy keys are dropped so the next save
    /// deletes them from the file.
    fn seeded_asr(mut self) -> Self {
        if let Some(v) = self.unknown.remove("whisper_model_path") {
            // Seed only names that are real registry rows from files that
            // still exist — a stale path or a non-registry side-load must not
            // become a ghost `asr.active` id.
            if self.asr.active.is_none() {
                if let Some(name) = v
                    .as_str()
                    .filter(|p| Path::new(p).is_file())
                    .and_then(|p| Path::new(p).file_name())
                    .and_then(|s| s.to_str())
                    .filter(|n| providers_local::registry::row(n).is_some())
                {
                    self.asr.active = Some(name.to_string());
                }
            }
        }
        if let Some(v) = self.unknown.remove("live_hop_ladder_ms") {
            if self.asr.whisper.hop_ladder_ms.is_none() {
                if let Ok(ladder) = serde_json::from_value::<Option<Vec<i64>>>(v) {
                    self.asr.whisper.hop_ladder_ms = ladder;
                }
            }
        }
        self
    }

    /// One-shot seed: the deleted live-captions axis collapses to the
    /// display boolean. Legacy `live_captions.choice == "off"` → false;
    /// auto/on/absent → true. The legacy block is dropped from `unknown` so
    /// the next save deletes it from the file.
    fn seeded_live_captions(mut self) -> Self {
        if let Some(v) = self.unknown.remove("live_captions") {
            if v.get("choice").and_then(|c| c.as_str()) == Some("off") {
                self.show_live_captions = false;
            }
        }
        self
    }

    /// Atomic write of this machine's file. Also prunes this machine's
    /// entries out of settings.json's legacy maps/blocks (one rewrite, only
    /// when something was actually pruned) so the shared file empties out
    /// as machines migrate.
    pub fn save(&self, settings_path: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        syncsafe::write_atomic(machine_settings_path(settings_path), &bytes)?;

        let name = crate::hardware::machine_name();
        let mut shared = Settings::load_or_default(settings_path);
        let pruned = shared.legacy_machines.remove(&name).is_some()
            | shared.legacy_live_captions_by_machine.remove(&name).is_some()
            | shared.legacy_diarizer_by_machine.remove(&name).is_some();
        if pruned {
            shared.save(settings_path)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Stamped at every save. A file that predates the field, or was written
    /// by a build that did not stamp it, reads as this build's version and
    /// is reconciled field by field like any other.
    #[serde(default = "default_schema")]
    pub schema_version: u32,

    /// Properties this build does not know, carried through untouched. The
    /// profile syncs across machines that may run different builds; dropping
    /// what we do not recognise would let an older machine delete a newer
    /// machine's settings on its next save.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,

    /// Provider used for AI features (meeting summary, Q&A, chapters,
    /// analysis, transcript polish). `None` means the user has not picked
    /// one; AI features show a "pick a provider" banner and copy-with-prompt
    /// paths in the UI. Per-request overrides take precedence.
    #[serde(default)]
    pub default_summary_provider: Option<ProviderId>,

    /// Id of the prompt applied to new summaries (see `summarize::prompts`).
    /// `None` / an unknown id falls back to the Daisy Summarizer built-in at
    /// resolve time.
    #[serde(default)]
    pub default_summary_prompt_id: Option<String>,

    /// The local user's display name. Used by the renderer/summarizer to
    /// label "Me" segments. Captured during first-run Welcome; editable from
    /// Settings → Profile.
    #[serde(default)]
    pub user_display_name: Option<String>,

    /// User-chosen order of the nav-rail items (keys like "record",
    /// "library"). Empty = default order. The Settings item is always
    /// anchored at the bottom and never part of this list.
    #[serde(default)]
    pub nav_order: Vec<String>,

    /// Checks daisy.smbr.app for a newer version on launch and periodically,
    /// and surfaces a dismissible banner. Notify-only; never downloads or
    /// installs anything. Default on.
    #[serde(default = "default_true")]
    pub auto_update_check: bool,

    /// Lead time, in seconds, before a calendar meeting's start at which the
    /// "about to start" reminder popup appears. No Settings UI; settings.json
    /// only. Default 60. 0 disables the reminder.
    #[serde(default = "default_reminder_lead")]
    pub reminder_lead_seconds: u64,

    // Transitional: an interim build stored per-machine blocks inline.
    // Kept while non-empty so other machines can still fold their entries;
    // each machine prunes its own on its first per-machine-file save.
    #[serde(default, rename = "machines", skip_serializing_if = "BTreeMap::is_empty")]
    legacy_machines: BTreeMap<String, MachineSettings>,

    // ---- Legacy machine-scoped fields (pre-`machines` files) -------------
    // Deserialize-only: read for one-shot per-machine seeding, never
    // written back. Deletable once every machine has saved a `machines`
    // entry.
    #[serde(default, rename = "default_mic_source_id", skip_serializing)]
    legacy_default_mic_source_id: Option<u32>,
    #[serde(default, rename = "aec_mode_override", skip_serializing)]
    legacy_aec_mode_override: Option<AecModeOverride>,
    // The two legacy per-machine maps keep serializing while non-empty:
    // they may still hold OTHER machines' entries, which only those
    // machines may fold into `machines` (platform-dependent defaults make
    // cross-machine folding wrong). Each machine prunes its own entry on
    // its first write; the empty maps then vanish from the file.
    #[serde(
        default,
        rename = "live_captions_by_machine",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    legacy_live_captions_by_machine: BTreeMap<String, serde_json::Value>,
    #[serde(default, rename = "denoise_enabled", skip_serializing)]
    legacy_denoise_enabled: bool,
    #[serde(default, rename = "whisper_model_path", skip_serializing)]
    legacy_whisper_model_path: Option<String>,
    #[serde(default, rename = "debug_level", skip_serializing)]
    legacy_debug_level: DebugLevel,
    #[serde(
        default,
        rename = "diarizer_by_machine",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    legacy_diarizer_by_machine: BTreeMap<String, String>,
    #[serde(default, rename = "live_hop_ladder_ms", skip_serializing)]
    legacy_live_hop_ladder_ms: Option<Vec<i64>>,
    #[serde(default = "default_zoom", rename = "ui_zoom", skip_serializing)]
    legacy_ui_zoom: f32,
    #[serde(default, rename = "mcp_enabled", skip_serializing)]
    legacy_mcp_enabled: bool,
    #[serde(default = "default_mcp_port", rename = "mcp_port", skip_serializing)]
    legacy_mcp_port: u16,
    #[serde(default, rename = "mcp_allow_write", skip_serializing)]
    legacy_mcp_allow_write: bool,
    // `calendar_subscriptions` lives in the encrypted vault (DecryptedKeys),
    // not here; serde ignores the key when present in older settings.json
    // files.
}

fn default_schema() -> u32 {
    Settings::SCHEMA
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

fn default_zoom() -> f32 {
    1.0
}

fn default_reminder_lead() -> u64 {
    60
}

fn default_mcp_port() -> u16 {
    32479
}

/// Follow the OS by default: the app should look like the rest of the desk.
pub fn default_appearance() -> String {
    "system".into()
}

/// This platform's diarizer default: speakrs only where it is
/// hardware-accelerated (macOS/CoreML); k-means everywhere else.
pub fn default_diarizer() -> String {
    if cfg!(target_os = "macos") {
        "speakrs".into()
    } else {
        "kmeans".into()
    }
}

/// Diagnostic logging verbosity. See `Settings::debug_level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DebugLevel {
    /// File at INFO+. The default.
    #[default]
    Off,
    /// File at DEBUG+.
    Basic,
    /// DEBUG+ plus live-Whisper decode + audio gain/clip tracing.
    Full,
}

impl DebugLevel {
    /// Verbose file logging (DEBUG+) for Basic and Full.
    pub fn verbose(self) -> bool {
        !matches!(self, DebugLevel::Off)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AecModeOverride {
    /// Use routing detection (default).
    #[default]
    Auto,
    /// Always run AEC at finalize, regardless of routing.
    Always,
    /// Never run AEC, regardless of routing detection.
    Never,
}

impl Settings {
    // The loader resets settings files whose schema_version differs from
    // SCHEMA to defaults.
    pub const SCHEMA: u32 = 5;

    pub fn defaults() -> Self {
        Self {
            schema_version: Self::SCHEMA,
            unknown: BTreeMap::new(),
            legacy_machines: BTreeMap::new(),
            default_summary_provider: None,
            default_summary_prompt_id: None,
            user_display_name: None,
            nav_order: Vec::new(),
            auto_update_check: true,
            reminder_lead_seconds: 60,
            legacy_default_mic_source_id: None,
            legacy_aec_mode_override: None,
            legacy_live_captions_by_machine: BTreeMap::new(),
            legacy_denoise_enabled: false,
            legacy_whisper_model_path: None,
            legacy_debug_level: DebugLevel::Off,
            legacy_diarizer_by_machine: BTreeMap::new(),
            legacy_live_hop_ladder_ms: None,
            legacy_ui_zoom: 1.0,
            legacy_mcp_enabled: false,
            legacy_mcp_port: 32479,
            legacy_mcp_allow_write: false,
        }
    }

    /// One-shot seed for a machine with no settings file yet: the interim
    /// inline block if an old build wrote one, else the legacy global
    /// fields. A foreign mic id is harmless (record-time fallback); a model
    /// path is adopted only when it exists on THIS machine.
    fn seed_machine(&self, name: &str) -> MachineSettings {
        if let Some(m) = self.legacy_machines.get(name) {
            return m.clone();
        }
        MachineSettings {
            unknown: BTreeMap::new(),
            default_mic_key: None,
            default_mic_source_id: self.legacy_default_mic_source_id,
            aec_mode_override: self
                .legacy_aec_mode_override
                .unwrap_or(AecModeOverride::Auto),
            show_live_captions: self
                .legacy_live_captions_by_machine
                .get(name)
                .and_then(|v| v.get("choice"))
                .and_then(|c| c.as_str())
                != Some("off"),
            denoise_enabled: self.legacy_denoise_enabled,
            debug_level: self.legacy_debug_level,
            diarizer: self
                .legacy_diarizer_by_machine
                .get(name)
                .cloned()
                .unwrap_or_else(default_diarizer),
            asr: AsrSettings {
                active: self
                    .legacy_whisper_model_path
                    .as_deref()
                    .filter(|p| Path::new(p).is_file())
                    .and_then(|p| Path::new(p).file_name())
                    .and_then(|s| s.to_str())
                    .filter(|n| providers_local::registry::row(n).is_some())
                    .map(str::to_string),
                whisper: providers_local::streaming::transcriber::WhisperTune {
                    hop_ladder_ms: self.legacy_live_hop_ladder_ms.clone(),
                    ..Default::default()
                },
                ..Default::default()
            },
            ui_zoom: self.legacy_ui_zoom,
            css_theme: None,
            appearance: default_appearance(),
            mcp_enabled: self.legacy_mcp_enabled,
            mcp_port: self.legacy_mcp_port,
            mcp_allow_write: self.legacy_mcp_allow_write,
        }
    }

    /// Read settings.json.
    ///
    /// An older file is reconciled, not reset: serde fills in properties this
    /// build added, `unknown` carries through anything it does not recognise,
    /// and the version is stamped forward so the next save writes the current
    /// shape. That covers additive schema changes only — a property that
    /// moved, changed meaning, or needs data from outside this file still
    /// needs a step in `migrations.rs`.
    ///
    /// A file written by a NEWER build is left alone and defaults are used:
    /// its known properties may mean something this build would misread.
    /// A file we cannot parse is kept beside the new one as
    /// `settings.json.corrupt` rather than silently overwritten.
    pub fn load_or_default(path: &Path) -> Self {
        let Ok(b) = syncsafe::read(path) else {
            return Self::defaults();
        };
        match serde_json::from_slice::<Self>(&b) {
            Ok(s) if s.schema_version > Self::SCHEMA => {
                log::warn!(
                    "settings.json schema_version {} is newer than this build's {}; using defaults without touching the file",
                    s.schema_version,
                    Self::SCHEMA
                );
                Self::defaults()
            }
            Ok(mut s) => {
                if s.schema_version != Self::SCHEMA {
                    log::info!(
                        "settings.json schema_version {} -> {}: reconciled (new properties defaulted, {} unrecognised kept)",
                        s.schema_version,
                        Self::SCHEMA,
                        s.unknown.len()
                    );
                    s.schema_version = Self::SCHEMA;
                }
                s
            }
            Err(e) => {
                log::warn!(
                    "settings.json is unreadable ({e}); using defaults (old file kept as settings.json.corrupt)"
                );
                let backup = path.with_file_name(format!(
                    "{}.corrupt",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("settings.json")
                ));
                if let Err(e) = syncsafe::rename(path, &backup) {
                    log::warn!("could not back up unreadable settings.json: {e}");
                }
                Self::defaults()
            }
        }
    }

    /// Atomic write via tmp+rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        syncsafe::write_atomic(path, &bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unrecognised_key_parses_and_survives_the_next_write() {
        // settings.json written by builds that stored a global live-captions
        // preference (auto/local/cloud/off) still parses, and the key drives
        // nothing here. It is kept rather than deleted: this build cannot
        // tell a key it has retired from one a newer build added. Retiring a
        // key for real is a migration step, which knows it is dead.
        let mut v = serde_json::to_value(Settings::defaults()).unwrap();
        v.as_object_mut().unwrap().insert("live_captions".into(), "cloud".into());
        let s: Settings = serde_json::from_value(v).unwrap();
        assert_eq!(s.schema_version, Settings::SCHEMA);
        assert_eq!(s.unknown.get("live_captions").unwrap(), "cloud");
        let written = serde_json::to_value(&s).unwrap();
        assert_eq!(written["live_captions"], "cloud");
        // …and it is carried at the top level, not nested under a field.
        assert!(!written.as_object().unwrap().contains_key("unknown"));
    }

    #[test]
    fn mcp_fields_default_off() {
        let m = MachineSettings::default();
        assert!(!m.mcp_enabled);
        assert!(!m.mcp_allow_write);
        assert_eq!(m.mcp_port, 32479);
    }

    #[test]
    fn legacy_live_captions_off_seeds_display_bool_false() {
        // Old machine file: axis block present, no show_live_captions.
        let m = serde_json::from_value::<MachineSettings>(serde_json::json!({
            "live_captions": { "choice": "off", "bench_xrt": 2.0 },
        }))
        .unwrap()
        .seeded_live_captions();
        assert!(!m.show_live_captions);
        // The legacy block is dropped so the next save deletes it.
        assert!(!m.unknown.contains_key("live_captions"));

        let m = serde_json::from_value::<MachineSettings>(serde_json::json!({
            "live_captions": { "choice": "auto" },
        }))
        .unwrap()
        .seeded_live_captions();
        assert!(m.show_live_captions);

        // Absent legacy → default true.
        let m = serde_json::from_value::<MachineSettings>(serde_json::json!({}))
            .unwrap()
            .seeded_live_captions();
        assert!(m.show_live_captions);
    }

    #[test]
    fn defaults_match_on_device_stack() {
        let d = Settings::defaults();
        assert_eq!(d.schema_version, Settings::SCHEMA);
        assert!(MachineSettings::default().asr.active.is_none());
        assert!(d.default_summary_provider.is_none());
    }

    #[test]
    fn machine_file_load_seeds_from_legacy_then_owns_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // A pre-split file: legacy globals + legacy per-machine maps.
        let file = serde_json::json!({
            "schema_version": Settings::SCHEMA,
            "default_mic_source_id": 42,
            "denoise_enabled": true,
            "debug_level": "basic",
            "ui_zoom": 1.25,
            "mcp_enabled": true,
            "mcp_port": 40000,
            "whisper_model_path": "/definitely/not/a/real/path/model.bin",
            "diarizer_by_machine": {
                crate::hardware::machine_name(): "speakrs",
                "other-machine": "speakrs",
            },
            "live_captions_by_machine": {
                "other-machine": { "choice": "on", "bench_xrt": 12.5, "benched_at_unix_seconds": 1 },
            },
        });
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();

        // No own file yet → seeded from legacy; foreign model path skipped.
        let m = MachineSettings::load(&path);
        assert_eq!(m.default_mic_source_id, Some(42));
        assert!(m.denoise_enabled);
        assert_eq!(m.debug_level, DebugLevel::Basic);
        assert_eq!(m.ui_zoom, 1.25);
        assert!(m.mcp_enabled);
        assert_eq!(m.mcp_port, 40000);
        // The foreign legacy model path is skipped (file doesn't exist here).
        assert!(m.asr.active.is_none());
        assert_eq!(m.diarizer, "speakrs");

        // Save: own file appears; own legacy map entry pruned from the
        // shared file; the other machine's entries survive untouched.
        m.save(&path).unwrap();
        assert!(machine_settings_path(&path).is_file());
        let shared: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let obj = shared.as_object().unwrap();
        assert_eq!(
            obj["diarizer_by_machine"],
            serde_json::json!({ "other-machine": "speakrs" })
        );
        assert_eq!(
            obj["live_captions_by_machine"]["other-machine"]["bench_xrt"],
            serde_json::json!(12.5)
        );
        // Legacy scalars stop serializing once the shared file is rewritten.
        assert!(!obj.contains_key("default_mic_source_id"));
        assert!(!obj.contains_key("mcp_port"));

        // Subsequent loads read the own file, not the (pruned) legacy data.
        let m2 = MachineSettings::load(&path);
        assert_eq!(m2, m);

        // Round-trip edit sticks.
        let mut m3 = m2;
        m3.diarizer = "kmeans".into();
        m3.save(&path).unwrap();
        assert_eq!(MachineSettings::load(&path).diarizer, "kmeans");
    }

    #[test]
    fn machine_file_load_adopts_interim_inline_block() {
        // A file written by the interim build that stored per-machine
        // blocks inline under "machines".
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let file = serde_json::json!({
            "schema_version": Settings::SCHEMA,
            "machines": {
                crate::hardware::machine_name(): {
                    "default_mic_source_id": 7,
                    "diarizer": "kmeans",
                    "ui_zoom": 1.5,
                },
            },
        });
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        let m = MachineSettings::load(&path);
        assert_eq!(m.default_mic_source_id, Some(7));
        assert_eq!(m.ui_zoom, 1.5);
        m.save(&path).unwrap();
        let shared: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(!shared.as_object().unwrap().contains_key("machines"));
    }
}

#[cfg(test)]
mod asr_settings_tests {
    use super::*;

    #[test]
    fn asr_block_roundtrips_and_keeps_unknown_fields() {
        let json = serde_json::json!({
            "active": "parakeet-tdt-0.6b-v2-int8",
            "parakeet": { "hop_s": 7 },
            "faint_audio_marker": false,
            "future_field": { "x": 1 },
        });
        let a: AsrSettings = serde_json::from_value(json).unwrap();
        assert_eq!(a.active.as_deref(), Some("parakeet-tdt-0.6b-v2-int8"));
        assert_eq!(a.parakeet.hop_s, 7, "explicit field applies");
        assert!(a.parakeet.window_s >= 10, "omitted fields take defaults");
        assert!(!a.faint_audio_marker);
        let back = serde_json::to_value(&a).unwrap();
        assert_eq!(back["future_field"]["x"], 1, "unknown fields survive a save");
    }

    #[test]
    fn defaults_enable_the_marker_and_no_active_model() {
        let a = AsrSettings::default();
        assert!(a.faint_audio_marker);
        assert!(a.active.is_none());
    }

    #[test]
    fn legacy_whisper_path_seeds_active_id() {
        // The deleted field arrives via the raw-JSON unknown map; only an
        // existing file whose name is a registry row seeds `active`.
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("ggml-small.en.bin");
        std::fs::write(&model, b"x").unwrap();
        let m = serde_json::from_value::<MachineSettings>(serde_json::json!({
            "whisper_model_path": model.to_string_lossy(),
            "live_hop_ladder_ms": [1000, 2000],
        }))
        .unwrap()
        .seeded_asr();
        assert_eq!(m.asr.active.as_deref(), Some("ggml-small.en.bin"));
        assert_eq!(m.asr.whisper.hop_ladder_ms.as_deref(), Some(&[1000i64, 2000][..]));
        // The legacy keys are dropped so the next save deletes them.
        assert!(!m.unknown.contains_key("whisper_model_path"));
        assert!(!m.unknown.contains_key("live_hop_ladder_ms"));

        // An explicit active id wins over the legacy path.
        let m = serde_json::from_value::<MachineSettings>(serde_json::json!({
            "whisper_model_path": model.to_string_lossy(),
            "asr": { "active": "parakeet-tdt-0.6b-v2-int8" },
        }))
        .unwrap()
        .seeded_asr();
        assert_eq!(m.asr.active.as_deref(), Some("parakeet-tdt-0.6b-v2-int8"));
    }

    #[test]
    fn legacy_whisper_path_never_seeds_ghost_ids() {
        // A stale path (file gone) seeds nothing.
        let m = serde_json::from_value::<MachineSettings>(serde_json::json!({
            "whisper_model_path": "/definitely/gone/ggml-small.en.bin",
        }))
        .unwrap()
        .seeded_asr();
        assert_eq!(m.asr.active, None);
        assert!(!m.unknown.contains_key("whisper_model_path"), "key still pruned");

        // A non-registry side-load name seeds nothing (resolution could
        // never run it; a later download must not self-activate a ghost).
        let dir = tempfile::tempdir().unwrap();
        let side = dir.path().join("my-custom-model.bin");
        std::fs::write(&side, b"x").unwrap();
        let m = serde_json::from_value::<MachineSettings>(serde_json::json!({
            "whisper_model_path": side.to_string_lossy(),
        }))
        .unwrap()
        .seeded_asr();
        assert_eq!(m.asr.active, None);
    }
}
