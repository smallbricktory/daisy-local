// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Recovery words match the phones byte for byte: the vector file is
//! smbr-daisy-ios `docs/sync/recovery-words-vectors.json`, which iOS and
//! Android pass too.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use sealfile::recovery::{
    generate_words, normalize_entry, unwrap_with_words, word_list, wrap_with_params, wrap_with_words, wrapping_key,
};
use sealfile::SealError;
use sha2::{Digest, Sha256};

fn vectors() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/recovery-words-vectors.json")).unwrap()
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn recovery_vectors_match_ios() {
    let v = vectors();
    let list = include_bytes!("../data/bip39-english.txt");
    assert_eq!(to_hex(&Sha256::digest(list)), v["word_list_sha256"].as_str().unwrap());
    assert_eq!(word_list().len(), 2048);
    assert_eq!(v["iterations"], 600_000);

    let phrase = v["phrase"].as_str().unwrap();
    let salt: [u8; 16] = hex(v["salt_hex"].as_str().unwrap()).try_into().unwrap();
    let nonce: [u8; 12] = hex(v["nonce_hex"].as_str().unwrap()).try_into().unwrap();
    let vault_key: [u8; 32] = hex(v["vault_key_hex"].as_str().unwrap()).try_into().unwrap();
    let words: Vec<&str> = v["words"].as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
    assert_eq!(words.join(" "), phrase);

    assert_eq!(to_hex(&*wrapping_key(phrase, &salt)), v["wrapping_key_hex"].as_str().unwrap());

    let file = wrap_with_params(phrase, &vault_key, salt, nonce, 1_700_000_000);
    let parsed: serde_json::Value = serde_json::from_slice(&file).unwrap();
    assert_eq!(to_hex(&B64.decode(parsed["wrapped_b64"].as_str().unwrap()).unwrap()), v["wrapped_hex"].as_str().unwrap());
    assert_eq!(
        String::from_utf8(file.clone()).unwrap(),
        format!(
            r#"{{"created_unix_seconds":1700000000,"iterations":600000,"kdf":"pbkdf2-sha256","nonce_b64":"{}","salt_b64":"{}","v":1,"wrapped_b64":"{}"}}"#,
            B64.encode(nonce),
            B64.encode(salt),
            B64.encode(hex(v["wrapped_hex"].as_str().unwrap())),
        ),
        "compact sorted JSON"
    );
    assert_eq!(unwrap_with_words(phrase, &file).unwrap(), vault_key);

    for case in v["entry"].as_array().unwrap() {
        let typed = case["typed"].as_str().unwrap();
        let want = case["words"].as_array().map(|w| w.iter().map(|x| x.as_str().unwrap()).collect::<Vec<_>>().join(" "));
        assert_eq!(normalize_entry(typed), want, "entry {typed:?}");
    }
}

#[test]
fn normalize_accepts_prefixes_and_case() {
    assert_eq!(normalize_entry("ABAN Zoo ladd ACT quan").as_deref(), Some("abandon zoo ladder act quantum"));
    assert_eq!(normalize_entry("• abandon\n• zoo\n• ladder\n• act\n• quantum").as_deref(), Some("abandon zoo ladder act quantum"));
}

#[test]
fn normalize_rejects_off_list_word() {
    assert_eq!(normalize_entry("abandon zoo ladder act bitcoin"), None);
    assert_eq!(normalize_entry("aba zoo ladder act quantum"), None);
    assert_eq!(normalize_entry(""), None);
}

#[test]
fn generated_words_wrap_and_unwrap() {
    let words = generate_words();
    assert!(words.iter().all(|w| word_list().contains(&w.as_str())));
    let phrase = words.join(" ");
    let file = wrap_with_words(&phrase, &[9u8; 32]);
    assert_eq!(unwrap_with_words(&phrase.to_uppercase(), &file).unwrap(), [9u8; 32]);
    let other = if words[0] == "zoo" { "abandon" } else { "zoo" };
    let wrong = format!("{other} {}", words[1..].join(" "));
    assert!(matches!(unwrap_with_words(&wrong, &file), Err(SealError::Auth)));
    assert!(matches!(unwrap_with_words("not five words", &file), Err(SealError::Auth)));
}
