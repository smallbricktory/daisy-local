//! Transport classification for the generic [`crate::chat::ChatCompleter`].
//! Callers map their provider onto one of these two transports.

/// Which wire protocol a chat provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatProvider {
    /// Anthropic Messages API (forced tool-use for structured output).
    Anthropic,
    /// OpenAI-compatible `chat/completions` (OpenAI, Groq, LM Studio, Ollama).
    OpenAiCompat,
}

/// `response_format` mode for OpenAI-compatible `chat/completions`.
/// OpenAI/Groq/Ollama accept `{"type":"json_object"}`; LM Studio rejects it
/// and takes the structured `json_schema` envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseFormat {
    /// `{"type":"json_object"}`.
    JsonObject,
    /// `{"type":"json_schema", "json_schema":{...}}` — required by LM Studio;
    /// grammar-constrains output on local engines.
    JsonSchema,
}

impl ResponseFormat {
    /// Pick the `response_format` mode for an OpenAI-compatible provider by
    /// its canonical wire name (`ProviderId::as_str()` / the summarizer
    /// `name`). LM Studio gets `json_schema`; every other provider gets
    /// `json_object`.
    pub fn for_provider(name: &str) -> Self {
        match name {
            "lm_studio" => ResponseFormat::JsonSchema,
            _ => ResponseFormat::JsonObject,
        }
    }
}
