// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Daisy's own summarizer against a local LM server, end to end.
//!
//! Everything else about the local arms is tested against a mock, which proves
//! the request we build and the reply we parse but not that a real local model
//! answers in the shape the parser needs. Ignored by default; point it at a
//! running server.
//!
//!   DAISY_LOCAL_LM_URL=http://localhost:1234/v1 DAISY_LOCAL_LM_MODEL=<id> \
//!     cargo test -p summarize --test local_lm_smoke -- --ignored --nocapture
use summarize::model::SummaryInput;
use summarize::openai_compat::OpenAICompatSummarizer;
use summarize::prompts::Envelope;
use summarize::Summarizer;

const TRANSCRIPT: &str = "\
[00:00:00] Rowan: the harbour survey came back two soundings short.
[00:00:12] Zoe: I can re-run the north channel on Thursday.
[00:00:20] Rowan: do that, and send the corrected chart to the pilotage office.
[00:00:31] Zoe: agreed. I will have it to them by Friday.
";

#[test]
#[ignore]
fn a_local_model_answers_in_the_shape_the_parser_needs() {
    let url = std::env::var("DAISY_LOCAL_LM_URL")
        .unwrap_or_else(|_| "http://localhost:1234/v1".into());
    let model = std::env::var("DAISY_LOCAL_LM_MODEL").expect("DAISY_LOCAL_LM_MODEL");
    let s = OpenAICompatSummarizer::new("lm_studio", url.clone(), None, model.clone(), None);

    let input = SummaryInput {
        session_id: "daisy-1788200000",
        title: Some("Harbour survey"),
        attendees: &[],
        user_notes_md: "",
        transcript_md: TRANSCRIPT,
        tag_prompts: &[],
        style_envelope: Envelope::Classic,
        style_directive: "",
    };
    let t = std::time::Instant::now();
    let out = s.summarize(&input, 1_788_200_400).expect("a local model must answer");
    eprintln!(
        "local_lm_smoke: {url} {model} in {:.1}s\n  summary: {}\n  actions: {:?}",
        t.elapsed().as_secs_f32(),
        out.markdown,
        out.structured.action_items
    );
    assert!(!out.markdown.trim().is_empty(), "an empty summary is not an answer");
}
