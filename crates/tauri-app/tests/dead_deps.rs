//! A dependency dropped from the code stays in the manifest: nothing in the
//! build fails, it just keeps being fetched, compiled and shipped in the
//! licence manifest.
//!
//! Matches the dependency's identifier against the crate's own sources.
//! A dep with no source reference is either dead or a version pin, and a pin
//! has to say so in PINNED below.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Deps with no source reference that are still load-bearing, and why.
const PINNED: &[(&str, &str, &str)] = &[
    // deep_filter reaches tract itself; these force the version window its
    // API needs. See the comment in crates/denoise/Cargo.toml.
    ("denoise", "tract-core", "pins deep_filter's tract version"),
    ("denoise", "tract-onnx", "pins deep_filter's tract version"),
    ("denoise", "tract-pulse", "pins deep_filter's tract version"),
    ("denoise", "tract-hir", "pins deep_filter's tract version"),
    // The package's lib target is named `df`; sources reference df::, never
    // the package name.
    ("denoise", "deep_filter", "lib name is df"),
];

fn read_rs(dir: &Path, out: &mut String) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            read_rs(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            if let Ok(s) = std::fs::read_to_string(&p) {
                out.push_str(&s);
            }
        }
    }
}

/// Dependency names from every `[…dependencies]` table in a manifest.
fn deps(manifest: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_deps = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = t.ends_with("dependencies]");
            continue;
        }
        if !in_deps || t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((name, _)) = t.split_once('=') else { continue };
        let name = name.trim().trim_matches('"');
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
            out.insert(name.to_string());
        }
    }
    out
}

#[test]
fn every_dependency_is_referenced() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");

    // Examples live outside their crate, wired in by `[[example]] path`.
    let mut shared = String::new();
    read_rs(&root.join("diagnostics"), &mut shared);

    let mut dead = Vec::new();
    let crates: Vec<PathBuf> = std::fs::read_dir(root.join("crates"))
        .expect("crates dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("Cargo.toml").is_file())
        .collect();

    for dir in crates {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).expect("manifest");
        let mut src = shared.clone();
        for sub in ["src", "tests", "examples", "benches"] {
            read_rs(&dir.join(sub), &mut src);
        }
        if let Ok(b) = std::fs::read_to_string(dir.join("build.rs")) {
            src.push_str(&b);
        }
        // Comment text must not count as usage, and the identifier must
        // stand alone, not appear inside a longer name.
        let code: String = src
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        for dep in deps(&manifest) {
            if PINNED.iter().any(|(c, d, _)| *c == name && *d == dep) {
                continue;
            }
            let ident = dep.replace('-', "_");
            let referenced = code.match_indices(&ident).any(|(i, _)| {
                let before = code[..i].chars().next_back();
                let after = code[i + ident.len()..].chars().next();
                !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                    && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
            });
            if !referenced {
                dead.push(format!("  {name}: {dep}"));
            }
        }
    }

    assert!(
        dead.is_empty(),
        "dependencies nothing references — drop them, or record the pin in PINNED:\n{}",
        dead.join("\n")
    );
}
