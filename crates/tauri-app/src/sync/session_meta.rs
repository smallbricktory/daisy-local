// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Merging `meta.json`, protocol section 5b. Both devices author a title, a
//! set of tags and the names on the voices, so every value carries the second
//! it was written and settles on its own, by the tie rule every record
//! shares (`sync::records::replaces`).

use std::collections::{BTreeMap, BTreeSet};

use recording::session_meta::{Extra, SessionMeta};

use crate::sync::records::{replaces, union_extra, Side};

/// One cell settled: the one side that has it, or the later of the two with
/// the desktop's on a tie, carrying the unknown keys of both.
fn settle<C: Clone>(desktop: Option<&C>, phone: Option<&C>, at: fn(&C) -> i64, extra: fn(&mut C) -> &mut Extra) -> Option<C> {
    match (desktop, phone) {
        (None, None) => None,
        (Some(c), None) | (None, Some(c)) => Some(c.clone()),
        (Some(d), Some(p)) => {
            let (mut dd, mut pp) = (d.clone(), p.clone());
            let keys = union_extra(extra(&mut dd), extra(&mut pp));
            let mut out = if replaces((at(d), Side::Desktop), (at(p), Side::Phone)) { pp } else { dd };
            *extra(&mut out) = keys;
            Some(out)
        }
    }
}

fn cells<C: Clone>(
    desktop: &BTreeMap<String, C>,
    phone: &BTreeMap<String, C>,
    at: fn(&C) -> i64,
    extra: fn(&mut C) -> &mut Extra,
) -> BTreeMap<String, C> {
    let keys: BTreeSet<&String> = desktop.keys().chain(phone.keys()).collect();
    keys.into_iter()
        .filter_map(|k| settle(desktop.get(k), phone.get(k), at, extra).map(|c| (k.clone(), c)))
        .collect()
}

/// Merges the phone's file into this desktop's. Each cell settles on its
/// own; a cell only one side has stands; a stamped `false` is kept. Every
/// run's names merge cell by cell with the same run's on the other side, and
/// runs only one side knows are kept whole. Unknown keys at every level are
/// the union, the desktop's value on a clash. Written from this desktop's
/// seat; the phone writes the same rule from its own and inverts it.
pub fn merge(desktop: &SessionMeta, phone: &SessionMeta) -> SessionMeta {
    let runs: BTreeSet<&String> = desktop.speaker_maps.keys().chain(phone.speaker_maps.keys()).collect();
    let empty = BTreeMap::new();
    SessionMeta {
        schema_version: desktop.schema_version.max(phone.schema_version),
        title: settle(desktop.title.as_ref(), phone.title.as_ref(), |c| c.updated_at_unix_seconds, |c| &mut c.extra),
        tag_ids: cells(&desktop.tag_ids, &phone.tag_ids, |c| c.updated_at_unix_seconds, |c| &mut c.extra),
        dismissed_suggested_tag_ids: cells(
            &desktop.dismissed_suggested_tag_ids,
            &phone.dismissed_suggested_tag_ids,
            |c| c.updated_at_unix_seconds,
            |c| &mut c.extra,
        ),
        speaker_maps: runs
            .into_iter()
            .map(|run| {
                let d = desktop.speaker_maps.get(run).unwrap_or(&empty);
                let p = phone.speaker_maps.get(run).unwrap_or(&empty);
                (run.clone(), cells(d, p, |c| c.updated_at_unix_seconds, |c| &mut c.extra))
            })
            .collect(),
        extra: union_extra(&desktop.extra, &phone.extra),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use recording::session_meta::Membership;

    fn titled(title: &str, at: i64) -> SessionMeta {
        let mut m = SessionMeta::default();
        m.set_title(title, at);
        m
    }

    #[test]
    fn the_later_title_wins() {
        assert_eq!(merge(&titled("mine", 10), &titled("theirs", 20)).title.unwrap().value, "theirs");
        assert_eq!(merge(&titled("mine", 30), &titled("theirs", 20)).title.unwrap().value, "mine");
    }

    #[test]
    fn a_tie_keeps_this_desktops_title() {
        assert_eq!(merge(&titled("mine", 10), &titled("theirs", 10)).title.unwrap().value, "mine");
    }

    #[test]
    fn a_title_only_one_side_has_is_taken_whatever_its_second() {
        assert_eq!(merge(&SessionMeta::default(), &titled("theirs", 1)).title.unwrap().value, "theirs");
        assert_eq!(merge(&titled("mine", 1), &SessionMeta::default()).title.unwrap().value, "mine");
    }

    #[test]
    fn each_value_settles_on_its_own() {
        let mut mine = SessionMeta::default();
        mine.set_title("named here", 30);
        mine.set_tag_ids(["t-1".to_string()], 10);
        let mut theirs = SessionMeta::default();
        theirs.set_title("named there", 20);
        theirs.set_tag_ids(["t-1".to_string(), "t-2".to_string()], 40);

        let out = merge(&mine, &theirs);
        assert_eq!(out.attached_tag_ids(), vec!["t-1", "t-2"], "the later tagging is theirs");
        assert_eq!(out.title.unwrap().value, "named here", "and the later title is this side's");
    }

    #[test]
    fn a_stamped_removal_beats_an_older_attachment_and_is_kept() {
        let mut mine = SessionMeta::default();
        mine.set_tag_ids(["t-1".to_string()], 10);
        let mut theirs = SessionMeta::default();
        theirs.tag_ids.insert("t-1".into(), Membership::new(false, 20));
        let out = merge(&mine, &theirs);
        assert!(out.attached_tag_ids().is_empty());
        assert_eq!(out.tag_ids["t-1"], Membership::new(false, 20));
        assert!(merge(&theirs, &mine).attached_tag_ids().is_empty(), "an older attachment does not walk back in");
    }

    #[test]
    fn names_from_different_runs_are_kept_side_by_side() {
        let mut mine = SessionMeta::default();
        mine.set_speaker("run-a", 0, "Priya", None, 10);
        let mut theirs = SessionMeta::default();
        theirs.set_speaker("run-b", 0, "Sam", None, 20);
        let out = merge(&mine, &theirs);
        assert_eq!(out.speakers("run-a").unwrap()["0"].display_name, "Priya", "their cluster 0 is not the same person");
        assert_eq!(out.speakers("run-b").unwrap()["0"].display_name, "Sam");
    }

    #[test]
    fn names_in_one_run_settle_cell_by_cell_and_a_tie_keeps_this_desktops() {
        let mut mine = SessionMeta::default();
        mine.set_speaker("run-a", 0, "Priya", None, 10);
        mine.set_speaker("run-a", 1, "Sam", None, 30);
        let mut theirs = SessionMeta::default();
        theirs.set_speaker("run-a", 0, "Priya N.", None, 20);
        theirs.set_speaker("run-a", 1, "Samuel", None, 30);
        let out = merge(&mine, &theirs);
        let run = out.speakers("run-a").unwrap();
        assert_eq!(run["0"].display_name, "Priya N.");
        assert_eq!(run["1"].display_name, "Sam");
    }

    #[test]
    fn a_name_cleared_here_stays_cleared_over_the_phones_older_name() {
        let mut mine = SessionMeta::default();
        mine.clear_speaker("run-a", 0, 20);
        let mut theirs = SessionMeta::default();
        theirs.set_speaker("run-a", 0, "Priya", Some("c-3".into()), 10);
        let cell = merge(&mine, &theirs).speakers("run-a").unwrap()["0"].clone();
        assert_eq!((cell.display_name.as_str(), cell.contact_id, cell.updated_at_unix_seconds), ("", None, 20));
    }

    #[test]
    fn unknown_keys_from_both_sides_survive_at_every_level() {
        let d: SessionMeta = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "zz": "desk", "zz_desk": 1,
            "title": { "value": "A", "updated_at_unix_seconds": 30, "zz_cell": "desk" },
        }))
        .unwrap();
        let p: SessionMeta = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "zz": "phone", "zz_phone": 2,
            "title": { "value": "B", "updated_at_unix_seconds": 20, "zz_cell": "phone", "zz_phone_cell": 3 },
        }))
        .unwrap();
        let out = serde_json::to_value(merge(&d, &p)).unwrap();
        assert_eq!((&out["zz"], &out["zz_desk"], &out["zz_phone"]), (&"desk".into(), &1.into(), &2.into()));
        assert_eq!(out["title"]["value"], "A");
        assert_eq!((&out["title"]["zz_cell"], &out["title"]["zz_phone_cell"]), (&"desk".into(), &3.into()));
    }

    #[test]
    fn merging_twice_says_what_merging_once_said() {
        let mut mine = SessionMeta::default();
        mine.set_title("here", 30);
        mine.set_tag_ids(["t-1".to_string()], 10);
        let mut theirs = SessionMeta::default();
        theirs.set_title("there", 20);
        theirs.set_tag_ids(["t-2".to_string()], 40);
        let once = merge(&mine, &theirs);
        assert_eq!(merge(&once, &theirs), once);
    }

    const DESKTOP: &str = include_str!("../../tests/fixtures/sync-seam/meta.desktop-v1.json");
    const PHONE: &str = include_str!("../../tests/fixtures/sync-seam/meta.phone-v1.json");
    const MERGED: &str = include_str!("../../tests/fixtures/sync-seam/meta.merged-v1.json");

    /// The vector bytes: two runs' names side by side, a stamped removal, a cell for a tag the catalog deleted, and unknown keys at every level. The merged file is the merge followed through the catalog's tombstones (`sync-seam/tags.merged-v2.json`).
    #[test]
    fn the_seam_fixtures_merge_to_the_pinned_bytes() {
        use recording::session_meta::encode;
        let d: SessionMeta = serde_json::from_str(DESKTOP).unwrap();
        let p: SessionMeta = serde_json::from_str(PHONE).unwrap();
        assert_eq!(encode(&d).unwrap(), DESKTOP.as_bytes(), "the desktop input is in canonical form");
        assert_eq!(encode(&p).unwrap(), PHONE.as_bytes(), "the phone input is in canonical form");
        let tags: crate::commands::tags::TagsFile =
            serde_json::from_str(include_str!("../../tests/fixtures/sync-seam/tags.merged-v2.json")).unwrap();
        let live: BTreeSet<String> = tags.tags.iter().map(|t| t.id.clone()).collect();
        let r = crate::sync::records::Retirements::of(&tags.deleted, &live, &tags.deleted);
        let settle_both = |a: &SessionMeta, b: &SessionMeta| {
            let mut m = merge(a, b);
            m.retire_tags(&r.moved, &r.gone_ids());
            m
        };
        let m = settle_both(&d, &p);
        assert_eq!(String::from_utf8(encode(&m).unwrap()).unwrap(), MERGED);
        assert_eq!(encode(&settle_both(&m, &p)).unwrap(), MERGED.as_bytes(), "merging again changes nothing");
    }
}
