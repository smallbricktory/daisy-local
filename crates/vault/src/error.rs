//! Error type for vault operations.

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("passphrase is too short: {0} chars (minimum {1})")]
    PassphraseTooShort(usize, usize),

    #[error("passphrase too weak (score {score}/4): {feedback}")]
    WeakPassphrase { score: u8, feedback: String },

    #[error("authentication failed — wrong passphrase or corrupted vault")]
    AuthenticationFailed,

    #[error("vault format unsupported: schema_version={0}")]
    UnsupportedVersion(u32),

    #[error("kdf error: {0}")]
    Kdf(String),

    #[error("cipher error: {0}")]
    Cipher(String),

    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, VaultError>;
