//! Shared summary driver: build the prompt, call the transport with the schema
//! for the style's envelope, deserialize + validate (one re-prompt on failure),
//! render deterministically, and assemble the persisted SessionSummary. Both
//! the Anthropic and OpenAI-compatible summarizers route through here.
use crate::anthropic::{sectioned_schema, summary_schema};
use crate::chat::ChatCompleter;
use crate::error::{Result, SummarizeError};
use crate::hash::source_inputs_hash;
use crate::model::{SectionedSummary, SessionSummary, SummaryInput, SummaryStructured};
use crate::prompt::{build_user_message, SYSTEM_PROMPT};
use crate::render::{assemble_summary, fold_sectioned, render_markdown, render_sectioned};
use crate::prompts::Envelope;

/// `decode_hint` is the transport-specific wording appended when the model's
/// first response doesn't fit the schema (Anthropic talks about a "tool",
/// OpenAI about a "JSON object").
pub fn drive(
    completer: &dyn ChatCompleter,
    name: &'static str,
    model: &str,
    input: &SummaryInput,
    now_unix: i64,
    decode_hint: &str,
) -> Result<SessionSummary> {
    let user_msg = build_user_message(input);
    let (structured, markdown) = match input.style_envelope {
        Envelope::Classic => run::<SummaryStructured>(
            completer,
            &user_msg,
            "emit_summary",
            &summary_schema(),
            decode_hint,
            |s| (s.clone(), render_markdown(s, input.title)),
        )?,
        Envelope::Sectioned => run::<SectionedSummary>(
            completer,
            &user_msg,
            "emit_summary",
            &sectioned_schema(),
            decode_hint,
            |s| (fold_sectioned(s), render_sectioned(s, input.title)),
        )?,
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

/// Generic call→deserialize→validate→re-prompt→render for one envelope type.
fn run<T>(
    completer: &dyn ChatCompleter,
    user_msg: &str,
    tool: &str,
    schema: &serde_json::Value,
    decode_hint: &str,
    finish: impl Fn(&T) -> (SummaryStructured, String),
) -> Result<(SummaryStructured, String)>
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
    if let Err(e) = parsed.validate() {
        parsed = call(&format!(
            "{user_msg}\n\n[Reminder: output MUST satisfy the schema and size limits. Previous attempt failed: {e}]"
        ))?;
        parsed.validate().map_err(SummarizeError::InvalidOutput)?;
    }
    Ok(finish(&parsed))
}

/// Validation hook for the generic driver; both impls forward to the
/// inherent `validate`.
pub trait Validatable {
    fn validate(&self) -> std::result::Result<(), String>;
}
impl Validatable for SummaryStructured {
    fn validate(&self) -> std::result::Result<(), String> {
        SummaryStructured::validate(self)
    }
}
impl Validatable for SectionedSummary {
    fn validate(&self) -> std::result::Result<(), String> {
        SectionedSummary::validate(self)
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
    }
    impl Scripted {
        fn new(responses: Vec<Value>) -> Self {
            Self { responses: Mutex::new(responses), calls: Mutex::new(0) }
        }
        fn calls(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
    }
    impl ChatCompleter for Scripted {
        fn complete_json(
            &self,
            _system: &str,
            _user: &str,
            _tool: &str,
            _schema: &serde_json::Value,
        ) -> crate::Result<Value> {
            *self.calls.lock().unwrap() += 1;
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
        let out = drive(&c, "test", "m", &input(Envelope::Classic), 7, "hint").unwrap();
        assert_eq!(c.calls(), 2);
        assert!(out.markdown.contains("**TL;DR.** Fine."));
        assert_eq!(out.generated_at_unix_seconds, 7);
    }

    #[test]
    fn validation_failure_reprompts_once_then_succeeds() {
        // Well-formed but EMPTY output fails validate() → one re-prompt.
        let empty = json!({"tldr":"  ","action_items":[],"decisions":[],"open_questions":[],"key_topics":[]});
        let c = Scripted::new(vec![empty, good_classic()]);
        let out = drive(&c, "test", "m", &input(Envelope::Classic), 0, "hint").unwrap();
        assert_eq!(c.calls(), 2);
        assert!(out.structured.decisions.contains(&"ship it".to_string()));
    }

    #[test]
    fn persistent_validation_failure_errors_after_the_retry() {
        let empty = json!({"tldr":"","action_items":[],"decisions":[],"open_questions":[],"key_topics":[]});
        let c = Scripted::new(vec![empty.clone(), empty]);
        let err = drive(&c, "test", "m", &input(Envelope::Classic), 0, "hint").unwrap_err();
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
        let out = drive(&c, "test", "m", &input(Envelope::Sectioned), 0, "hint").unwrap();
        assert_eq!(c.calls(), 1);
        assert!(out.markdown.contains("**Summary.** Good sync."));
        assert!(out.markdown.contains("## Scope"));
        // Folded into the persisted classic shape: exec→tldr, actions carried.
        assert_eq!(out.structured.tldr, "Good sync.");
        assert_eq!(out.structured.action_items.len(), 1);
        assert!(out.structured.decisions.is_empty());
    }
}
