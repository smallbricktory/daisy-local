// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! OpenAI-compatible meeting summarizer (OpenAI, LM Studio, Ollama, Groq). The
//! wire transport (json_object chat/completions + 429 retry) lives in
//! `chat::OpenAiChat`; this module owns the summary schema, prompt, and the
//! re-prompt-on-bad-schema loop.
use crate::budget::ContextBudget;
use crate::chat::{ChatCompleter, OpenAiChat};
use crate::error::Result;
use crate::model::{SessionSummary, SummaryInput};
use crate::state_provider::ResponseFormat;
use crate::Summarizer;

pub struct OpenAICompatSummarizer {
    completer: Box<dyn ChatCompleter>,
    name: &'static str,
    model: String,
    budget: Option<ContextBudget>,
}

impl OpenAICompatSummarizer {
    /// `budget` is the context length of a local endpoint (LM Studio,
    /// Ollama); a meeting longer than it is summarized in windows and the
    /// reply is capped at the budget's output reserve. Cloud providers pass
    /// `None`.
    pub fn new(
        name: &'static str,
        base_url: String,
        api_key: Option<String>,
        model: String,
        budget: Option<ContextBudget>,
    ) -> Self {
        let chat = OpenAiChat::new(api_key, base_url, model.clone(), ResponseFormat::for_provider(name));
        let chat = match budget {
            Some(b) => chat.with_max_tokens(b.output_reserve()),
            None => chat,
        };
        Self { completer: Box::new(chat), name, model, budget }
    }

    /// Build a summarizer over an arbitrary completer (e.g. the Daisy Cloud
    /// gateway). `model` is only a display/hash label here — the transport owns
    /// the real routing.
    pub fn with_completer(
        name: &'static str,
        model: String,
        completer: Box<dyn ChatCompleter>,
    ) -> Self {
        Self { completer, name, model, budget: None }
    }

    /// A local completer's window: the meeting is summarized in windows that
    /// fit `budget`, the same path LM Studio and Ollama take.
    pub fn with_budget(mut self, budget: ContextBudget) -> Self {
        self.budget = Some(budget);
        self
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
            self.budget.as_ref(),
            "Respond with a single JSON object only — every list field must be a JSON array, with no XML/HTML tags inside the values.",
        )
    }
}
