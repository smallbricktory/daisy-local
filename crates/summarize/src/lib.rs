//! Meeting-summary engine: Summarizer trait + Anthropic / OpenAI-compat impls,
//! layered prompt-injection guardrails, deterministic structured→markdown
//! rendering.
pub mod anthropic;
pub mod chapters;
pub mod chat;
pub mod defaults;
pub mod engine;
pub mod error;
pub mod gateway;
pub mod hash;
pub mod model;
pub mod openai_compat;
pub mod polish;
pub mod prompt;
pub mod render;
pub mod state_provider;
pub mod prompts;
pub use error::{Result, SummarizeError};
pub use model::{
    ActionItem, AttendeeRef, Section, SectionedSummary, SessionSummary, SpeakerRole, SummaryInput,
    SummaryStructured, TagPromptRef,
};
pub use prompts::{seed_prompts, Envelope, Prompt};

pub trait Summarizer: Send + Sync {
    fn name(&self) -> &'static str;
    fn model(&self) -> &str;
    fn summarize(&self, input: &SummaryInput, now_unix: i64) -> Result<SessionSummary>;
}
