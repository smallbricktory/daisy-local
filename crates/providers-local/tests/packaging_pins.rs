// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The packaging script's nemotron pins and the win/linux bundle overlays equal the registry's. The Windows installer keeps its non-solid compressor and its no-skip file errors.

use providers_local::registry;

#[test]
fn tauri_conf_overlays_bundle_exactly_the_nemotron_file_set() {
    // Every registry model file appears in the overlays.
    let registry_files: std::collections::BTreeSet<String> =
        registry::NEMOTRON_FILES.iter().map(|(n, _, _)| n.to_string()).collect();
    for conf in ["tauri.linux.conf.json", "tauri.windows.conf.json"] {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tauri-app").join(conf);
        let s = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
        // Bundle destinations are `models/nemotron/<id>/<file>` strings (the
        // source keys start with `../../` and don't match the prefix).
        let prefix = format!("models/nemotron/{}/", registry::NEMOTRON_ID);
        let bundled: std::collections::BTreeSet<String> =
            s.split('"').filter_map(|part| part.strip_prefix(&prefix)).map(str::to_string).collect();
        assert_eq!(bundled, registry_files, "{conf} nemotron resources drifted from registry.rs");
    }
}

fn repo_file(rel: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn registry_nemotron_pins() -> std::collections::BTreeMap<String, String> {
    registry::NEMOTRON_FILES
        .iter()
        .map(|(n, s, _)| (n.to_string(), s.to_string()))
        .collect()
}

#[test]
fn nemotron_script_pins_equal_registry_pins() {
    let s = repo_file("scripts/download-nemotron.sh");
    let mut script_pins = std::collections::BTreeMap::new();
    for line in s.lines() {
        // Lines of the form `fetch "<file>" "<64-hex>"`.
        let parts: Vec<&str> = line.split('"').collect();
        if parts.len() == 5 && parts[0].trim() == "fetch" && parts[3].len() == 64 {
            script_pins.insert(parts[1].to_string(), parts[3].to_string());
        }
    }
    assert_eq!(script_pins, registry_nemotron_pins(), "download-nemotron.sh pins drifted from registry.rs");
    assert!(s.contains(registry::NEMOTRON_RELEASE_BASE), "release base drifted");
    assert!(s.contains(registry::NEMOTRON_ID), "model dir name drifted");
}

#[test]
fn nemotron_sha256sums_equal_registry_pins() {
    let s = repo_file("scripts/nemotron-export/SHA256SUMS");
    let mut pins = std::collections::BTreeMap::new();
    for line in s.lines().filter(|l| !l.trim().is_empty()) {
        let (sha, name) = line.split_once("  ").expect("`<sha256>  <file>` lines");
        pins.insert(name.to_string(), sha.to_string());
    }
    assert_eq!(pins, registry_nemotron_pins(), "SHA256SUMS drifted from registry.rs");
}

#[test]
fn nsis_installer_extracts_each_file_directly_and_never_skips_one() {
    let conf = repo_file("crates/tauri-app/tauri.conf.json");
    assert!(
        conf.contains(r#""installerHooks": "windows/installer-hooks.nsh""#),
        "tauri.conf.json must include the NSIS installer hooks"
    );
    let hooks = repo_file("crates/tauri-app/windows/installer-hooks.nsh");
    let lines: Vec<&str> = hooks.lines().map(str::trim).collect();
    assert!(conf.contains(r#""compression": "none""#), "the NSIS payload must stay uncompressed");
    assert!(!hooks.contains("SetCompressor"), "makensis rejects SetCompressor in the hooks include");
    assert!(lines.contains(&"AllowSkipFiles off"), "a file write error must not be skippable");
}

#[test]
fn nemotron_publish_script_targets_the_registry_release() {
    let s = repo_file("scripts/publish-nemotron-model.sh");
    let tag = registry::NEMOTRON_RELEASE_BASE.rsplit('/').next().unwrap();
    assert!(s.contains(tag), "release tag drifted");
    assert!(s.contains("smallbricktory/daisy-local"));
    assert!(s.contains("SHA256SUMS"), "the publish script must verify against the pin list");
}

#[test]
fn portable_zip_stages_every_bundled_model_beside_the_exe() {
    // The portable zip places each bundled model at the same relative path as the NSIS install.
    let ps1 = repo_file("scripts/build-release.ps1");
    assert!(ps1.contains("$resources = $portable\n") || ps1.contains("$resources = $portable\r\n"),
        "models must stage at the zip root, not under a resources\\ subfolder");
    let mut checked = 0;
    for conf in ["tauri.conf.json", "tauri.windows.conf.json"] {
        let s = repo_file(&format!("crates/tauri-app/{conf}"));
        for src in s.split('"').filter_map(|p| p.strip_prefix("../../models/")) {
            let (staged, parent) = if let Some(rest) = src.strip_prefix("nemotron/") {
                let id = rest.split('/').next().unwrap();
                (format!("$repo_root\\models\\nemotron\\{id}\""), "nemotron".to_string())
            } else {
                let (dir, _) = src.rsplit_once('/').unwrap();
                (format!("$repo_root\\models\\{}", src.replace('/', "\\")), dir.replace('/', "\\"))
            };
            let line = ps1.lines().find(|l| l.contains(&staged))
                .unwrap_or_else(|| panic!("build-release.ps1 does not stage {src} ({conf})"));
            assert!(line.contains(&format!("\"$resources\\models\\{parent}\\\"")),
                "{src} staged to the wrong zip path: {line}");
            checked += 1;
        }
    }
    assert!(checked >= 10, "bundle resources parse found only {checked} model entries");
}

const NEMOTRON3_DIAR_PINS: [(&str, &str); 5] = [
    ("nemotron3_diar_fast32.onnx", "8ddce2b99b08495a66730f4c2de9d06fd0b23b72d2ab8888f3508b3f665090ca"),
    ("learnable_sil_emb.bin", "d4417b3c0eabdf7c47032fac2b5b5a7ee83d819a6ddda8fd8eaf74e2b5cc4ac7"),
    ("nemotron3_diar.json", "50eb7469e743bc6d3ec92183436fa62f603d9bae59edba6c916987e5e7f314d5"),
    ("NOTICE", "1fb96eb65f7f8be54c4cb6e81d4fbc53a2564de8df4d0372eb930d314fbefd6c"),
    ("LICENSE-OpenMDW-1.1.txt", "2ab44b68365473c112f5092211a38f231cb23e50de68b75a13369adbd76a74df"),
];

#[test]
fn nemotron3_diar_script_pins_are_the_release_sums() {
    let s = repo_file("scripts/download-nemotron3-diar.sh");
    let mut script_pins = std::collections::BTreeMap::new();
    for line in s.lines() {
        let parts: Vec<&str> = line.split('"').collect();
        if parts.len() == 5 && parts[0].trim() == "fetch" && parts[3].len() == 64 {
            script_pins.insert(parts[1].to_string(), parts[3].to_string());
        }
    }
    let want: std::collections::BTreeMap<String, String> =
        NEMOTRON3_DIAR_PINS.iter().map(|(n, h)| (n.to_string(), h.to_string())).collect();
    assert_eq!(script_pins, want, "download-nemotron3-diar.sh pins drifted");
    assert!(s.contains("releases/download/diarizer-nemotron3-ga-2026-09-23-1"), "release tag drifted");
    assert!(s.contains("models/nemotron3-diar"), "model dir name drifted");
    assert!(script_pins.contains_key(providers_local::nemotron3_diar::MODEL_FILE));
    assert!(script_pins.contains_key(providers_local::nemotron3_diar::SIL_FILE));
}

#[test]
fn tauri_conf_bundles_exactly_the_nemotron3_diar_file_set_on_every_os() {
    // The base conf applies to macOS, Windows and Linux alike; the per-OS
    // overlays leave the diarizer to it.
    let want: std::collections::BTreeSet<String> =
        NEMOTRON3_DIAR_PINS.iter().map(|(n, _)| format!("models/nemotron3-diar/{n}")).collect();
    let base = repo_file("crates/tauri-app/tauri.conf.json");
    let bundled: std::collections::BTreeSet<String> = base
        .split('"')
        .filter(|p| p.starts_with("models/nemotron3-diar/"))
        .map(str::to_string)
        .collect();
    assert_eq!(bundled, want, "tauri.conf.json nemotron3-diar resources drifted from the script");
    for (n, _) in NEMOTRON3_DIAR_PINS {
        let src = format!("\"../../models/nemotron3-diar/{n}\"");
        assert!(base.contains(&src), "{src} is not a resource source");
    }
    for conf in ["tauri.linux.conf.json", "tauri.windows.conf.json", "tauri.macos.conf.json"] {
        assert!(
            !repo_file(&format!("crates/tauri-app/{conf}")).contains("nemotron3-diar"),
            "{conf} must leave the diarizer to the base conf"
        );
    }
}

#[test]
fn appimage_and_runtime_resolve_the_bundled_nemotron3_diar_dir() {
    // Windows and macOS: setup() points the env var at resource_dir()/models/nemotron3-diar.
    // The AppImage has no such resource dir; AppRun exports the staged copy.
    let main = repo_file("crates/tauri-app/src/main.rs");
    assert!(
        main.contains(r#"("models/nemotron3-diar", "DAISY_NEMOTRON3_DIAR_DIR")"#),
        "main.rs setup() must point DAISY_NEMOTRON3_DIAR_DIR at the bundled resource dir"
    );
    let repack = repo_file("scripts/repack-appimage.sh");
    assert!(repack.contains(r#"export DAISY_NEMOTRON3_DIAR_DIR="$HERE/usr/share/daisy/models/nemotron3-diar""#));
    assert!(repack.contains(r#"nd_dst="$appdir/usr/share/daisy/models/nemotron3-diar""#));
    for (f, _) in NEMOTRON3_DIAR_PINS {
        assert!(repack.contains(f), "repack-appimage.sh does not verify {f}");
    }
}

#[test]
fn every_release_path_stages_the_nemotron3_diar_model() {
    for script in [
        "scripts/build-inside.sh",
        "scripts/build-release.ps1",
        "scripts/build-release-mac.sh",
        "scripts/build-local-all.sh",
        "scripts/build-ui-test.sh",
        "scripts/build-ui-test.ps1",
    ] {
        let s = repo_file(script);
        assert!(s.contains("nemotron3-diar"), "{script} does not stage the nemotron3-diar model");
    }
    let ci = repo_file(".github/workflows/ci.yml");
    assert_eq!(ci.matches("bash scripts/download-nemotron3-diar.sh").count(), 2, "both CI model stages");
    assert_eq!(ci.matches("models/nemotron3-diar").count(), 2, "both CI model caches");
    assert_eq!(ci.matches("'scripts/download-nemotron3-diar.sh'").count(), 2, "both cache keys");
}
