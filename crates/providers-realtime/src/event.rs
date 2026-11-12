// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Events emitted by a RealtimeTranscriber as audio is streamed in.

use transcript::Segment;

/// Engine-side timing for one unit of work, on the stream clock (ms).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineStat {
    /// One decode turn. `cost_ms` holds the wall-clock cost of each decode
    /// step in the turn (one per chunk for a chunked engine), `step_ms` the
    /// audio those steps advanced, `decoded_ms` the stream position decoded
    /// through, and `backlog_ms` the queued audio the turn drained beyond
    /// the frame that woke it.
    Decode { decoded_ms: u32, step_ms: u32, cost_ms: Vec<u32>, backlog_ms: u32 },
    /// One result batch from a recognizer that decodes on its own threads.
    /// `audio_end_ms` is the batch's last word end and `fed_ms` the audio
    /// handed to the recognizer when the batch arrived.
    Result { audio_end_ms: u32, fed_ms: u32, is_final: bool, words: u32 },
}

#[derive(Debug, Clone)]
pub enum RealtimeEvent {
    /// Provider produced an interim hypothesis. May be revised before
    /// being finalized.
    Interim { segment: Segment },

    /// Provider committed a final segment. It is not revised afterwards.
    /// `segment.words` spans the same text word by word.
    Final { segment: Segment },

    /// Decode throughput has been below real time long enough that backlog
    /// keeps growing (emitted throttled while the condition holds). The
    /// pipeline's supervisor decides whether to demote the session to
    /// record-only; audio is never dropped by the backend itself.
    FallingBehind,

    /// Engine timing for the session telemetry. Sent with `try_send`: a full
    /// channel drops it.
    Stat(EngineStat),

    /// Provider stopped cleanly or hit a recoverable error. The trait
    /// itself does not restart.
    Error(String),
}
