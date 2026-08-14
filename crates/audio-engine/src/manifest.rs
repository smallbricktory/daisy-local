//! Recording manifest. Persisted as JSON next to the WAV files.

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelManifest {
    pub source_id: u32,
    pub source_node_name: String,
    pub source_description: String,
    pub wav_path: PathBuf,
    pub captured_at_unix_seconds: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingManifest {
    pub schema_version: u32,
    pub started_at_unix_seconds: i64,
    pub duration_seconds: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub mic: ChannelManifest,
    pub system: ChannelManifest,
}

impl RecordingManifest {
    pub const SCHEMA: u32 = 1;

    pub fn write(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self).expect("serializable");
        std::fs::write(path, json)?;
        Ok(())
    }
}
