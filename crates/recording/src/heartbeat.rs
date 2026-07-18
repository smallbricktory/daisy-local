//! PID + heartbeat for liveness detection.
//!
//! Format on disk: two ASCII lines — `<pid>\n<unix_seconds>\n`, in a sidecar
//! text file.

use crate::error::{RecordingError, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct Heartbeat {
    path: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub struct HeartbeatSnapshot {
    pub pid: u32,
    pub last_update_unix: u64,
}

impl HeartbeatSnapshot {
    pub fn age_seconds(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(self.last_update_unix)
    }
}

impl Heartbeat {
    /// Create or overwrite the heartbeat at `path` and stamp it now.
    pub fn create(path: &Path) -> Result<Self> {
        let hb = Self {
            path: path.to_path_buf(),
        };
        hb.touch()?;
        Ok(hb)
    }

    /// Stamp the heartbeat with the current PID and timestamp.
    pub fn touch(&self) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let body = format!("{}\n{}\n", std::process::id(), now);
        // Written via tmp+rename; a partial write is never observable.
        let tmp = self.path.with_extension("hb.tmp");
        {
            let mut f = syncsafe::create(&tmp).map_err(|e| RecordingError::Io {
                path: tmp.clone(),
                source: e,
            })?;
            f.write_all(body.as_bytes())
                .map_err(|e| RecordingError::Io {
                    path: tmp.clone(),
                    source: e,
                })?;
        }
        syncsafe::rename(&tmp, &self.path).map_err(|e| RecordingError::Io {
            path: self.path.clone(),
            source: e,
        })?;
        Ok(())
    }

    pub fn read(path: &Path) -> Result<HeartbeatSnapshot> {
        let s = syncsafe::read_to_string(path).map_err(|e| RecordingError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let mut lines = s.lines();
        let pid: u32 = lines
            .next()
            .and_then(|x| x.parse().ok())
            .ok_or_else(|| RecordingError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "missing pid line",
                ),
            })?;
        let ts: u64 = lines
            .next()
            .and_then(|x| x.parse().ok())
            .ok_or_else(|| RecordingError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "missing ts line",
                ),
            })?;
        Ok(HeartbeatSnapshot {
            pid,
            last_update_unix: ts,
        })
    }

    /// Is the heartbeat at `path` still considered alive?
    /// `max_age_secs` is the staleness threshold: heartbeat is alive if age < max_age_secs.
    pub fn is_alive(path: &Path, max_age_secs: u64) -> bool {
        match Self::read(path) {
            Ok(snap) => snap.age_seconds() < max_age_secs,
            Err(_) => false,
        }
    }
}
