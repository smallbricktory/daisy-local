// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The drift detector: every fixture that has crossed the seam, decoded the
//! way the phone expects the desktop to decode it. Byte-identical copies of
//! ~/Sync/daisy-ios/sync-seam/fixtures/ and ios/DaisyTests/Fixtures/sync-seam/.
use summarize::SessionSummary;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/sync-seam/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[test]
fn v2_decodes_with_every_field() {
    let s = SessionSummary::decode(&fixture("summary.v2.json")).unwrap();
    assert_eq!(s.v, 2);
    assert_eq!(s.session_id, "daisy-1700000000");
    assert_eq!(s.produced_by.provider, "lmstudio");
    assert!(s.structured.tldr.starts_with("The team agreed"));
    assert_eq!(s.structured.action_items.len(), 2);
    assert_eq!(s.structured.action_items[0].task, "Merge the parser branch");
    assert_eq!(s.structured.action_items[0].owner.as_deref(), Some("Sam"));
    assert_eq!(s.structured.action_items[0].due.as_deref(), Some("Thursday"));
    assert_eq!(s.structured.decisions.len(), 2);
    assert_eq!(s.structured.open_questions.len(), 1);
    assert_eq!(s.chapters.len(), 2);
    assert_eq!(s.chapters[0].start_ms, Some(0));
    assert_eq!(s.chapters[1].start_ms, Some(754_000));
    assert_eq!(s.skipped.len(), 1);
    assert_eq!(s.turn_count, 12);
}

#[test]
fn v2_future_decodes_to_the_defaults() {
    let s = SessionSummary::decode(&fixture("summary.v2-future.json")).unwrap();
    assert_eq!(s.structured.action_items.len(), 2);
    assert_eq!(s.structured.action_items[1].task, "Book the room");
    assert_eq!(s.chapters.len(), 2);
    assert_eq!(s.chapters[0].title, "Opening");
    assert_eq!(s.chapters[0].start_ms, None);
    assert_eq!(s.chapters[1].start_ms, Some(120_000));
    assert!(s.structured.decisions.is_empty());
    assert_eq!(s.produced_by.provider, "");
    assert_eq!(s.markdown, "");
    assert!(!s.user_edited);
    assert_eq!(s.turn_count, 0);
    assert!(s.skipped.is_empty());
}

#[test]
fn the_desktops_own_v1_file_converts() {
    let s = SessionSummary::decode(&fixture("summary.desktop-v1.json")).unwrap();
    assert_eq!(s.v, 2);
    assert_eq!(s.structured.action_items.len(), 2);
    assert!(s.source_inputs_hash.is_some());
    assert_eq!(s.produced_by.device, "");
    assert!(!s.produced_by.provider.is_empty());
}

#[test]
fn the_phones_v1_file_is_refused_not_guessed() {
    assert!(SessionSummary::decode(&fixture("summary.phone-v1.json")).is_err());
}

#[test]
fn a_newer_v_is_refused() {
    assert!(SessionSummary::decode(
        br#"{"v":3,"session_id":"daisy-1","generated_at_unix_seconds":1,"tldr":"x"}"#
    )
    .is_err());
}

#[test]
fn encode_is_v2_flat_and_round_trips() {
    let s = SessionSummary::decode(&fixture("summary.v2.json")).unwrap();
    let bytes = s.encode();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["v"], 2);
    assert!(value.get("structured").is_none());
    assert!(value.get("schema_version").is_none());
    assert_eq!(value["action_items"][0]["task"], "Merge the parser branch");
    assert!(value["chapters"][0].get("start_hms").is_none());
    assert_eq!(SessionSummary::decode(&bytes).unwrap(), s);
}

#[test]
fn chapters_read_start_hms() {
    let s = SessionSummary::decode(
        br#"{"v":2,"session_id":"daisy-1","generated_at_unix_seconds":1,"tldr":"x",
        "chapters":[{"title":"A","start_hms":"00:12:34"}]}"#,
    )
    .unwrap();
    assert_eq!(s.chapters[0].start_ms, Some(754_000));
}

// ---- corpus byte-identity ---------------------------------------------------
//
// "Byte-identical copies" above is a doc comment, not a check. These two
// tests are the check: they recompute sha256 over the fixtures and assert
// against the committed manifest, so an edit to either corpus that skips
// regenerating the manifest fails here instead of only showing up as a
// silent divergence from the phone's copy. Regenerate with:
//   scripts/seam-digest.sh crates/tauri-app/tests/fixtures/sync-seam \
//     > crates/tauri-app/tests/fixtures/sync-seam.sha256
//   scripts/seam-digest.sh crates/tauri-app/tests/fixtures/sync-vectors-v1.json \
//     > crates/tauri-app/tests/fixtures/sync-vectors-v1.sha256

/// Every regular file under `dir`, as paths relative to it, sorted the same
/// way `scripts/seam-digest.sh` sorts them (`LC_ALL=C sort` over ASCII
/// names, which agrees with Rust's default `Ord` on `String`).
fn relative_files_sorted(dir: &std::path::Path) -> Vec<String> {
    fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                out.push(
                    path.strip_prefix(base)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_string(),
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn the_seam_corpus_matches_its_committed_digest() {
    let root = env!("CARGO_MANIFEST_DIR");
    let dir = std::path::Path::new(root).join("tests/fixtures/sync-seam");
    let names = relative_files_sorted(&dir);

    let mut recomputed = String::new();
    for name in &names {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        recomputed.push_str(&format!("{}  {name}\n", sha256_hex(&bytes)));
    }

    let committed =
        std::fs::read_to_string(format!("{root}/tests/fixtures/sync-seam.sha256")).unwrap();
    assert_eq!(
        recomputed, committed,
        "tests/fixtures/sync-seam.sha256 is stale — regenerate it (see comment above)"
    );
}

#[test]
fn the_sync_vectors_file_matches_its_committed_digest() {
    let root = env!("CARGO_MANIFEST_DIR");
    let bytes = std::fs::read(format!("{root}/tests/fixtures/sync-vectors-v1.json")).unwrap();
    let recomputed = format!("{}  sync-vectors-v1.json\n", sha256_hex(&bytes));

    let committed =
        std::fs::read_to_string(format!("{root}/tests/fixtures/sync-vectors-v1.sha256")).unwrap();
    assert_eq!(
        recomputed, committed,
        "tests/fixtures/sync-vectors-v1.sha256 is stale — regenerate it (see comment above)"
    );
}
