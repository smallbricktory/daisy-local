//! Rotated-JSONL storage engine, generic over the record type.
//!
//! Append-only: one JSON object per line in `<dir>/current.jsonl`; past
//! ~400 KB it is renamed to the next sequence number (0001.jsonl,
//! 0002.jsonl, …) and a fresh current.jsonl starts; only the newest
//! `KEEP_ROTATED` rotated files are retained. A partial final line
//! (crash mid-write) is newline-terminated before the next append and
//! skipped by the reader.
use crate::error::Result;
use serde::{de::DeserializeOwned, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

const ROTATE_BYTES: u64 = 400 * 1024;
/// Rotated files kept after each rotation; older ones are deleted. With
/// current.jsonl that caps a store at ~2 MB.
const KEEP_ROTATED: usize = 4;

fn current_path(dir: &Path) -> PathBuf {
    dir.join("current.jsonl")
}

/// Rotated files, newest sequence first: [0003.jsonl, 0002.jsonl, 0001.jsonl].
fn rotated_paths_newest_first(dir: &Path) -> Vec<PathBuf> {
    let mut seqs: Vec<u32> = std::fs::read_dir(dir)
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
    seqs.into_iter().map(|n| dir.join(format!("{n:04}.jsonl"))).collect()
}

pub fn rotate(dir: &Path) -> Result<()> {
    let cur = current_path(dir);
    if !cur.is_file() {
        return Ok(());
    }
    let next = rotated_paths_newest_first(dir)
        .first()
        .and_then(|p| p.file_stem().and_then(|s| s.to_string_lossy().parse::<u32>().ok()))
        .unwrap_or(0)
        + 1;
    syncsafe::rename(&cur, dir.join(format!("{next:04}.jsonl")))?;
    // Retention: drop rotated files beyond the keep-count, oldest first.
    for old in rotated_paths_newest_first(dir).into_iter().skip(KEEP_ROTATED) {
        if let Err(e) = syncsafe::remove_file(&old) {
            log::warn!("jsonl store: pruning {} failed: {e}", old.display());
        }
    }
    Ok(())
}

pub fn append<T: Serialize>(dir: &Path, rec: &T) -> Result<()> {
    syncsafe::create_dir_all(dir)?;
    let p = current_path(dir);
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
        rotate(dir)?;
    }
    Ok(())
}

/// Lines of one file, newest (last line) first; unparseable lines are skipped.
fn read_file_newest_first<T: DeserializeOwned>(p: &Path) -> Vec<T> {
    let Ok(text) = syncsafe::read_to_string(p) else { return Vec::new() };
    text.lines()
        .filter_map(|l| serde_json::from_str::<T>(l).ok())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// Newest-first across current + rotated files. `skip` entries are consumed
/// before `limit` are returned.
pub fn read<T: DeserializeOwned>(dir: &Path, limit: usize, skip: usize) -> Result<Vec<T>> {
    let mut out = Vec::with_capacity(limit);
    let mut to_skip = skip;
    let mut files = vec![current_path(dir)];
    files.extend(rotated_paths_newest_first(dir));
    for f in files {
        if out.len() >= limit {
            break;
        }
        let rows = read_file_newest_first::<T>(&f);
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
