// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! `meta.json`: what a person wrote on a meeting — its title, the tags they
//! attached, the suggestions they waved away and the names they gave the
//! voices. Both devices author these, so each value carries the second it was
//! written and merges on its own.
//!
//! The manifest holds a copy of the title, the tags and the current run's
//! names, which is where this desktop's screens, search and summaries read
//! them. The copy is rewritten from here after every change. The manifest
//! stays in this machine's library, and its `diarization_id` names the
//! current run of a session with no `speakers.json` (`current_run`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::manifest::{SessionManifest, SpeakerLabel};

pub const FILE_NAME: &str = "meta.json";

/// The shape this build writes. A file arriving under a higher number is read
/// for the keys it shares and its own are carried through `extra`.
pub const SCHEMA: u32 = 1;

pub type Extra = serde_json::Map<String, serde_json::Value>;

pub fn path(session_dir: &Path) -> PathBuf {
    session_dir.join(FILE_NAME)
}

/// The bytes of every file both devices write and hash: compact, keys in
/// order at every depth, slashes bare, non-ASCII raw, no trailing newline.
pub fn canonical<T: Serialize>(value: &T) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::to_value(value)?)
}

/// One authored value and the second it was authored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell<T> {
    pub value: T,
    pub updated_at_unix_seconds: i64,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl<T> Cell<T> {
    pub fn new(value: T, at: i64) -> Self {
        Self { value, updated_at_unix_seconds: at, extra: Extra::new() }
    }
}

/// Whether an id is attached, and when that was last decided. A detach is a
/// stamped `false` and stays on the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Membership {
    pub on: bool,
    pub updated_at_unix_seconds: i64,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl Membership {
    pub fn new(on: bool, at: i64) -> Self {
        Self { on, updated_at_unix_seconds: at, extra: Extra::new() }
    }
}

/// A name given to one cluster. The cosine and the email stay on the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Speaker {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_id: Option<String>,
    pub display_name: String,
    pub updated_at_unix_seconds: i64,
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// One run's names, keyed by cluster number written as a decimal string.
pub type SpeakerMap = BTreeMap<String, Speaker>;

fn schema_default() -> u32 {
    SCHEMA
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    #[serde(default = "schema_default")]
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Cell<String>>,
    #[serde(default)]
    pub tag_ids: BTreeMap<String, Membership>,
    #[serde(default)]
    pub dismissed_suggested_tag_ids: BTreeMap<String, Membership>,
    /// Every run's names, keyed by the run's `diarization_id`.
    #[serde(default)]
    pub speaker_maps: BTreeMap<String, SpeakerMap>,
    /// Keys another Daisy wrote that this build does not declare.
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl Default for SessionMeta {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA,
            title: None,
            tag_ids: BTreeMap::new(),
            dismissed_suggested_tag_ids: BTreeMap::new(),
            speaker_maps: BTreeMap::new(),
            extra: Extra::new(),
        }
    }
}

impl SessionMeta {
    /// What a session with no file of its own says, read off the manifest at
    /// the second it was finalized. The manifest's names sit under `run`, the
    /// run that numbered them; a session with no current run carries none.
    pub fn carried_over(manifest: &SessionManifest, run: Option<&str>) -> Self {
        let at = manifest.finalized_at_unix_seconds.unwrap_or(manifest.created_at_unix_seconds);
        let mut meta = Self::default();
        if let Some(title) = &manifest.title {
            meta.title = Some(Cell::new(title.clone(), at));
        }
        for id in &manifest.tag_ids {
            meta.tag_ids.insert(id.clone(), Membership::new(true, at));
        }
        for id in &manifest.dismissed_suggested_tag_ids {
            meta.dismissed_suggested_tag_ids.insert(id.clone(), Membership::new(true, at));
        }
        if let Some(run) = run {
            let names = manifest
                .speaker_map
                .iter()
                .map(|label| {
                    let speaker = Speaker {
                        contact_id: label.contact_id.clone(),
                        display_name: label.display_name.clone(),
                        updated_at_unix_seconds: at,
                        extra: Extra::new(),
                    };
                    (label.cluster_id.to_string(), speaker)
                })
                .collect();
            meta.speaker_maps.insert(run.to_string(), names);
        }
        meta
    }

    /// The ids attached right now.
    pub fn attached_tag_ids(&self) -> Vec<String> {
        self.tag_ids.iter().filter(|(_, m)| m.on).map(|(id, _)| id.clone()).collect()
    }

    pub fn dismissed_tag_ids(&self) -> Vec<String> {
        self.dismissed_suggested_tag_ids
            .iter()
            .filter(|(_, m)| m.on)
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn set_title(&mut self, title: &str, at: i64) {
        self.title = Some(Cell::new(title.to_string(), at));
    }

    /// Attaches exactly these ids: anything attached and not named is stamped
    /// off rather than dropped.
    pub fn set_tag_ids<I: IntoIterator<Item = String>>(&mut self, ids: I, at: i64) {
        set_membership(&mut self.tag_ids, ids, at);
    }

    pub fn set_dismissed_tag_ids<I: IntoIterator<Item = String>>(&mut self, ids: I, at: i64) {
        set_membership(&mut self.dismissed_suggested_tag_ids, ids, at);
    }

    /// Follows the tags the catalog retired: a cell under an id merged into
    /// another moves to the id that stands, and a cell under a deleted id goes.
    pub fn retire_tags(&mut self, moved: &BTreeMap<String, String>, gone: &BTreeSet<String>) {
        for rows in [&mut self.tag_ids, &mut self.dismissed_suggested_tag_ids] {
            rows.retain(|id, _| !gone.contains(id));
            let arriving: Vec<(String, Membership)> =
                rows.iter().filter_map(|(id, cell)| moved.get(id).map(|to| (to.clone(), cell.clone()))).collect();
            rows.retain(|id, _| !moved.contains_key(id));
            for (to, cell) in arriving {
                // Two retired ids can land on one that stands; the later
                // decision stays.
                match rows.get(&to) {
                    Some(held) if held.updated_at_unix_seconds >= cell.updated_at_unix_seconds => {}
                    _ => {
                        rows.insert(to, cell);
                    }
                }
            }
        }
    }

    /// Follows the contacts the contact list retired, in every run: a name
    /// under a contact merged into another points at the one that stands, and
    /// one under a deleted contact loses the id and keeps the name it shows.
    pub fn retire_contacts(&mut self, moved: &BTreeMap<String, String>, gone: &BTreeSet<String>) {
        for speaker in self.speaker_maps.values_mut().flat_map(|m| m.values_mut()) {
            let Some(contact) = &speaker.contact_id else { continue };
            if gone.contains(contact) {
                speaker.contact_id = None;
            } else if let Some(to) = moved.get(contact) {
                speaker.contact_id = Some(to.clone());
            }
        }
    }

    /// The names under one run.
    pub fn speakers(&self, run: &str) -> Option<&SpeakerMap> {
        self.speaker_maps.get(run)
    }

    pub fn set_speaker(&mut self, run: &str, cluster_id: u32, display_name: &str, contact_id: Option<String>, at: i64) {
        self.speaker_maps.entry(run.to_string()).or_default().insert(
            cluster_id.to_string(),
            Speaker { contact_id, display_name: display_name.to_string(), updated_at_unix_seconds: at, extra: Extra::new() },
        );
    }

    /// Clears one cluster's name with a stamped `""` cell. The key stays.
    pub fn clear_speaker(&mut self, run: &str, cluster_id: u32, at: i64) {
        self.set_speaker(run, cluster_id, "", None, at);
    }

    /// Names one cluster only if the run has no cell for it yet. A cell
    /// already there, cleared or named, is left as it is.
    pub fn set_speaker_if_absent(&mut self, run: &str, cluster_id: u32, display_name: &str, at: i64) {
        self.speaker_maps.entry(run.to_string()).or_default().entry(cluster_id.to_string()).or_insert_with(|| Speaker {
            contact_id: None,
            display_name: display_name.to_string(),
            updated_at_unix_seconds: at,
            extra: Extra::new(),
        });
    }

    /// The rows the manifest carries for one run; a `""` name is a cleared
    /// one and reads as unnamed.
    pub fn speaker_labels(&self, run: &str) -> Vec<SpeakerLabel> {
        self.speakers(run)
            .into_iter()
            .flatten()
            .filter_map(|(cluster, speaker)| {
                let cluster_id = cluster.parse::<u32>().ok()?;
                Some(SpeakerLabel {
                    cluster_id,
                    display_name: speaker.display_name.clone(),
                    email: None,
                    voiceprint_id: None,
                    match_confidence: None,
                    extra: serde_json::Map::new(),
                    contact_id: speaker.contact_id.clone(),
                })
            })
            .collect()
    }
}

fn set_membership<I: IntoIterator<Item = String>>(rows: &mut BTreeMap<String, Membership>, ids: I, at: i64) {
    let wanted: BTreeSet<String> = ids.into_iter().collect();
    for (id, cell) in rows.iter_mut() {
        let on = wanted.contains(id);
        if cell.on != on {
            cell.on = on;
            cell.updated_at_unix_seconds = at;
        }
    }
    for id in wanted {
        rows.entry(id).or_insert_with(|| Membership::new(true, at));
    }
}

/// Every read-modify-write of a `meta.json`, whoever makes it: a person here,
/// the hello that merges the phone's copy, or a cascade after a profile merge.
/// Held from the read to the write.
static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn held() -> std::sync::MutexGuard<'static, ()> {
    WRITE.lock().unwrap_or_else(|e| e.into_inner())
}

/// What a session says about itself: its own file, or its manifest.
/// Reading never writes — a session that predates the file keeps
/// none until something is authored on it.
pub fn load(session_dir: &Path) -> SessionMeta {
    if let Ok(bytes) = syncsafe::read(path(session_dir)) {
        if let Ok(meta) = serde_json::from_slice::<SessionMeta>(&bytes) {
            return meta;
        }
    }
    match read_manifest(session_dir) {
        Some(manifest) => SessionMeta::carried_over(&manifest, current_run(session_dir).as_deref()),
        None => SessionMeta::default(),
    }
}

fn read_manifest(session_dir: &Path) -> Option<SessionManifest> {
    syncsafe::read(session_dir.join("manifest.json")).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

/// Authors one change and projects it onto the manifest. `false` when the
/// change left the file as it was.
pub fn update<F: FnOnce(&mut SessionMeta)>(session_dir: &Path, f: F) -> crate::Result<bool> {
    let _held = held();
    let mut meta = load(session_dir);
    let before = meta.clone();
    f(&mut meta);
    if meta == before {
        return Ok(false);
    }
    write(session_dir, &meta)?;
    Ok(true)
}

/// Projects the session's own file onto the manifest and `speakers.json`
/// again, after something wrote those without reading it — an import writes
/// the phone's manifest over this desktop's. A session with no file has
/// nothing to project.
pub fn project_file(session_dir: &Path) -> crate::Result<()> {
    let _held = held();
    if !path(session_dir).is_file() {
        return Ok(());
    }
    project(session_dir, &load(session_dir))
}

/// The run of a speaker table that names none.
pub const LEGACY_RUN: &str = "legacy";

/// The run this session's clusters were numbered by: the `diarization_id`
/// of the speaker table it reads. A session with a `speakers.json` reads
/// that file's, or [`LEGACY_RUN`] when it names none; one without — a
/// recording made here — reads its manifest's, which never leaves this
/// machine.
pub fn current_run(session_dir: &Path) -> Option<String> {
    if session_dir.join(crate::speakers::FILE_NAME).is_file() {
        let named = crate::speakers::read(session_dir).ok().flatten().and_then(|f| f.diarization_id);
        return Some(named.unwrap_or_else(|| LEGACY_RUN.to_string()));
    }
    read_manifest(session_dir)?.diarization_id
}

/// Gives a session that no run has numbered a run of this machine's own,
/// recorded on its manifest, and seeds the names its manifest shows into that
/// run's map when the session has a file. `None` when the session already has
/// a run or has no manifest to record one on.
pub fn mint_run(session_dir: &Path) -> crate::Result<Option<String>> {
    let _held = held();
    if current_run(session_dir).is_some() || !session_dir.join("manifest.json").is_file() {
        return Ok(None);
    }
    let run = uuid::Uuid::new_v4().to_string();
    record_run(session_dir, &run)?;
    // The names the manifest shows are this numbering's.
    if path(session_dir).is_file() {
        let mut meta = load(session_dir);
        let names =
            read_manifest(session_dir).and_then(|m| SessionMeta::carried_over(&m, Some(&run)).speaker_maps.remove(&run));
        if let (Some(names), false) = (names, meta.speaker_maps.contains_key(&run)) {
            if !names.is_empty() {
                meta.speaker_maps.insert(run.clone(), names);
                write(session_dir, &meta)?;
            }
        }
    }
    Ok(Some(run))
}

fn run_of(session_dir: &Path) -> crate::Result<String> {
    current_run(session_dir).ok_or_else(|| crate::RecordingError::NoRun(session_dir.to_path_buf()))
}

/// Names one cluster of the current run. A session with no run has no
/// clusters to name and is an error.
pub fn name_speaker(
    session_dir: &Path,
    cluster_id: u32,
    display_name: &str,
    contact_id: Option<String>,
    at: i64,
) -> crate::Result<bool> {
    let run = run_of(session_dir)?;
    update(session_dir, |m| m.set_speaker(&run, cluster_id, display_name, contact_id, at))
}

/// Takes the name off one cluster of the current run.
pub fn unname_speaker(session_dir: &Path, cluster_id: u32, at: i64) -> crate::Result<bool> {
    let run = run_of(session_dir)?;
    update(session_dir, |m| m.clear_speaker(&run, cluster_id, at))
}

/// Starts a new diarization run here: mints its id, records it as the
/// current run and carries the names of the clusters it kept over from the
/// run before, which renumbered nothing it kept. The earlier run's map stays
/// on the file.
pub fn start_run(session_dir: &Path, kept: &[u32]) -> crate::Result<String> {
    let _held = held();
    let run = uuid::Uuid::new_v4().to_string();
    let mut meta = load(session_dir);
    let carried: SpeakerMap = current_run(session_dir)
        .and_then(|old| meta.speaker_maps.get(&old).cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|(cluster, _)| cluster.parse::<u32>().is_ok_and(|id| kept.contains(&id)))
        .collect();
    record_run(session_dir, &run)?;
    if !carried.is_empty() {
        meta.speaker_maps.insert(run.clone(), carried);
        write(session_dir, &meta)?;
    }
    Ok(run)
}

/// Records `run` as the run that numbered this session's clusters.
pub fn set_current_run(session_dir: &Path, run: &str) -> crate::Result<()> {
    let _held = held();
    record_run(session_dir, run)
}

/// The run goes on the speaker table the session reads: its `speakers.json`
/// when it has one, else its manifest.
fn record_run(session_dir: &Path, run: &str) -> crate::Result<()> {
    if session_dir.join(crate::speakers::FILE_NAME).is_file() {
        crate::speakers::set_run(session_dir, run)?;
        return Ok(());
    }
    if !session_dir.join("manifest.json").is_file() {
        return Ok(());
    }
    crate::session::Session::load(session_dir)?.update_manifest(|m| m.diarization_id = Some(run.to_string()))
}

pub fn encode(meta: &SessionMeta) -> serde_json::Result<Vec<u8>> {
    canonical(meta)
}

fn write(session_dir: &Path, meta: &SessionMeta) -> crate::Result<()> {
    let file = path(session_dir);
    let bytes = encode(meta)?;
    syncsafe::write_atomic_synced(&file, &bytes).map_err(|e| crate::RecordingError::Io { path: file, source: e })?;
    project(session_dir, meta)
}

/// Copies the title, the tags and the current run's names onto the manifest,
/// where this desktop's readers find them. A session with no manifest is not
/// an error.
fn project(session_dir: &Path, meta: &SessionMeta) -> crate::Result<()> {
    let file = session_dir.join("manifest.json");
    if !file.is_file() {
        return Ok(());
    }
    let run = current_run(session_dir);
    let mut session = crate::session::Session::load(session_dir)?;
    session.update_manifest(|m| {
        if let Some(title) = &meta.title {
            m.title = Some(title.value.clone()).filter(|t| !t.trim().is_empty());
        }
        m.tag_ids = meta.attached_tag_ids();
        m.dismissed_suggested_tag_ids = meta.dismissed_tag_ids();
        let Some(run) = &run else { return };
        let named = meta.speaker_labels(run);
        // A name lands on the row that holds it, and a row this file says
        // nothing about is left where it is. The voiceprint and the cosine
        // are this machine's own and never travel, so the row keeps them.
        // The email belongs to the contact: a row whose contact is gone
        // loses it, and a row whose contact moved keeps it.
        // A cleared name takes the row off, as a blank name typed here does.
        for label in &named {
            if label.display_name.trim().is_empty() {
                m.speaker_map.retain(|s| s.cluster_id != label.cluster_id);
                continue;
            }
            match m.speaker_map.iter_mut().find(|s| s.cluster_id == label.cluster_id) {
                Some(row) => {
                    if row.contact_id.is_some() && label.contact_id.is_none() {
                        row.email = None;
                    }
                    row.display_name = label.display_name.clone();
                    row.contact_id = label.contact_id.clone();
                }
                None => m.speaker_map.push(label.clone()),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> SessionManifest {
        serde_json::from_value(serde_json::json!({
            "schema_version": 4,
            "session_id": "daisy-1",
            "created_at_unix_seconds": 100,
            "sample_rate": 16000,
            "channels": 1,
            "chunks": [],
            "finalized_at_unix_seconds": 500,
        }))
        .unwrap()
    }

    const RUN: &str = "run-a";

    #[test]
    fn a_session_with_no_file_reads_its_manifest_at_the_second_it_was_finalized() {
        let mut m = manifest();
        m.title = Some("Board review".into());
        m.tag_ids = vec!["t-1".into()];
        m.dismissed_suggested_tag_ids = vec!["t-4".into()];
        let meta = SessionMeta::carried_over(&m, m.diarization_id.as_deref());
        assert_eq!(meta.title.as_ref().unwrap().value, "Board review");
        assert_eq!(meta.title.as_ref().unwrap().updated_at_unix_seconds, 500);
        assert_eq!(meta.tag_ids["t-1"], Membership::new(true, 500));
        assert_eq!(meta.dismissed_suggested_tag_ids["t-4"], Membership::new(true, 500));
        assert!(meta.speaker_maps.is_empty(), "nothing records which run those clusters came from");
    }

    #[test]
    fn a_session_numbered_by_a_run_here_carries_its_names_under_that_run() {
        let mut m = manifest();
        m.diarization_id = Some(RUN.into());
        m.speaker_map = vec![SpeakerLabel {
            cluster_id: 2,
            display_name: "Priya".into(),
            email: None,
            voiceprint_id: None,
            match_confidence: None,
            extra: serde_json::Map::new(),
            contact_id: Some("c-3".into()),
        }];
        let meta = SessionMeta::carried_over(&m, m.diarization_id.as_deref());
        assert_eq!(meta.speakers(RUN).unwrap()["2"].display_name, "Priya");
        assert_eq!(meta.speakers(RUN).unwrap()["2"].updated_at_unix_seconds, 500);
    }

    #[test]
    fn a_session_that_never_finalized_carries_the_second_it_was_created() {
        let mut m = manifest();
        m.finalized_at_unix_seconds = None;
        m.title = Some("Draft".into());
        assert_eq!(SessionMeta::carried_over(&m, m.diarization_id.as_deref()).title.unwrap().updated_at_unix_seconds, 100);
    }

    #[test]
    fn a_detached_tag_stays_on_the_file_as_a_stamped_no() {
        let mut meta = SessionMeta::default();
        meta.set_tag_ids(["t-1".to_string(), "t-9".to_string()], 10);
        meta.set_tag_ids(["t-1".to_string()], 20);
        assert_eq!(meta.tag_ids["t-9"], Membership::new(false, 20), "the removal is stamped, not dropped");
        assert_eq!(meta.tag_ids["t-1"], Membership::new(true, 10), "an id that did not move keeps its second");
        assert_eq!(meta.attached_tag_ids(), vec!["t-1"]);
    }

    #[test]
    fn a_deleted_tag_leaves_no_row_and_a_merged_one_follows_the_tag_that_stands() {
        let mut meta = SessionMeta::default();
        meta.set_tag_ids(["gone".to_string(), "old".to_string(), "kept".to_string()], 10);
        meta.retire_tags(
            &BTreeMap::from([("old".to_string(), "new".to_string())]),
            &BTreeSet::from(["gone".to_string()]),
        );
        assert!(!meta.tag_ids.contains_key("gone"), "no id is left for an older yes to return under");
        assert!(!meta.tag_ids.contains_key("old"));
        assert_eq!(meta.tag_ids["new"], Membership::new(true, 10));
        assert!(meta.tag_ids.contains_key("kept"), "an id the catalog did not retire stays");
    }

    #[test]
    fn two_retired_ids_landing_on_one_winner_keep_the_later_decision() {
        let mut meta = SessionMeta::default();
        meta.tag_ids.insert("a".into(), Membership::new(true, 10));
        meta.tag_ids.insert("b".into(), Membership::new(false, 20));
        meta.retire_tags(
            &BTreeMap::from([("a".to_string(), "w".to_string()), ("b".to_string(), "w".to_string())]),
            &BTreeSet::new(),
        );
        assert_eq!(meta.tag_ids["w"], Membership::new(false, 20));
    }

    #[test]
    fn a_deleted_contact_leaves_the_speaker_its_name_in_every_run() {
        let mut meta = SessionMeta::default();
        meta.set_speaker(RUN, 0, "Priya", Some("c-3".into()), 10);
        meta.set_speaker("run-b", 4, "Priya", Some("c-3".into()), 10);
        meta.set_speaker(RUN, 1, "Ana", Some("c-4".into()), 10);
        meta.set_speaker(RUN, 2, "Kai", Some("c-5".into()), 10);
        meta.retire_contacts(
            &BTreeMap::from([("c-5".to_string(), "c-6".to_string())]),
            &BTreeSet::from(["c-3".to_string()]),
        );
        assert_eq!(meta.speakers(RUN).unwrap()["0"].contact_id, None);
        assert_eq!(meta.speakers(RUN).unwrap()["0"].display_name, "Priya");
        assert_eq!(meta.speakers("run-b").unwrap()["4"].contact_id, None);
        assert_eq!(meta.speakers(RUN).unwrap()["1"].contact_id.as_deref(), Some("c-4"));
        assert_eq!(meta.speakers(RUN).unwrap()["2"].contact_id.as_deref(), Some("c-6"));
    }

    #[test]
    fn the_wire_spelling_is_the_one_the_phone_writes() {
        let mut meta = SessionMeta::default();
        meta.set_title("Board review", 1700000900);
        meta.set_tag_ids(["t-1".to_string()], 1700000900);
        meta.set_speaker("9f3c", 0, "Priya", Some("c-3".into()), 1700000900);
        let v: serde_json::Value = serde_json::from_slice(&encode(&meta).unwrap()).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["title"]["value"], "Board review");
        assert_eq!(v["title"]["updated_at_unix_seconds"], 1700000900i64);
        assert_eq!(v["tag_ids"]["t-1"]["on"], true);
        assert_eq!(v["speaker_maps"]["9f3c"]["0"]["display_name"], "Priya");
        assert_eq!(v["speaker_maps"]["9f3c"]["0"]["contact_id"], "c-3");
        assert!(v.get("diarization_id").is_none(), "the current run never travels");
        assert!(v["dismissed_suggested_tag_ids"].is_object(), "an empty map is written, never left out");
    }

    #[test]
    fn keys_another_daisy_wrote_survive_a_rewrite_here_at_every_level() {
        let raw = br#"{"dismissed_suggested_tag_ids":{},"schema_version":1,"speaker_maps":{"r":{"0":{"display_name":"A","updated_at_unix_seconds":1,"zz_speaker":1}}},"tag_ids":{"t":{"on":true,"updated_at_unix_seconds":1,"zz_cell":2}},"title":{"updated_at_unix_seconds":1,"value":"T","zz_title":3},"zz_top":7}"#;
        let meta: SessionMeta = serde_json::from_slice(raw).unwrap();
        assert_eq!(String::from_utf8(encode(&meta).unwrap()).unwrap(), String::from_utf8(raw.to_vec()).unwrap());
    }

    #[test]
    fn a_file_with_no_schema_key_reads_as_this_shape() {
        let meta: SessionMeta = serde_json::from_slice(br#"{"tag_ids":{}}"#).unwrap();
        assert_eq!(meta.schema_version, SCHEMA);
    }

    fn session_with_manifest(m: &SessionManifest) -> tempfile::TempDir {
        let td = tempfile::tempdir().unwrap();
        std::fs::write(td.path().join("manifest.json"), serde_json::to_vec_pretty(m).unwrap()).unwrap();
        td
    }

    fn on_run(run: &str) -> SessionManifest {
        let mut m = manifest();
        m.diarization_id = Some(run.into());
        m
    }

    fn manifest_of(td: &tempfile::TempDir) -> SessionManifest {
        serde_json::from_slice(&std::fs::read(td.path().join("manifest.json")).unwrap()).unwrap()
    }

    #[test]
    fn reading_a_session_that_predates_the_file_writes_nothing() {
        let mut m = manifest();
        m.title = Some("Board review".into());
        let td = session_with_manifest(&m);
        assert_eq!(load(td.path()).title.unwrap().value, "Board review");
        assert!(!path(td.path()).exists(), "a read never materialises the file");
    }

    #[test]
    fn a_change_writes_the_file_and_the_manifest_copy() {
        let td = session_with_manifest(&manifest());
        assert!(update(td.path(), |meta| meta.set_title("Renamed", 900)).unwrap());
        assert!(path(td.path()).exists());
        assert_eq!(manifest_of(&td).title.as_deref(), Some("Renamed"), "the copy the phone still reads");
    }

    #[test]
    fn a_change_that_changes_nothing_writes_nothing() {
        let td = session_with_manifest(&manifest());
        update(td.path(), |meta| meta.set_title("Renamed", 900)).unwrap();
        assert!(!update(td.path(), |meta| meta.set_title("Renamed", 900)).unwrap());
    }

    #[test]
    fn a_manifest_written_over_without_reading_the_file_is_projected_again() {
        let td = session_with_manifest(&manifest());
        update(td.path(), |meta| meta.set_title("Harbour review", 900)).unwrap();
        let mut m = manifest_of(&td);
        m.title = Some("Standup".into());
        std::fs::write(td.path().join("manifest.json"), serde_json::to_vec_pretty(&m).unwrap()).unwrap();
        project_file(td.path()).unwrap();
        assert_eq!(manifest_of(&td).title.as_deref(), Some("Harbour review"));
    }

    #[test]
    fn the_projection_keeps_the_cosine_and_the_email_it_does_not_carry() {
        let mut m = on_run(RUN);
        m.speaker_map = vec![SpeakerLabel {
            cluster_id: 0,
            display_name: "Cluster 1".into(),
            email: Some("priya@example.com".into()),
            voiceprint_id: Some("vp-1".into()),
            match_confidence: Some(0.83),
            extra: serde_json::Map::new(),
            contact_id: None,
        }];
        let td = session_with_manifest(&m);
        update(td.path(), |meta| meta.set_speaker(RUN, 0, "Priya", Some("c-3".into()), 900)).unwrap();
        let back = manifest_of(&td);
        let row = &back.speaker_map[0];
        assert_eq!(row.display_name, "Priya");
        assert_eq!(row.contact_id.as_deref(), Some("c-3"));
        assert_eq!(row.email.as_deref(), Some("priya@example.com"), "the email never travelled and must stay");
        assert_eq!(row.match_confidence, Some(0.83), "a cosine means nothing elsewhere and must stay here");
        assert_eq!(row.voiceprint_id.as_deref(), Some("vp-1"));
    }

    #[test]
    fn a_row_whose_contact_was_deleted_loses_the_email_and_keeps_the_name() {
        let mut m = on_run(RUN);
        m.speaker_map = vec![SpeakerLabel {
            cluster_id: 0,
            display_name: "Priya".into(),
            email: Some("priya@example.com".into()),
            voiceprint_id: None,
            match_confidence: None,
            extra: serde_json::Map::new(),
            contact_id: Some("c-3".into()),
        }];
        let td = session_with_manifest(&m);
        update(td.path(), |meta| meta.retire_contacts(&BTreeMap::new(), &BTreeSet::from(["c-3".to_string()]))).unwrap();
        let row = &manifest_of(&td).speaker_map[0];
        assert_eq!((row.display_name.as_str(), row.contact_id.as_deref(), row.email.as_deref()), ("Priya", None, None));
    }

    #[test]
    fn only_the_current_runs_names_reach_the_manifest() {
        let td = session_with_manifest(&on_run(RUN));
        update(td.path(), |meta| {
            meta.set_speaker("run-elsewhere", 0, "Someone else", None, 900);
            meta.set_speaker(RUN, 1, "Priya", None, 900);
        })
        .unwrap();
        let rows: Vec<(u32, String)> =
            manifest_of(&td).speaker_map.into_iter().map(|s| (s.cluster_id, s.display_name)).collect();
        assert_eq!(rows, [(1, "Priya".to_string())]);
    }

    #[test]
    fn a_new_run_carries_the_names_of_the_clusters_it_kept_and_leaves_the_old_map() {
        let td = session_with_manifest(&on_run(RUN));
        update(td.path(), |meta| {
            meta.set_speaker(RUN, 0, "Priya", None, 900);
            meta.set_speaker(RUN, 1, "Sam", None, 900);
        })
        .unwrap();
        let new = start_run(td.path(), &[1]).unwrap();
        assert_ne!(new, RUN);
        assert_eq!(current_run(td.path()).as_deref(), Some(new.as_str()));
        let meta = load(td.path());
        assert_eq!(meta.speakers(&new).unwrap().keys().collect::<Vec<_>>(), ["1"]);
        assert_eq!(meta.speakers(RUN).unwrap().len(), 2, "a run's map is never pruned");
    }

    #[test]
    fn a_cleared_name_is_a_stamped_empty_cell_and_reads_as_unnamed() {
        let mut m = on_run(RUN);
        m.speaker_map = vec![SpeakerLabel {
            cluster_id: 0,
            display_name: "Priya".into(),
            email: None,
            voiceprint_id: None,
            match_confidence: None,
            extra: serde_json::Map::new(),
            contact_id: None,
        }];
        let td = session_with_manifest(&m);
        std::fs::write(
            td.path().join("speakers.json"),
            serde_json::to_vec(&serde_json::json!({ "schema_version": 1, "diarization_id": RUN, "speakers": [{ "id": 0, "name": "Priya" }] })).unwrap(),
        )
        .unwrap();
        unname_speaker(td.path(), 0, 900).unwrap();
        let cell = load(td.path()).speakers(RUN).unwrap()["0"].clone();
        assert_eq!((cell.display_name.as_str(), cell.updated_at_unix_seconds), ("", 900), "the key stays, stamped");
        assert!(manifest_of(&td).speaker_map.is_empty(), "the manifest row goes");
        assert_eq!(
            crate::speakers::read(td.path()).unwrap().unwrap().speakers[0].name.as_deref(),
            Some("Priya"),
            "the table is the phone's; clearing a name here never touches it"
        );
    }

    #[test]
    fn a_speaker_table_names_the_current_run_over_the_manifest() {
        let td = session_with_manifest(&on_run("run-manifest"));
        std::fs::write(
            td.path().join("speakers.json"),
            serde_json::to_vec(&serde_json::json!({ "schema_version": 2, "diarization_id": "run-phone", "speakers": [{ "id": 0 }] })).unwrap(),
        )
        .unwrap();
        assert_eq!(current_run(td.path()).as_deref(), Some("run-phone"));
        assert!(name_speaker(td.path(), 0, "Priya", None, 900).unwrap());
        assert_eq!(load(td.path()).speakers("run-phone").unwrap()["0"].display_name, "Priya");
        let run = start_run(td.path(), &[0]).unwrap();
        let table = crate::speakers::read(td.path()).unwrap().unwrap();
        assert_eq!(table.diarization_id.as_deref(), Some(run.as_str()), "a re-run here is recorded on the table it rewrote");
    }

    #[test]
    fn a_speaker_table_that_names_no_run_is_the_legacy_run() {
        let td = session_with_manifest(&manifest());
        let table = serde_json::to_vec(&serde_json::json!({ "schema_version": 2, "speakers": [{ "id": 0 }] })).unwrap();
        std::fs::write(td.path().join("speakers.json"), &table).unwrap();
        assert_eq!(current_run(td.path()).as_deref(), Some(LEGACY_RUN));
        assert_eq!(mint_run(td.path()).unwrap(), None, "a table has a run already");
        assert!(name_speaker(td.path(), 0, "Priya", None, 900).unwrap());
        assert_eq!(load(td.path()).speakers(LEGACY_RUN).unwrap()["0"].display_name, "Priya");
        assert!(manifest_of(&td).diarization_id.is_none(), "nothing minted");
        assert!(crate::speakers::read(td.path()).unwrap().unwrap().diarization_id.is_none(), "the table names no run still");
    }

    #[test]
    fn a_session_no_run_numbered_gets_a_run_of_its_own_once() {
        let td = session_with_manifest(&manifest());
        let first = mint_run(td.path()).unwrap().expect("a session with no run gets one");
        assert_eq!(current_run(td.path()).as_deref(), Some(first.as_str()));
        assert_eq!(manifest_of(&td).diarization_id.as_deref(), Some(first.as_str()), "recorded on the manifest");
        assert_eq!(mint_run(td.path()).unwrap(), None, "a second call finds the run");
        assert_eq!(current_run(td.path()).as_deref(), Some(first.as_str()));
    }

    #[test]
    fn a_session_with_no_run_takes_no_name() {
        let td = session_with_manifest(&manifest());
        assert!(matches!(name_speaker(td.path(), 0, "Priya", None, 900), Err(crate::RecordingError::NoRun(_))));
        assert!(matches!(unname_speaker(td.path(), 0, 900), Err(crate::RecordingError::NoRun(_))));
        assert!(!path(td.path()).exists(), "nothing is written");
        assert!(manifest_of(&td).diarization_id.is_none(), "nothing is minted");
    }

    #[test]
    fn a_run_minted_for_a_session_with_a_file_takes_the_names_the_manifest_shows() {
        let mut m = manifest();
        m.speaker_map = vec![SpeakerLabel {
            cluster_id: 3,
            display_name: "Priya".into(),
            email: None,
            voiceprint_id: None,
            match_confidence: None,
            extra: serde_json::Map::new(),
            contact_id: Some("c-3".into()),
        }];
        let td = session_with_manifest(&m);
        update(td.path(), |meta| meta.set_title("Board review", 900)).unwrap();
        let run = mint_run(td.path()).unwrap().unwrap();
        let names = load(td.path()).speakers(&run).cloned().unwrap();
        assert_eq!((names["3"].display_name.as_str(), names["3"].updated_at_unix_seconds), ("Priya", 500));
    }

    /// `speakers.json` with one unnamed cluster, as a diarized session has.
    fn with_speakers_file(td: &tempfile::TempDir, id: u32) {
        let file = serde_json::json!({
            "schema_version": 1, "diarization_id": RUN,
            "speakers": [{ "id": id, "name": serde_json::Value::Null }],
        });
        std::fs::write(td.path().join("speakers.json"), serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    }

    #[test]
    fn a_cluster_only_the_manifest_knows_about_survives_an_unrelated_edit() {
        let td = session_with_manifest(&on_run(RUN));
        update(td.path(), |meta| meta.set_title("Board review", 800)).unwrap();

        // A writer that touches the manifest alone: adding a participant by
        // hand, or a migration stamping historical rows.
        let mut m = manifest_of(&td);
        m.speaker_map.push(SpeakerLabel {
            cluster_id: 10_000,
            display_name: "Added by hand".into(),
            email: None,
            voiceprint_id: None,
            match_confidence: None,
            extra: serde_json::Map::new(),
            contact_id: None,
        });
        std::fs::write(td.path().join("manifest.json"), serde_json::to_vec_pretty(&m).unwrap()).unwrap();

        update(td.path(), |meta| meta.set_title("Renamed", 900)).unwrap();
        assert_eq!(
            manifest_of(&td).speaker_map.iter().map(|s| s.cluster_id).collect::<Vec<_>>(),
            vec![10_000],
            "a row this file says nothing about is not this file's to delete"
        );
    }

    #[test]
    fn a_name_set_here_lands_on_the_manifest_and_leaves_the_table_untouched() {
        let td = session_with_manifest(&on_run(RUN));
        with_speakers_file(&td, 0);
        let before = std::fs::read(td.path().join("speakers.json")).unwrap();

        update(td.path(), |meta| meta.set_speaker(RUN, 0, "Priya", None, 900)).unwrap();

        assert_eq!(
            manifest_of(&td).speaker_map.iter().find(|s| s.cluster_id == 0).map(|s| s.display_name.clone()),
            Some("Priya".to_string()),
            "the manifest is the read path; nothing else merges the name onto it"
        );
        assert_eq!(
            std::fs::read(td.path().join("speakers.json")).unwrap(),
            before,
            "the phone's table is never rewritten for a name"
        );
    }

    #[test]
    fn the_table_keeps_the_phones_bytes_after_a_name_lands_through_meta() {
        let td = session_with_manifest(&on_run(RUN));
        let file = serde_json::json!({
            "schema_version": 1, "diarization_id": RUN,
            "speakers": [{ "id": 0, "name": "Priya", "name_source": "match", "match_cosine": 0.83 }],
        });
        std::fs::write(td.path().join("speakers.json"), serde_json::to_vec_pretty(&file).unwrap()).unwrap();
        let before = std::fs::read(td.path().join("speakers.json")).unwrap();

        update(td.path(), |meta| meta.set_speaker(RUN, 0, "Priya", None, 900)).unwrap();

        assert_eq!(
            std::fs::read(td.path().join("speakers.json")).unwrap(),
            before,
            "name, name_source and match_cosine ride through exactly as the phone wrote them"
        );
    }

    #[test]
    fn a_session_with_no_manifest_still_takes_the_write() {
        let td = tempfile::tempdir().unwrap();
        assert!(update(td.path(), |meta| meta.set_title("Standalone", 900)).unwrap());
        assert_eq!(load(td.path()).title.unwrap().value, "Standalone");
    }

    #[test]
    fn a_cleared_title_empties_the_manifest_copy() {
        let mut m = manifest();
        m.title = Some("Board review".into());
        let td = session_with_manifest(&m);
        update(td.path(), |meta| meta.set_title("", 900)).unwrap();
        assert!(manifest_of(&td).title.is_none(), "a title cleared here is cleared there");
        assert_eq!(load(td.path()).title.unwrap().value, "", "the file keeps the stamped clearing");
    }
}

#[cfg(test)]
mod wire_shape {
    use super::*;

    /// Decoding and re-encoding lands on the same bytes.
    #[test]
    fn encoding_what_was_decoded_gives_the_bytes_back() {
        let raw = br#"{"dismissed_suggested_tag_ids":{},"schema_version":1,"speaker_maps":{"9f3c":{"0":{"contact_id":"c-3","display_name":"Priya","updated_at_unix_seconds":1700000900}}},"tag_ids":{"t-1":{"on":true,"updated_at_unix_seconds":1700000900}},"title":{"updated_at_unix_seconds":1700000900,"value":"Board review"},"zz_from_another_daisy":7}"#;
        let meta: SessionMeta = serde_json::from_slice(raw).unwrap();
        assert_eq!(
            String::from_utf8(encode(&meta).unwrap()).unwrap(),
            String::from_utf8(raw.to_vec()).unwrap(),
            "the file this build writes is the file it read"
        );
    }

    /// Sorted at every depth, no spaces, slashes bare, non-ASCII raw, no
    /// trailing newline: the form the phone's encoder writes.
    #[test]
    fn the_bytes_are_the_ones_the_phone_writes() {
        let mut meta = SessionMeta::default();
        meta.set_title("Board / review \u{201c}\u{e9}\u{201d} \u{65e5}\u{672c}", 1_700_000_900);
        meta.tag_ids.insert("t-1".into(), Membership::new(true, 1_700_000_900));

        let out = String::from_utf8(encode(&meta).unwrap()).unwrap();
        assert_eq!(out, r#"{"dismissed_suggested_tag_ids":{},"schema_version":1,"speaker_maps":{},"tag_ids":{"t-1":{"on":true,"updated_at_unix_seconds":1700000900}},"title":{"updated_at_unix_seconds":1700000900,"value":"Board / review “é” 日本"}}"#);
        assert!(!out.contains(" : "), "the phone's indented form is not what either side writes");
        assert!(!out.ends_with('\n'), "no trailing newline");
        assert!(out.contains(" / "), "a slash is written bare");
    }

    /// The same values built in any order hash the same.
    #[test]
    fn the_same_values_reach_the_same_bytes_whatever_order_they_were_written_in() {
        let mut a = SessionMeta::default();
        a.set_title("Board review", 10);
        a.set_tag_ids(["t-9".to_string(), "t-1".to_string()], 20);
        a.set_speaker("r", 1, "Sam", None, 30);
        a.set_speaker("r", 0, "Priya", None, 30);

        let mut b = SessionMeta::default();
        b.set_speaker("r", 0, "Priya", None, 30);
        b.set_speaker("r", 1, "Sam", None, 30);
        b.set_tag_ids(["t-1".to_string(), "t-9".to_string()], 20);
        b.set_title("Board review", 10);

        assert_eq!(encode(&a).unwrap(), encode(&b).unwrap());
    }
}
