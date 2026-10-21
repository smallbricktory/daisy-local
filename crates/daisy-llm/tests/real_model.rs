// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The worker binary against a real GGUF. Ignored by default.
//!
//!   DAISY_LLM_TEST_MODEL=~/dev/daisy-map-reduce/models/gemma-4-E4B-it-GGUF/gemma-4-E4B-it-UD-Q4_K_XL.gguf \
//!     cargo test -p daisy-llm --test real_model -- --ignored --nocapture

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn model() -> String {
    std::env::var("DAISY_LLM_TEST_MODEL").expect("DAISY_LLM_TEST_MODEL")
}

#[test]
#[ignore]
fn probe_lists_a_device_and_ram() {
    let out = Command::new(env!("CARGO_BIN_EXE_daisy-llm"))
        .arg("--probe")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["ram_bytes"].as_u64().unwrap() > 1 << 30);
    assert!(!v["devices"].as_array().unwrap().is_empty());
}

#[test]
#[ignore]
fn a_schema_request_yields_json_that_matches_it_and_free_text_does_not_need_to() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_daisy-llm"))
        .args(["--model", &model(), "--ctx", "4096"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();

    let schema = serde_json::json!({"type":"object","properties":{"tldr":{"type":"string"},"decisions":{"type":"array","items":{"type":"string"}}},"required":["tldr","decisions"]});
    let req = serde_json::json!({
        "id": "s1", "system": "You summarize meetings.",
        "messages": [{"role":"user","content":"Rowan: the harbour survey came back two soundings short. Zoe: I will re-run the north channel Thursday. Rowan: agreed, send the chart Friday."}],
        "schema": schema, "max_tokens": 200
    });
    writeln!(stdin, "{req}").unwrap();
    let reply: serde_json::Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(reply["id"], "s1");
    assert!(reply["error"].is_null(), "{reply}");
    let parsed: serde_json::Value = serde_json::from_str(reply["text"].as_str().unwrap()).unwrap();
    assert!(parsed["tldr"].is_string());
    assert!(parsed["decisions"].is_array());
    assert!(reply["prompt_tokens"].as_u64().unwrap() > 10);

    let free = serde_json::json!({"id":"f1","system":"","messages":[{"role":"user","content":"Say hello in one word."}],"max_tokens":16});
    writeln!(stdin, "{free}").unwrap();
    let reply: serde_json::Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(reply["id"], "f1");
    assert!(!reply["text"].as_str().unwrap().trim().is_empty());

    drop(stdin);
    assert!(child.wait().unwrap().success());
}

/// The production chapters schema against a real model. Its `start_hms`
/// pattern is where a regex class the GBNF parser has no escape for reaches
/// the sampler: conversion succeeds, the grammar then fails to load and the
/// whole extraction errors after the model is in memory.
#[test]
#[ignore]
fn the_chapters_schema_loads_as_a_grammar_and_constrains_the_reply() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_daisy-llm"))
        .args(["--model", &model(), "--ctx", "4096"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();

    let schema = summarize::schemas::all()
        .into_iter()
        .find(|(name, _)| *name == "chapters")
        .expect("chapters is a production schema")
        .1;
    let req = serde_json::json!({
        "id": "c1", "system": "You split meetings into chapters.",
        "messages": [{"role":"user","content":"[00:00:00] Rowan: the harbour survey came back two soundings short.\n[00:04:10] Zoe: I will re-run the north channel Thursday.\n[00:09:30] Rowan: agreed, send the chart Friday."}],
        "schema": schema, "max_tokens": 300
    });
    writeln!(stdin, "{req}").unwrap();
    let reply: serde_json::Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert!(reply["error"].is_null(), "{reply}");
    let parsed: serde_json::Value = serde_json::from_str(reply["text"].as_str().unwrap()).unwrap();
    let first = &parsed["chapters"][0];
    assert!(first["title"].as_str().map(|s| !s.is_empty()).unwrap_or(false), "{parsed}");
    let hms = first["start_hms"].as_str().unwrap_or_default();
    let shaped = hms.len() == 8
        && hms.chars().enumerate().all(|(i, c)| if i == 2 || i == 5 { c == ':' } else { c.is_ascii_digit() });
    assert!(shaped, "start_hms not hh:mm:ss: {hms}");

    drop(stdin);
    assert!(child.wait().unwrap().success());
}
