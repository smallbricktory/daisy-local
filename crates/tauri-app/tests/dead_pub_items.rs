// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Every crate here is internal — nothing outside the workspace links them —
//! so a `pub` item nothing calls is dead. rustc's dead_code lint cannot say
//! so: `pub` in a library crate is a public API by definition, and the lint
//! stays quiet.
//!
//! Scans declarations, not behaviour, so it is deliberately conservative:
//! trait methods, impl blocks and macro-generated items are skipped, and a
//! name mentioned anywhere else in the workspace counts as used.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Items exempt from the scan, with the reason they cannot be seen.
const ALLOWED: &[(&str, &str)] = &[
    ("TRACK_SYSTEM", "mirrors a C ABI constant; completes the pair"),
    ("STATE_ERROR", "mirrors a C ABI constant; completes the pair"),
    ("UNKNOWN", "OutputClass variant completing the enum"),
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn no_unreferenced_pub_items() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    rust_files(&root.join("diagnostics"), &mut files);

    let sources: Vec<(PathBuf, String)> = files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok().map(|s| (p.clone(), s)))
        .collect();
    let corpus: String = sources.iter().map(|(_, s)| s.as_str()).collect();

    // name -> "path:line" of the sole declaration.
    let mut declared: BTreeMap<String, String> = BTreeMap::new();
    for (path, text) in &sources {
        if path.components().any(|c| c.as_os_str() == "tests") {
            continue;
        }
        // `in_test` covers the test module the attribute opens and ends at
        // the next column-0 item, so declarations AFTER a mid-file test
        // module are scanned again.
        let mut in_test = false;
        let mut test_item_opened = false;
        for (i, line) in text.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("#[cfg(test)]") {
                in_test = true;
                test_item_opened = false;
                continue;
            }
            if in_test {
                if !test_item_opened {
                    // The item the attribute applies to (usually `mod … {`).
                    test_item_opened = true;
                    continue;
                }
                let col0_item = !line.is_empty()
                    && !line.starts_with([' ', '\t', '}'])
                    && !line.starts_with("//")
                    && !line.starts_with("#[");
                if !col0_item {
                    continue;
                }
                in_test = false;
            }
            let Some(rest) = t.strip_prefix("pub ") else { continue };
            let rest = rest.strip_prefix("async ").unwrap_or(rest);
            let Some((kw, tail)) = rest.split_once(' ') else { continue };
            if !matches!(kw, "fn" | "struct" | "enum" | "trait" | "const" | "static") {
                continue;
            }
            let name: String = tail
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() || ALLOWED.iter().any(|(n, _)| *n == name) {
                continue;
            }
            let rel = path.strip_prefix(&root).unwrap_or(path).display();
            declared.entry(name).or_insert_with(|| format!("{rel}:{}", i + 1));
        }
    }

    let mut dead: Vec<String> = Vec::new();
    for (name, at) in &declared {
        // Word-boundary count across the whole workspace; one is the decl.
        let uses = corpus.match_indices(name.as_str()).filter(|(i, _)| {
            let before = corpus[..*i].chars().next_back();
            let after = corpus[i + name.len()..].chars().next();
            !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
        });
        if uses.count() <= 1 {
            dead.push(format!("  {name}  (declared {at})"));
        }
    }

    assert!(
        dead.is_empty(),
        "pub items nothing references — delete them, or drop `pub`:\n{}",
        dead.join("\n")
    );
}
