//! Tauri-managed state: profile and decrypted vault keys.

use crate::profile::ProfileDir;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;
use zeroize::Zeroizing;

pub struct AppState {
    pub profile: ProfileDir,
}

impl AppState {
    pub fn new(profile: ProfileDir) -> Self {
        Self { profile }
    }
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
        }
    }

    /// Which wire protocol this provider speaks for structured-JSON chat
    /// (summary / analysis / polish). Anthropic uses its Messages API;
    /// everything else is OpenAI-compatible chat/completions.
    pub fn chat_provider(&self) -> summarize::state_provider::ChatProvider {
        // The app layer routes DaisyGateway through its own signed transport;
        // this function is never invoked for it.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
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
    /// Unknown provider names (entries from removed providers) are dropped
    /// at parse; the `purge-removed-providers` migration persists the
    /// removal.
    #[serde(deserialize_with = "providers_drop_unknown")]
    pub providers: BTreeMap<ProviderId, ProviderConfig>,
    #[serde(default)]
    pub integrations: Vec<Integration>,
    /// Cross-session speaker voiceprints. Vectors are local-only and
    /// AES-encrypted at rest via the vault envelope.
    #[serde(default)]
    pub voiceprints: Vec<Voiceprint>,
    /// Subscribed ICS calendars. The URLs carry secret read-tokens.
    #[serde(default)]
    pub calendar_subscriptions: Vec<CalendarSubscription>,
    /// Per-install Ed25519 signing seed (base64, 32 bytes) for the Daisy Cloud
    /// gateway. Generated lazily on first gateway selection. Never
    /// transmitted; only the derived public key is sent.
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
    #[serde(default)]
    pub email: Option<String>,
    /// Legacy single embedding. `embeddings()` folds it into the gallery.
    /// New enrollments leave this empty and write `vectors`.
    #[serde(default)]
    pub vector: Vec<f32>,
    /// Gallery of L2-normalized speaker embeddings (WeSpeaker, dim
    /// `voiceprints::EMBED_DIM`). Enrolling the same identity again appends
    /// here.
    #[serde(default)]
    pub vectors: Vec<Vec<f32>>,
    pub created_at_unix_seconds: i64,
    /// Sessions this voiceprint has matched in.
    #[serde(default)]
    pub session_ids: Vec<String>,
    /// The Contact this voice belongs to. Set on enroll or by the Contacts
    /// migration.
    #[serde(default)]
    pub contact_id: Option<String>,
}

impl Voiceprint {
    /// All embeddings for this identity: the gallery, plus the legacy single
    /// `vector` when non-empty.
    pub fn embeddings(&self) -> Vec<&Vec<f32>> {
        let mut out: Vec<&Vec<f32>> = self.vectors.iter().collect();
        if !self.vector.is_empty() {
            out.push(&self.vector);
        }
        out
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

/// Tauri-managed state for the unlocked vault. Mutex-guarded; `None` =
/// locked. `DecryptedKeys` does not derive Zeroize: locking drops the keys
/// without explicitly zeroing each String.
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
        // A vault written by a build that still had the deepgram provider
        // must unlock: the unknown entry is dropped, the rest survive.
        let dk: DecryptedKeys = serde_json::from_str(
            r#"{"schema_version":4,"providers":{"deepgram":{"api_key":"dg","model":"nova-3","base_url":null},"groq":{"api_key":"gsk","model":null,"base_url":null}}}"#,
        )
        .unwrap();
        assert_eq!(dk.providers.len(), 1);
        assert!(dk.providers.contains_key(&ProviderId::Groq));
    }

    #[test]
    fn daisy_gateway_wire_contract() {
        // Daisy Cloud gateway: snake_case wire name and a dedicated chat
        // transport.
        assert_eq!(ProviderId::DaisyGateway.as_str(), "daisy_gateway");
        assert_eq!("daisy_gateway".parse::<ProviderId>().unwrap(), ProviderId::DaisyGateway);
        // chat_provider() is never invoked for DaisyGateway; it falls through
        // to the OpenAiCompat shape.
        assert_eq!(
            ProviderId::DaisyGateway.chat_provider(),
            summarize::state_provider::ChatProvider::OpenAiCompat
        );
    }
}
