//! The batch `Transcriber` trait + session orchestrator, and HTTP model
//! discovery for the (summarization) providers. The sole transcriber
//! implementation is on-device Whisper in `providers-local`.

pub mod error;
pub mod models;
pub mod orchestrator;

pub use error::{ProviderError, Result};
pub use models::{list_models, normalize_compat_base, probe_chat_path};
pub use orchestrator::{clear_chunk_transcript_checkpoints, transcribe_session};
use std::path::Path;
use transcript::Segment;

/// A blocking transcription provider: a 16 kHz mono WAV file in,
/// time-stamped segments in chronological order out.
pub trait Transcriber: Send + Sync {
    /// Stable string identifying the provider (e.g. `"whisper_local"`).
    fn name(&self) -> &'static str;

    /// Stable string identifying the model in use (e.g. a ggml file name).
    fn model(&self) -> &str;

    /// Transcribe a WAV file. `language_hint` of `Some("en")` accelerates
    /// Whisper-family models; `None` lets the model auto-detect.
    fn transcribe(&self, wav_path: &Path, language_hint: Option<&str>) -> Result<Vec<Segment>>;
}
