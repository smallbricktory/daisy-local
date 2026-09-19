// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Test double for `daisy-llm`: same CLI, same line protocol, canned
//! behaviors keyed by the last user message. A model path containing `hang`
//! makes every request hang instead. `FAKE_LLM_PROBE_DELAY_MS` holds the
//! probe reply back by that many milliseconds. Ships nowhere.

use std::io::{BufRead, Write};

const PROBE: &str = r#"{"devices":[{"name":"Fake GPU","backend":"fake","kind":"gpu","memory_bytes":8589934592}],"ram_bytes":34359738368}"#;
const SUMMARY: &str = r#"{"tldr":"fake","action_items":[],"decisions":[],"open_questions":[],"key_topics":[]}"#;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--probe") => {
            if let Some(ms) = std::env::var("FAKE_LLM_PROBE_DELAY_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
            {
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
            let line = std::env::var("FAKE_LLM_PROBE").unwrap_or_else(|_| PROBE.to_string());
            println!("{line}");
        }
        Some("--model") => serve(args.get(1).map(String::as_str).unwrap_or("")),
        _ => {
            eprintln!("usage: fake-llm-worker --probe | --model <p> --ctx <n>");
            std::process::exit(2);
        }
    }
}

fn serve(model: &str) {
    let hang_every_request = model.contains("hang");
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines().map_while(|l| l.ok()) {
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = v["id"].as_str().unwrap_or("").to_string();
        if hang_every_request {
            std::thread::sleep(std::time::Duration::from_secs(120));
            continue;
        }
        let last = v["messages"].as_array().and_then(|m| m.last()).and_then(|m| m["content"].as_str()).unwrap_or("");
        let reply = match last {
            "die" => std::process::exit(3),
            "hang" => {
                std::thread::sleep(std::time::Duration::from_secs(120));
                continue;
            }
            "error" => serde_json::json!({"id": id, "error": "fake failure"}),
            "free" => serde_json::json!({"id": id, "text": format!("hello from fake {}", std::process::id()), "prompt_tokens": 3, "completion_tokens": 3, "seconds": 0.01}),
            _ => serde_json::json!({"id": id, "text": SUMMARY, "prompt_tokens": 10, "completion_tokens": 20, "seconds": 0.02}),
        };
        writeln!(out, "{reply}").unwrap();
        out.flush().unwrap();
    }
}
