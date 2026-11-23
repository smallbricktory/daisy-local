// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! On macOS, links clang's runtime builtins, which the prebuilt ONNX Runtime
//! calls for its OS version checks.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CC");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let cc = std::env::var("CC").unwrap_or_else(|_| "clang".into());
    match Command::new(&cc).arg("--print-runtime-dir").output() {
        Ok(out) if out.status.success() => {
            let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
            println!("cargo:rustc-link-search=native={dir}");
            println!("cargo:rustc-link-lib=static=clang_rt.osx");
        }
        _ => println!("cargo:warning=`{cc} --print-runtime-dir` failed; the release link needs clang_rt.osx"),
    }
}
