//! Realtime (streaming) transcription providers: consume an audio sample
//! stream and emit transcript events.
//!
//! For batch (file-in, segments-out) transcription, see `providers-http`
//! and its `Transcriber` trait.

pub mod error;
pub mod event;

pub use error::{RealtimeError, Result};
pub use event::RealtimeEvent;

use transcript::Segment;

/// A streaming transcription provider.
///
/// Implementations consume audio samples from `audio_rx`, push events
/// through `events_tx`, and return the list of accepted final segments when
/// the input channel closes.
///
/// Implementations must:
/// - Close cleanly when audio_rx drops
/// - Emit `RealtimeEvent::Error` for transient problems and return Err only
///   for fatal cases
#[async_trait::async_trait]
pub trait RealtimeTranscriber: Send + Sync {
    /// Stable string identifying the provider (e.g. `"whisper"`).
    fn name(&self) -> &'static str;

    /// Stable string identifying the model in use (e.g. `"ggml-base.en.bin"`).
    fn model(&self) -> &str;

    /// Consume audio samples from `audio_rx` (16-bit signed mono at
    /// `sample_rate`), push events through `events_tx`, return the list
    /// of accepted final segments when the input channel closes.
    async fn run(
        &self,
        sample_rate: u32,
        audio_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<i16>>,
        events_tx: tokio::sync::mpsc::Sender<RealtimeEvent>,
    ) -> Result<Vec<Segment>>;
}
