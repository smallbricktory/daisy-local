// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Transcript model: per-chunk per-track segment lists.

pub mod cpu;
pub mod error;
pub mod model;
pub mod text;
pub mod rms;
pub mod dedup;
pub mod span_clip;
pub mod reading;
pub mod render;
pub mod backchannel;
pub mod casing;
pub mod promote;
pub mod echo_direction;
pub mod energy_gate;
pub mod hallucination;
pub mod gap;
pub mod split;
pub mod wav_io;

pub use error::{Result, TranscriptError};

/// Keeps `items[i]` where `keep[i]` is true, by rebuild — no reliance on
/// `Vec::retain`'s visit order. `keep` must be as long as `items`; a short
/// mask keeps the unmasked tail.
pub fn retain_by_mask<T>(items: &mut Vec<T>, keep: &[bool]) {
    debug_assert_eq!(items.len(), keep.len());
    let old = std::mem::take(items);
    items.extend(
        old.into_iter()
            .zip(keep.iter().copied().chain(std::iter::repeat(true)))
            .filter_map(|(x, k)| k.then_some(x)),
    );
}
pub use wav_io::{read_wav_mono_f32, read_wav_mono_i16, write_wav_mono_i16, WavIoError};
pub use model::{ChunkTranscript, Segment, SessionTranscript, Track, TrackTranscript, Word};
