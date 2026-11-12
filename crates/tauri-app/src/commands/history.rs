// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Outbound-integration push history — `<profile>/integration_history/`
//! rotated JSONL (storage engine: `jsonl_store`).
//!
//! A plaintext append-only log of which meeting was pushed to which
//! destination, when, and with what result.
use crate::error::Result;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub at_unix_seconds: i64,
    pub session_id: String,
    pub meeting_id: String,
    pub meeting_title: Option<String>,
    pub integration_id: String,
    pub integration_name: String,
    /// Destination kind, e.g. "webhook".
    pub kind: String,
    /// Subset of {"summary", "notes", "transcript"} included in the push.
    pub payloads_sent: Vec<String>,
    /// "ok" or "error: <message>".
    pub status: String,
}

pub fn history_dir(app: &AppState) -> PathBuf {
    app.profile.root().join("integration_history")
}

/// Newest-first across current + rotated files. `skip` entries are consumed
/// before `limit` are returned.
pub fn read_history(app: &AppState, limit: usize, skip: usize) -> Result<Vec<HistoryEntry>> {
    super::jsonl_store::read(&history_dir(app), limit, skip)
}

pub fn append_history(app: &AppState, entry: HistoryEntry) -> Result<()> {
    super::jsonl_store::append(&history_dir(app), &entry)
}
