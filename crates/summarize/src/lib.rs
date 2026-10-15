// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Meeting-summary engine: Summarizer trait + Anthropic / OpenAI-compat impls,
//! layered prompt-injection guardrails, deterministic structured→markdown
//! rendering.
pub mod anthropic;
pub mod budget;
pub mod chapters;
pub mod chat;
pub mod defaults;
pub mod engine;
pub mod error;
pub mod fence;
pub mod fold;
pub mod gateway;
pub mod hash;
pub mod http;
pub mod local;
pub mod model;
pub mod openai_compat;
pub mod polish;
pub mod prompt;
pub mod render;
pub mod schemas;
pub mod state_provider;
pub mod window;
pub mod prompts;
pub use budget::ContextBudget;
pub use error::{Result, SummarizeError};
pub use model::{
    ActionItem, AttendeeRef, ProducedBy, SessionSummary, SkippedWindow, SpeakerRole, SummaryChapter,
    SummaryInput, SummaryStructured, TagPromptRef,
};
pub use prompts::{seed_prompts, Envelope, Prompt};

pub trait Summarizer: Send + Sync {
    fn name(&self) -> &'static str;
    fn model(&self) -> &str;
    fn summarize(&self, input: &SummaryInput, now_unix: i64) -> Result<SessionSummary>;
}
