//! Error type for the transcript layer.

#[derive(Debug, thiserror::Error)]
pub enum TranscriptError {
    #[error("manifest decode/encode error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io error at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, TranscriptError>;
