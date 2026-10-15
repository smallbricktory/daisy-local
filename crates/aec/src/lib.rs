// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Acoustic echo cancellation (WebRTC AEC3).
//!
//! Streaming AEC: feed [`AcousticEchoCanceller::FRAME_SIZE`] samples of mic +
//! far at a time; output is the same number of echo-cancelled mic samples.
//! AEC3 estimates the mic↔reference delay itself, so callers only need the
//! two streams roughly paired, not sample-aligned.

pub mod align;
pub mod denormals;
pub mod echo_canceller;
pub mod error;

pub use echo_canceller::AcousticEchoCanceller;
pub use error::{Error, Result};
