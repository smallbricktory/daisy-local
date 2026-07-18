//! Anthropic meeting summarizer. The wire transport (forced tool call + 429
//! retry) lives in `chat::AnthropicChat`; this module owns the summary schema,
//! prompt, and the re-prompt-on-bad-schema loop.
use crate::chat::AnthropicChat;
use crate::error::Result;
use crate::model::{SessionSummary, SummaryInput};
use crate::Summarizer;
use serde_json::json;

pub struct AnthropicSummarizer {
    completer: AnthropicChat,
    model: String,
}

impl AnthropicSummarizer {
    pub fn with_config(api_key: String, base_url: String, model: String) -> Self {
        Self {
            completer: AnthropicChat::new(api_key, base_url, model.clone()),
            model,
        }
    }
}

/// Structured-summary JSON schema. Shared shape for both transports.
pub(crate) fn summary_schema() -> serde_json::Value {
    json!({"type":"object","properties":{
        "tldr":{"type":"string"},
        "action_items":{"type":"array","items":{"type":"object","properties":{"text":{"type":"string"},"owner":{"type":["string","null"]},"due":{"type":["string","null"]}},"required":["text"]}},
        "decisions":{"type":"array","items":{"type":"string"}},
        "open_questions":{"type":"array","items":{"type":"string"}},
        "key_topics":{"type":"array","items":{"type":"string"}}
    },"required":["tldr","action_items","decisions","open_questions","key_topics"]})
}

/// Sectioned-summary JSON schema (exec summary + per-topic sections + actions).
pub(crate) fn sectioned_schema() -> serde_json::Value {
    json!({"type":"object","properties":{
        "exec_summary":{"type":"string"},
        "sections":{"type":"array","items":{"type":"object","properties":{
            "heading":{"type":"string"},
            "bullets":{"type":"boolean"},
            "content":{"type":"array","items":{"type":"string"}}
        },"required":["heading","bullets","content"]}},
        "action_items":{"type":"array","items":{"type":"object","properties":{"text":{"type":"string"},"owner":{"type":["string","null"]},"due":{"type":["string","null"]}},"required":["text"]}}
    },"required":["exec_summary","sections","action_items"]})
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    #[test]
    fn sectioned_schema_has_required_fields() {
        let s = sectioned_schema();
        let req = s["required"].as_array().unwrap();
        assert!(req.iter().any(|v| v == "exec_summary"));
        assert!(req.iter().any(|v| v == "sections"));
        assert!(req.iter().any(|v| v == "action_items"));
        let sec = &s["properties"]["sections"]["items"]["properties"];
        assert!(sec.get("heading").is_some());
        assert!(sec.get("bullets").is_some());
        assert!(sec.get("content").is_some());
    }
}

impl Summarizer for AnthropicSummarizer {
    fn name(&self) -> &'static str {
        "anthropic"
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn summarize(&self, input: &SummaryInput, now_unix: i64) -> Result<SessionSummary> {
        crate::engine::drive(
            &self.completer,
            self.name(),
            &self.model,
            input,
            now_unix,
            "Emit ONLY the structured object the tool expects — every list field must be a JSON array, with no XML/HTML tags inside the values.",
        )
    }
}
