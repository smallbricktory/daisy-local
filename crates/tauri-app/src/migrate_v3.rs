//! One-shot vault v2 -> v3 cutover. Idempotent (marker file `.migrated_v3`).
//! - Extracts DecryptedKeys.tags into `<profile>/tags.json` (plain JSON).
//! - Strips the `tags` field from the vault payload.
//! - Bumps the in-memory schema_version to 3 and re-encrypts to disk.

use crate::commands::lifecycle::re_encrypt_keys;
use crate::commands::tags::{tags_path, TagsFile};
use crate::error::{AppError, Result};
use crate::state::{AppState, Tag, VaultState};

const MARKER: &str = ".migrated_v3";

pub fn v3_cutover(app: &AppState, vs: &VaultState) -> Result<()> {
    let marker = app.profile.root().join(MARKER);
    if marker.is_file() {
        return Ok(());
    }

    // Reads tags from the on-disk vault payload (parsed as a Value, which
    // retains the `tags` field) before the re-encrypt below.
    let tags_from_disk: Vec<Tag> = {
        let path = app.profile.root().join("keys.vault.json");
        match syncsafe::read(&path) {
            Ok(bytes) if !bytes.is_empty() => {
                let env: vault::Envelope = match serde_json::from_slice(&bytes) {
                    Ok(e) => e,
                    Err(_) => return Ok(()),
                };
                let pass_guard = vs.passphrase.lock().unwrap();
                let pass = pass_guard.as_ref().ok_or_else(|| {
                    AppError::Config("vault passphrase missing during v3 migration".into())
                })?;
                let plain = vault::decrypt(&env, pass.as_str())
                    .map_err(|e| AppError::Config(format!("v3 migration decrypt: {e}")))?;
                let v: serde_json::Value = serde_json::from_slice(&plain)
                    .map_err(|e| AppError::Config(format!("v3 migration parse: {e}")))?;
                serde_json::from_value(v.get("tags").cloned().unwrap_or(serde_json::json!([])))
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        }
    };

    // Bumps the in-memory schema_version to 3.
    {
        let mut g = vs.keys.lock().unwrap();
        let Some(keys) = g.as_mut() else {
            return Err(AppError::Config("vault locked during v3 migration".into()));
        };
        keys.schema_version = 3;
    }

    // Writes tags.json only when not already present.
    let tp = tags_path(app);
    if !tp.is_file() && !tags_from_disk.is_empty() {
        let payload = TagsFile {
            schema_version: 1,
            tags: tags_from_disk,
        };
        let tmp = tp.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&payload)?;
        syncsafe::write(&tmp, &bytes)?;
        syncsafe::rename(&tmp, &tp)?;
    }

    re_encrypt_keys(app, vs)?;

    syncsafe::write(&marker, b"1")
        .map_err(|e| AppError::Config(format!("write v3 marker: {e}")))?;
    Ok(())
}
