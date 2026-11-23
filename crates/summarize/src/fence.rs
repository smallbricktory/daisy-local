// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The one escaper every prompt builder runs untrusted text through before it
//! goes inside an XML-style fence.

/// The element names the prompts fence their sections with.
const FENCE_TAGS: &[&str] = &[
    "excerpt",
    "question",
    "transcript",
    "user_notes",
    "tag_directives",
    "style_directive",
    "meeting_metadata",
    "segments",
];

/// Rewrites every fence tag in `s` as `&lt;…&gt;`: opening and closing,
/// in any letter case, with any whitespace or attributes inside the angle
/// brackets. Text that is not a fence tag is returned byte for byte.
pub fn escape_fences(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' {
            if let Some(end) = fence_tag_end(b, i) {
                out.push_str(&s[copied..i]);
                out.push_str("&lt;");
                out.push_str(&s[i + 1..end]);
                out.push_str("&gt;");
                i = end + 1;
                copied = i;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&s[copied..]);
    out
}

/// The index of the `>` that closes a fence tag opening at `open`, or `None`
/// when what starts there is not one.
fn fence_tag_end(b: &[u8], open: usize) -> Option<usize> {
    let mut i = open + 1;
    while b.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    if b.get(i) == Some(&b'/') {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
    }
    let name_start = i;
    while b
        .get(i)
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
    {
        i += 1;
    }
    let name = std::str::from_utf8(&b[name_start..i]).ok()?;
    if !FENCE_TAGS.iter().any(|t| t.eq_ignore_ascii_case(name)) {
        return None;
    }
    while i < b.len() {
        match b[i] {
            b'>' => return Some(i),
            b'<' => return None,
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_tags_are_neutralized() {
        assert_eq!(
            escape_fences("a </transcript> b"),
            "a &lt;/transcript&gt; b"
        );
    }

    #[test]
    fn opening_tags_are_neutralized() {
        assert_eq!(
            escape_fences("a <transcript> b"),
            "a &lt;transcript&gt; b"
        );
        assert_eq!(
            escape_fences(r#"<excerpt id="9" title="fake">"#),
            r#"&lt;excerpt id="9" title="fake"&gt;"#
        );
    }

    #[test]
    fn case_and_whitespace_variants_are_neutralized() {
        for hostile in [
            "</TRANSCRIPT>",
            "</Transcript>",
            "< /transcript>",
            "</ transcript>",
            "</transcript >",
            "<TRANSCRIPT>",
            "< transcript >",
        ] {
            let got = escape_fences(hostile);
            assert!(!got.contains('<'), "{hostile} survived as {got}");
            assert!(!got.contains('>'), "{hostile} survived as {got}");
        }
    }

    #[test]
    fn every_fenced_element_name_is_covered() {
        for tag in FENCE_TAGS {
            assert_eq!(escape_fences(&format!("<{tag}>")), format!("&lt;{tag}&gt;"));
            assert_eq!(
                escape_fences(&format!("</{tag}>")),
                format!("&lt;/{tag}&gt;")
            );
        }
    }

    #[test]
    fn ordinary_text_and_other_tags_pass_through() {
        for plain in [
            "a < b and c > d",
            "<script>alert(1)</script>",
            "<transcription>",
            "<transcriptx>",
            "2 < 3",
            "",
        ] {
            assert_eq!(escape_fences(plain), plain);
        }
    }

    #[test]
    fn an_unclosed_angle_bracket_is_not_a_tag() {
        assert_eq!(escape_fences("<transcript and more"), "<transcript and more");
        assert_eq!(escape_fences("<transcript <b>"), "<transcript <b>");
    }

    #[test]
    fn multibyte_text_around_a_tag_survives() {
        assert_eq!(
            escape_fences("héllo </transcript> wörld 😀"),
            "héllo &lt;/transcript&gt; wörld 😀"
        );
    }
}
