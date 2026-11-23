// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Builds the CoreBluetooth shim when the *target* is macOS. `cfg!` in a
//! build script describes the host, so a Mac cross-compiling for Windows
//! would otherwise emit framework link flags no other platform accepts.
fn main() {
    println!("cargo:rerun-if-changed=src/macos/shim.swift");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = std::env::var("OUT_DIR").unwrap();
    let lib = std::path::PathBuf::from(&out).join("libdaisyble.a");
    let status = std::process::Command::new("swiftc")
        .args([
            "-emit-library",
            "-static",
            "-O",
            "-target",
            "arm64-apple-macosx15.0",
            "-module-name",
            "daisyble",
            "-o",
            lib.to_str().unwrap(),
            "src/macos/shim.swift",
        ])
        .status()
        .expect("run swiftc");
    assert!(status.success(), "swiftc failed building shim.swift");

    println!("cargo:rustc-link-search=native={out}");
    println!("cargo:rustc-link-lib=static=daisyble");
    for fw in ["CoreBluetooth", "Foundation"] {
        println!("cargo:rustc-link-lib=framework={fw}");
    }
    // Swift OS runtime (ABI-stable, OS-provided at macOS 15+).
    println!("cargo:rustc-link-search=native=/usr/lib/swift");
    println!("cargo:rustc-link-lib=dylib=swiftCore");
}
