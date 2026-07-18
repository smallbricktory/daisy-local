//! Backend helpers for "Save as ..." dialog flows: the frontend picks the
//! destination path via a native file picker; this command writes the bytes.
//! No path validation is performed beyond what the kernel enforces.

use crate::error::{AppError, Result};

pub fn save_text_file_impl(path: &std::path::Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            syncsafe::create_dir_all(parent)
                .map_err(|e| AppError::Config(format!("create parent dir: {e}")))?;
        }
    }
    syncsafe::write(path, contents).map_err(|e| AppError::Config(format!("write: {e}")))?;
    Ok(())
}
