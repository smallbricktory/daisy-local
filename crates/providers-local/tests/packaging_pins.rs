//! The packaging script's parakeet pins must equal the registry row's —
//! one drifting from the other ships a bundle the app then re-verifies
//! against different hashes.

use providers_local::registry;

fn script() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/download-parakeet.sh");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn script_pins_equal_registry_pins() {
    let row = registry::row("parakeet-tdt-0.6b-v2-int8").expect("parakeet row");
    let s = script();
    // Lines of the form `<file> <64-hex>` inside the pins block.
    let mut script_pins = std::collections::BTreeMap::new();
    for line in s.lines() {
        let mut parts = line.split_whitespace();
        if let (Some(name), Some(sha), None) = (parts.next(), parts.next(), parts.next()) {
            if sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
                script_pins.insert(name.to_string(), sha.to_string());
            }
        }
    }
    let registry_pins: std::collections::BTreeMap<String, String> = row
        .files
        .iter()
        .map(|(n, s)| (n.to_string(), s.to_string()))
        .collect();
    assert_eq!(script_pins, registry_pins, "download-parakeet.sh pins drifted from registry.rs");
}

#[test]
fn tauri_conf_overlays_bundle_exactly_the_registry_file_set() {
    // The win/linux bundle overlays list the parakeet files one by one; a
    // file added to or renamed in the registry row must be re-listed there or
    // it is fetched by the script yet never shipped, failing at load time in
    // the field.
    let row = registry::row("parakeet-tdt-0.6b-v2-int8").expect("parakeet row");
    let registry_files: std::collections::BTreeSet<String> =
        row.files.iter().map(|(n, _)| n.to_string()).collect();
    for conf in ["tauri.linux.conf.json", "tauri.windows.conf.json"] {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tauri-app")
            .join(conf);
        let s = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
        // Bundle destinations are `models/parakeet/<id>/<file>` strings (the
        // source keys start with `../../` and don't match the prefix).
        let prefix = format!("models/parakeet/{}/", row.id);
        let bundled: std::collections::BTreeSet<String> = s
            .split('"')
            .filter_map(|part| part.strip_prefix(&prefix))
            .map(str::to_string)
            .collect();
        assert_eq!(bundled, registry_files, "{conf} parakeet resources drifted from registry.rs");
    }
}

#[test]
fn script_tarball_pin_equals_registry() {
    let s = script();
    assert!(
        s.contains(registry::PARAKEET_TARBALL_SHA256),
        "tarball sha in download-parakeet.sh drifted from registry.rs"
    );
    assert!(
        s.contains(registry::PARAKEET_TARBALL_URL),
        "tarball url in download-parakeet.sh drifted from registry.rs"
    );
}
