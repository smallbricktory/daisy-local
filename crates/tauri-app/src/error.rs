//! AppError — what Tauri commands return on failure. Serialized to the
//! frontend as `{ kind, code, message, friendly }`:
//!   * `kind`     — stable variant tag
//!   * `code`     — stable DAISY-Exxx code
//!   * `message`  — raw technical detail
//!   * `friendly` — plain-language message shown to the user

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not recording")]
    NotRecording,
    #[error("already recording")]
    AlreadyRecording,
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("recording: {0}")]
    Recording(String),
    #[error("transcript: {0}")]
    Transcript(String),
    #[error("provider: {0}")]
    Provider(String),
    #[error("io: {0}")]
    Io(String),
    #[error("config: {0}")]
    Config(String),
    #[error("model missing: {size}")]
    ModelMissing { size: String },
    #[error("license expired")]
    LicenseExpired,
    #[error("vault is locked")]
    VaultLocked,
    #[error("daisy cloud not entitled")]
    GatewayNotEntitled,
}

impl AppError {
    /// Stable variant tag.
    pub fn kind(&self) -> &'static str {
        match self {
            AppError::NotRecording => "NotRecording",
            AppError::AlreadyRecording => "AlreadyRecording",
            AppError::SessionNotFound(_) => "SessionNotFound",
            AppError::Recording(_) => "Recording",
            AppError::Transcript(_) => "Transcript",
            AppError::Provider(_) => "Provider",
            AppError::Io(_) => "Io",
            AppError::Config(_) => "Config",
            AppError::ModelMissing { .. } => "ModelMissing",
            AppError::LicenseExpired => "LicenseExpired",
            AppError::VaultLocked => "VaultLocked",
            AppError::GatewayNotEntitled => "GatewayNotEntitled",
        }
    }

    /// Stable error code.
    pub fn code(&self) -> &'static str {
        match self {
            AppError::NotRecording => "DAISY-E001",
            AppError::AlreadyRecording => "DAISY-E002",
            AppError::SessionNotFound(_) => "DAISY-E003",
            AppError::Recording(_) => "DAISY-E010",
            AppError::Transcript(_) => "DAISY-E011",
            AppError::Provider(_) => "DAISY-E012",
            AppError::Io(_) => "DAISY-E013",
            AppError::Config(_) => "DAISY-E014",
            AppError::ModelMissing { .. } => "DAISY-E016",
            AppError::LicenseExpired => "DAISY-E015",
            AppError::VaultLocked => "DAISY-E017",
            AppError::GatewayNotEntitled => "DAISY-E018",
        }
    }

    /// True for faults (logged at `warn`); false for expected control-flow
    /// outcomes (logged at `debug`).
    fn is_fault(&self) -> bool {
        matches!(
            self,
            AppError::SessionNotFound(_)
                | AppError::Recording(_)
                | AppError::Transcript(_)
                | AppError::Provider(_)
                | AppError::Io(_)
                | AppError::Config(_)
        )
    }

    /// Plain-language message for the UI.
    pub fn friendly(&self) -> String {
        match self {
            AppError::NotRecording => "No recording is in progress.".into(),
            AppError::AlreadyRecording => "A recording is already in progress.".into(),
            AppError::SessionNotFound(_) => {
                "That meeting couldn't be found — it may have been deleted.".into()
            }
            AppError::Recording(m) => format!("Something went wrong with the audio engine. {m}"),
            AppError::Transcript(m) => format!("Couldn't process the transcript. {m}"),
            AppError::Provider(m) => format!(
                "The AI provider request failed — check your API key, model, and network. {m}"
            ),
            AppError::Io(m) => format!("A file couldn't be read or written. {m}"),
            AppError::Config(m) => m.clone(),
            AppError::ModelMissing { size } => format!(
                "The local Whisper model ({size}) hasn't been downloaded yet."
            ),
            AppError::LicenseExpired => {
                "Your trial has ended. Activate a license to record, transcribe, and summarize. \
                 Your existing meetings stay accessible."
                    .into()
            }
            AppError::VaultLocked => {
                "The vault is locked. Unlock it to continue.".into()
            }
            AppError::GatewayNotEntitled => {
                "Daisy Cloud is for internal use only. For more information, see \
                 https://www.daisylocal.app/faq#what-is-daisy-cloud."
                    .into()
            }
        }
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        // Every command failure is logged here at serialization time.
        if self.is_fault() {
            log::warn!("command error [{}] {}", self.code(), self);
        } else {
            log::debug!("command error [{}] {}", self.code(), self);
        }
        let mut st = s.serialize_struct("AppError", 4)?;
        st.serialize_field("kind", self.kind())?;
        st.serialize_field("code", self.code())?;
        st.serialize_field("message", &self.to_string())?;
        st.serialize_field("friendly", &self.friendly())?;
        st.end()
    }
}

impl From<recording::RecordingError> for AppError {
    fn from(e: recording::RecordingError) -> Self {
        AppError::Recording(e.to_string())
    }
}
impl From<transcript::TranscriptError> for AppError {
    fn from(e: transcript::TranscriptError) -> Self {
        AppError::Transcript(e.to_string())
    }
}
impl From<providers_http::ProviderError> for AppError {
    fn from(e: providers_http::ProviderError) -> Self {
        AppError::Provider(e.to_string())
    }
}
impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Io(e.to_string())
    }
}
impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Config(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_missing_serializes_with_size_in_message_and_stable_kind_code() {
        let e = AppError::ModelMissing { size: "base.en".into() };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""kind":"ModelMissing""#), "{json}");
        assert!(json.contains(r#""code":"DAISY-E016""#), "{json}");
        assert!(json.contains("base.en"), "{json}");
    }

    #[test]
    fn vault_locked_serializes_with_stable_kind_code_and_friendly() {
        let e = AppError::VaultLocked;
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""kind":"VaultLocked""#), "{json}");
        assert!(json.contains(r#""code":"DAISY-E017""#), "{json}");
        assert!(json.contains("vault is locked"), "{json}");
    }
}
