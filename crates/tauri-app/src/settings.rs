//! Settings file reader/writer (plaintext, preferences only — secrets
//! live in the encrypted vault at <profile>/keys.vault.json).

use crate::state::ProviderId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Per-machine live-captions preference. `Auto` follows the stored benchmark
/// verdict (hardware detection until one exists).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LiveCaptionsChoice {
    #[default]
    Auto,
    On,
    Off,
}

/// Live-captions state for one machine, keyed by machine name in
/// [`Settings::live_captions_by_machine`]. Lives in the synced profile;
/// each machine reads and writes only its own entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MachineLiveCaptions {
    #[serde(default)]
    pub choice: LiveCaptionsChoice,
    /// Batch xRT measured by the whisper speed benchmark on this machine.
    #[serde(default)]
    pub bench_xrt: Option<f64>,
    #[serde(default)]
    pub benched_at_unix_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub schema_version: u32,
    pub default_mic_source_id: Option<u32>,
    pub aec_mode_override: AecModeOverride,

    /// Live-captions decision per machine name (see `hardware::machine_name`).
    #[serde(default)]
    pub live_captions_by_machine: BTreeMap<String, MachineLiveCaptions>,

    /// DFN3 noise suppression at finalize, applied to the saved audio only.
    /// Default off. Diarization always uses the echo-cancelled track, never
    /// the denoised one. The ASR feed is never denoised.
    #[serde(default = "default_false")]
    pub denoise_enabled: bool,

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

    /// Optional path to a side-loaded Whisper ggml model file (takes
    /// precedence over the DAISY_WHISPER_MODEL_DIR resolution). No Settings
    /// UI; settings.json only.
    #[serde(default)]
    pub whisper_model_path: Option<String>,

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

    /// Diagnostic logging level (Settings → Recordings). `Off` (default): file
    /// at INFO+. `Basic`: DEBUG+. `Full`: DEBUG+ plus live-Whisper decode
    /// tracing and audio gain/clip telemetry (sets `DAISY_LIVE_TRACE` at
    /// startup). Console stays WARN+ regardless. Read at app startup;
    /// changing it requires a restart.
    #[serde(default)]
    pub debug_level: DebugLevel,

    /// Known speaker count (0/None = auto-detect). The k-means clusterer pins
    /// k to it; passed through as `cluster_speakers`' `known_count`. Applied
    /// on the next Diarize run.
    #[serde(default)]
    pub diarize_max_speakers: Option<u32>,

    /// Which diarizer the finalize/re-diarize path uses: `"kmeans"`
    /// (WeSpeaker + k-means) or `"speakrs"` (pyannote community-1 pipeline).
    /// Platform-dependent default: speakrs on macOS, k-means elsewhere. Set
    /// from Settings → Voiceprints; speakrs failures fall back to k-means.
    #[serde(default = "default_diarizer")]
    pub diarizer: String,

    /// Live-caption catch-up hop ladder (ms), ascending. Overrides the
    /// built-in `[1000,1500,2000,3000,4000,5000]`. No Settings UI;
    /// settings.json only. Malformed input falls back to the default.
    /// `None` = default.
    #[serde(default)]
    pub live_hop_ladder_ms: Option<Vec<i64>>,

    /// Whole-UI zoom factor for the webview (1.0 = 100%). Applied via the
    /// native webview zoom on launch and adjusted with Cmd/Ctrl +/-/0.
    /// Clamped 0.5..=2.0 on the frontend.
    #[serde(default = "default_zoom")]
    pub ui_zoom: f32,

    /// Lead time, in seconds, before a calendar meeting's start at which the
    /// "about to start" reminder popup appears. No Settings UI; settings.json
    /// only. Default 60. 0 disables the reminder.
    #[serde(default = "default_reminder_lead")]
    pub reminder_lead_seconds: u64,

    /// Exposes the read-only loopback MCP server for querying the meeting
    /// library. Off by default. The server binds 127.0.0.1 only and refuses
    /// queries while the vault is locked. See docs/MCP.md.
    #[serde(default)]
    pub mcp_enabled: bool,

    /// TCP port for the loopback MCP server. No Settings UI; settings.json
    /// only. Changing it takes effect on the next toggle or app restart.
    #[serde(default = "default_mcp_port")]
    pub mcp_port: u16,

    /// Allows the MCP server's write tools (text-only session import). Off by
    /// default; separate from `mcp_enabled`. Read live per request; toggling
    /// requires no restart. See docs/MCP.md.
    #[serde(default)]
    pub mcp_allow_write: bool,
    // `calendar_subscriptions` lives in the encrypted vault (DecryptedKeys),
    // not here; serde ignores the key when present in older settings.json
    // files.
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

fn default_diarizer() -> String {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AecModeOverride {
    /// Use routing detection (default).
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
            default_mic_source_id: None,
            aec_mode_override: AecModeOverride::Auto,
            live_captions_by_machine: BTreeMap::new(),
            denoise_enabled: false,
            default_summary_provider: None,
            default_summary_prompt_id: None,
            whisper_model_path: None,
            user_display_name: None,
            nav_order: Vec::new(),
            auto_update_check: true,
            debug_level: DebugLevel::Off,
            diarize_max_speakers: None,
            diarizer: default_diarizer(),
            live_hop_ladder_ms: None,
            ui_zoom: 1.0,
            reminder_lead_seconds: 60,
            mcp_enabled: false,
            mcp_port: 32479,
            mcp_allow_write: false,
        }
    }

    /// Read settings.json. Returns defaults on missing/corrupt file or
    /// schema mismatch.
    pub fn load_or_default(path: &Path) -> Self {
        match syncsafe::read(path) {
            Ok(b) => match serde_json::from_slice::<Self>(&b) {
                Ok(s) if s.schema_version == Self::SCHEMA => s,
                _ => Self::defaults(),
            },
            Err(_) => Self::defaults(),
        }
    }

    /// Atomic write via tmp+rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        syncsafe::write(&tmp, &bytes)?;
        syncsafe::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_live_captions_key_is_ignored() {
        // settings.json written by builds that stored a live-captions
        // preference (auto/local/cloud/off) still parses; the stale key is
        // ignored and drops off on the next write.
        let mut v = serde_json::to_value(Settings::defaults()).unwrap();
        v.as_object_mut().unwrap().insert("live_captions".into(), "cloud".into());
        let s: Settings = serde_json::from_value(v).unwrap();
        assert_eq!(s.schema_version, Settings::SCHEMA);
        assert!(!serde_json::to_value(&s).unwrap().as_object().unwrap().contains_key("live_captions"));
    }

    #[test]
    fn mcp_fields_default_off() {
        let d = Settings::defaults();
        assert!(!d.mcp_enabled);
        assert!(!d.mcp_allow_write);
        assert_eq!(d.mcp_port, 32479);
        // settings.json written before the MCP fields existed still parses
        // (schema stays 5; fields are serde-defaulted).
        let mut v = serde_json::to_value(&d).unwrap();
        v.as_object_mut().unwrap().remove("mcp_enabled");
        v.as_object_mut().unwrap().remove("mcp_port");
        let s: Settings = serde_json::from_value(v).unwrap();
        assert!(!s.mcp_enabled);
        assert_eq!(s.mcp_port, 32479);
    }

    #[test]
    fn defaults_match_on_device_stack() {
        let d = Settings::defaults();
        assert_eq!(d.schema_version, Settings::SCHEMA);
        assert!(d.whisper_model_path.is_none());
        assert!(d.default_summary_provider.is_none());
    }
}
