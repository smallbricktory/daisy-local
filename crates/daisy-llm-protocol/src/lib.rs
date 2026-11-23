// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The wire between Daisy and its `daisy-llm` worker: one JSON object per line
//! in each direction, plus the one-line device report `--probe` writes.
//!
//! `schema` present → the reply is sampled under a grammar built from it;
//! absent → free text.
//!
//! The request's text fields are [`Cow`]: the app writes borrowed slices and
//! the worker reads borrowed slices of the line it holds.

pub mod process_power;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message<'a> {
    #[serde(borrow)]
    pub role: Cow<'a, str>,
    #[serde(borrow)]
    pub content: Cow<'a, str>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request<'a> {
    #[serde(borrow)]
    pub id: Cow<'a, str>,
    #[serde(borrow)]
    pub system: Cow<'a, str>,
    #[serde(borrow)]
    pub messages: Vec<Message<'a>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Cow<'a, Value>>,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub id: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub seconds: f64,
}

impl Reply {
    pub fn error(id: &str, msg: impl Into<String>) -> Self {
        Reply { id: id.to_string(), text: None, error: Some(msg.into()), prompt_tokens: 0, completion_tokens: 0, seconds: 0.0 }
    }
}

/// What this machine can run: one ggml backend device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    pub backend: String,
    /// `gpu` | `igpu` | `cpu` | `accel` | `unknown`
    pub kind: String,
    pub memory_bytes: u64,
}

/// The device list and physical RAM, one line from `daisy-llm --probe`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    pub devices: Vec<Device>,
    pub ram_bytes: u64,
}

/// Configures `cmd` so spawning the `daisy-llm` worker does not open a
/// console window on Windows. No-op on other platforms.
#[cfg_attr(not(windows), allow(unused_mut))]
pub fn no_window(mut cmd: std::process::Command) -> std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_with_and_without_schema() {
        let with = Request {
            id: "r1".into(),
            system: "sys".into(),
            messages: vec![Message { role: "user".into(), content: "hi".into() }],
            schema: Some(Cow::Owned(serde_json::json!({"type":"object"}))),
            max_tokens: 64,
        };
        let s = serde_json::to_string(&with).unwrap();
        assert!(!s.contains('\n'));
        assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), with);
        let without: Request = serde_json::from_str(r#"{"id":"r2","system":"","messages":[],"max_tokens":8}"#).unwrap();
        assert_eq!(without.schema, None);
    }

    // The app writes borrowed slices; the worker reads them back out of the
    // line it holds. An escape in the text still decodes, as an owned copy.
    #[test]
    fn request_text_borrows_the_line_it_was_read_from() {
        let line = r#"{"id":"r1","system":"sys","messages":[{"role":"user","content":"plain"},{"role":"user","content":"a\nb"}],"max_tokens":8}"#;
        let req: Request = serde_json::from_str(line).unwrap();
        assert!(matches!(req.system, Cow::Borrowed(_)));
        assert!(matches!(req.messages[0].content, Cow::Borrowed(_)));
        assert_eq!(req.messages[1].content, "a\nb");
    }

    #[test]
    fn reply_defaults_fill_missing_fields() {
        let r: Reply = serde_json::from_str(r#"{"id":"r1","text":"{}"}"#).unwrap();
        assert_eq!(r.text.as_deref(), Some("{}"));
        assert_eq!(r.error, None);
        assert_eq!(r.seconds, 0.0);
        let e = Reply::error("r1", "boom");
        assert_eq!(e.error.as_deref(), Some("boom"));
    }

    #[test]
    fn probe_serializes_the_gate_fields() {
        let p = Probe {
            devices: vec![Device { name: "NVIDIA GeForce RTX 3070 Ti".into(), backend: "Vulkan".into(), kind: "gpu".into(), memory_bytes: 8_589_934_592 }],
            ram_bytes: 34_359_738_368,
        };
        let v: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert_eq!(v["devices"][0]["kind"], "gpu");
        assert_eq!(v["devices"][0]["memory_bytes"], 8_589_934_592u64);
        assert_eq!(v["ram_bytes"], 34_359_738_368u64);
    }
}
