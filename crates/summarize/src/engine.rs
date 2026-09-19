// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Shared summary driver: build the prompt, call the transport with the schema
//! for the style's envelope, deserialize + validate (one re-prompt on failure),
//! render deterministically, and assemble the persisted SessionSummary. Both
//! the Anthropic and OpenAI-compatible summarizers route through here.
//!
//! With a [`ContextBudget`], a transcript longer than one request can carry
//! is summarized in windows (one request each) and the parts are merged in
//! code by `fold`; the merged envelope is clamped and validated like a
//! single reply.
use crate::anthropic::{sectioned_schema, summary_schema};
use crate::budget::{estimate_tokens, ContextBudget};
use crate::chat::ChatCompleter;
use crate::error::{Result, SummarizeError};
use crate::fold::{merge_classic, merge_sectioned};
use crate::hash::source_inputs_hash;
use crate::model::{SectionedSummary, SessionSummary, SummaryInput, SummaryStructured};
use crate::prompt::{build_user_message, build_user_message_part, SYSTEM_PROMPT};
use crate::prompts::Envelope;
use crate::render::{assemble_summary, fold_sectioned, render_markdown, render_sectioned};
use crate::window::pack_windows;

/// Tokens of the transport's JSON contract wording around the schema, plus
/// the chat template's role markers.
pub const CONTRACT_TOKENS: usize = 64;

/// `decode_hint` is the transport-specific wording appended when the model's
/// first response doesn't fit the schema (Anthropic talks about a "tool",
/// OpenAI about a "JSON object"). `budget` is the context length of a local
/// endpoint; `None` sends the whole meeting in one request.
pub fn drive(
    completer: &dyn ChatCompleter,
    name: &'static str,
    model: &str,
    input: &SummaryInput,
    now_unix: i64,
    budget: Option<&ContextBudget>,
    decode_hint: &str,
) -> Result<SessionSummary> {
    let (structured, markdown) = match input.style_envelope {
        Envelope::Classic => {
            let parts = collect::<SummaryStructured>(
                completer,
                input,
                &summary_schema(),
                decode_hint,
                budget,
            )?;
            let s = merged(parts, merge_classic)?;
            let md = render_markdown(&s, input.title);
            (s, md)
        }
        Envelope::Sectioned => {
            let parts = collect::<SectionedSummary>(
                completer,
                input,
                &sectioned_schema(),
                decode_hint,
                budget,
            )?;
            let s = merged(parts, merge_sectioned)?;
            (fold_sectioned(&s), render_sectioned(&s, input.title))
        }
    };
    let hash = source_inputs_hash(input, name, model);
    Ok(assemble_summary(
        input.session_id,
        name,
        model,
        now_unix,
        hash,
        structured,
        markdown,
    ))
}

/// One reply per window, or one for the whole meeting when it fits (or when
/// there is no budget).
fn collect<T>(
    completer: &dyn ChatCompleter,
    input: &SummaryInput,
    schema: &serde_json::Value,
    decode_hint: &str,
    budget: Option<&ContextBudget>,
) -> Result<Vec<T>>
where
    T: serde::de::DeserializeOwned + Validatable,
{
    const TOOL: &str = "emit_summary";
    let Some(budget) = budget else {
        return Ok(vec![run::<T>(completer, &build_user_message(input), TOOL, schema, decode_hint)?]);
    };
    let overhead = prompt_overhead(input, schema);
    if estimate_tokens(input.transcript_md) <= budget.room(overhead) {
        return Ok(vec![run::<T>(completer, &build_user_message(input), TOOL, schema, decode_hint)?]);
    }
    let windows = pack_windows(input.transcript_md, budget.window_tokens(overhead)?);
    let n = windows.len();
    windows
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let part = SummaryInput { transcript_md: w, ..*input };
            let msg = build_user_message_part(&part, Some((i + 1, n)));
            run::<T>(completer, &msg, TOOL, schema, decode_hint)
        })
        .collect()
}

/// Everything in a request except the transcript: system prompt, schema
/// contract, and the user message's metadata, notes, and directives.
fn prompt_overhead(input: &SummaryInput, schema: &serde_json::Value) -> usize {
    let without_transcript = SummaryInput { transcript_md: "", ..*input };
    estimate_tokens(SYSTEM_PROMPT)
        + estimate_tokens(&serde_json::to_string(schema).unwrap_or_default())
        + CONTRACT_TOKENS
        + estimate_tokens(&build_user_message_part(&without_transcript, Some((1, 1))))
}

/// A single reply stands as it is; several are merged, then held to the same
/// size limits as one reply.
fn merged<T: Validatable>(mut parts: Vec<T>, merge: impl Fn(&[T]) -> T) -> Result<T> {
    if parts.len() == 1 {
        return Ok(parts.remove(0));
    }
    let mut out = merge(&parts);
    out.clamp();
    out.validate().map_err(SummarizeError::InvalidOutput)?;
    Ok(out)
}

/// Generic call→deserialize→validate→re-prompt for one envelope type.
fn run<T>(
    completer: &dyn ChatCompleter,
    user_msg: &str,
    tool: &str,
    schema: &serde_json::Value,
    decode_hint: &str,
) -> Result<T>
where
    T: serde::de::DeserializeOwned + Validatable,
{
    let call = |msg: &str| -> Result<T> {
        let v = completer.complete_json(SYSTEM_PROMPT, msg, tool, schema)?;
        serde_json::from_value(v).map_err(|e| SummarizeError::Decode(format!("tool input: {e}")))
    };
    let mut parsed = match call(user_msg) {
        Ok(s) => s,
        Err(SummarizeError::Decode(d)) => call(&format!(
            "{user_msg}\n\n[Your previous response did not fit the schema ({d}). {decode_hint}]"
        ))?,
        Err(e) => return Err(e),
    };
    parsed.clamp();
    if let Err(e) = parsed.validate() {
        parsed = call(&format!(
            "{user_msg}\n\n[Reminder: output MUST satisfy the schema and size limits. Previous attempt failed: {e}]"
        ))?;
        parsed.clamp();
        parsed.validate().map_err(SummarizeError::InvalidOutput)?;
    }
    Ok(parsed)
}

/// Validation hook for the generic driver; both impls forward to the
/// inherent `validate`.
pub trait Validatable {
    fn validate(&self) -> std::result::Result<(), String>;
    /// Repair what is repairable before validating. Over-long prose is
    /// clipped rather than thrown away: the alternative is discarding a whole
    /// analysis the user waited for because its summary ran long.
    fn clamp(&mut self) {}
}
impl Validatable for SummaryStructured {
    fn validate(&self) -> std::result::Result<(), String> {
        SummaryStructured::validate(self)
    }
    fn clamp(&mut self) {
        SummaryStructured::clamp(self)
    }
}
impl Validatable for SectionedSummary {
    fn validate(&self) -> std::result::Result<(), String> {
        SectionedSummary::validate(self)
    }
    fn clamp(&mut self) {
        SectionedSummary::clamp(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SummaryInput;
    use serde_json::{json, Value};
    use std::sync::Mutex;

    /// Scripted completer: returns each queued response once, records calls.
    struct Scripted {
        responses: Mutex<Vec<Value>>,
        calls: Mutex<u32>,
        users: Mutex<Vec<String>>,
    }
    impl Scripted {
        fn new(responses: Vec<Value>) -> Self {
            Self {
                responses: Mutex::new(responses),
                calls: Mutex::new(0),
                users: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
        fn users(&self) -> Vec<String> {
            self.users.lock().unwrap().clone()
        }
    }
    impl ChatCompleter for Scripted {
        fn complete_json(
            &self,
            _system: &str,
            user: &str,
            _tool: &str,
            _schema: &serde_json::Value,
        ) -> crate::Result<Value> {
            *self.calls.lock().unwrap() += 1;
            self.users.lock().unwrap().push(user.to_string());
            Ok(self.responses.lock().unwrap().remove(0))
        }
    }

    fn input(envelope: Envelope) -> SummaryInput<'static> {
        SummaryInput {
            session_id: "s",
            title: Some("T"),
            attendees: &[],
            user_notes_md: "",
            transcript_md: "hello",
            tag_prompts: &[],
            style_envelope: envelope,
            style_directive: "",
        }
    }

    fn good_classic() -> Value {
        json!({"tldr":"Fine.","action_items":[],"decisions":["ship it"],"open_questions":[],"key_topics":[]})
    }

    #[test]
    fn decode_failure_reprompts_once_then_succeeds() {
        // `null` fails SummaryStructured deserialization (missing tldr) → the
        // engine re-prompts with the decode hint and accepts the second reply.
        let c = Scripted::new(vec![json!(null), good_classic()]);
        let out = drive(&c, "test", "m", &input(Envelope::Classic), 7, None, "hint").unwrap();
        assert_eq!(c.calls(), 2);
        assert!(out.markdown.contains("**TL;DR.** Fine."));
        assert_eq!(out.generated_at_unix_seconds, 7);
    }

    #[test]
    fn validation_failure_reprompts_once_then_succeeds() {
        // Well-formed but EMPTY output fails validate() → one re-prompt.
        let empty = json!({"tldr":"  ","action_items":[],"decisions":[],"open_questions":[],"key_topics":[]});
        let c = Scripted::new(vec![empty, good_classic()]);
        let out = drive(&c, "test", "m", &input(Envelope::Classic), 0, None, "hint").unwrap();
        assert_eq!(c.calls(), 2);
        assert!(out.structured.decisions.contains(&"ship it".to_string()));
    }

    #[test]
    fn persistent_validation_failure_errors_after_the_retry() {
        let empty = json!({"tldr":"","action_items":[],"decisions":[],"open_questions":[],"key_topics":[]});
        let c = Scripted::new(vec![empty.clone(), empty]);
        let err = drive(&c, "test", "m", &input(Envelope::Classic), 0, None, "hint").unwrap_err();
        assert_eq!(c.calls(), 2);
        assert!(matches!(err, SummarizeError::InvalidOutput(_)));
    }

    #[test]
    fn sectioned_envelope_folds_and_renders_sectioned_markdown() {
        let sectioned = json!({
            "exec_summary": "Good sync.",
            "sections": [{"heading": "Scope", "bullets": true, "content": ["trimmed"]}],
            "action_items": [{"text": "ship", "owner": null, "due": null}]
        });
        let c = Scripted::new(vec![sectioned]);
        let out = drive(&c, "test", "m", &input(Envelope::Sectioned), 0, None, "hint").unwrap();
        assert_eq!(c.calls(), 1);
        assert!(out.markdown.contains("**Summary.** Good sync."));
        assert!(out.markdown.contains("## Scope"));
        // Folded into the persisted classic shape: exec→tldr, actions carried.
        assert_eq!(out.structured.tldr, "Good sync.");
        assert_eq!(out.structured.action_items.len(), 1);
        assert!(out.structured.decisions.is_empty());
    }

    /// A transcript of `turns` one-line turns, each about 40 tokens.
    fn long_transcript(turns: usize) -> String {
        (0..turns)
            .map(|i| {
                format!(
                    "[00:{:02}:00] **Ana**: point number {i} about the pilot scope and the exporter budget for this quarter, with a bit more detail on timing.\n",
                    i % 60
                )
            })
            .collect()
    }

    #[test]
    fn a_short_meeting_with_a_budget_is_one_whole_meeting_request() {
        let c = Scripted::new(vec![good_classic()]);
        let budget = ContextBudget::new(Some(8192));
        let out = drive(&c, "test", "m", &input(Envelope::Classic), 0, Some(&budget), "hint").unwrap();
        assert_eq!(c.calls(), 1);
        let user = &c.users()[0];
        assert!(!user.contains("Transcript part"), "{user}");
        assert!(out.markdown.contains("**TL;DR.** Fine."));
    }

    #[test]
    fn a_long_meeting_is_windowed_and_the_parts_are_merged() {
        let transcript = long_transcript(120);
        let mut inp = input(Envelope::Classic);
        inp.transcript_md = &transcript;
        let budget = ContextBudget::new(Some(4096));
        // Enough replies for any window count; each names its own decision.
        let replies: Vec<Value> = (0..50)
            .map(|i| {
                json!({
                    "tldr": format!("Topic{i} settled, part{i} done."),
                    "action_items": [{"text": "send the checklist", "owner": if i == 1 { Some("Ana") } else { None }, "due": null}],
                    "decisions": [format!("decision {i}"), "No decisions were made."],
                    "open_questions": [],
                    "key_topics": ["pilot"]
                })
            })
            .collect();
        let c = Scripted::new(replies);
        let out = drive(&c, "test", "m", &inp, 0, Some(&budget), "hint").unwrap();
        let n = c.calls() as usize;
        assert!(n >= 2, "expected several windows, got {n}");
        let users = c.users();
        assert!(users[0].contains(&format!("Transcript part 1 of {n}")));
        assert!(users[n - 1].contains(&format!("Transcript part {n} of {n}")));
        // Every window carries a slice of the transcript, in order, nothing lost.
        let joined: String = users
            .iter()
            .map(|u| {
                let start = u.find("<transcript>\n").unwrap() + "<transcript>\n".len();
                let end = u.find("\n</transcript>").unwrap();
                u[start..end].to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(joined.replace('\n', ""), transcript.replace('\n', ""));
        // Merged: distinct decisions kept, filler dropped, one action item with
        // the named owner, tldr sentences in order.
        assert_eq!(out.structured.decisions.len(), n);
        assert_eq!(out.structured.action_items.len(), 1);
        assert_eq!(out.structured.action_items[0].owner.as_deref(), Some("Ana"));
        assert!(out.structured.tldr.starts_with("Topic0 settled, part0 done. Topic1 settled, part1 done."), "{}", out.structured.tldr);
        assert_eq!(out.structured.key_topics, vec!["pilot"]);
    }

    #[test]
    fn a_context_too_small_for_any_window_is_a_named_error() {
        let transcript = long_transcript(120);
        let mut inp = input(Envelope::Classic);
        inp.transcript_md = &transcript;
        let budget = ContextBudget::new(Some(2048));
        let c = Scripted::new(vec![]);
        let err = drive(&c, "test", "m", &inp, 0, Some(&budget), "hint").unwrap_err();
        assert_eq!(c.calls(), 0);
        assert!(matches!(err, SummarizeError::ContextTooSmall { context_tokens: 2048, .. }), "{err}");
    }
}
