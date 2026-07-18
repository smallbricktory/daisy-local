//! Workflow run history — `<profile>/workflow_history/` rotated JSONL.
//!
//! Append-only: one JSON object per line in `current.jsonl`; past ~400 KB it
//! is renamed to the next sequence number (0001.jsonl, 0002.jsonl, …) and a
//! fresh current.jsonl starts.
use crate::error::Result;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

const ROTATE_BYTES: u64 = 400 * 1024;

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

fn current_path(app: &AppState) -> PathBuf {
    history_dir(app).join("current.jsonl")
}

/// Rotated files, newest sequence first: [0003.jsonl, 0002.jsonl, 0001.jsonl].
fn rotated_paths_newest_first(app: &AppState) -> Vec<PathBuf> {
    let mut seqs: Vec<u32> = std::fs::read_dir(history_dir(app))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    name.strip_suffix(".jsonl").and_then(|s| s.parse::<u32>().ok())
                })
                .collect()
        })
        .unwrap_or_default();
    seqs.sort_unstable_by(|a, b| b.cmp(a));
    seqs.into_iter()
        .map(|n| history_dir(app).join(format!("{n:04}.jsonl")))
        .collect()
}

pub fn rotate(app: &AppState) -> Result<()> {
    let cur = current_path(app);
    if !cur.is_file() {
        return Ok(());
    }
    let next = rotated_paths_newest_first(app)
        .first()
        .and_then(|p| p.file_stem().and_then(|s| s.to_string_lossy().parse::<u32>().ok()))
        .unwrap_or(0)
        + 1;
    syncsafe::rename(&cur, history_dir(app).join(format!("{next:04}.jsonl")))?;
    Ok(())
}

pub fn append_run(app: &AppState, rec: &WorkflowRunRecord) -> Result<()> {
    syncsafe::create_dir_all(history_dir(app))?;
    let p = current_path(app);
    let mut line = serde_json::to_string(rec)?;
    line.push('\n');
    let mut f = syncsafe::retry(|| std::fs::OpenOptions::new().create(true).append(true).open(&p))?;
    // If the last byte isn't a newline, terminate the partial final line
    // before appending.
    if f.metadata()?.len() > 0 {
        use std::io::{Read, Seek, SeekFrom};
        let mut check = syncsafe::open(&p)?;
        check.seek(SeekFrom::End(-1))?;
        let mut last = [0u8; 1];
        check.read_exact(&mut last)?;
        if last[0] != b'\n' {
            f.write_all(b"\n")?;
        }
    }
    f.write_all(line.as_bytes())?;
    if f.metadata().map(|m| m.len() >= ROTATE_BYTES).unwrap_or(false) {
        drop(f);
        rotate(app)?;
    }
    Ok(())
}

/// Lines of one file, newest (last line) first; unparseable lines are skipped.
fn read_file_newest_first(p: &PathBuf) -> Vec<WorkflowRunRecord> {
    let Ok(text) = syncsafe::read_to_string(p) else { return Vec::new() };
    text.lines()
        .filter_map(|l| serde_json::from_str::<WorkflowRunRecord>(l).ok())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// Newest-first across current + rotated files. `skip` entries are consumed
/// before `limit` are returned.
pub fn read_runs(app: &AppState, limit: usize, skip: usize) -> Result<Vec<WorkflowRunRecord>> {
    let mut out = Vec::with_capacity(limit);
    let mut to_skip = skip;
    let mut files = vec![current_path(app)];
    files.extend(rotated_paths_newest_first(app));
    for f in files {
        if out.len() >= limit {
            break;
        }
        let rows = read_file_newest_first(&f);
        for r in rows {
            if to_skip > 0 {
                to_skip -= 1;
                continue;
            }
            out.push(r);
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(out)
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
}
