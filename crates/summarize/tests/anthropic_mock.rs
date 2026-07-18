use summarize::anthropic::AnthropicSummarizer;
use summarize::model::SummaryInput;
use summarize::Summarizer;

#[test]
fn anthropic_parses_tool_use_response() {
    let mut server = mockito::Server::new();
    let body = serde_json::json!({"content":[
        {"type":"text","text":"ignored"},
        {"type":"tool_use","name":"emit_summary","input":{"tldr":"Did stuff.","action_items":[{"text":"Email Bob","owner":"Danny"}],"decisions":["Ship"],"open_questions":[],"key_topics":["Oracle"]}}
    ]}).to_string();
    let m = server
        .mock("POST", "/messages")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body)
        .create();
    let s = AnthropicSummarizer::with_config(
        "test-key".into(),
        server.url(),
        "claude-haiku-4-5-20251001".into(),
    );
    let input = SummaryInput {
        session_id: "s",
        title: Some("T"),
        attendees: &[],
        user_notes_md: "",
        transcript_md: "Bob said hi",
        tag_prompts: &[],
        style_envelope: summarize::Envelope::Classic,
        style_directive: "",
    };
    let out = s.summarize(&input, 12345).unwrap();
    m.assert();
    assert_eq!(out.structured.tldr, "Did stuff.");
    assert_eq!(out.structured.action_items[0].owner.as_deref(), Some("Danny"));
    assert!(out.markdown.contains("**TL;DR.** Did stuff."));
    assert_eq!(out.provider, "anthropic");
    assert_eq!(out.generated_at_unix_seconds, 12345);
    assert!(!out.source_inputs_hash.is_empty());
}

#[test]
fn anthropic_surfaces_bad_status() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("POST", "/messages")
        .with_status(401)
        .with_body("nope")
        .create();
    let s = AnthropicSummarizer::with_config("bad".into(), server.url(), "m".into());
    let input = SummaryInput {
        session_id: "s",
        title: None,
        attendees: &[],
        user_notes_md: "",
        transcript_md: "x",
        tag_prompts: &[],
        style_envelope: summarize::Envelope::Classic,
        style_directive: "",
    };
    let err = s.summarize(&input, 0).unwrap_err();
    m.assert();
    assert!(matches!(
        err,
        summarize::SummarizeError::BadStatus { status: 401, .. }
    ));
}
