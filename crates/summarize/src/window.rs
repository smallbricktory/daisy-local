// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Splits a rendered transcript into windows that each fit a token budget.
//! Units are speaker turns (`[hh:mm:ss] **Name**: text`); header lines are
//! dropped, continuation lines stay with their turn, and no window shares
//! text with another.
use crate::budget::estimate_tokens;

/// One speaker turn of the markdown transcript, header lines removed.
pub fn transcript_units(md: &str) -> Vec<String> {
    let mut units: Vec<String> = Vec::new();
    for line in md.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() || is_header(trimmed) {
            continue;
        }
        if is_turn_start(trimmed) || units.is_empty() {
            units.push(trimmed.to_string());
        } else {
            let last = units.last_mut().expect("non-empty");
            last.push('\n');
            last.push_str(trimmed);
        }
    }
    units
}

fn is_header(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('#') || (t.starts_with('_') && t.ends_with('_') && t.len() > 1)
}

/// `[hh:mm:ss] ` at the start of a line marks a turn.
fn is_turn_start(line: &str) -> bool {
    let b = line.as_bytes();
    b.len() > 10
        && b[0] == b'['
        && b[9] == b']'
        && b[3] == b':'
        && b[6] == b':'
        && [1, 2, 4, 5, 7, 8].iter().all(|&i| b[i].is_ascii_digit())
}

/// The `[hh:mm:ss] **Name**: ` prefix of a turn, when it has one.
fn turn_prefix(unit: &str) -> &str {
    if !is_turn_start(unit) {
        return "";
    }
    match unit.find("**: ") {
        Some(i) if unit[..i].matches("**").count() == 1 => &unit[..i + 4],
        _ => &unit[..11.min(unit.len())],
    }
}

/// Pack turns into windows of at most `budget_tokens` (estimated). A turn
/// longer than the budget is split at whitespace, and at characters where a
/// run has none, each piece keeping the turn's speaker prefix.
pub fn pack_windows(md: &str, budget_tokens: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_tokens = 0usize;
    for unit in transcript_units(md) {
        for piece in split_to_budget(&unit, budget_tokens) {
            let cost = estimate_tokens(&piece) + 1;
            if !current.is_empty() && current_tokens + cost > budget_tokens {
                out.push(std::mem::take(&mut current));
                current_tokens = 0;
            }
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(&piece);
            current_tokens += cost;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// `unit` whole when it fits, else pieces that each fit, prefix repeated.
fn split_to_budget(unit: &str, budget_tokens: usize) -> Vec<String> {
    if estimate_tokens(unit) < budget_tokens {
        return vec![unit.to_string()];
    }
    let prefix = turn_prefix(unit);
    let body = &unit[prefix.len()..];
    let room = budget_tokens.saturating_sub(estimate_tokens(prefix) + 1).max(50);
    let mut pieces: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in body.split_whitespace() {
        for part in cut_word(word, room) {
            let joined_cost = estimate_tokens(&current) + estimate_tokens(&part) + 1;
            if !current.is_empty() && joined_cost > room {
                pieces.push(format!("{prefix}{current}"));
                current.clear();
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(&part);
        }
    }
    if !current.is_empty() {
        pieces.push(format!("{prefix}{current}"));
    }
    pieces
}

/// A word longer than `room` tokens is cut at character boundaries; a run
/// with no separator has nothing better.
fn cut_word(word: &str, room: usize) -> Vec<String> {
    if estimate_tokens(word) <= room {
        return vec![word.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for c in word.chars() {
        let mut probe = current.clone();
        probe.push(c);
        if !current.is_empty() && estimate_tokens(&probe) > room {
            out.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = "# Transcript: daisy-1\n_Provider: live (live)_\n\n## Chunk 1\n\n[00:00:04] **Me**: How are we doing?\n[00:00:11] **Ana**: Going well.\n\n[00:01:04] **Me**: Second paragraph here.\n";

    #[test]
    fn units_are_turns_without_headers() {
        let u = transcript_units(MD);
        assert_eq!(
            u,
            vec![
                "[00:00:04] **Me**: How are we doing?",
                "[00:00:11] **Ana**: Going well.",
                "[00:01:04] **Me**: Second paragraph here.",
            ]
        );
    }

    #[test]
    fn continuation_lines_stay_with_their_turn() {
        let u = transcript_units("[00:00:04] **Me**: first line\nsecond line of the same turn\n[00:00:09] **Ana**: next");
        assert_eq!(u.len(), 2);
        assert_eq!(u[0], "[00:00:04] **Me**: first line\nsecond line of the same turn");
    }

    #[test]
    fn everything_fits_one_window() {
        let w = pack_windows(MD, 4000);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("How are we doing?") && w[0].contains("Second paragraph here."));
        assert!(!w[0].contains("# Transcript"));
    }

    #[test]
    fn windows_never_share_a_turn_and_keep_order() {
        let md: String = (0..40)
            .map(|i| format!("[00:{:02}:00] **Me**: turn number {i} with a few words of speech in it.\n", i))
            .collect();
        let w = pack_windows(&md, 120);
        assert!(w.len() > 1);
        let joined = w.join("\n");
        assert_eq!(joined, transcript_units(&md).join("\n"));
        for win in &w {
            assert!(estimate_tokens(win) <= 120 + 1, "window over budget: {}", estimate_tokens(win));
        }
    }

    #[test]
    fn a_long_turn_splits_at_words_with_its_prefix_repeated() {
        let body: String = vec!["word"; 400].join(" ");
        let md = format!("[00:00:04] **Ana**: {body}");
        let w = pack_windows(&md, 200);
        assert!(w.len() > 1);
        for win in &w {
            assert!(win.starts_with("[00:00:04] **Ana**: "));
            assert!(estimate_tokens(win) <= 200);
        }
        let rebuilt: Vec<&str> = w.iter().map(|s| &s["[00:00:04] **Ana**: ".len()..]).collect();
        assert_eq!(rebuilt.join(" "), body);
    }

    #[test]
    fn an_unsegmented_run_splits_at_characters() {
        // 600 kana with no whitespace: a whitespace-only splitter would return it whole.
        let body: String = "あ".repeat(600);
        let md = format!("[00:00:04] **Ana**: {body}");
        let w = pack_windows(&md, 200);
        assert!(w.len() >= 3);
        for win in &w {
            assert!(win.starts_with("[00:00:04] **Ana**: "));
            assert!(estimate_tokens(win) <= 200);
        }
        let total: usize = w.iter().map(|s| s.chars().filter(|c| *c == 'あ').count()).sum();
        assert_eq!(total, 600);
    }

    #[test]
    fn prefix_detection_handles_names_with_colons() {
        assert_eq!(turn_prefix("[00:00:04] **Dr. Who: TARDIS**: text"), "[00:00:04] **Dr. Who: TARDIS**: ");
        assert_eq!(turn_prefix("[00:00:04] no speaker"), "[00:00:04] ");
        assert_eq!(turn_prefix("plain"), "");
    }
}
