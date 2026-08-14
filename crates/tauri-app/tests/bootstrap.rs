use std::path::PathBuf;
use tauri_app_core::bootstrap::Bootstrap;

#[test]
fn serde_roundtrip_preserves_fields() {
    let b = Bootstrap {
        schema_version: Bootstrap::SCHEMA,
        profile_dir: PathBuf::from("/home/test/OneDrive/Daisy"),
    };
    let json = serde_json::to_string_pretty(&b).unwrap();
    let back: Bootstrap = serde_json::from_str(&json).unwrap();
    assert_eq!(back.schema_version, 1);
    assert_eq!(back.profile_dir, PathBuf::from("/home/test/OneDrive/Daisy"));
}

#[test]
fn schema_version_constant_is_one() {
    assert_eq!(Bootstrap::SCHEMA, 1);
}
