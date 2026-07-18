use tauri_app_core::commands::lifecycle::{init_vault_impl, unlock_vault_impl};
use tauri_app_core::commands::tags::{
    create_tag_impl, delete_tag_impl, list_tags_impl, update_tag_impl, CreateTagRequest,
    UpdateTagRequest,
};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, VaultState};

const PASS: &str = "correct-horse-battery-staple-extended-22";

fn temp_app() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    (AppState::new(profile), dir)
}

#[test]
fn tag_crud_roundtrip() {
    let (app, _d) = temp_app();
    // Vault stays unlocked.
    let vs = VaultState::new();
    init_vault_impl(&app, &vs, PASS).unwrap();
    let t = create_tag_impl(
        &app,
        CreateTagRequest {
            name: "NWND".into(),
            color_hex: "#3B4B9B".into(),
            prompt_md: Some("use Northwind Logistics".into()),
            vocab_md: None,
        },
    )
    .unwrap();
    assert_eq!(t.use_count, 0);
    assert!(create_tag_impl(
        &app,
        CreateTagRequest {
            name: "nwnd".into(),
            color_hex: "#FF6A00".into(),
            prompt_md: None,
            vocab_md: None,
        },
    )
    .is_err());
    let t2 = update_tag_impl(
        &app,
        UpdateTagRequest {
            id: t.id.clone(),
            name: Some("Northwind Logistics".into()),
            color_hex: None,
            prompt_md: Some(None),
            vocab_md: None,
        },
    )
    .unwrap();
    assert_eq!(t2.name, "Northwind Logistics");
    assert_eq!(t2.prompt_md, None);
    // Tags persist in tags.json independent of the vault.
    let vs2 = VaultState::new();
    unlock_vault_impl(&app, &vs2, PASS).unwrap();
    let tags = list_tags_impl(&app).unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].name, "Northwind Logistics");
    let res = delete_tag_impl(&app, &t.id, false).unwrap();
    assert_eq!(res.dangling_session_count, 0);
    assert!(list_tags_impl(&app).unwrap().is_empty());
}
