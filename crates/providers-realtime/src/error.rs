// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Error type for realtime transcription providers.

#[derive(Debug, thiserror::Error)]
pub enum RealtimeError {
    #[error("backend init failed: {0}")]
    Backend(String),
}

pub type Result<T> = std::result::Result<T, RealtimeError>;
