//! source_inputs_hash — invalidates a cached summary when any input changes.
use crate::model::{SummaryInput, TagPromptRef};
use sha2::{Digest, Sha256};

pub fn source_inputs_hash(input: &SummaryInput, provider: &str, model: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"transcript:");
    h.update(input.transcript_md.as_bytes());
    h.update(b"\0");
    h.update(b"notes:");
    h.update(input.user_notes_md.as_bytes());
    h.update(b"\0");
    h.update(b"tags:");
    let mut prompts: Vec<&TagPromptRef> = input.tag_prompts.iter().collect();
    prompts.sort_by_key(|p| p.name);
    for p in prompts {
        h.update(p.name.as_bytes());
        h.update(b"=");
        h.update(p.prompt_md.as_bytes());
        h.update(b"#");
        h.update(p.terms.as_bytes());
        h.update(b";");
    }
    h.update(b"\0style_envelope:");
    h.update(match input.style_envelope {
        crate::prompts::Envelope::Classic => b"classic".as_slice(),
        crate::prompts::Envelope::Sectioned => b"sectioned".as_slice(),
    });
    h.update(b"\0style_directive:");
    h.update(input.style_directive.as_bytes());
    h.update(b"\0provider:");
    h.update(provider.as_bytes());
    h.update(b"\0model:");
    h.update(model.as_bytes());
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SummaryInput, TagPromptRef};
    fn i<'a>(t: &'a str, n: &'a str) -> SummaryInput<'a> {
        SummaryInput {
            session_id: "s",
            title: None,
            attendees: &[],
            user_notes_md: n,
            transcript_md: t,
            tag_prompts: &[],
            style_envelope: crate::prompts::Envelope::Classic,
            style_directive: "",
        }
    }
    #[test]
    fn same_inputs_same_hash() {
        assert_eq!(
            source_inputs_hash(&i("a", "b"), "p", "m"),
            source_inputs_hash(&i("a", "b"), "p", "m")
        );
    }
    #[test]
    fn whitespace_diff_changes_hash() {
        assert_ne!(
            source_inputs_hash(&i("a", "b"), "p", "m"),
            source_inputs_hash(&i("a ", "b"), "p", "m")
        );
    }
    #[test]
    fn model_change_changes_hash() {
        assert_ne!(
            source_inputs_hash(&i("a", "b"), "p", "m1"),
            source_inputs_hash(&i("a", "b"), "p", "m2")
        );
    }
    #[test]
    fn style_directive_changes_the_hash() {
        let mut a = i("tx", "nt");
        a.style_directive = "narrative";
        let mut b = i("tx", "nt");
        b.style_directive = "bullets";
        assert_ne!(source_inputs_hash(&a, "p", "m"), source_inputs_hash(&b, "p", "m"));
    }
    #[test]
    fn envelope_changes_the_hash() {
        let mut a = i("tx", "nt");
        a.style_envelope = crate::prompts::Envelope::Classic;
        let mut b = i("tx", "nt");
        b.style_envelope = crate::prompts::Envelope::Sectioned;
        assert_ne!(source_inputs_hash(&a, "p", "m"), source_inputs_hash(&b, "p", "m"));
    }
    #[test]
    fn vocab_terms_change_the_hash() {
        let a = [TagPromptRef { name: "T", prompt_md: "p", terms: "Zephyr" }];
        let b = [TagPromptRef { name: "T", prompt_md: "p", terms: "Aurora" }];
        let mut ia = i("tx", "nt");
        ia.tag_prompts = &a;
        let mut ib = i("tx", "nt");
        ib.tag_prompts = &b;
        assert_ne!(
            source_inputs_hash(&ia, "prov", "mdl"),
            source_inputs_hash(&ib, "prov", "mdl")
        );
    }
}
