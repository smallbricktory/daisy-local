use transcript::text::{bigram_jaccard, normalize};

#[test]
fn normalize_handles_case_punctuation_whitespace() {
    assert_eq!(normalize("Hello, world!  How ARE you?"), "hello world how are you");
}

#[test]
fn normalize_strips_whisper_noise_tokens() {
    assert_eq!(normalize("[BLANK_AUDIO]"), "");
    assert_eq!(normalize("hello [BLANK_AUDIO] there"), "hello there");
    assert_eq!(normalize("♪ music ♪"), "music");
    assert_eq!(normalize("(applause) thanks"), "thanks");
}

#[test]
fn jaccard_identical_strings_is_one() {
    let s = "the quick brown fox jumps over the lazy dog";
    assert!((bigram_jaccard(s, s) - 1.0).abs() < 1e-6);
}

#[test]
fn jaccard_disjoint_strings_is_zero() {
    let a = "completely different words here today";
    let b = "totally separate phrase entirely about cats";
    assert_eq!(bigram_jaccard(a, b), 0.0);
}

#[test]
fn jaccard_high_for_known_bleed_case() {
    // Bleed case: the mic captured what came out of the speakers. After AEC +
    // transcription, bleed text looks like the system text minus the leading
    // filler.
    let mic = "marco or sorry not marco miles and ask him if him and dana should be added as approvers";
    let sys = "after this afternoon i'll talk to marco or sorry not marco miles and ask him if him and dana should be added as approvers";
    let s = bigram_jaccard(mic, sys);
    assert!(s > 0.6, "expected high similarity, got {s}");
}

#[test]
fn jaccard_low_for_independent_speech() {
    // Both speakers discussing the same topic, different wording.
    let mic = "the vendor demo ran long so we skipped the budget review";
    let sys = "let's move the retro to thursday and keep friday clear";
    let s = bigram_jaccard(mic, sys);
    assert!(s < 0.3, "expected low similarity, got {s}");
}

#[test]
fn jaccard_empty_strings_yield_zero() {
    assert_eq!(bigram_jaccard("", ""), 0.0);
    assert_eq!(bigram_jaccard("hello", ""), 0.0);
    assert_eq!(bigram_jaccard("", "hello"), 0.0);
}

#[test]
fn jaccard_single_word_uses_unigram_fallback() {
    // A single-word string falls back to the unigram match.
    assert_eq!(bigram_jaccard("yeah", "yeah"), 1.0);
    assert_eq!(bigram_jaccard("yeah", "right"), 0.0);
}
