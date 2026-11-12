// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! AEC engine error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("audio processing module: {0}")]
    Apm(#[from] webrtc_audio_processing::Error),

    #[error("invalid frame: expected {expected} samples, got {actual}")]
    InvalidFrame { expected: usize, actual: usize },
}

pub type Result<T> = std::result::Result<T, Error>;
