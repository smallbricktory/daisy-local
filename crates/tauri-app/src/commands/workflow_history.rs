// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Workflow run history — `<profile>/workflow_history/` rotated JSONL
//! (storage engine: `jsonl_store`).
use crate::error::Result;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub label: String,
    /// "ok" or "error: <one line>".
    pub status: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunRecord {
    pub run_id: String,
    pub at_unix_seconds: i64,
    pub workflow_id: String,
    pub workflow_name: String,
    pub session_id: String,
    pub session_title: Option<String>,
    pub trigger: super::workflows::TriggerEvent,
    /// "ok" (all steps ok) | "partial" (some failed) | "error" (all failed)
    /// | "gave-up" (dropped after crash-retry cap).
    pub status: String,
    pub steps: Vec<StepRecord>,
}

pub fn history_dir(app: &AppState) -> PathBuf {
    app.profile.root().join("workflow_history")
}

pub fn rotate(app: &AppState) -> Result<()> {
    super::jsonl_store::rotate(&history_dir(app))
}

pub fn append_run(app: &AppState, rec: &WorkflowRunRecord) -> Result<()> {
    super::jsonl_store::append(&history_dir(app), rec)
}

/// Newest-first across current + rotated files. `skip` entries are consumed
/// before `limit` are returned.
pub fn read_runs(app: &AppState, limit: usize, skip: usize) -> Result<Vec<WorkflowRunRecord>> {
    super::jsonl_store::read(&history_dir(app), limit, skip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::workflows::TriggerEvent;

    fn app() -> (crate::state::AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (crate::state::AppState::new(profile), tmp)
    }

    fn rec(i: usize) -> WorkflowRunRecord {
        WorkflowRunRecord {
            run_id: format!("r{i}"),
            at_unix_seconds: i as i64,
            workflow_id: "w1".into(),
            workflow_name: "Design specs".into(),
            session_id: "s1".into(),
            session_title: Some("Acme kickoff".into()),
            trigger: TriggerEvent::Finalized,
            status: "ok".into(),
            steps: vec![StepRecord { label: "Run prompt: PM".into(), status: "ok".into(), duration_ms: 5 }],
        }
    }

    #[test]
    fn append_then_read_newest_first_with_paging() {
        let (app, _t) = app();
        for i in 0..5 { append_run(&app, &rec(i)).unwrap(); }
        let page1 = read_runs(&app, 2, 0).unwrap();
        assert_eq!(page1.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(), ["r4", "r3"]);
        let page2 = read_runs(&app, 2, 2).unwrap();
        assert_eq!(page2.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(), ["r2", "r1"]);
    }

    #[test]
    fn truncated_last_line_is_skipped() {
        let (app, _t) = app();
        append_run(&app, &rec(0)).unwrap();
        // Simulate crash mid-write: garbage partial line at the tail.
        use std::io::Write;
        let p = history_dir(&app).join("current.jsonl");
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        write!(f, "{{\"run_id\": \"trunc").unwrap();
        let rows = read_runs(&app, 10, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].run_id, "r0");
        // Next append still works and is readable.
        append_run(&app, &rec(1)).unwrap();
        assert_eq!(read_runs(&app, 10, 0).unwrap().len(), 2);
    }

    #[test]
    fn rotation_moves_current_aside_and_reader_walks_both() {
        let (app, _t) = app();
        for i in 0..4 { append_run(&app, &rec(i)).unwrap(); }
        // Force rotation regardless of size threshold.
        rotate(&app).unwrap();
        assert!(history_dir(&app).join("0001.jsonl").is_file());
        for i in 4..6 { append_run(&app, &rec(i)).unwrap(); }
        let all = read_runs(&app, 100, 0).unwrap();
        assert_eq!(all.len(), 6);
        assert_eq!(all[0].run_id, "r5"); // newest first, current file first
        assert_eq!(all[5].run_id, "r0"); // oldest from rotated file
        // Paging across the file boundary.
        let page = read_runs(&app, 3, 1).unwrap();
        assert_eq!(page.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(), ["r4", "r3", "r2"]);
    }

    #[test]
    fn rotation_prunes_rotated_files_beyond_keep_count() {
        let (app, _t) = app();
        for i in 0..6 {
            append_run(&app, &rec(i)).unwrap();
            rotate(&app).unwrap();
        }
        let dir = history_dir(&app);
        // KEEP_ROTATED = 4: 0001/0002 pruned, 0003..0006 retained.
        assert!(!dir.join("0001.jsonl").exists());
        assert!(!dir.join("0002.jsonl").exists());
        assert!(dir.join("0003.jsonl").is_file());
        assert!(dir.join("0006.jsonl").is_file());
        // The reader walks only the retained set.
        let all = read_runs(&app, 100, 0).unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].run_id, "r5");
        assert_eq!(all[3].run_id, "r2");
    }
}
