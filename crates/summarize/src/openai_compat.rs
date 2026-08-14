//! OpenAI-compatible meeting summarizer (OpenAI, LM Studio, Ollama, Groq). The
//! wire transport (json_object chat/completions + 429 retry) lives in
//! `chat::OpenAiChat`; this module owns the summary schema, prompt, and the
//! re-prompt-on-bad-schema loop.
use crate::chat::{ChatCompleter, OpenAiChat};
use crate::error::Result;
use crate::model::{SessionSummary, SummaryInput};
use crate::state_provider::ResponseFormat;
use crate::Summarizer;

pub struct OpenAICompatSummarizer {
    completer: Box<dyn ChatCompleter>,
    name: &'static str,
    model: String,
}

impl OpenAICompatSummarizer {
    pub fn new(
        name: &'static str,
        base_url: String,
        api_key: Option<String>,
        model: String,
    ) -> Self {
        Self {
            completer: Box::new(OpenAiChat::new(
                api_key,
                base_url,
                model.clone(),
                ResponseFormat::for_provider(name),
            )),
            name,
            model,
        }
    }

    /// Build a summarizer over an arbitrary completer (e.g. the Daisy Cloud
    /// gateway). `model` is only a display/hash label here — the transport owns
    /// the real routing.
    pub fn with_completer(
        name: &'static str,
        model: String,
        completer: Box<dyn ChatCompleter>,
    ) -> Self {
        Self { completer, name, model }
    }
}

impl Summarizer for OpenAICompatSummarizer {
    fn name(&self) -> &'static str {
        self.name
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn summarize(&self, input: &SummaryInput, now_unix: i64) -> Result<SessionSummary> {
        crate::engine::drive(
            self.completer.as_ref(),
            self.name(),
            &self.model,
            input,
            now_unix,
            "Respond with a single JSON object only — every list field must be a JSON array, with no XML/HTML tags inside the values.",
        )
    }
}
