// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Every first-party source file carries the SPDX + copyright header.
//!

use std::fs;
use std::path::{Path, PathBuf};

const SPDX: &str = "SPDX-License-Identifier: Elastic-2.0";
const COPYRIGHT: &str = "Copyright (c) 2026 Apexis Solutions Inc.";

/// Extensions carrying first-party source.
const EXTENSIONS: &[&str] = &["rs", "ts", "tsx", "swift", "sh", "ps1"];

/// Directory names skipped wherever they appear: third-party trees, build
/// output, model weights and scratch.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".playwright-mcp",
    ".remember",
    "bench-cache",
    "diagnostics-out",
    "dist",
    "dist-ui-test",
    "gen",
    "models",
    "node_modules",
    "target",
    "testdata-field",
    "vendor",
    "worktrees",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/license/../.. is the repo root")
        .to_path_buf()
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_ref()) {
                collect(&path, out);
            }
            continue;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_string());
        if ext.map(|e| EXTENSIONS.contains(&e.as_str())).unwrap_or(false) {
            out.push(path);
        }
    }
}

#[test]
fn every_first_party_source_file_carries_the_license_header() {
    let root = repo_root();
    let mut files = Vec::new();
    collect(&root, &mut files);
    assert!(files.len() > 300, "walked only {} files — the walk is broken", files.len());

    let mut missing = Vec::new();
    for path in &files {
        let src = match fs::read_to_string(path) {
            Ok(s) => s,
            // Non-UTF-8 source is skipped, not failed.
            Err(_) => continue,
        };
        // The header sits in the first few lines, after a shebang if present.
        let head: String = src.lines().take(5).collect::<Vec<_>>().join("\n");
        if !head.contains(SPDX) || !head.contains(COPYRIGHT) {
            missing.push(path.strip_prefix(&root).unwrap_or(path).display().to_string());
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "{} source file(s) missing the license header:\n  {}\n\nAdd these two lines at the top \
         (after any shebang), with the file's comment syntax:\n  {SPDX}\n  Copyright (c) 2026 \
         Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE",
        missing.len(),
        missing.join("\n  "),
    );
}
