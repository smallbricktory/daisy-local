// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! PID + heartbeat for liveness detection.
//!
//! Format on disk: ASCII lines `<pid>\n<unix_seconds>\n<host>\n`, in a
//! sidecar text file. Files written before the host line existed carry the
//! first two lines only.

use crate::error::{RecordingError, Result};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct Heartbeat {
    path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct HeartbeatSnapshot {
    pub pid: u32,
    pub last_update_unix: u64,
    /// Host name of the machine that wrote it; `None` in files that
    /// predate the host line.
    pub host: Option<String>,
}

static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Names this machine for every heartbeat this process writes. The app
/// passes its network-independent machine name before any recording
/// starts; an empty name marks no heartbeat as this machine's. Returns
/// false when the name was already fixed (by an earlier call or read).
pub fn set_this_host(name: String) -> bool {
    HOST.set(name).is_ok()
}

/// This machine's name, as heartbeats record it: the one
/// [`set_this_host`] gave, else the resolver's host name.
pub fn this_host() -> &'static str {
    HOST.get_or_init(|| sysinfo::System::host_name().unwrap_or_default())
}

impl HeartbeatSnapshot {
    pub fn age_seconds(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(self.last_update_unix)
    }

    /// True when this machine wrote the heartbeat.
    pub fn written_here(&self) -> bool {
        let here = this_host();
        !here.is_empty() && self.host.as_deref() == Some(here)
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
        let body = format!("{}\n{}\n{}\n", std::process::id(), now, this_host());
        syncsafe::write_atomic(&self.path, body.as_bytes()).map_err(|e| RecordingError::Io {
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
        let host = lines.next().map(str::trim).filter(|h| !h.is_empty()).map(str::to_string);
        Ok(HeartbeatSnapshot {
            pid,
            last_update_unix: ts,
            host,
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
