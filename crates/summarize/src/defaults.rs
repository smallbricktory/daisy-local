//! Single source of truth for summary-provider defaults. Used by:
//!   - the Tauri `build_summarizer` / `polish_impl` credential resolution
//!   - the `source_inputs_hash` cache-skip check, which MUST agree with the
//!     summarizer's actual model string.

#[derive(Debug, Clone, Copy)]
pub struct SummaryProviderDefaults {
    pub model: &'static str,
    pub base_url: &'static str,
    /// Environment variable fallback for the API key. Empty = none.
    pub env_key: &'static str,
}

pub const ANTHROPIC: SummaryProviderDefaults = SummaryProviderDefaults {
    model: "claude-sonnet-4-6",
    base_url: "https://api.anthropic.com/v1",
    env_key: "ANTHROPIC_API_KEY",
};
pub const OPENAI: SummaryProviderDefaults = SummaryProviderDefaults {
    model: "gpt-4o-mini",
    base_url: "https://api.openai.com/v1",
    env_key: "OPENAI_API_KEY",
};
pub const LM_STUDIO: SummaryProviderDefaults = SummaryProviderDefaults {
    model: "local-model",
    base_url: "http://localhost:1234/v1",
    env_key: "OPENAI_API_KEY",
};
pub const OLLAMA: SummaryProviderDefaults = SummaryProviderDefaults {
    model: "llama3.1",
    base_url: "http://localhost:11434/v1",
    env_key: "",
};
pub const GROQ: SummaryProviderDefaults = SummaryProviderDefaults {
    model: "llama-3.3-70b-versatile",
    base_url: "https://api.groq.com/openai/v1",
    env_key: "GROQ_API_KEY",
};

pub fn for_provider(name: &str) -> Option<SummaryProviderDefaults> {
    match name {
        "anthropic" => Some(ANTHROPIC),
        "openai" => Some(OPENAI),
        "lm_studio" => Some(LM_STUDIO),
        "ollama" => Some(OLLAMA),
        "groq" => Some(GROQ),
        _ => None,
    }
}
