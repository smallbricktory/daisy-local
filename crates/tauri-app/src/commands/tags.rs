// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Tag CRUD over `<profile>/tags.json`, which a paired phone also merges.

use crate::error::{AppError, Result};
use crate::state::{AppState, Tag};
use crate::sync::records::{self, merge_rows, tombstones, union_extra, union_tombstones, FileMerge, Record, Side, Tombstone};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TagsFile {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub tags: Vec<Tag>,
    /// Tags deleted, or merged into another; kept for good so neither
    /// device's copy brings them back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<Tombstone>,
    /// Keys another Daisy wrote at the top of the file that this build does
    /// not declare.
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for TagsFile {
    fn default() -> Self {
        Self { schema_version: 1, tags: vec![], deleted: vec![], extra: Default::default() }
    }
}

fn default_schema() -> u32 {
    1
}

pub fn tags_path(app: &AppState) -> PathBuf {
    app.profile.root().join("tags.json")
}

pub fn load_tags_file(app: &AppState) -> Result<TagsFile> {
    let p = tags_path(app);
    super::file_cache::cached(&p, || records::read_file(&p))
}

/// The file's bytes: canonical, with tags and tombstones each sorted by id.
pub fn encode(file: &TagsFile) -> serde_json::Result<Vec<u8>> {
    let mut f = file.clone();
    for t in f.tags.iter_mut() {
        t.use_count = 0;
    }
    f.tags.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    f.deleted.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    crate::sync::records::canonical(&f)
}

/// Every read-modify-write of `tags.json`, from a person here or a merge of
/// the phone's copy, holds this from the read to the write.
static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn save_tags_file(app: &AppState, file: &TagsFile) -> Result<()> {
    let _held = records::hold(&WRITE);
    records::write_file(&tags_path(app), encode(file)?)
}

/// Reads the catalog, applies `f` and writes the result, with no other
/// writer in between.
pub fn edit_tags<R>(app: &AppState, f: impl FnOnce(&mut TagsFile) -> Result<R>) -> Result<R> {
    records::edit_file(&WRITE, &tags_path(app), encode, f)
}

/// Every write of a session's tag lists.
pub fn edit_session_tags(
    app: &AppState,
    session_dir: &Path,
    edit: impl FnOnce(&mut recording::session_meta::SessionMeta),
) -> Result<()> {
    let retired = records::tag_retirements(app.profile.root());
    recording::session_meta::update(session_dir, |meta| {
        edit(meta);
        meta.retire_tags(&retired.moved, &retired.gone_ids());
    })
    .map_err(|e| AppError::Config(format!("write meta.json: {e}")))?;
    Ok(())
}

fn fold(s: &str) -> String {
    s.trim().to_lowercase()
}

impl Record for Tag {
    fn id(&self) -> &str {
        &self.id
    }
    /// The trimmed names match ignoring case.
    fn same(&self, other: &Self) -> bool {
        fold(&self.name) == fold(&other.name)
    }
    fn updated_at(&self) -> Option<i64> {
        self.updated_at_unix_seconds
    }
    fn row_time(&self) -> i64 {
        self.updated_at_unix_seconds.unwrap_or(self.created_at_unix_seconds)
    }
    fn extra(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.extra
    }
    fn extra_mut(&mut self) -> &mut serde_json::Map<String, serde_json::Value> {
        &mut self.extra
    }
    /// The earliest creation and the latest edit.
    fn absorb(&mut self, group: &[(&Self, Side)]) {
        for (t, _) in group {
            self.created_at_unix_seconds = self.created_at_unix_seconds.min(t.created_at_unix_seconds);
            self.updated_at_unix_seconds = self.updated_at_unix_seconds.max(t.updated_at_unix_seconds);
        }
    }
}

/// Merges the phone's catalog into this desktop's.
pub fn merge_files(desktop: &TagsFile, phone: &TagsFile) -> FileMerge<TagsFile> {
    let graves = union_tombstones(&desktop.deleted, &phone.deleted);
    let m = merge_rows(&desktop.tags, &phone.tags, graves, true);
    let file = TagsFile {
        schema_version: desktop.schema_version.max(phone.schema_version).max(1),
        tags: m.rows.clone(),
        deleted: tombstones(m.graves.clone()),
        extra: union_extra(&desktop.extra, &phone.extra),
    };
    FileMerge::of(file, &m)
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

fn uses_by_tag(app: &AppState) -> std::collections::HashMap<String, u32> {
    let mut counts = std::collections::HashMap::new();
    for s in super::library::list_sessions_impl(app).unwrap_or_default() {
        for id in s.tag_ids {
            *counts.entry(id).or_insert(0) += 1;
        }
    }
    counts
}

pub fn list_tags_impl(app: &AppState) -> Result<Vec<Tag>> {
    let mut tags = load_tags_file(app)?.tags;
    let counts = uses_by_tag(app);
    for t in tags.iter_mut() {
        t.use_count = counts.get(&t.id).copied().unwrap_or(0);
    }
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
    edit_tags(app, |file| {
        if file.tags.iter().any(|t| t.name.eq_ignore_ascii_case(&name)) {
            return Err(AppError::Config(format!("a tag named {name} already exists")));
        }
        let tag = Tag {
            extra: Default::default(),
            id: uuid::Uuid::new_v4().to_string(),
            name,
            color_hex: req.color_hex,
            prompt_md: req.prompt_md.filter(|s| !s.trim().is_empty()),
            vocab_md: crate::commands::transcribe_priming::sanitize_vocab(req.vocab_md),
            created_at_unix_seconds: now_unix(),
            use_count: 0,
            updated_at_unix_seconds: Some(now_unix()),
        };
        file.tags.push(tag.clone());
        Ok(tag)
    })
}

pub fn update_tag_impl(app: &AppState, req: UpdateTagRequest) -> Result<Tag> {
    if let Some(c) = &req.color_hex {
        validate_color(c)?;
    }
    edit_tags(app, |file| {
        if let Some(new_name) = &req.name {
            let n = new_name.trim();
            if n.is_empty() {
                return Err(AppError::Config("tag name is empty".into()));
            }
            if file.tags.iter().any(|t| t.id != req.id && t.name.eq_ignore_ascii_case(n)) {
                return Err(AppError::Config(format!("a tag named {n} already exists")));
            }
        }
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
        t.updated_at_unix_seconds = Some(now_unix());
        Ok(t.clone())
    })
}

/// Delete a tag. If `force` is false and any session references it, returns
/// Err. The tag leaves a tombstone, so the phone's copy does not bring it
/// back, and every session drops it.
pub fn delete_tag_impl(app: &AppState, id: &str, force: bool) -> Result<DeleteTagResult> {
    let referencing = crate::commands::meeting::sessions_referencing_tag(app, id)?;
    if !referencing.is_empty() && !force {
        return Err(AppError::Config(format!(
            "{} session(s) reference this tag — pass force to detach and delete",
            referencing.len()
        )));
    }
    let at = edit_tags(app, |file| Ok(records::delete_row(&mut file.tags, &mut file.deleted, id, now_unix())))?;
    let gone = records::Retirements { gone: [(id.to_string(), at)].into(), ..Default::default() };
    records::apply_retirements(app, None, records::Channel::Tags, &gone)?;
    Ok(DeleteTagResult {
        dangling_session_count: referencing.len(),
    })
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

    fn tag(id: &str, name: &str, at: Option<i64>) -> Tag {
        Tag {
            id: id.into(),
            name: name.into(),
            color_hex: "#123456".into(),
            prompt_md: None,
            vocab_md: None,
            created_at_unix_seconds: 100,
            use_count: 1,
            updated_at_unix_seconds: at,
            extra: Default::default(),
        }
    }

    fn file(tags: Vec<Tag>) -> TagsFile {
        TagsFile { tags, ..Default::default() }
    }

    #[test]
    fn a_tag_only_one_side_holds_is_kept_with_its_id() {
        let m = merge_files(&file(vec![tag("d1", "Ops", None)]), &file(vec![tag("p1", "Harbour", None)]));
        let ids: Vec<&str> = m.file.tags.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["d1", "p1"]);
        assert!(m.desktop_ids.is_empty() && m.phone_ids.is_empty());
    }

    #[test]
    fn the_same_name_in_any_case_is_one_tag_and_with_no_edits_the_desktops_stands() {
        let m = merge_files(&file(vec![tag("d1", "harbour", None)]), &file(vec![tag("p1", "HARBOUR ", None)]));
        assert_eq!(m.file.tags.len(), 1);
        assert_eq!(m.file.tags[0].id, "d1");
        assert_eq!(m.phone_ids, [("p1".to_string(), "d1".to_string())].into());
        let d = &m.file.deleted[0];
        assert_eq!((d.id.as_str(), d.into.as_deref()), ("p1", Some("d1")), "the id that lost is retired into the one that stands");
    }

    #[test]
    fn the_later_edit_wins_whole() {
        let mut long = tag("d1", "Harbour", Some(10));
        long.prompt_md = Some("long prompt here".into());
        let m = merge_files(&file(vec![long]), &file(vec![tag("p1", "harbour", Some(20))]));
        assert_eq!(m.file.tags[0].id, "p1");
        assert_eq!(m.file.tags[0].prompt_md, None);
        assert_eq!(m.desktop_ids, [("d1".to_string(), "p1".to_string())].into());
    }

    #[test]
    fn a_rename_that_lands_on_another_of_this_sides_tags_collapses_them() {
        let mut theirs = tag("t1", "Ops", Some(20));
        theirs.prompt_md = Some("prompt".into());
        let m = merge_files(&file(vec![tag("t1", "Operations", Some(10)), tag("d2", "Ops", Some(5))]), &file(vec![theirs]));
        assert_eq!(m.file.tags.len(), 1);
        assert_eq!(m.file.tags[0].id, "t1");
        assert_eq!(m.file.tags[0].prompt_md.as_deref(), Some("prompt"));
        assert_eq!(m.desktop_ids, [("d2".to_string(), "t1".to_string())].into());
    }

    #[test]
    fn a_tag_deleted_on_either_side_stays_deleted() {
        let mut deleted = file(vec![]);
        deleted.deleted = vec![Tombstone::delete("t1", 100)];
        let held = file(vec![tag("t1", "Ops", Some(100))]);
        assert!(merge_files(&deleted, &held).file.tags.is_empty());
        assert!(merge_files(&held, &deleted).file.tags.is_empty());
        let edited_after = file(vec![tag("t1", "Ops", Some(101))]);
        assert_eq!(merge_files(&deleted, &edited_after).file.tags.len(), 1, "an edit after the delete stands");
    }

    /// The shared sync vectors: every file re-encodes to itself and the inputs merge to the expected output.
    #[test]
    fn the_seam_fixtures_merge_to_the_pinned_bytes() {
        let read = |name: &str| std::fs::read(format!("{}/tests/fixtures/sync-seam/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let (d_bytes, p_bytes, merged) = (read("tags.desktop-v2.json"), read("tags.phone-v2.json"), read("tags.merged-v2.json"));
        let d: TagsFile = serde_json::from_slice(&d_bytes).unwrap();
        let p: TagsFile = serde_json::from_slice(&p_bytes).unwrap();
        assert_eq!(encode(&d).unwrap(), d_bytes, "the desktop input is in canonical form");
        assert_eq!(encode(&p).unwrap(), p_bytes, "the phone input is in canonical form");
        let m = merge_files(&d, &p);
        assert_eq!(String::from_utf8(encode(&m.file).unwrap()).unwrap(), String::from_utf8(merged.clone()).unwrap());
        assert_eq!(m.phone_ids, [("t-phone-ops".to_string(), "t-desk-ops".to_string())].into());
        assert!(m.desktop_ids.is_empty());
        assert_eq!(encode(&merge_files(&m.file, &p).file).unwrap(), merged, "merging again changes nothing");
    }

    #[test]
    fn the_bytes_are_canonical_with_every_count_zero() {
        let f = TagsFile { tags: vec![tag("b", "B", Some(1)), tag("a", "A", None)], ..Default::default() };
        let out = String::from_utf8(encode(&f).unwrap()).unwrap();
        assert_eq!(
            out,
            r##"{"schema_version":1,"tags":[{"color_hex":"#123456","created_at_unix_seconds":100,"id":"a","name":"A","prompt_md":null,"use_count":0,"vocab_md":null},{"color_hex":"#123456","created_at_unix_seconds":100,"id":"b","name":"B","prompt_md":null,"updated_at_unix_seconds":1,"use_count":0,"vocab_md":null}]}"##
        );
    }
}
