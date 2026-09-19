// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Local streaming-whisper: serial sliding-window + LocalAgreement-2 live ASR.
//! Implements the `RealtimeTranscriber` trait used by the live pipeline
//! (`LiveMode::Realtime { client }`).
pub mod agreement;
pub mod controller;
pub mod emit;
pub mod vad;
pub mod live_metrics;
pub mod transcriber;
pub mod window;
