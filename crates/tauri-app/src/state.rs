// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Tauri-managed state: profile and decrypted vault keys.

use crate::profile::ProfileDir;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;
use zeroize::Zeroizing;

pub struct AppState {
    pub profile: ProfileDir,
    /// Model downloads waiting or running on this machine.
    pub downloads: crate::downloads::DownloadQueue,
    /// Serializes MachineSettings load-mutate-save cycles process-wide.
    machine_settings_lock: Mutex<()>,
    /// The same for the profile's settings.json.
    settings_lock: Mutex<()>,
    /// Told which file changed after every settings write.
    settings_written: std::sync::OnceLock<Box<dyn Fn(SettingsFile) + Send + Sync>>,
    /// The pairing key while Settings → Mobile Sync shows one, or the mark
    /// that it ran out.
    pub sync_window: Mutex<Option<crate::commands::sync::SyncWindow>>,
    /// The embedding model, loaded on first use and held for the process.
    embedder: Mutex<Option<embeddings::Encoder>>,
    /// Daisy's built-in model on this machine (see `crate::builtin_model`).
    pub builtin: crate::builtin_model::BuiltinModel,
}

impl AppState {
    pub fn new(profile: ProfileDir) -> Self {
        Self {
            downloads: crate::downloads::DownloadQueue::new(
                crate::downloads::queue_path(&profile.settings_path()),
                profile.models_dir(),
            ),
            profile,
            machine_settings_lock: Mutex::new(()),
            settings_lock: Mutex::new(()),
            settings_written: std::sync::OnceLock::new(),
            sync_window: Mutex::new(None),
            embedder: Mutex::new(None),
            builtin: Default::default(),
        }
    }

    /// Runs `f` with the process-wide embedding encoder, loading it on the
    /// first call. A failed load is retried on the next call.
    pub fn with_embedder<T>(
        &self,
        f: impl FnOnce(&mut embeddings::Encoder) -> crate::error::Result<T>,
    ) -> crate::error::Result<T> {
        let mut slot = self.embedder.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            let encoder = embeddings::Encoder::load().map_err(|e| {
                crate::error::AppError::Config(format!("embedding model not available: {e}"))
            })?;
            *slot = Some(encoder);
        }
        f(slot.as_mut().expect("encoder loaded above"))
    }

    /// This machine's settings, loaded under the process-wide lock.
    pub fn machine_settings(&self) -> crate::settings::MachineSettings {
        let _guard = self
            .machine_settings_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::settings::MachineSettings::load(&self.profile.settings_path())
    }

    /// Load-mutate-save under one lock. Returns the settings as saved.
    pub fn update_machine_settings(
        &self,
        f: impl FnOnce(&mut crate::settings::MachineSettings),
    ) -> std::io::Result<crate::settings::MachineSettings> {
        let saved = {
            let _guard = self
                .machine_settings_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let path = self.profile.settings_path();
            let mut m = crate::settings::MachineSettings::load(&path);
            f(&mut m);
            m.save(&path)?;
            m
        };
        self.settings_changed(SettingsFile::Machine);
        Ok(saved)
    }

    /// The profile's settings, loaded under the process-wide lock.
    pub fn settings(&self) -> crate::settings::Settings {
        let _guard = self.settings_lock.lock().unwrap_or_else(|e| e.into_inner());
        crate::settings::Settings::load_or_default(&self.profile.settings_path())
    }

    /// Load-mutate-save of the profile's settings under one lock. Returns the
    /// settings as saved.
    pub fn update_settings(
        &self,
        f: impl FnOnce(&mut crate::settings::Settings),
    ) -> std::io::Result<crate::settings::Settings> {
        let saved = {
            let _guard = self.settings_lock.lock().unwrap_or_else(|e| e.into_inner());
            let path = self.profile.settings_path();
            let mut s = crate::settings::Settings::load_or_default(&path);
            f(&mut s);
            s.save(&path)?;
            s
        };
        self.settings_changed(SettingsFile::Profile);
        Ok(saved)
    }

    /// Installs what runs after every settings write. The first call wins.
    pub fn on_settings_changed(&self, f: impl Fn(SettingsFile) + Send + Sync + 'static) {
        let _ = self.settings_written.set(Box::new(f));
    }

    /// Runs the installed hook for `which`: a write outside the two update
    /// methods above (the device list renumbering the mic view) calls it too.
    pub fn settings_changed(&self, which: SettingsFile) {
        if let Some(f) = self.settings_written.get() {
            f(which);
        }
    }

    /// The AI features provider on this machine: its own choice when it has
    /// one, else the profile's `default_summary_provider`.
    pub fn ai_provider(&self) -> Option<ProviderId> {
        self.ai_provider_in(None)
    }

    /// [`AppState::ai_provider`] against already-loaded profile settings.
    /// `profile` is read from disk only when it is `None` and this machine has
    /// made no choice of its own.
    pub fn ai_provider_in(
        &self,
        profile: Option<&crate::settings::Settings>,
    ) -> Option<ProviderId> {
        self.machine_settings().ai_provider.unwrap_or_else(|| match profile {
            Some(s) => s.default_summary_provider,
            None => crate::settings::Settings::load_or_default(&self.profile.settings_path())
                .default_summary_provider,
        })
    }

    /// Sets this machine's AI features provider; `None` is none on this
    /// machine.
    pub fn set_ai_provider(&self, provider: Option<ProviderId>) -> std::io::Result<()> {
        self.update_machine_settings(|m| m.ai_provider = Some(provider)).map(|_| ())
    }
}

/// Which settings file a write changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFile {
    /// The profile's settings.json.
    Profile,
    /// This machine's settings.<MACHINE>.json.
    Machine,
}

/// Every AI (summarization) provider Daisy can talk to. Wire format is the
/// snake_case name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    Groq,
    Openai,
    Anthropic,
    LmStudio,
    Ollama,
    DaisyGateway,
    Local,
}

impl ProviderId {
    /// Canonical wire name (matches the serde tag).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Groq => "groq",
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::LmStudio => "lm_studio",
            Self::Ollama => "ollama",
            Self::DaisyGateway => "daisy_gateway",
            Self::Local => "local",
        }
    }

    /// Which wire protocol this provider speaks for structured-JSON chat
    /// (summary / analysis / polish). Anthropic uses its Messages API;
    /// everything else is OpenAI-compatible chat/completions.
    pub fn chat_provider(&self) -> summarize::state_provider::ChatProvider {
        match self {
            Self::Anthropic => summarize::state_provider::ChatProvider::Anthropic,
            _ => summarize::state_provider::ChatProvider::OpenAiCompat,
        }
    }

}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ProviderId {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "groq" => Ok(Self::Groq),
            "openai" => Ok(Self::Openai),
            "anthropic" => Ok(Self::Anthropic),
            "lm_studio" => Ok(Self::LmStudio),
            "ollama" => Ok(Self::Ollama),
            "daisy_gateway" => Ok(Self::DaisyGateway),
            "local" => Ok(Self::Local),
            _ => Err(format!("unknown provider: {s}")),
        }
    }
}

fn providers_drop_unknown<'de, D>(
    de: D,
) -> std::result::Result<BTreeMap<ProviderId, ProviderConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: BTreeMap<String, ProviderConfig> = serde::Deserialize::deserialize(de)?;
    Ok(raw
        .into_iter()
        .filter_map(|(k, v)| k.parse::<ProviderId>().ok().map(|id| (id, v)))
        .collect())
}

/// Per-provider config, stored only inside the encrypted vault.
/// `api_key` is the secret; `model` and `base_url` are optional overrides.
/// `context_tokens` is the context length a local endpoint (LM Studio,
/// Ollama) serves, entered by the user; a longer meeting is summarized in
/// windows that fit it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    #[serde(default)]
    pub context_tokens: Option<u32>,
}

/// A subscribed read-only calendar (ICS feed). Stored in the encrypted
/// vault; the `url` carries a secret token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarSubscription {
    /// Stable id (uuid v4).
    pub id: String,
    /// Human-readable label ("Work", "Personal", "Team Standup").
    pub name: String,
    /// `https://`-flavoured ICS URL (the secret-token publish link).
    pub url: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Hex color used as a stripe / tint when rendering events.
    #[serde(default = "default_calendar_color")]
    pub color_hex: String,
    /// Recordings started from this calendar's events get auto-tagged with
    /// this tag id (orphan-safe; a deleted tag id is ignored).
    #[serde(default)]
    pub tag_id: Option<String>,
    /// ICS event UIDs dismissed from the Calendar view; persists across
    /// refreshes. Capped FIFO at 1000 by the dismiss command.
    #[serde(default)]
    pub dismissed_event_uids: Vec<String>,
}

fn default_calendar_color() -> String {
    "#5BA3D0".into()
}

/// A user-defined tag with an optional summary directive. Stored in the
/// plaintext `<profile>/tags.json` file (see commands::tags).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: String,        // uuid v4 — stable across renames
    pub name: String,
    pub color_hex: String, // "#RRGGBB" or "#RGB"
    pub prompt_md: Option<String>,
    #[serde(default)]
    pub vocab_md: Option<String>,
    pub created_at_unix_seconds: i64,
    pub use_count: u32,
    /// When a person last changed this tag on any device; absent on a tag
    /// nobody has edited since the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_unix_seconds: Option<i64>,
    /// Keys another Daisy client wrote that this build does not declare;
    /// carried through a rewrite untouched.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Which payloads an outbound destination should receive for a meeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PayloadSelection {
    #[serde(default)]
    pub summary: bool,
    #[serde(default)]
    pub notes: bool,
    #[serde(default)]
    pub transcript: bool,
}

/// How a webhook destination authenticates. Carries a secret in two of the
/// three variants; stored only inside the encrypted vault payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebhookAuth {
    None,
    /// A custom request header, e.g. `X-API-Key: <value>`.
    Header { name: String, value: String },
    /// `Authorization: Bearer <token>`.
    Bearer { token: String },
}

impl Default for WebhookAuth {
    fn default() -> Self {
        Self::None
    }
}

/// The destination type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntegrationKind {
    Webhook { url: String, auth: WebhookAuth },
}

/// A user-defined outbound destination ("send this meeting to …"). Stored
/// inside the encrypted vault payload; can carry secrets (auth header value /
/// bearer token).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Integration {
    pub id: String, // uuid v4
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub kind: IntegrationKind,
    #[serde(default)]
    pub payloads: PayloadSelection,
}

fn default_true() -> bool {
    true
}

/// Decrypted vault payload, held in memory while the app is unlocked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecryptedKeys {
    pub schema_version: u32,
    /// Unknown provider names are dropped at parse.
    #[serde(deserialize_with = "providers_drop_unknown")]
    pub providers: BTreeMap<ProviderId, ProviderConfig>,
    #[serde(default)]
    pub integrations: Vec<Integration>,
    /// Cross-session speaker voiceprints. Vectors are local-only and
    /// AES-encrypted at rest via the vault envelope.
    #[serde(default)]
    pub voiceprints: Vec<Voiceprint>,
    /// Samples taken back from a voice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voice_removed: Vec<crate::sync::voices::Removed>,
    /// Voices deleted or merged into another (`voices.json`'s `deleted`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voice_deleted: Vec<crate::sync::records::Tombstone>,
    /// Top-level keys of `voices.json` this build does not declare.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub voices_extra: serde_json::Map<String, serde_json::Value>,
    /// Subscribed ICS calendars. The URLs carry secret read-tokens.
    #[serde(default)]
    pub calendar_subscriptions: Vec<CalendarSubscription>,
    /// Per-install Ed25519 signing seed (base64, 32 bytes). Generated lazily
    /// on first use. Only the derived public key is sent.
    #[serde(default)]
    pub install_privkey: Option<String>,
    /// Bearer token for the loopback MCP server. Encrypted at rest and
    /// available only while unlocked. Minted on first enable. See
    /// `mcp::token` + `mcp::server`.
    #[serde(default)]
    pub mcp_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Voiceprint {
    /// Stable id (uuid v4). Persisted in `SpeakerLabel.voiceprint_id`.
    pub id: String,
    pub display_name: String,
    /// This machine's own; `contacts.json` carries a person's emails.
    #[serde(default)]
    pub email: Option<String>,
    /// Legacy single embedding; the `voice-samples` migration moves it into
    /// `samples`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vector: Vec<f32>,
    /// Legacy gallery; moved into `samples` like `vector`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vectors: Vec<Vec<f32>>,
    /// The gallery: L2-normalized speaker embeddings, each with where it came
    /// from. Travels in `voices.json` (see `sync::voices`).
    #[serde(default)]
    pub samples: Vec<crate::sync::voices::Sample>,
    pub created_at_unix_seconds: i64,
    /// When a person last named or renamed this voice on either device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_unix_seconds: Option<i64>,
    /// Sessions this voiceprint has matched in. This machine's own.
    #[serde(default)]
    pub session_ids: Vec<String>,
    /// The Contact this voice belongs to. Set on enroll or by the Contacts
    /// migration.
    #[serde(default)]
    pub contact_id: Option<String>,
    /// Keys the other device wrote on this voice that this build does not
    /// declare; carried through untouched.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Voiceprint {
    /// The gallery in the space this build embeds in. A sample from another
    /// model is carried, never compared.
    pub fn embeddings(&self) -> Vec<Vec<f32>> {
        self.samples
            .iter()
            .filter(|s| s.model == crate::sync::voices::MODEL)
            .filter_map(|s| crate::sync::voices::decode_vector(&s.v))
            .collect()
    }
}

// Voiceprint equality is bit-for-bit on the embedding floats; approximate
// similarity uses `cosine`.
impl Eq for Voiceprint {}

impl DecryptedKeys {
    pub const SCHEMA: u32 = 4;
}

impl Default for DecryptedKeys {
    fn default() -> Self {
        Self {
            schema_version: Self::SCHEMA,
            providers: BTreeMap::new(),
            integrations: Vec::new(),
            voiceprints: Vec::new(),
            voice_removed: Vec::new(),
            voice_deleted: Vec::new(),
            voices_extra: serde_json::Map::new(),
            calendar_subscriptions: Vec::new(),
            install_privkey: None,
            mcp_token: None,
        }
    }
}

/// Gets the install Ed25519 signing seed (32 bytes), generating + storing one
/// in `keys` if absent or malformed. Returns `(seed, generated)`; when
/// `generated` is true the caller MUST persist `keys` (re-encrypt the vault)
/// and register the derived public key.
pub fn get_or_create_install_seed(keys: &mut DecryptedKeys) -> (Vec<u8>, bool) {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    if let Some(b64) = &keys.install_privkey {
        if let Ok(bytes) = B64.decode(b64) {
            if bytes.len() == 32 {
                return (bytes, false);
            }
        }
    }
    let seed: [u8; 32] = rand::random();
    keys.install_privkey = Some(B64.encode(seed));
    (seed.to_vec(), true)
}

/// Gets the loopback MCP server's bearer token, minting + storing one in
/// `keys` if absent. Returns `(token, generated)`; when `generated` is true
/// the caller MUST persist `keys` (re-encrypt the vault).
pub fn get_or_create_mcp_token(keys: &mut DecryptedKeys) -> (String, bool) {
    if let Some(tok) = &keys.mcp_token {
        if !tok.trim().is_empty() {
            return (tok.clone(), false);
        }
    }
    let tok = crate::mcp::token::generate();
    keys.mcp_token = Some(tok.clone());
    (tok, true)
}

/// Reads the existing install seed (32 bytes) from the vault, or `None` when
/// not set up. Generation + pubkey registration happen via
/// [`get_or_create_install_seed`].
pub fn install_seed(keys: &DecryptedKeys) -> Option<Vec<u8>> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    let bytes = B64.decode(keys.install_privkey.as_ref()?).ok()?;
    (bytes.len() == 32).then_some(bytes)
}

/// Derives the base64 Ed25519 public key from a 32-byte seed.
pub fn pubkey_b64_from_seed(seed: &[u8]) -> Option<String> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    let arr: [u8; 32] = seed.try_into().ok()?;
    let signing = ed25519_dalek::SigningKey::from_bytes(&arr);
    Some(B64.encode(signing.verifying_key().to_bytes()))
}

/// Tauri-managed state for the unlocked vault. Mutex-guarded; `None` = locked.
pub struct VaultState {
    pub keys: Mutex<Option<DecryptedKeys>>,
    /// Unlock passphrase, held while unlocked; writes re-encrypt with it.
    /// Cleared on lock.
    pub passphrase: Mutex<Option<Zeroizing<String>>>,
}

impl VaultState {
    pub fn new() -> Self {
        Self {
            keys: Mutex::new(None),
            passphrase: Mutex::new(None),
        }
    }
    pub fn is_unlocked(&self) -> bool {
        self.keys.lock().unwrap().is_some()
    }
}

impl Default for VaultState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_keeps_keys_it_does_not_declare() {
        let json = r##"{"id":"t1","name":"ops","color_hex":"#123456","prompt_md":null,"created_at_unix_seconds":1,"use_count":0,"phone_icon":"gear"}"##;
        let t: Tag = serde_json::from_str(json).unwrap();
        let v: serde_json::Value = serde_json::to_value(&t).unwrap();
        assert_eq!(v["phone_icon"], serde_json::json!("gear"));
        assert_eq!(v["name"], serde_json::json!("ops"));
    }

    #[test]
    fn mcp_token_minted_once_then_reused() {
        let mut k = DecryptedKeys::default();
        assert!(k.mcp_token.is_none());
        let (t1, gen1) = get_or_create_mcp_token(&mut k);
        assert!(gen1, "first call mints");
        assert!(t1.len() >= 40);
        let (t2, gen2) = get_or_create_mcp_token(&mut k);
        assert!(!gen2, "second call reuses");
        assert_eq!(t1, t2);
        // An old vault payload without the field loads with mcp_token = None.
        let old: DecryptedKeys =
            serde_json::from_str(r#"{"schema_version":4,"providers":{}}"#).unwrap();
        assert!(old.mcp_token.is_none());
    }

    #[test]
    fn decrypted_keys_v3_roundtrips_and_old_payload_loads() {
        let mut dk = DecryptedKeys::default();
        assert_eq!(dk.schema_version, 4);
        dk.providers.insert(
            ProviderId::Groq,
            ProviderConfig {
                api_key: Some("gsk_test".into()),
                model: Some("whisper-large-v3-turbo".into()),
                base_url: Some("https://api.groq.com/openai/v1".into()),
                context_tokens: None,
            },
        );
        let json = serde_json::to_string(&dk).unwrap();
        let back: DecryptedKeys = serde_json::from_str(&json).unwrap();
        assert_eq!(dk, back);
        // v1-shape payloads still load: missing fields default empty.
        let old: DecryptedKeys =
            serde_json::from_str(r#"{"schema_version":1,"providers":{}}"#).unwrap();
        assert!(old.providers.is_empty());
        assert!(old.integrations.is_empty());
        // A provider entry carrying a `roles` key still loads; serde ignores
        // the unknown key.
        let pre: DecryptedKeys = serde_json::from_str(
            r#"{"schema_version":2,"providers":{"openai":{"api_key":"sk","model":null,"base_url":null,"roles":["transcription"]}}}"#,
        )
        .unwrap();
        assert!(pre.providers.contains_key(&ProviderId::Openai));
    }

    #[test]
    fn decrypted_keys_v4_install_privkey_roundtrips_and_v3_loads() {
        assert_eq!(DecryptedKeys::SCHEMA, 4);
        let mut k = DecryptedKeys::default();
        assert!(k.install_privkey.is_none());
        k.install_privkey = Some("AAAA".into());
        let json = serde_json::to_string(&k).unwrap();
        let back: DecryptedKeys = serde_json::from_str(&json).unwrap();
        assert_eq!(back.install_privkey.as_deref(), Some("AAAA"));
        // A v3 payload (no install_privkey) still loads (serde default).
        let v3: DecryptedKeys =
            serde_json::from_str(r#"{"schema_version":3,"providers":{}}"#).unwrap();
        assert!(v3.install_privkey.is_none());
    }

    #[test]
    fn install_seed_generates_persists_and_derives_pubkey() {
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        let mut k = DecryptedKeys::default();
        let (seed1, gen1) = get_or_create_install_seed(&mut k);
        assert!(gen1 && seed1.len() == 32 && k.install_privkey.is_some());
        let (seed2, gen2) = get_or_create_install_seed(&mut k);
        assert!(!gen2 && seed2 == seed1);
        let pk = pubkey_b64_from_seed(&seed1).unwrap();
        assert_eq!(B64.decode(&pk).unwrap().len(), 32);
    }

    #[test]
    fn unknown_provider_entries_are_dropped_on_load() {
        // An unknown provider entry is dropped; the rest survive.
        let dk: DecryptedKeys = serde_json::from_str(
            r#"{"schema_version":4,"providers":{"deepgram":{"api_key":"dg","model":"nova-3","base_url":null},"groq":{"api_key":"gsk","model":null,"base_url":null}}}"#,
        )
        .unwrap();
        assert_eq!(dk.providers.len(), 1);
        assert!(dk.providers.contains_key(&ProviderId::Groq));
    }

    #[test]
    fn daisy_gateway_wire_contract() {
        assert_eq!(ProviderId::DaisyGateway.as_str(), "daisy_gateway");
        assert_eq!("daisy_gateway".parse::<ProviderId>().unwrap(), ProviderId::DaisyGateway);
        assert_eq!(
            ProviderId::DaisyGateway.chat_provider(),
            summarize::state_provider::ChatProvider::OpenAiCompat
        );
    }

    /// Runs only where the embedding model is staged (`DAISY_EMBED_DIR`).
    #[test]
    fn embedder_is_loaded_once_per_process() {
        if !embeddings::model_dir().join("model.onnx").is_file() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let app = AppState::new(ProfileDir::at(tmp.path()).unwrap());
        let first = app
            .with_embedder(|e| Ok(e as *const embeddings::Encoder as usize))
            .unwrap();
        let second = app
            .with_embedder(|e| Ok(e as *const embeddings::Encoder as usize))
            .unwrap();
        assert_eq!(first, second);
    }
}
