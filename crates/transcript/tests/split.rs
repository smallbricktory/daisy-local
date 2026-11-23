// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use transcript::split::{normalize_split_groups, rejoin_split_groups, split_by_turns};
use transcript::{Segment, SessionTranscript};

/// Words of 300 ms each, back to back from `start`, 100 ms apart.
fn talk(start: i64, words: &[&str]) -> Segment {
    let mut at = start;
    let mut seg = Segment::from_words(words.iter().map(|t| {
        let w = (t.to_string(), at, at + 300);
        at += 400;
        w
    }))
    .unwrap();
    seg.confidence = Some(0.8);
    seg
}

fn texts(segs: &[Segment]) -> Vec<(&str, Option<u32>)> {
    segs.iter().map(|s| (s.text.as_str(), s.speaker_id)).collect()
}

#[test]
fn handover_mid_segment_splits_in_two() {
    let seg = talk(1_000, &["we", "should", "ship", "friday", "sounds", "good", "to", "me"]);
    let turns = [(900, 2_700, 0), (2_700, 4_300, 1)];
    let out = split_by_turns(&seg, &turns, 7);
    assert_eq!(texts(&out), [("we should ship friday", Some(0)), ("sounds good to me", Some(1))]);
    assert!(out.iter().all(|p| p.split_group == Some(7) && p.confidence == Some(0.8)));
    assert!(out.iter().all(Segment::words_consistent));
    assert_eq!(out[0].start_ms, seg.start_ms);
    assert_eq!(out[1].end_ms, seg.end_ms);
}

#[test]
fn outer_edges_keep_original_padding() {
    let mut seg = talk(1_000, &["alpha", "beta", "gamma", "delta", "eps", "zeta"]);
    seg.set_span(800, 4_000);
    let out = split_by_turns(&seg, &[(0, 2_000, 0), (2_000, 5_000, 1)], 1);
    assert_eq!(out.len(), 2);
    assert_eq!((out[0].start_ms, out[1].end_ms), (800, 4_000));
    assert_eq!(out[0].end_ms, out[0].words.last().unwrap().end_ms);
}

#[test]
fn backchannel_stays_with_main_speaker() {
    let seg = talk(0, &["the", "plan", "is", "to", "yeah", "migrate", "the", "queue", "first"]);
    // "yeah" sits at 1600..1900, inside a turn of speaker 1.
    let turns = [(0, 1_600, 0), (1_600, 2_000, 1), (2_000, 4_000, 0)];
    let out = split_by_turns(&seg, &turns, 1);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].speaker_id, Some(0));
    assert_eq!(out[0].text, seg.text);
    assert_eq!(out[0].split_group, None);
}

#[test]
fn single_word_run_is_absorbed_but_a_two_word_run_over_400ms_is_kept() {
    let seg = talk(
        0,
        &["one", "two", "three", "four", "five", "ok", "yes", "six", "seven", "eight"],
    );
    // Only "ok" lands in speaker 1: one word, absorbed.
    let turns = [(0, 2_000, 0), (2_000, 2_300, 1), (2_300, 4_000, 0)];
    let out = split_by_turns(&seg, &turns, 1);
    assert_eq!(out.len(), 1);

    // "ok yes" spans 700 ms with two words: kept.
    let turns = [(0, 2_000, 0), (2_000, 2_900, 1), (2_900, 4_000, 2)];
    let out = split_by_turns(&seg, &turns, 1);
    assert_eq!(out.iter().map(|s| s.speaker_id).collect::<Vec<_>>(), [Some(0), Some(1), Some(2)]);
}

#[test]
fn middle_run_under_400ms_joins_longer_neighbour() {
    let seg = talk(0, &["a1", "a2", "b1", "c1", "c2", "c3", "c4"]);
    // b1 (800..1100) is one word between speakers 0 and 2; the longer side wins.
    let turns = [(0, 800, 0), (800, 1_200, 1), (1_200, 3_000, 2)];
    let out = split_by_turns(&seg, &turns, 4);
    assert_eq!(texts(&out), [("a1 a2", Some(0)), ("b1 c1 c2 c3 c4", Some(2))]);
    assert!(out.iter().all(Segment::words_consistent));
}

#[test]
fn without_words_takes_majority_speaker() {
    let seg = Segment { start_ms: 1_000, end_ms: 3_000, text: "no word times".into(), ..Default::default() };
    let turns = [(0, 1_500, 0), (1_500, 3_500, 1)];
    let out = split_by_turns(&seg, &turns, 1);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].speaker_id, Some(1));
    assert_eq!(out[0].split_group, None);
}

#[test]
fn inconsistent_words_take_majority_speaker() {
    let mut seg = talk(0, &["hello", "there", "friend", "again"]);
    seg.text = "edited text".into();
    let turns = [(0, 600, 0), (600, 1_600, 1)];
    let out = split_by_turns(&seg, &turns, 1);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].speaker_id, Some(1));
    assert_eq!(out[0].text, "edited text");
}

#[test]
fn no_overlap_uses_nearest_within_300ms_else_unchanged() {
    let mut seg = Segment { start_ms: 1_000, end_ms: 2_000, text: "x".into(), speaker_id: Some(9), ..Default::default() };
    assert_eq!(split_by_turns(&seg, &[(0, 800, 3)], 1)[0].speaker_id, Some(3));
    assert_eq!(split_by_turns(&seg, &[(2_200, 3_000, 4)], 1)[0].speaker_id, Some(4));
    assert_eq!(split_by_turns(&seg, &[(0, 500, 3)], 1)[0].speaker_id, Some(9));
    seg.speaker_id = None;
    assert_eq!(split_by_turns(&seg, &[], 1)[0].speaker_id, None);
}

#[test]
fn words_in_a_gap_take_the_nearest_turn_within_300ms() {
    let seg = talk(0, &["red", "blue", "green", "pink", "teal", "gold"]);
    // Words 0..2400; a 700 ms gap 1200..1900 between turns. green (800..1100 mid 950) inside;
    // pink mid 1350 is 150 from turn 0; teal mid 1750 is 150 from turn 1.
    let turns = [(0, 1_200, 0), (1_900, 3_000, 1)];
    let out = split_by_turns(&seg, &turns, 2);
    assert_eq!(texts(&out), [("red blue green pink", Some(0)), ("teal gold", Some(1))]);
}

#[test]
fn words_beyond_300ms_of_any_turn_inherit_the_previous_label() {
    let seg = talk(0, &["one", "two", "three", "four", "five", "six"]);
    // three..six lie far from the only turn.
    let out = split_by_turns(&seg, &[(0, 900, 5)], 1);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].speaker_id, Some(5));
}

#[test]
fn leading_unlabelled_words_take_the_next_label() {
    let seg = talk(0, &["one", "two", "three", "four"]);
    let out = split_by_turns(&seg, &[(1_200, 2_000, 6)], 1);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].speaker_id, Some(6));
}

fn handover() -> (Segment, Vec<(u32, u32, u32)>) {
    let mut seg = talk(1_000, &["we", "should", "ship", "friday", "sounds", "good", "to", "me"]);
    seg.set_span(900, 4_400);
    (seg, vec![(900, 2_700, 0), (2_700, 4_400, 1)])
}

#[test]
fn rejoin_restores_the_original_segment() {
    let (seg, turns) = handover();
    let pieces = split_by_turns(&seg, &turns, 3);
    assert_eq!(pieces.len(), 2);
    let back = rejoin_split_groups(pieces);
    assert_eq!(back.len(), 1);
    let mut expect = seg.clone();
    expect.speaker_id = back[0].speaker_id;
    assert_eq!(back[0], expect);
}

#[test]
fn rejoin_speaker_is_the_longest_piece_and_leaves_ungrouped_alone() {
    let (seg, turns) = handover();
    let mut segs = vec![talk(0, &["solo"])];
    segs.extend(split_by_turns(&seg, &turns, 3));
    segs.push(talk(9_000, &["tail"]));
    let back = rejoin_split_groups(segs);
    assert_eq!(back.len(), 3);
    assert_eq!(back[1].speaker_id, Some(1));
    assert_eq!(back[1].split_group, None);
    assert_eq!(back[0].text, "solo");
    assert_eq!(back[2].text, "tail");
}

#[test]
fn rejoin_keeps_different_groups_apart() {
    let (seg, turns) = handover();
    let mut segs = split_by_turns(&seg, &turns, 1);
    let mut second = seg.clone();
    second.shift(10_000);
    let t2: Vec<_> = turns.iter().map(|&(s, e, id)| (s + 10_000, e + 10_000, id)).collect();
    segs.extend(split_by_turns(&second, &t2, 2));
    assert_eq!(rejoin_split_groups(segs).len(), 2);
}

#[test]
fn split_rejoin_split_is_idempotent() {
    let (seg, turns) = handover();
    let first = split_by_turns(&seg, &turns, 5);
    let again = split_by_turns(&rejoin_split_groups(first.clone())[0], &turns, 5);
    assert_eq!(first, again);
}

#[test]
fn every_piece_keeps_words_consistent() {
    let (seg, turns) = handover();
    for p in split_by_turns(&seg, &turns, 1) {
        assert!(p.words_consistent());
        assert!(!p.words.is_empty());
    }
}

#[test]
fn split_group_round_trips_and_old_payloads_load() {
    let (seg, turns) = handover();
    for p in split_by_turns(&seg, &turns, 12) {
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains(r#""split_group":12"#));
        assert_eq!(serde_json::from_str::<Segment>(&json).unwrap(), p);
    }
    let old = r#"{"start_ms":0,"end_ms":900,"text":"Hello there","confidence":null,"speaker_id":2}"#;
    let seg: Segment = serde_json::from_str(old).unwrap();
    assert_eq!(seg.split_group, None);
    assert_eq!(serde_json::to_string(&seg).unwrap(), old);
}

/// A transcript of one chunk with these tracks, each a list of segments.
fn transcript_of(tracks: Vec<(&str, Vec<Segment>)>) -> SessionTranscript {
    serde_json::from_value(serde_json::json!({
        "schema_version": 1, "session_id": "s", "provider": "p", "model": "m",
        "transcribed_at_unix_seconds": 0,
        "chunks": [{ "chunk_index": 0, "tracks": tracks.iter().map(|(t, segs)| serde_json::json!({
            "track": t, "source_wav_relative": "x.wav", "segments": segs,
        })).collect::<Vec<_>>() }],
    }))
    .unwrap()
}

fn groups(st: &SessionTranscript) -> Vec<Vec<Option<u32>>> {
    st.chunks[0].tracks.iter().map(|t| t.segments.iter().map(|s| s.split_group).collect()).collect()
}

#[test]
fn split_groups_are_renumbered_densely_and_broken_ones_cleared() {
    let (seg, turns) = handover();
    let piece = |group: u32| {
        let mut p = split_by_turns(&seg, &turns, group);
        p.truncate(1);
        p.remove(0)
    };
    let plain = talk(20_000, &["fictional", "words"]);
    let mut wordless = piece(3);
    wordless.words.clear();
    let mut st = transcript_of(vec![
        (
            "system",
            vec![
                piece(u32::MAX), piece(u32::MAX), // one run: kept
                plain.clone(),
                piece(5), plain.clone(), piece(5), // two runs: cleared
                piece(3), wordless,               // a piece without words: cleared
                piece(40), piece(40),             // kept
            ],
        ),
        ("mic", vec![piece(9), piece(9)]), // kept, numbered after the system track
    ]);
    assert!(normalize_split_groups(&mut st));
    assert_eq!(
        groups(&st),
        vec![
            vec![Some(0), Some(0), None, None, None, None, None, None, Some(1), Some(1)],
            vec![Some(2), Some(2)],
        ]
    );
    assert!(!normalize_split_groups(&mut st), "a normalized transcript is left alone");
}

#[test]
fn a_group_on_two_tracks_is_cleared_on_both() {
    let (seg, turns) = handover();
    let pieces = split_by_turns(&seg, &turns, 6);
    let mut st = transcript_of(vec![("system", pieces.clone()), ("mic", pieces)]);
    assert!(normalize_split_groups(&mut st));
    assert_eq!(groups(&st), vec![vec![None, None], vec![None, None]]);
}
