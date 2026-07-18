//! Deterministic markdown rendering of a summary. This is the trust boundary:
//! the structured object the model emitted is the source of truth; summary.md is
//! produced here, never by the model. Both envelopes (classic + sectioned)
//! render through this module.
use crate::model::{ActionItem, SectionedSummary, SessionSummary, SummaryStructured};
use std::fmt::Write;

pub fn render_markdown(s: &SummaryStructured, title: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(t) = title {
        let _ = writeln!(out, "# {t}\n");
    }
    let _ = writeln!(out, "**TL;DR.** {}\n", s.tldr.trim());
    render_action_items(&mut out, &s.action_items);
    if !s.decisions.is_empty() {
        let _ = writeln!(out, "## Decisions\n");
        for d in &s.decisions {
            let _ = writeln!(out, "- {}", d.trim());
        }
        out.push('\n');
    }
    if !s.open_questions.is_empty() {
        let _ = writeln!(out, "## Open questions\n");
        for q in &s.open_questions {
            let _ = writeln!(out, "- {}", q.trim());
        }
        out.push('\n');
    }
    if !s.key_topics.is_empty() {
        let _ = writeln!(out, "## Key topics\n");
        for k in &s.key_topics {
            let _ = writeln!(out, "- {}", k.trim());
        }
        out.push('\n');
    }
    out
}

/// Render a sectioned-style summary: exec summary, then a heading + body per
/// section (bullets or prose), then action items.
pub fn render_sectioned(s: &SectionedSummary, title: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(t) = title {
        let _ = writeln!(out, "# {t}\n");
    }
    let _ = writeln!(out, "**Summary.** {}\n", s.exec_summary.trim());
    for sec in &s.sections {
        if sec.heading.trim().is_empty() {
            continue;
        }
        let _ = writeln!(out, "## {}\n", sec.heading.trim());
        for item in &sec.content {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            if sec.bullets {
                let _ = writeln!(out, "- {item}");
            } else {
                let _ = writeln!(out, "{item}\n");
            }
        }
        out.push('\n');
    }
    render_action_items(&mut out, &s.action_items);
    out
}

/// Fold a transient sectioned summary into the persisted classic shape: the
/// exec summary becomes the TL;DR and action items carry over. The other
/// classic lists stay empty; sections live only in the rendered markdown.
pub fn fold_sectioned(s: &SectionedSummary) -> SummaryStructured {
    SummaryStructured {
        tldr: s.exec_summary.clone(),
        action_items: s.action_items.clone(),
        decisions: vec![],
        open_questions: vec![],
        key_topics: vec![],
    }
}

/// Shared action-items block (used by both renderers).
fn render_action_items(out: &mut String, items: &[ActionItem]) {
    if items.is_empty() {
        return;
    }
    let _ = writeln!(out, "## Action items\n");
    for a in items {
        let mut line = format!("- {}", a.text.trim());
        if let Some(o) = &a.owner {
            if !o.trim().is_empty() {
                line.push_str(&format!(" — _{}_", o.trim()));
            }
        }
        if let Some(d) = &a.due {
            if !d.trim().is_empty() {
                line.push_str(&format!(" (due {})", d.trim()));
            }
        }
        let _ = writeln!(out, "{line}");
    }
    out.push('\n');
}

#[allow(clippy::too_many_arguments)]
pub fn assemble_summary(
    session_id: &str,
    provider: &str,
    model: &str,
    now_unix: i64,
    hash: String,
    structured: SummaryStructured,
    markdown: String,
) -> SessionSummary {
    SessionSummary {
        schema_version: SessionSummary::SCHEMA,
        session_id: session_id.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        generated_at_unix_seconds: now_unix,
        source_inputs_hash: hash,
        structured,
        markdown,
        user_edited: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ActionItem, Section, SectionedSummary};

    #[test]
    fn renders_prose_and_bullet_sections() {
        let s = SectionedSummary {
            exec_summary: "We aligned on pricing.".into(),
            sections: vec![
                Section {
                    heading: "Pricing".into(),
                    bullets: true,
                    content: vec!["per-seat".into(), "grandfather legacy".into()],
                },
                Section {
                    heading: "Background".into(),
                    bullets: false,
                    content: vec!["The team debated tiers.".into()],
                },
            ],
            action_items: vec![ActionItem {
                text: "draft FAQ".into(),
                owner: Some("you".into()),
                due: Some("Fri".into()),
            }],
        };
        let md = render_sectioned(&s, Some("Q3 Planning"));
        assert!(md.contains("# Q3 Planning"));
        assert!(md.contains("**Summary.** We aligned on pricing."));
        assert!(md.contains("## Pricing\n"));
        assert!(md.contains("- per-seat\n"));
        assert!(md.contains("## Background\n"));
        assert!(md.contains("The team debated tiers."));
        assert!(md.contains("## Action items"));
        assert!(md.contains("- draft FAQ — _you_ (due Fri)"));
    }

    #[test]
    fn skips_empty_headings_and_blank_items() {
        let s = SectionedSummary {
            exec_summary: "ok".into(),
            sections: vec![
                Section { heading: "  ".into(), bullets: true, content: vec!["orphan".into()] },
                Section { heading: "Real".into(), bullets: true, content: vec!["".into(), "  ".into(), "kept".into()] },
            ],
            action_items: vec![],
        };
        let md = render_sectioned(&s, None);
        assert!(!md.contains("orphan"), "section with blank heading skipped entirely");
        assert!(md.contains("## Real"));
        assert_eq!(md.matches("- ").count(), 1, "blank items dropped");
        assert!(md.contains("- kept"));
        assert!(!md.contains("## Action items"), "no action items -> no block");
    }

    #[test]
    fn folds_sectioned_into_structured() {
        let s = SectionedSummary {
            exec_summary: "exec text".into(),
            sections: vec![],
            action_items: vec![ActionItem {
                text: "do x".into(),
                owner: None,
                due: None,
            }],
        };
        let st = fold_sectioned(&s);
        assert_eq!(st.tldr, "exec text");
        assert_eq!(st.action_items.len(), 1);
        assert!(st.decisions.is_empty() && st.open_questions.is_empty() && st.key_topics.is_empty());
    }
}
