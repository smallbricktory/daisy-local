// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use transcript::Segment;

fn w(text: &str, s: i64, e: i64) -> (String, i64, i64) {
    (text.into(), s, e)
}

#[test]
fn from_words_joins_trims_and_clamps() {
    let seg = Segment::from_words([
        w(" Hello", 100, 400),
        w("  ", 400, 450),
        w("there.", 400, 900),
        w("late", 800, 2_000),
    ])
    .unwrap();
    assert_eq!(seg.text, "Hello there. late");
    assert_eq!((seg.start_ms, seg.end_ms), (100, 2_000));
    assert_eq!(seg.words.len(), 3);
    assert!(seg.words_consistent());
}

#[test]
fn from_words_floors_negative_times_and_rejects_empty() {
    let seg = Segment::from_words([w("a", -50, 100), w("b", 100, 200)]).unwrap();
    assert_eq!(seg.words[0].start_ms, 0);
    assert!(seg.words_consistent());
    assert!(Segment::from_words([w("  ", 0, 1)]).is_none());
}

#[test]
fn words_beyond_the_span_break_the_invariant() {
    let mut seg = Segment::from_words([w("a", 0, 100), w("b", 100, 200)]).unwrap();
    seg.end_ms = 150;
    assert!(!seg.words_consistent());
    seg.set_span(0, 150);
    assert!(seg.words_consistent());
    seg.text = "changed".into();
    assert!(!seg.words_consistent());
}

#[test]
fn old_payload_without_words_deserializes_and_reserializes_unchanged() {
    let old = r#"{"start_ms":0,"end_ms":900,"text":"Hello there","confidence":null,"speaker_id":2}"#;
    let seg: Segment = serde_json::from_str(old).unwrap();
    assert!(seg.words.is_empty());
    assert_eq!(serde_json::to_string(&seg).unwrap(), old);
}

#[test]
fn old_session_transcript_payload_deserializes() {
    let old = r#"{"schema_version":1,"session_id":"s","provider":"p","model":"m","transcribed_at_unix_seconds":1,
        "chunks":[{"chunk_index":0,"tracks":[{"track":"mic","source_wav_relative":"a.wav",
        "segments":[{"start_ms":0,"end_ms":10,"text":"hi","confidence":null}]}]}]}"#;
    let t: transcript::SessionTranscript = serde_json::from_str(old).unwrap();
    assert!(t.chunks[0].tracks[0].segments[0].words.is_empty());
}

#[test]
fn words_round_trip() {
    let seg = Segment::from_words([w("Hello", 0, 400), w("there", 400, 900)]).unwrap();
    let back: Segment = serde_json::from_str(&serde_json::to_string(&seg).unwrap()).unwrap();
    assert_eq!(seg, back);
}

#[test]
fn shift_moves_words_and_floors_at_zero() {
    let mut seg = Segment::from_words([w("a", 1_000, 1_200), w("b", 1_200, 1_500)]).unwrap();
    seg.shift(-1_100);
    assert_eq!((seg.start_ms, seg.end_ms), (0, 400));
    assert_eq!(seg.words[1].start_ms, 100);
    assert!(seg.words_consistent());
}

#[test]
fn drop_leading_tokens_uses_word_times_or_clears_phrases() {
    let mut seg =
        Segment::from_words([w("one", 0, 100), w("two", 100, 300), w("three", 300, 600)]).unwrap();
    seg.drop_leading_tokens(1);
    assert_eq!(seg.text, "two three");
    assert_eq!(seg.start_ms, 100);
    assert!(seg.words_consistent());
    assert_eq!(seg.words.len(), 2);

    let mut phrase = Segment::from_words([w("one two", 0, 200), w("three", 200, 300)]).unwrap();
    phrase.drop_leading_tokens(1);
    assert_eq!(phrase.text, "two three");
    assert!(phrase.words.is_empty());
}

#[test]
fn retext_carries_one_for_one_rewrites_and_clears_the_rest() {
    let mut seg = Segment::from_words([w("Okay", 0, 100), w("so", 100, 200)]).unwrap();
    seg.retext("okay, so".into());
    assert_eq!(seg.words.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["okay,", "so"]);
    assert_eq!(seg.words[1].start_ms, 100);
    assert!(seg.words_consistent());
    seg.retext("something else entirely".into());
    assert!(seg.words.is_empty());
}

#[test]
fn masking_keeps_words_joined_to_text() {
    let mut seg =
        Segment::from_words([w("what", 0, 100), w("the", 100, 200), w("fucking", 200, 400)]).unwrap();
    seg.mask_profanity();
    assert_eq!(seg.text, "what the f***ing");
    assert!(seg.words_consistent());
}

fn timed(words: &[(&str, u32)]) -> Segment {
    Segment::from_words(words.iter().map(|(t, s)| (t.to_string(), *s as i64, *s as i64 + 100))).unwrap()
}

fn times(seg: &Segment) -> Vec<(String, u32, u32)> {
    seg.words.iter().map(|w| (w.text.clone(), w.start_ms, w.end_ms)).collect()
}

#[test]
fn retext_with_unchanged_text_keeps_phrase_runs() {
    let mut seg = Segment::from_words([w("North Wind", 0, 200), w("and", 200, 300)]).unwrap();
    seg.retext("North Wind and".into());
    assert_eq!(seg.words.len(), 2);
}

#[test]
fn retext_regroups_phrase_runs_by_their_token_counts() {
    let mut seg = Segment::from_words([w("North Wind", 0, 200), w("and", 200, 300)]).unwrap();
    seg.retext("north wind And".into());
    assert_eq!(times(&seg), [("north wind".into(), 0, 200), ("And".into(), 200, 300)]);
    assert!(seg.words_consistent());
}

#[test]
fn realign_filler_removal_keeps_the_surviving_times() {
    let mut seg = timed(&[("I,", 0), ("I", 200), ("mean,", 400), ("it", 600), ("works", 800)]);
    seg.realign("I mean, it works".into());
    assert_eq!(seg.text, "I mean, it works");
    assert_eq!(seg.words.len(), 4);
    let t = times(&seg);
    assert_eq!(t[1], ("mean,".into(), 400, 500));
    assert_eq!(t[3], ("works".into(), 800, 900));
    assert!(seg.words_consistent());
}

#[test]
fn realign_inserted_token_sits_at_the_previous_words_end() {
    let mut seg = timed(&[("we", 0), ("ship", 200), ("today", 400)]);
    seg.realign("we will ship today".into());
    let t = times(&seg);
    assert_eq!(t[1], ("will".into(), 100, 100));
    assert_eq!(t[2], ("ship".into(), 200, 300));
    assert!(seg.words_consistent());
}

#[test]
fn realign_detects_a_swap_instead_of_mis_attaching_times() {
    let mut seg = timed(&[("a", 0), ("b", 200), ("c", 400), ("d", 600)]);
    seg.realign("a c b d".into());
    let t = times(&seg);
    // A swapped pair keeps one word's times; the other word is zero-length.
    let zero_length = t.iter().filter(|(_, s, e)| s == e).count();
    assert_eq!(zero_length, 1, "{t:?}");
    for (txt, s, e) in &t {
        let own = match txt.as_str() {
            "a" => 0,
            "b" => 200,
            "c" => 400,
            _ => 600,
        };
        assert!(*s == own || s == e, "{t:?}");
    }
    assert!(seg.words_consistent());
}

#[test]
fn realign_clears_words_when_under_half_the_tokens_match() {
    let mut seg = timed(&[("alpha", 0), ("beta", 200), ("gamma", 400)]);
    seg.realign("completely different sentence here".into());
    assert!(seg.words.is_empty());
}

#[test]
fn cut_after_drops_words_past_the_cut_with_their_text() {
    let mut seg = timed(&[("one", 0), ("two", 200), ("three", 400), ("four", 600)]);
    assert!(seg.cut_after(450));
    assert_eq!(seg.text, "one two three");
    assert_eq!(seg.end_ms, 450);
    assert!(seg.words_consistent());
    let mut late = timed(&[("x", 500)]);
    assert!(!late.cut_after(450));
}
