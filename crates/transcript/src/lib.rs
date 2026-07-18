//! Transcript model: per-chunk per-track segment lists.

pub mod error;
pub mod model;
pub mod text;
pub mod rms;
pub mod dedup;
pub mod render;
pub mod backchannel;
pub mod promote;
pub mod echo_direction;
pub mod energy_gate;
pub mod gap;

pub use error::{Result, TranscriptError};
pub use model::{ChunkTranscript, Segment, SessionTranscript, Track, TrackTranscript};
