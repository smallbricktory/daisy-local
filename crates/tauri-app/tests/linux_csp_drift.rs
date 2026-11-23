// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The Linux config overrides the CSP to let `<audio>` load `file:` and
//! `data:` sources. Every other directive has to match the base config.

use serde_json::Value;
use std::path::Path;

fn security(file: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {file}: {e}"));
    let json: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {file}: {e}"));
    json["app"]["security"].clone()
}

fn directives(csp: &str) -> Vec<(String, String)> {
    csp.split(';')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| match d.split_once(' ') {
            Some((name, value)) => (name.to_string(), value.to_string()),
            None => (d.to_string(), String::new()),
        })
        .collect()
}

#[test]
fn linux_csp_differs_from_base_only_in_media_src() {
    let base = security("tauri.conf.json");
    let linux = security("tauri.linux.conf.json");
    for key in ["csp", "devCsp"] {
        let b = directives(base[key].as_str().unwrap_or_else(|| panic!("base {key}")));
        let l = directives(linux[key].as_str().unwrap_or_else(|| panic!("linux {key}")));
        let names = |d: &[(String, String)]| d.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        assert_eq!(names(&b), names(&l), "{key}: directive list drifted");
        for ((name, bv), (_, lv)) in b.iter().zip(l.iter()) {
            if name == "media-src" {
                assert_eq!(lv, "'self' data: file:", "{key}: Linux media-src");
                assert_ne!(bv, lv, "{key}: base media-src should differ from Linux");
            } else {
                assert_eq!(bv, lv, "{key}: `{name}` drifted from the base config");
            }
        }
    }
}
