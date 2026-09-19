// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Every JSON schema a completer is handed in production, by name. The
//! grammar-constrained local worker converts each one in a test.

pub fn all() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("summary", crate::anthropic::summary_schema()),
        ("sectioned", crate::anthropic::sectioned_schema()),
        ("chapters", crate::chapters::chapters_schema()),
        ("polish", crate::polish::polish_schema()),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_schema_is_an_object_with_required_keys() {
        for (name, s) in super::all() {
            assert_eq!(s["type"], "object", "{name}");
            assert!(s["required"].as_array().map(|a| !a.is_empty()).unwrap_or(false), "{name}");
        }
    }
}
