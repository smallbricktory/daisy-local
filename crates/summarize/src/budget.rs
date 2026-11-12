// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Context budget for a local endpoint: how many transcript tokens fit one
//! request once the prompt, schema and reply have their share. Tokens are
//! estimated per character, since the model's own tokenizer is not available.
use crate::error::{Result, SummarizeError};

/// Tokens per character for scripts written with no space between words
/// (Han, kana, Hangul, Thai family).
pub const WIDE_TOKENS_PER_CHAR: f32 = 1.0;
/// Tokens per character for every other script.
pub const OTHER_TOKENS_PER_CHAR: f32 = 0.34;

/// Scripts whose characters each cost about a token.
pub fn is_wide(c: char) -> bool {
    matches!(
        c as u32,
        0x1100..=0x11FF       // Hangul Jamo
        | 0x0E00..=0x0EFF     // Thai, Lao
        | 0x1000..=0x109F     // Myanmar
        | 0x1780..=0x17FF     // Khmer
        | 0x3040..=0x30FF     // Hiragana, Katakana
        | 0x31F0..=0x31FF     // Katakana extensions
        | 0x3400..=0x4DBF     // CJK Extension A
        | 0x4E00..=0x9FFF     // CJK Unified
        | 0xAC00..=0xD7AF     // Hangul syllables
        | 0xF900..=0xFAFF     // CJK Compatibility
        | 0x20000..=0x2A6DF   // CJK Extension B
    )
}

/// Conservative token count for `s`.
pub fn estimate_tokens(s: &str) -> usize {
    let mut wide = 0usize;
    let mut other = 0usize;
    for c in s.chars() {
        if is_wide(c) {
            wide += 1;
        } else {
            other += 1;
        }
    }
    (wide as f32 * WIDE_TOKENS_PER_CHAR + other as f32 * OTHER_TOKENS_PER_CHAR).ceil() as usize
}

/// The context length a local endpoint serves, as the user entered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    pub context_tokens: u32,
}

impl ContextBudget {
    /// Ollama's documented default; LM Studio's is model-dependent.
    pub const DEFAULT_CONTEXT_TOKENS: u32 = 4096;
    /// A transcript window below this is too small to summarize.
    pub const MIN_WINDOW_TOKENS: usize = 1000;
    /// Share of the context kept free of the input for the server's chat
    /// template and the estimator's error.
    const SLACK: f32 = 0.10;

    pub fn new(context_tokens: Option<u32>) -> Self {
        Self {
            context_tokens: context_tokens
                .filter(|n| *n > 0)
                .unwrap_or(Self::DEFAULT_CONTEXT_TOKENS),
        }
    }

    /// Tokens reserved for the reply and sent as the request's output cap.
    /// The floor keeps a 4k context usable.
    pub fn output_reserve(&self) -> u32 {
        (self.context_tokens / 4).clamp(1536, 8192)
    }

    /// Transcript tokens one request can carry once `overhead_tokens` (system
    /// prompt, schema, metadata, notes, directives) and the reply have theirs.
    /// Zero when they leave nothing.
    pub fn room(&self, overhead_tokens: usize) -> usize {
        let usable = (self.context_tokens as f32 * (1.0 - Self::SLACK)).floor() as usize;
        usable
            .saturating_sub(self.output_reserve() as usize)
            .saturating_sub(overhead_tokens)
    }

    /// The transcript window for a meeting that needs more than one request.
    pub fn window_tokens(&self, overhead_tokens: usize) -> Result<usize> {
        let window = self.room(overhead_tokens);
        if window < Self::MIN_WINDOW_TOKENS {
            return Err(SummarizeError::ContextTooSmall {
                context_tokens: self.context_tokens,
                needed: self.output_reserve() as usize + overhead_tokens + Self::MIN_WINDOW_TOKENS,
            });
        }
        Ok(window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_text_costs_about_a_third_of_a_token_per_char() {
        let s = "The quick brown fox jumps over the lazy dog."; // 44 chars
        assert_eq!(estimate_tokens(s), 15);
    }

    #[test]
    fn cjk_and_hangul_cost_a_token_per_char() {
        assert_eq!(estimate_tokens("日本語"), 3);
        assert_eq!(estimate_tokens("한국어"), 3);
        assert_eq!(estimate_tokens("ไทย"), 3);
        // Mixed: 3 wide + 4 other ("abc " with the space).
        assert_eq!(estimate_tokens("abc 日本語"), 3 + (4.0f32 * 0.34).ceil() as usize);
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn blank_or_zero_context_means_4096() {
        assert_eq!(ContextBudget::new(None).context_tokens, 4096);
        assert_eq!(ContextBudget::new(Some(0)).context_tokens, 4096);
        assert_eq!(ContextBudget::new(Some(32768)).context_tokens, 32768);
    }

    #[test]
    fn output_reserve_is_a_quarter_between_floor_and_cap() {
        assert_eq!(ContextBudget::new(Some(4096)).output_reserve(), 1536);
        assert_eq!(ContextBudget::new(Some(8192)).output_reserve(), 2048);
        assert_eq!(ContextBudget::new(Some(131072)).output_reserve(), 8192);
    }

    #[test]
    fn window_is_context_minus_slack_reserve_and_overhead() {
        // 8192 * 0.9 = 7372; - 2048 reserve - 1000 overhead = 4324.
        assert_eq!(ContextBudget::new(Some(8192)).window_tokens(1000).unwrap(), 4324);
        // 4096 * 0.9 = 3686; - 1536 - 1000 = 1150.
        assert_eq!(ContextBudget::new(Some(4096)).window_tokens(1000).unwrap(), 1150);
    }

    #[test]
    fn too_small_a_context_is_a_named_error() {
        let err = ContextBudget::new(Some(2048)).window_tokens(1000).unwrap_err();
        match err {
            SummarizeError::ContextTooSmall { context_tokens, needed } => {
                assert_eq!(context_tokens, 2048);
                assert_eq!(needed, 1536 + 1000 + 1000);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
