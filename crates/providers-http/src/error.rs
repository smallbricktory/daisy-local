//! Error type for transcription and model-discovery calls.

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("provider returned non-success status {status}: {body}")]
    BadStatus { status: u16, body: String },

    #[error("response decode error: {0}")]
    Decode(#[from] serde_json::Error),

    /// Catch-all for failures that don't map to the variants above
    /// (e.g. local whisper.cpp errors, audio decode failures).
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ProviderError>;
