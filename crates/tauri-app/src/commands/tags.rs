//! Tag CRUD over a plaintext `<profile>/tags.json` file. The file is written
//! atomically via `.tmp` + rename.

use crate::error::{AppError, Result};
use crate::state::{AppState, Tag};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::now_unix;

fn validate_color(hex: &str) -> Result<()> {
    let h = hex
        .strip_prefix('#')
        .ok_or_else(|| AppError::Config("color must start with #".into()))?;
    if (h.len() == 3 || h.len() == 6) && h.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(AppError::Config(format!("invalid color hex: {hex}")))
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TagsFile {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub tags: Vec<Tag>,
}
fn default_schema() -> u32 {
    1
}

pub fn tags_path(app: &AppState) -> PathBuf {
    app.profile.root().join("tags.json")
}

pub fn load_tags_file(app: &AppState) -> Result<TagsFile> {
    let p = tags_path(app);
    if !p.is_file() {
        return Ok(TagsFile {
            schema_version: 1,
            tags: vec![],
        });
    }
    let bytes = syncsafe::read(&p)?;
    serde_json::from_slice::<TagsFile>(&bytes)
        .map_err(|e| AppError::Config(format!("parse tags.json: {e}")))
}

pub fn save_tags_file(app: &AppState, file: &TagsFile) -> Result<()> {
    let p = tags_path(app);
    let tmp = p.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(file)?;
    syncsafe::write(&tmp, &bytes)?;
    syncsafe::rename(&tmp, &p)?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct CreateTagRequest {
    pub name: String,
    pub color_hex: String,
    pub prompt_md: Option<String>,
    pub vocab_md: Option<String>,
}
#[derive(Debug, Deserialize)]
pub struct UpdateTagRequest {
    pub id: String,
    pub name: Option<String>,
    pub color_hex: Option<String>,
    pub prompt_md: Option<Option<String>>,
    pub vocab_md: Option<Option<String>>,
}
#[derive(Debug, Serialize)]
pub struct DeleteTagResult {
    pub dangling_session_count: usize,
}

pub fn list_tags_impl(app: &AppState) -> Result<Vec<Tag>> {
    let mut tags = load_tags_file(app)?.tags;
    tags.sort_by(|a, b| {
        b.use_count
            .cmp(&a.use_count)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(tags)
}

pub fn search_tags_impl(app: &AppState, query: &str) -> Result<Vec<Tag>> {
    let q = query.to_lowercase();
    Ok(list_tags_impl(app)?
        .into_iter()
        .filter(|t| t.name.to_lowercase().contains(&q))
        .collect())
}

pub fn create_tag_impl(app: &AppState, req: CreateTagRequest) -> Result<Tag> {
    validate_color(&req.color_hex)?;
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Config("tag name is empty".into()));
    }
    let mut file = load_tags_file(app)?;
    if file.tags.iter().any(|t| t.name.eq_ignore_ascii_case(&name)) {
        return Err(AppError::Config(format!(
            "a tag named {name} already exists"
        )));
    }
    let tag = Tag {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        color_hex: req.color_hex,
        prompt_md: req.prompt_md.filter(|s| !s.trim().is_empty()),
        vocab_md: crate::commands::transcribe_priming::sanitize_vocab(req.vocab_md),
        created_at_unix_seconds: now_unix(),
        use_count: 0,
    };
    file.tags.push(tag.clone());
    save_tags_file(app, &file)?;
    Ok(tag)
}

pub fn update_tag_impl(app: &AppState, req: UpdateTagRequest) -> Result<Tag> {
    if let Some(c) = &req.color_hex {
        validate_color(c)?;
    }
    let mut file = load_tags_file(app)?;
    if let Some(new_name) = &req.name {
        let n = new_name.trim();
        if n.is_empty() {
            return Err(AppError::Config("tag name is empty".into()));
        }
        if file
            .tags
            .iter()
            .any(|t| t.id != req.id && t.name.eq_ignore_ascii_case(n))
        {
            return Err(AppError::Config(format!("a tag named {n} already exists")));
        }
    }
    let updated = {
        let t = file
            .tags
            .iter_mut()
            .find(|t| t.id == req.id)
            .ok_or_else(|| AppError::Config(format!("no tag with id {}", req.id)))?;
        if let Some(n) = req.name {
            t.name = n.trim().to_string();
        }
        if let Some(c) = req.color_hex {
            t.color_hex = c;
        }
        if let Some(p) = req.prompt_md {
            t.prompt_md = p.filter(|s| !s.trim().is_empty());
        }
        if let Some(v) = req.vocab_md {
            t.vocab_md = crate::commands::transcribe_priming::sanitize_vocab(v);
        }
        t.clone()
    };
    save_tags_file(app, &file)?;
    Ok(updated)
}

/// Delete a tag. If `force` is false and any session references it, returns Err.
/// If `force` is true, detaches from all sessions then removes it.
pub fn delete_tag_impl(app: &AppState, id: &str, force: bool) -> Result<DeleteTagResult> {
    let referencing = crate::commands::meeting::sessions_referencing_tag(app, id)?;
    if !referencing.is_empty() && !force {
        return Err(AppError::Config(format!(
            "{} session(s) reference this tag — pass force to detach and delete",
            referencing.len()
        )));
    }
    if force {
        for sid in &referencing {
            crate::commands::meeting::detach_tag_from_session(app, sid, id)?;
        }
    }
    let mut file = load_tags_file(app)?;
    file.tags.retain(|t| t.id != id);
    save_tags_file(app, &file)?;
    Ok(DeleteTagResult {
        dangling_session_count: referencing.len(),
    })
}

/// Bump use_count by `delta` (positive on attach, negative on detach) for the
/// listed ids. Missing ids are silently ignored.
pub fn adjust_use_counts(app: &AppState, ids: &[String], delta: i32) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut file = load_tags_file(app)?;
    let mut changed = false;
    for t in file.tags.iter_mut() {
        if ids.contains(&t.id) {
            t.use_count = match delta.signum() {
                1 => t.use_count.saturating_add(delta as u32),
                -1 => t.use_count.saturating_sub((-delta) as u32),
                _ => t.use_count,
            };
            changed = true;
        }
    }
    if changed {
        save_tags_file(app, &file)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_color_hex() {
        assert!(validate_color("#FF6A00").is_ok());
        assert!(validate_color("#abc").is_ok());
        assert!(validate_color("FF6A00").is_err());
        assert!(validate_color("#ggg").is_err());
        assert!(validate_color("#FF6A0").is_err());
    }
}
