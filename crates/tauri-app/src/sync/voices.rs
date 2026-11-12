// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! `voices.json`, section 5c of the protocol: the voices both devices know,
//! each a name and a gallery of speaker embeddings. Merged by the rules every
//! record merges by (`sync::records`), with tombstones for a deleted voice
//! and for a sample taken back, so neither comes back from the other side.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::sync::records::{self, merge_rows, replaces, tombstones, union_extra, union_tombstones, FileMerge, Record, Side, Tombstone};

pub const FILE: &str = "voices.json";
/// The embedding space both devices write: the weights' published name.
pub const MODEL: &str = "wespeaker-voxceleb-resnet34-LM";
/// Keyed samples kept per voice, the most recent first. The enrolment
/// sample, the one with no source, is kept beside them.
pub const MAX_SAMPLES: usize = 16;

type Extra = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoicesFile {
    pub schema_version: u32,
    #[serde(default)]
    pub voices: Vec<Voice>,
    #[serde(default)]
    pub removed: Vec<Removed>,
    #[serde(default)]
    pub deleted: Vec<Tombstone>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl Default for VoicesFile {
    fn default() -> Self {
        Self { schema_version: 1, voices: vec![], removed: vec![], deleted: vec![], extra: Extra::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Voice {
    pub id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_id: Option<String>,
    pub created_at_unix_seconds: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_unix_seconds: Option<i64>,
    #[serde(default)]
    pub samples: Vec<Sample>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// Where a sample came from. Two samples with the same key are the same
/// sample; an enrollment has every part absent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
pub struct SampleKey {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diarization_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<u32>,
    /// A vector this desktop held without a source, numbered once when given one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    #[serde(flatten)]
    pub key: SampleKey,
    pub model: String,
    /// Base64 of little-endian f32. Carried as the bytes that arrived.
    pub v: String,
    pub at: i64,
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// A sample taken back from a voice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removed {
    pub voice: String,
    #[serde(flatten)]
    pub key: SampleKey,
    pub at: i64,
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// The file's bytes: canonical, every array in its defined order. Both
/// devices hash these bytes to tell whether they hold the same file, so one
/// set of values has one spelling.
pub fn encode(file: &VoicesFile) -> serde_json::Result<Vec<u8>> {
    let mut f = file.clone();
    normalize(&mut f);
    records::canonical(&f)
}

pub fn decode(bytes: &[u8]) -> serde_json::Result<VoicesFile> {
    serde_json::from_slice(bytes)
}

/// Most recent first; then by source, an absent part first.
fn sample_order(a: &Sample, b: &Sample) -> Ordering {
    b.at.cmp(&a.at).then_with(|| a.key.cmp(&b.key))
}

/// Puts a gallery in sample order and keeps the enrolment sample (every
/// part of its source absent) and the first [`MAX_SAMPLES`] others.
fn cap(samples: &mut Vec<Sample>) {
    samples.sort_by(sample_order);
    let mut keyed = 0;
    samples.retain(|s| {
        if s.key == SampleKey::default() {
            return true;
        }
        keyed += 1;
        keyed <= MAX_SAMPLES
    });
}

fn normalize(f: &mut VoicesFile) {
    for v in f.voices.iter_mut() {
        cap(&mut v.samples);
    }
    f.voices.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    f.removed.sort_by(|a, b| a.voice.as_bytes().cmp(b.voice.as_bytes()).then_with(|| a.key.cmp(&b.key)));
    f.deleted.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
}

fn latest(updated: Option<i64>, created: i64, samples: &[Sample]) -> i64 {
    samples.iter().map(|s| s.at).fold(updated.unwrap_or(created), i64::max)
}

impl Record for Voice {
    fn id(&self) -> &str {
        &self.id
    }
    /// One contact is one voice.
    fn same(&self, other: &Self) -> bool {
        self.contact_id.is_some() && self.contact_id == other.contact_id
    }
    fn updated_at(&self) -> Option<i64> {
        self.updated_at_unix_seconds
    }
    /// The later of its last edit (or its creation) and its newest sample: a
    /// sample added after the other side deleted the voice brings it back.
    fn row_time(&self) -> i64 {
        latest(self.updated_at_unix_seconds, self.created_at_unix_seconds, &self.samples)
    }
    fn extra(&self) -> &Extra {
        &self.extra
    }
    fn extra_mut(&mut self) -> &mut Extra {
        &mut self.extra
    }
    /// The earliest creation, and the union of every gallery: one source
    /// twice keeps the later sample, then the desktop's, then the smaller
    /// vector by bytes, with the unknown keys of all of them.
    fn absorb(&mut self, group: &[(&Self, Side)]) {
        let mut samples: BTreeMap<SampleKey, (Sample, Side, Extra)> = BTreeMap::new();
        for (voice, side) in group {
            self.created_at_unix_seconds = self.created_at_unix_seconds.min(voice.created_at_unix_seconds);
            for s in &voice.samples {
                match samples.get_mut(&s.key) {
                    None => {
                        samples.insert(s.key.clone(), (s.clone(), *side, s.extra.clone()));
                    }
                    Some((held, held_side, extra)) => {
                        for (k, v) in &s.extra {
                            extra.entry(k.clone()).or_insert_with(|| v.clone());
                        }
                        let wins = replaces((held.at, *held_side), (s.at, *side))
                            || ((s.at, *side) == (held.at, *held_side) && s.v.as_bytes() < held.v.as_bytes());
                        if wins {
                            *held = s.clone();
                            *held_side = *side;
                        }
                    }
                }
            }
        }
        self.samples = samples
            .into_values()
            .map(|(mut s, _, extra)| {
                s.extra = extra;
                s
            })
            .collect();
    }
    /// A voice named again after a delete keeps only the samples taken
    /// after it: a delete takes every vector that existed when it was made.
    fn outlive(&mut self, at: i64) {
        self.samples.retain(|s| s.at > at);
    }
}

/// Merges the phone's file into this desktop's, over the union of both, in
/// order: deleted voices go, voices that are one voice collapse into one,
/// samples taken back go, and each gallery keeps its enrolment sample and
/// its newest [`MAX_SAMPLES`] others.
/// Merging the result again changes nothing. A voice that lost is buried
/// with no `into`: its references follow the ids the merge returns.
pub fn merge(desktop: &VoicesFile, phone: &VoicesFile) -> FileMerge<VoicesFile> {
    let graves = union_tombstones(&desktop.deleted, &phone.deleted);
    let m = merge_rows(&desktop.voices, &phone.voices, graves, false);

    // Samples taken back, following voices that were merged away.
    let mut removed: BTreeMap<(String, SampleKey), (Removed, Side)> = BTreeMap::new();
    for (list, side) in [(&desktop.removed, Side::Desktop), (&phone.removed, Side::Phone)] {
        for r in list {
            let mut r = r.clone();
            if let Some(to) = m.retired.get(&r.voice) {
                r.voice = to.clone();
            }
            match removed.get_mut(&(r.voice.clone(), r.key.clone())) {
                None => {
                    removed.insert((r.voice.clone(), r.key.clone()), (r, side));
                }
                Some((held, held_side)) => {
                    let wins = replaces((held.at, *held_side), (r.at, side));
                    let extra = if side == Side::Desktop {
                        union_extra(&r.extra, &held.extra)
                    } else {
                        union_extra(&held.extra, &r.extra)
                    };
                    if wins {
                        *held = r;
                        *held_side = side;
                    }
                    held.extra = extra;
                }
            }
        }
    }
    let mut voices = m.rows.clone();
    for v in voices.iter_mut() {
        v.samples.retain(|s| {
            removed.get(&(v.id.clone(), s.key.clone())).is_none_or(|(r, _)| s.at > r.at)
        });
    }

    let mut out = VoicesFile {
        schema_version: desktop.schema_version.max(phone.schema_version).max(1),
        voices,
        removed: removed.into_values().map(|(r, _)| r).collect(),
        deleted: tombstones(m.graves.clone()),
        extra: union_extra(&desktop.extra, &phone.extra),
    };
    normalize(&mut out);
    FileMerge::of(out, &m)
}

/// The file this desktop's vault stands for.
pub fn project(keys: &crate::state::DecryptedKeys) -> VoicesFile {
    VoicesFile {
        schema_version: 1,
        voices: keys
            .voiceprints
            .iter()
            .map(|v| Voice {
                id: v.id.clone(),
                display_name: v.display_name.clone(),
                contact_id: v.contact_id.clone(),
                created_at_unix_seconds: v.created_at_unix_seconds,
                updated_at_unix_seconds: v.updated_at_unix_seconds,
                samples: v.samples.clone(),
                extra: v.extra.clone(),
            })
            .collect(),
        removed: keys.voice_removed.clone(),
        deleted: keys.voice_deleted.clone(),
        extra: keys.voices_extra.clone(),
    }
}

/// The bytes a hello offers and hashes.
/// `None` while the vault holds no voice, no tombstone and no key of
/// another Daisy's: then there is no file, as with any other record.
pub fn bytes(keys: &crate::state::DecryptedKeys) -> Option<Vec<u8>> {
    let f = project(keys);
    if f == VoicesFile::default() {
        return None;
    }
    encode(&f).ok()
}

/// Puts a merged file into the vault. What this machine keeps for itself on
/// a voice (its email, the sessions it matched in) stays with the voice, and
/// follows one that was merged into another.
pub fn install(keys: &mut crate::state::DecryptedKeys, merged: VoicesFile, renamed: &BTreeMap<String, String>) {
    let mut local: BTreeMap<String, (Option<String>, Vec<String>)> = BTreeMap::new();
    for v in keys.voiceprints.drain(..) {
        let to = renamed.get(&v.id).cloned().unwrap_or(v.id);
        let entry = local.entry(to).or_default();
        if entry.0.is_none() {
            entry.0 = v.email;
        }
        for s in v.session_ids {
            if !entry.1.contains(&s) {
                entry.1.push(s);
            }
        }
    }
    keys.voiceprints = merged
        .voices
        .into_iter()
        .map(|v| {
            let (email, session_ids) = local.remove(&v.id).unwrap_or_default();
            crate::state::Voiceprint {
                id: v.id,
                display_name: v.display_name,
                email,
                vector: Vec::new(),
                vectors: Vec::new(),
                samples: v.samples,
                created_at_unix_seconds: v.created_at_unix_seconds,
                updated_at_unix_seconds: v.updated_at_unix_seconds,
                session_ids,
                contact_id: v.contact_id,
                extra: v.extra,
            }
        })
        .collect();
    keys.voice_removed = merged.removed;
    keys.voice_deleted = merged.deleted;
    keys.voices_extra = merged.extra;
}

/// Gives each vector a voice held without a source a sample of its own, numbered in the order the gallery held them. Run once, by the `voice-samples` migration.
pub fn mint_legacy(v: &mut crate::state::Voiceprint) {
    let mut vectors = std::mem::take(&mut v.vectors);
    let single = std::mem::take(&mut v.vector);
    if !single.is_empty() {
        vectors.push(single);
    }
    let first = v.samples.iter().filter_map(|s| s.key.legacy).max().map_or(0, |n| n + 1);
    for (i, vector) in vectors.into_iter().enumerate() {
        let sample = Sample {
            key: SampleKey { legacy: Some(first + i as u32), ..Default::default() },
            model: MODEL.to_string(),
            v: encode_vector(&vector),
            at: v.created_at_unix_seconds,
            extra: Extra::new(),
        };
        add_sample(&mut v.samples, sample);
    }
}

/// Adds a sample to a gallery: one from the same source is replaced, and the
/// gallery keeps its enrolment sample and its newest [`MAX_SAMPLES`] others.
pub fn add_sample(samples: &mut Vec<Sample>, sample: Sample) {
    samples.retain(|s| s.key != sample.key);
    samples.push(sample);
    cap(samples);
}

/// Takes one sample back from a voice and records it, so the other side's
/// copy does not return it.
pub fn take_back(keys: &mut crate::state::DecryptedKeys, voice: &str, key: &SampleKey, at: i64) {
    if let Some(v) = keys.voiceprints.iter_mut().find(|v| v.id == voice) {
        v.samples.retain(|s| &s.key != key);
    }
    match keys.voice_removed.iter_mut().find(|r| r.voice == voice && &r.key == key) {
        Some(r) => r.at = r.at.max(at),
        None => keys.voice_removed.push(Removed { voice: voice.to_string(), key: key.clone(), at, extra: Extra::new() }),
    }
}

/// Records a voice as gone: deleted, or merged into another here.
pub fn retire(keys: &mut crate::state::DecryptedKeys, id: &str, at: i64) {
    match keys.voice_deleted.iter_mut().find(|d| d.id == id) {
        Some(d) => d.at = d.at.max(at),
        None => keys.voice_deleted.push(Tombstone::delete(id, at)),
    }
}

/// Deletes every voice that belongs to a contact in `gone` (contact id to
/// when it was deleted), no earlier than the voice's own latest moment, so
/// each sample it holds goes with it.
pub fn retire_contacts(keys: &mut crate::state::DecryptedKeys, gone: &BTreeMap<String, i64>) {
    let mut retired = Vec::new();
    keys.voiceprints.retain(|v| {
        let Some(at) = v.contact_id.as_ref().and_then(|c| gone.get(c)) else { return true };
        retired.push((v.id.clone(), (*at).max(latest(v.updated_at_unix_seconds, v.created_at_unix_seconds, &v.samples))));
        false
    });
    for (id, at) in retired {
        retire(keys, &id, at);
    }
}

/// A vector as the wire carries it.
pub fn encode_vector(v: &[f32]) -> String {
    use base64::Engine;
    let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A vector off the wire; `None` when the bytes are not whole floats.
pub fn decode_vector(s: &str) -> Option<Vec<f32>> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(s).ok()?;
    if bytes.len() % 4 != 0 {
        return None;
    }
    Some(bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(session: Option<&str>, speaker: Option<u32>, legacy: Option<u32>, v: &[f32], at: i64) -> Sample {
        Sample {
            key: SampleKey {
                session: session.map(str::to_string),
                diarization_id: session.map(|_| "0b7f5a52-0000-4000-8000-000000000001".to_string()),
                speaker,
                legacy,
            },
            model: MODEL.into(),
            v: encode_vector(v),
            at,
            extra: Extra::new(),
        }
    }

    fn voice(id: &str, name: &str, contact: Option<&str>, created: i64, updated: Option<i64>, samples: Vec<Sample>) -> Voice {
        Voice {
            id: id.into(),
            display_name: name.into(),
            contact_id: contact.map(str::to_string),
            created_at_unix_seconds: created,
            updated_at_unix_seconds: updated,
            samples,
            extra: Extra::new(),
        }
    }

    #[test]
    fn a_gallery_from_before_samples_is_numbered_once() {
        let mut v: crate::state::Voiceprint = serde_json::from_value(serde_json::json!({
            "id": "vp1", "display_name": "Ana", "created_at_unix_seconds": 100,
            "vector": [0.0, 1.0], "vectors": [[1.0, 0.0], [0.6, 0.8]],
        }))
        .unwrap();
        mint_legacy(&mut v);
        assert!(v.vector.is_empty() && v.vectors.is_empty());
        let got: Vec<(Option<u32>, Vec<f32>)> =
            v.samples.iter().map(|s| (s.key.legacy, decode_vector(&s.v).unwrap())).collect();
        assert_eq!(got, [(Some(0), vec![1.0, 0.0]), (Some(1), vec![0.6, 0.8]), (Some(2), vec![0.0, 1.0])]);
        assert!(v.samples.iter().all(|s| s.at == 100 && s.model == MODEL && s.key.session.is_none()));
        let again = v.samples.clone();
        mint_legacy(&mut v);
        assert_eq!(v.samples, again, "a second run finds nothing to number");
        let stored = serde_json::to_value(&v).unwrap();
        assert!(stored.get("vector").is_none() && stored.get("vectors").is_none());
    }

    #[test]
    fn a_vector_round_trips_through_its_bytes() {
        let v = [0.5f32, -0.25, 1.0e-7, 0.0];
        assert_eq!(decode_vector(&encode_vector(&v)).unwrap(), v);
        assert!(decode_vector("AAA=").is_none(), "two bytes are not a float");
    }

    #[test]
    fn absent_parts_are_not_written() {
        let f = VoicesFile {
            voices: vec![voice("a", "Ana", None, 1, None, vec![sample(None, None, None, &[1.0], 1)])],
            ..Default::default()
        };
        let s = String::from_utf8(encode(&f).unwrap()).unwrap();
        assert!(!s.contains("null"), "{s}");
        assert!(!s.contains("contact_id") && !s.contains("session") && !s.contains("updated_at"), "{s}");
    }

    #[test]
    fn a_deleted_voice_stays_deleted_until_something_newer_arrives() {
        let desktop = VoicesFile { deleted: vec![Tombstone::delete("a", 50)], ..Default::default() };
        let old = VoicesFile {
            voices: vec![voice("a", "Ana", None, 10, Some(50), vec![sample(None, None, None, &[1.0], 40)])],
            ..Default::default()
        };
        assert!(merge(&desktop, &old).file.voices.is_empty(), "a tie goes to the delete");
        let named_again = VoicesFile {
            voices: vec![voice("a", "Ana", None, 10, Some(20), vec![sample(Some("daisy-9"), Some(1), None, &[1.0], 60)])],
            ..Default::default()
        };
        assert_eq!(merge(&desktop, &named_again).file.voices.len(), 1, "a sample after the delete brings it back");
    }

    #[test]
    fn a_sample_taken_back_on_one_side_goes_on_both() {
        let s = sample(Some("daisy-1"), Some(2), None, &[1.0], 30);
        let desktop = VoicesFile {
            voices: vec![voice("a", "Ana", None, 1, Some(5), vec![])],
            removed: vec![Removed { voice: "a".into(), key: s.key.clone(), at: 31, extra: Extra::new() }],
            ..Default::default()
        };
        let phone = VoicesFile { voices: vec![voice("a", "Ana", None, 1, Some(5), vec![s])], ..Default::default() };
        let m = merge(&desktop, &phone).file;
        assert!(m.voices[0].samples.is_empty());
        assert_eq!(m.voices.len(), 1, "a voice with no samples keeps its name");
    }

    #[test]
    fn the_gallery_keeps_its_newest_sixteen() {
        let samples: Vec<Sample> = (0..20).map(|i| sample(Some(&format!("daisy-{i:02}")), Some(1), None, &[i as f32], i)).collect();
        let f = VoicesFile { voices: vec![voice("a", "Ana", None, 1, None, samples)], ..Default::default() };
        let m = merge(&f, &VoicesFile::default()).file;
        let ats: Vec<i64> = m.voices[0].samples.iter().map(|s| s.at).collect();
        assert_eq!(ats, (4..20).rev().collect::<Vec<_>>());
    }

    /// The vector bytes: every file re-encodes to itself, the two inputs merge to the third, and merging the third with the phone's input changes nothing.
    fn the_merge_both_devices_compute(desktop: &str, phone: &str, merged: &str) {
        let (d, p) = (decode(desktop.as_bytes()).unwrap(), decode(phone.as_bytes()).unwrap());
        assert_eq!(encode(&d).unwrap(), desktop.as_bytes(), "the desktop input is in canonical form");
        assert_eq!(encode(&p).unwrap(), phone.as_bytes(), "the phone input is in canonical form");
        let m = merge(&d, &p).file;
        assert_eq!(String::from_utf8(encode(&m).unwrap()).unwrap(), merged);
        assert_eq!(encode(&merge(&m, &p).file).unwrap(), merged.as_bytes(), "merging again changes nothing");
    }

    #[test]
    fn the_v1_fixtures_merge_to_the_pinned_bytes() {
        let (d, p, merged) = (
            include_str!("../../tests/fixtures/sync-seam/voices.desktop-v1.json"),
            include_str!("../../tests/fixtures/sync-seam/voices.phone-v1.json"),
            include_str!("../../tests/fixtures/sync-seam/voices.merged-v1.json"),
        );
        the_merge_both_devices_compute(d, p, merged);
        assert_eq!(merged.len(), 1316);
        // Nothing in these two ties, so which side is which does not move
        // the answer.
        let swapped = merge(&decode(p.as_bytes()).unwrap(), &decode(d.as_bytes()).unwrap()).file;
        assert_eq!(encode(&swapped).unwrap(), merged.as_bytes());
    }

    #[test]
    fn the_v2_fixtures_carry_both_sides_unknown_keys_at_every_level() {
        the_merge_both_devices_compute(
            include_str!("../../tests/fixtures/sync-seam/voices.desktop-v2.json"),
            include_str!("../../tests/fixtures/sync-seam/voices.phone-v2.json"),
            include_str!("../../tests/fixtures/sync-seam/voices.merged-v2.json"),
        );
    }

    #[test]
    fn the_enrolment_sample_stays_beside_the_newest_sixteen() {
        let mut samples: Vec<Sample> =
            (0..17).map(|i| sample(Some(&format!("daisy-{i:02}")), Some(1), None, &[i as f32], 100 + i)).collect();
        samples.push(sample(None, None, None, &[9.0], 1));
        let f = VoicesFile { voices: vec![voice("a", "Ana", None, 1, None, samples)], ..Default::default() };
        let m = merge(&f, &VoicesFile::default()).file;
        let kept = &m.voices[0].samples;
        assert_eq!(kept.len(), 17);
        assert!(kept.iter().any(|s| s.key == SampleKey::default()), "the enrolment sample is kept");
        let ats: Vec<i64> = kept.iter().filter(|s| s.key != SampleKey::default()).map(|s| s.at).collect();
        assert_eq!(ats, (101..117).rev().collect::<Vec<_>>(), "the newest sixteen keyed samples");
        assert_eq!(merge(&m, &VoicesFile::default()).file, m, "merging again changes nothing");

        let mut gallery = kept.clone();
        add_sample(&mut gallery, sample(Some("daisy-99"), Some(1), None, &[1.0], 500));
        assert_eq!(gallery.len(), 17);
        assert!(gallery.iter().any(|s| s.key == SampleKey::default()), "adding a sample keeps the enrolment");
    }

    #[test]
    fn an_empty_vault_holds_no_file() {
        let keys = crate::state::DecryptedKeys::default();
        assert!(bytes(&keys).is_none());
    }
}
