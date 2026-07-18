//! Transcript data model — pure data, serde-only.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Track {
    /// Echo-cancelled mic. Preferred when present in the session.
    MicAec,
    /// Raw mic capture (no AEC).
    Mic,
    /// System audio (loopback).
    System,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Segment {
    pub start_ms: u32,
    pub end_ms: u32,
    pub text: String,
    pub confidence: Option<f32>,
    /// Diarized speaker cluster id, when the provider returns one. Stable
    /// within a single session; mapped to a display name via
    /// `SessionManifest::speaker_map`. `None` means the provider did not
    /// diarize (older transcripts, providers without diarization).
    #[serde(default)]
    pub speaker_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackTranscript {
    pub track: Track,
    pub source_wav_relative: PathBuf,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkTranscript {
    pub chunk_index: u32,
    pub tracks: Vec<TrackTranscript>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionTranscript {
    pub schema_version: u32,
    pub session_id: String,
    pub provider: String,
    pub model: String,
    pub transcribed_at_unix_seconds: i64,
    pub chunks: Vec<ChunkTranscript>,
}

impl SessionTranscript {
    pub const SCHEMA: u32 = 1;
}
