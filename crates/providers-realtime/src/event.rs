//! Events emitted by a RealtimeTranscriber as audio is streamed in.

use transcript::Segment;

#[derive(Debug, Clone)]
pub enum RealtimeEvent {
    /// Provider produced an interim hypothesis. May be revised before
    /// being finalized.
    Interim { segment: Segment },

    /// Provider committed a final segment. It is not revised afterwards.
    Final { segment: Segment },

    /// An energy-positive window decoded to zero words and a VAD confirmed
    /// speech-like audio: possible missed speech. Annotation only — the
    /// decode already ran; nothing is gated on this.
    PossibleMissedSpeech { start_ms: u32, end_ms: u32 },

    /// Decode throughput has been below real time long enough that backlog
    /// keeps growing (emitted throttled while the condition holds). The
    /// pipeline's supervisor decides whether to demote the session to
    /// record-only; audio is never dropped by the backend itself.
    FallingBehind,

    /// Provider stopped cleanly or hit a recoverable error. The trait
    /// itself does not restart.
    Error(String),
}
