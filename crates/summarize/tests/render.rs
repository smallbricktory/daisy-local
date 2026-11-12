// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use summarize::model::{ActionItem, SummaryStructured};
use summarize::render::render_markdown;

#[test]
fn renders_sections_and_owners() {
    let s = SummaryStructured {
        tldr: "Did the thing.".into(),
        action_items: vec![
            ActionItem {
                task: "Email Bob".into(),
                owner: Some("Rowan".into()),
                due: Some("Friday".into()),
            },
            ActionItem {
                task: "File ticket".into(),
                owner: None,
                due: None,
            },
        ],
        decisions: vec!["Ship it".into()],
        open_questions: vec!["When?".into()],
        key_topics: vec!["Onboarding".into(), "SSO".into()],
    };
    let md = render_markdown(&s, Some("Sync"));
    assert!(md.starts_with("# Sync"));
    assert!(md.contains("**TL;DR.** Did the thing."));
    assert!(md.contains("- Email Bob — _Rowan_ (due Friday)"));
    assert!(md.contains("- File ticket\n"));
    assert!(md.contains("## Decisions"));
    assert!(md.contains("## Key topics"));
}

#[test]
fn empty_sections_omitted() {
    let s = SummaryStructured {
        tldr: "x".into(),
        action_items: vec![],
        decisions: vec![],
        open_questions: vec![],
        key_topics: vec![],
    };
    let md = render_markdown(&s, None);
    assert!(!md.contains("## Action items"));
    assert!(md.contains("**TL;DR.** x"));
}
