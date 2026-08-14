//! Prompts: a name + an output envelope + advisory directive text. One store
//! covers summary presets and analysis topics. Built-ins are plain data
//! (const directives).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Envelope {
    Classic,
    Sectioned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub id: String,
    pub name: String,
    pub output: Envelope,
    #[serde(default)]
    pub directive_md: String,
    #[serde(default)]
    pub builtin: bool,
}

pub const DAISY_ID: &str = "builtin:daisy";

/// Minimum length for a Daisy Summarizer directive override. A stored
/// override shorter than this is ignored and the code default wins.
/// Mirrored in the frontend save validation (Settings → Prompts).
pub const DAISY_DIRECTIVE_MIN_CHARS: usize = 40;

pub const ZOOM_ID: &str = "builtin:zoom";
pub const OTTER_ID: &str = "builtin:otter";
pub const PM_ID: &str = "builtin:pm";
pub const CONSULTANT_ID: &str = "builtin:consultant";
pub const TEAMLEAD_ID: &str = "builtin:teamlead";

const DAISY_DIRECTIVE: &str = "Produce Daisy's standard meeting summary. Write the TL;DR as 1-3 tight sentences stating the outcome of the meeting, addressed to the meeting owner as \"you\". List action items each with an owner, and a due date when one was stated. List only decisions actually made (not proposals still in the air), open questions still unresolved at the end, and the key topics discussed. Be concrete — prefer names, dates, and numbers from the meeting over generalities. Do not infer people's emotions or infer sentiment. Leave a section empty rather than padding it.";

const ZOOM_DIRECTIVE: &str = "Produce an executive summary as a short narrative paragraph (2-4 sentences). Then create one section per major topic or chapter of the meeting: each section has a concise heading and a narrative body of flowing prose, NOT bullet points. Prefer narrative over lists. End with a brief set of action items.";

const OTTER_DIRECTIVE: &str = "Begin with a chapter summary: a narrative overview of the meeting, one flowing paragraph per major chapter in chronological order, covering what was discussed and what it concluded. After the chapter summary, produce an outline: one section per topic, each with a concise heading and supporting bullet points that capture the key details discussed. Use bullet points only in the outline, never in the chapter summary. End with action items.";

// Coaching topics: same output contract for all three; the frameworks differ.
const PM_DIRECTIVE: &str = "Analyze how the (self) attendee communicated in this meeting. Produce: a one-paragraph executive assessment, then a \"Strengths\" section (up to 3 bullets) and an \"Improvements\" section (up to 3 bullets), each bullet citing a concrete moment from the transcript. No action items. Do not add timestamps.\n\nCoach for Project Manager voice. Reference frameworks: clear status (RAG), explicit risk articulation, stakeholder alignment, action-item ownership, concise updates, closing loops. Strength signals: names specific risks/blockers with mitigations; assigns owners and dates explicitly; closes loops on prior commitments; drives decisions with concrete options; sets explicit expectations of what done looks like. Improvement signals: hedging without specificity; long context dumps without naming the decision needed; action items without an owner or by-when; skipped follow-ups; talking around a blocker without naming it.";

const CONSULTANT_DIRECTIVE: &str = "Analyze how the (self) attendee communicated in this meeting. Produce: a one-paragraph executive assessment, then a \"Strengths\" section (up to 3 bullets) and an \"Improvements\" section (up to 3 bullets), each bullet citing a concrete moment from the transcript. No action items. Do not add timestamps.\n\nCoach for Management Consultant voice. Reference frameworks: Pyramid Principle (recommendation-first), MECE structure, hypothesis-driven framing, executive presence, structured listening. Strength signals: leads with the recommendation then evidence; frames structure explicitly (\"three considerations\"); pushes back diplomatically with data; synthesizes a wandering conversation into named buckets; names the question before answering it. Improvement signals: burying the recommendation under context; overlapping or incomplete categories; stating opinions as facts without an anchor; talking past unstated concerns instead of surfacing them; filler that signals uncertainty when the speaker is decisive.";

const TEAMLEAD_DIRECTIVE: &str = "Analyze how the (self) attendee communicated in this meeting. Produce: a one-paragraph executive assessment, then a \"Strengths\" section (up to 3 bullets) and an \"Improvements\" section (up to 3 bullets), each bullet citing a concrete moment from the transcript. No action items. Do not add timestamps.\n\nCoach for Team Lead voice. Reference frameworks: coaching questions, psychological safety, clear delegation, SBI (Situation-Behavior-Impact) feedback, recognition. Strength signals: asks open questions before prescribing (\"What have you tried?\"); acknowledges specific contributions by name; delegates with both an owner AND an outcome; gives feedback in SBI form; invites dissent before deciding. Improvement signals: jumping to solutions before understanding the problem; vague praise or criticism with no specifics; delegation without success criteria; cutting people off mid-thought; leaving disagreement unaddressed.";

/// Built-in prompts, seeded into the prompts file on first run.
pub fn seed_prompts() -> Vec<Prompt> {
    vec![
        Prompt {
            id: DAISY_ID.into(),
            name: "Daisy Summarizer".into(),
            output: Envelope::Classic,
            directive_md: DAISY_DIRECTIVE.into(),
            builtin: true,
        },
        Prompt {
            id: ZOOM_ID.into(),
            name: "Zoom-style Meeting Notes".into(),
            output: Envelope::Sectioned,
            directive_md: ZOOM_DIRECTIVE.into(),
            builtin: true,
        },
        Prompt {
            id: OTTER_ID.into(),
            name: "Otter-style Meeting Notes".into(),
            output: Envelope::Sectioned,
            directive_md: OTTER_DIRECTIVE.into(),
            builtin: true,
        },
        Prompt {
            id: PM_ID.into(),
            name: "Project Manager coaching".into(),
            output: Envelope::Sectioned,
            directive_md: PM_DIRECTIVE.into(),
            builtin: true,
        },
        Prompt {
            id: CONSULTANT_ID.into(),
            name: "Management Consultant coaching".into(),
            output: Envelope::Sectioned,
            directive_md: CONSULTANT_DIRECTIVE.into(),
            builtin: true,
        },
        Prompt {
            id: TEAMLEAD_ID.into(),
            name: "Team Lead coaching".into(),
            output: Envelope::Sectioned,
            directive_md: TEAMLEAD_DIRECTIVE.into(),
            builtin: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seeds_six_builtins_with_stable_ids() {
        let s = seed_prompts();
        assert_eq!(s.len(), 6);
        assert!(s.iter().all(|x| x.builtin));
        assert_eq!(s[0].id, DAISY_ID);
        assert_eq!(s[0].output, Envelope::Classic);
        assert_eq!(s[0].name, "Daisy Summarizer");
        assert!(!s[0].directive_md.is_empty(), "summarizer ships its default directive");
        assert!(s[1..]
            .iter()
            .all(|x| x.output == Envelope::Sectioned && !x.directive_md.is_empty()));
    }
    #[test]
    fn envelope_serializes_lowercase() {
        let j = serde_json::to_string(&Envelope::Sectioned).unwrap();
        assert_eq!(j, "\"sectioned\"");
    }
}
