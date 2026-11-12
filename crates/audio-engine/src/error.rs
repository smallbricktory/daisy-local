// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Engine-wide error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("audio I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("WAV: {0}")]
    Wav(#[from] hound::Error),

    #[error("invalid frame: {0}")]
    InvalidFrame(String),

    #[error("PipeWire: {0}")]
    PipeWire(String),

    #[error("WASAPI: {0}")]
    Wasapi(String),

    #[error("macOS audio: {0}")]
    Macos(String),

    #[error("subprocess: {0}")]
    Subprocess(String),

    #[error("source not found: {0}")]
    SourceNotFound(String),

    /// A chunk's track file could not be created. `track` is `"mic"` or
    /// `"system"`.
    #[error("cannot open the {track} track file {}: {source}", path.display())]
    TrackFileOpen {
        track: &'static str,
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    /// Returned by every audio operation on platforms without a native
    /// backend.
    #[error("audio backend not yet implemented on this platform: {0}")]
    NotSupported(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;
