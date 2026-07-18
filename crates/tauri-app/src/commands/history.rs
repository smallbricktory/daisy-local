//! Outbound-integration push history — `<profile>/integration_history.json`.
//!
//! A plaintext append-only log of which meeting was pushed to which
//! destination, when, and with what result. Capped at the most recent
//! 1000 events.

use crate::error::Result;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_ENTRIES: usize = 1000;

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

fn history_path(app: &AppState) -> PathBuf {
    app.profile.root().join("integration_history.json")
}

fn load(app: &AppState) -> Vec<HistoryEntry> {
    match syncsafe::read(history_path(app)) {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Newest-first; `limit` caps the returned count.
pub fn read_history(app: &AppState, limit: Option<usize>) -> Result<Vec<HistoryEntry>> {
    let mut entries = load(app);
    entries.sort_by(|a, b| b.at_unix_seconds.cmp(&a.at_unix_seconds));
    if let Some(n) = limit {
        entries.truncate(n);
    }
    Ok(entries)
}

pub fn append_history(app: &AppState, entry: HistoryEntry) -> Result<()> {
    let mut entries = load(app);
    entries.push(entry);
    if entries.len() > MAX_ENTRIES {
        let drop = entries.len() - MAX_ENTRIES;
        entries.drain(0..drop);
    }
    let bytes = serde_json::to_vec_pretty(&entries)?;
    let p = history_path(app);
    let tmp = p.with_extension("json.tmp");
    syncsafe::write(&tmp, &bytes)?;
    syncsafe::rename(&tmp, &p)?;
    Ok(())
}
