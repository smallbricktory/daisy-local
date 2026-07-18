//! Local streaming-whisper: serial sliding-window + LocalAgreement-2 live ASR.
//! Implements the `RealtimeTranscriber` trait used by the live pipeline
//! (`LiveMode::Realtime { client }`).
pub mod agreement;
pub mod controller;
pub mod live_metrics;
pub mod transcriber;
pub mod window;
