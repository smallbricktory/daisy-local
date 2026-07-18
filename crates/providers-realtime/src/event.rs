//! Events emitted by a RealtimeTranscriber as audio is streamed in.

use transcript::Segment;

#[derive(Debug, Clone)]
pub enum RealtimeEvent {
    /// Provider produced an interim hypothesis. May be revised before
    /// being finalized.
    Interim { segment: Segment },

    /// Provider committed a final segment. It is not revised afterwards.
    Final { segment: Segment },

    /// Provider stopped cleanly or hit a recoverable error. The trait
    /// itself does not restart.
    Error(String),
}
