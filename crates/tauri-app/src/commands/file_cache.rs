// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Mtime-keyed in-memory cache for small profile metadata files.
//!
//! Small non-recording JSON (tags, contacts, prompts) was re-read and
//! re-parsed on every list call. This caches the parsed value keyed by the
//! file's mtime: a cheap `stat` on the hot path, a re-parse only when the
//! file actually changed. A local write OR a Syncthing sync-in bumps the
//! mtime, so the cache stays fresh across machines without a restart — the
//! same discipline the library-list cache uses.
//!
//! mtime granularity can be coarse (WSL 9p, network shares), so a write and
//! read inside the same tick could otherwise serve a stale value; every
//! local writer calls [`invalidate`] after saving to close that window.
//! Cross-machine syncs are seconds apart and are caught by the mtime alone.

use crate::error::Result;
use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

struct Entry {
    mtime: Option<SystemTime>,
    value: Arc<dyn Any + Send + Sync>,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Entry>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Return the cached parse of `path` when its mtime is unchanged, else run
/// `load` and cache the result. `load` must be a pure function of the file's
/// current contents (the value is reused for every caller until the mtime
/// moves). The value is cloned out to the caller.
pub fn cached<T, F>(path: &Path, load: F) -> Result<T>
where
    T: Clone + Send + Sync + 'static,
    F: FnOnce() -> Result<T>,
{
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    {
        let guard = cache().lock().unwrap();
        if let Some(e) = guard.get(path) {
            if e.mtime == mtime {
                if let Some(v) = e.value.downcast_ref::<T>() {
                    return Ok(v.clone());
                }
            }
        }
    }
    let value = load()?;
    cache().lock().unwrap().insert(
        path.to_path_buf(),
        Entry { mtime, value: Arc::new(value.clone()) },
    );
    Ok(value)
}

/// Drop a path's cache entry. Every local writer calls this after saving so
/// the next read re-parses even if the mtime tick has not advanced.
pub fn invalidate(path: &Path) {
    cache().lock().unwrap().remove(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reparses_only_when_mtime_moves() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.json");
        std::fs::write(&p, b"1").unwrap();
        let calls = std::cell::Cell::new(0);
        let load = |p: &Path| {
            calls.set(calls.get() + 1);
            let n: i32 = std::fs::read_to_string(p).unwrap().trim().parse().unwrap();
            cached(p, || Ok(n))
        };
        assert_eq!(load(&p).unwrap(), 1);
        assert_eq!(load(&p).unwrap(), 1); // served from cache
        assert_eq!(calls.get(), 2); // loader ran, cached() short-circuited the parse
        // The cached VALUE ignores the on-disk change until mtime moves...
        // force a new mtime and confirm the fresh value is picked up.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&p, b"2").unwrap();
        assert_eq!(load(&p).unwrap(), 2);
    }

    #[test]
    fn invalidate_forces_reparse_same_tick() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("y.json");
        std::fs::write(&p, b"7").unwrap();
        assert_eq!(cached(&p, || Ok(7i32)).unwrap(), 7);
        // Rewrite within the same mtime tick, then invalidate: the next read
        // must not serve the stale cached value.
        std::fs::write(&p, b"9").unwrap();
        invalidate(&p);
        assert_eq!(cached(&p, || Ok(9i32)).unwrap(), 9);
    }

    #[test]
    fn missing_file_has_none_mtime_and_still_caches() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("absent.json");
        assert_eq!(cached(&p, || Ok(vec![1u8, 2, 3])).unwrap(), vec![1, 2, 3]);
        // Second call served from cache (None == None mtime).
        assert_eq!(cached::<Vec<u8>, _>(&p, || panic!("should not reload")).unwrap(), vec![1, 2, 3]);
    }
}
