// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Engine-typed ASR model registry: one data table drives model listing,
//! resolution, download verification, and packaging pins.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Whisper,
    Parakeet,
    SpeechAnalyzer,
    /// Built-in summary model (llama.cpp GGUF shards) — listed in
    /// [`LLM_REGISTRY`], never in [`REGISTRY`].
    Llm,
}

pub struct ModelRow {
    /// Stable id stored in `asr.active`: the on-disk file name (whisper) or
    /// model directory name (parakeet).
    pub id: &'static str,
    pub engine: Engine,
    pub display: &'static str,
    /// Approximate install size in bytes (drives UI and packaging asserts).
    pub bytes: u64,
    pub multilingual: bool,
    pub license: &'static str,
    /// Where the license text lives; empty for OS-managed models.
    pub license_url: &'static str,
    /// Files with sha256 pins; empty for OS-managed models.
    pub files: &'static [(&'static str, &'static str)],
    /// Platform floor: bundled, undeletable and preselected on these targets.
    pub floor_on: &'static [&'static str],
}

macro_rules! whisper_row {
    ($size:literal, $display:literal, $bytes:expr, $multi:expr, $sha:literal) => {
        ModelRow {
            id: concat!("ggml-", $size, ".bin"),
            engine: Engine::Whisper,
            display: $display,
            bytes: $bytes,
            multilingual: $multi,
            license: "MIT",
            license_url: "https://github.com/openai/whisper/blob/main/LICENSE",
            files: &[(concat!("ggml-", $size, ".bin"), $sha)],
            floor_on: &[],
        }
    };
}

pub const REGISTRY: &[ModelRow] = &[
    whisper_row!("tiny", "Tiny", 78_000_000, true,
        "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21"),
    whisper_row!("tiny.en", "Tiny (English)", 78_000_000, false,
        "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f"),
    whisper_row!("base", "Base", 148_000_000, true,
        "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe"),
    whisper_row!("base.en", "Base (English)", 148_000_000, false,
        "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002"),
    whisper_row!("small", "Small", 488_000_000, true,
        "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b"),
    whisper_row!("small.en", "Small (English)", 488_000_000, false,
        "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d"),
    whisper_row!("medium", "Medium", 1_534_000_000, true,
        "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208"),
    whisper_row!("medium.en", "Medium (English)", 1_534_000_000, false,
        "cc37e93478338ec7700281a7ac30a10128929eb8f427dda2e865faa8f6da4356"),
    whisper_row!("large-v3", "Large v3", 3_095_000_000, true,
        "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2"),
    whisper_row!("large-v3-turbo", "Large v3 Turbo", 1_625_000_000, true,
        "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69"),
    ModelRow {
        id: "parakeet-tdt-0.6b-v2-int8",
        engine: Engine::Parakeet,
        display: "TDT 0.6B v2 (int8)",
        bytes: 661_328_880,
        multilingual: false,
        license: "CC-BY-4.0",
        license_url: "https://creativecommons.org/licenses/by/4.0/",
        files: &[
            ("encoder.int8.onnx", "a32b12d17bbbc309d0686fbbcc2987b5e9b8333a7da83fa6b089f0a2acd651ab"),
            ("decoder.int8.onnx", "b6bb64963457237b900e496ee9994b59294526439fbcc1fecf705b31a15c6b4e"),
            ("joint.pred.int8.onnx", "460003416f807eba35c248f0908d4d8377c57a9909709362b1bcfa6b86b17c8e"),
            ("joint.enc.int8.onnx", "46399f4ac2c5a09199980a0c15a14bc4961a21adc613e8af4fc9929c5e074ce4"),
            ("joint.joint_net.int8.onnx", "4be698e206ac2b5860fbcedde08f64db9012a415acd9ecdde80f589e0cf34f94"),
            ("nemo128.onnx", "e1f0c3c95be11f19026ff341b946aba9b8ca51ba3c2268de91466bf3336ffff7"),
            ("tokens.txt", "ec182b70dd42113aff6c5372c75cac58c952443eb22322f57bbd7f53977d497d"),
            ("model_desc.json", "5a4d575dd6557a03b3d1fd1db4700454a056b1b410bde5e350beb76a2548a439"),
        ],
        floor_on: &["windows", "linux"],
    },
    ModelRow {
        id: "speechanalyzer",
        engine: Engine::SpeechAnalyzer,
        display: "Apple SpeechAnalyzer",
        bytes: 0,
        multilingual: true,
        license: "OS-managed",
        license_url: "",
        files: &[],
        floor_on: &["macos"],
    },
];

/// Built-in LLM rows: `<models>/llm/<id>/` holds the pinned shards; the
/// engine loads the first shard and llama.cpp follows the split.
pub const LLM_REGISTRY: &[ModelRow] = &[ModelRow {
    id: "gemma-4-e4b-it-q4",
    engine: Engine::Llm,
    display: "Gemma 4 E4B",
    bytes: 5_126_307_360,
    multilingual: true,
    license: "Gemma Terms of Use",
    license_url: "https://ai.google.dev/gemma/terms",
    files: &[
        (
            "gemma-4-e4b-it-q4-00001-of-00004.gguf",
            "2f71b3ae3338de9b701ed3b2fdf09c8e04bf58cde5584f8f068cb38e1cb1141b",
        ),
        (
            "gemma-4-e4b-it-q4-00002-of-00004.gguf",
            "f1617594d6b099bf0cb3adc8caa4b547d0623dc8b3dc4e1bdf6a3f10e2484151",
        ),
        (
            "gemma-4-e4b-it-q4-00003-of-00004.gguf",
            "6a8098f3fb16c84160f01fec539fee970dd356ea682c74ce0e05c8b2385f6e23",
        ),
        (
            "gemma-4-e4b-it-q4-00004-of-00004.gguf",
            "e704a456b90ac92a86a23d11e08b9bf3eedd30d1679dd1e41738e0aeef99f244",
        ),
    ],
    floor_on: &[],
}];

/// Download base for LLM shards (daisy-local release assets).
pub const LLM_RELEASE_BASE: &str =
    "https://github.com/smallbricktory/daisy-local/releases/download/llm-gemma-4-e4b-q4-1";

pub fn llm_row(id: &str) -> Option<&'static ModelRow> {
    LLM_REGISTRY.iter().find(|r| r.id == id)
}

/// Default whisper row id: the fallback model and the dev-override file name.
pub const DEFAULT_WHISPER_ID: &str = "ggml-base.en.bin";

/// Download source for the parakeet row: the upstream release tarball, which
/// carries the int8 file set under [`PARAKEET_TARBALL_ROOT`]/.
pub const PARAKEET_TARBALL_URL: &str =
    "https://github.com/jason-ni/parakeet-rs/releases/download/v0.1.0/parakeet-tdt-0.6b-v2-onnx.tar.gz";
pub const PARAKEET_TARBALL_SHA256: &str =
    "ea3dfcc0a5f98be157754a29e05db129deece29b260f33599de4a5af59952b2c";
pub const PARAKEET_TARBALL_ROOT: &str = "parakeet-tdt-0.6b-v2";

impl ModelRow {
    /// Every pinned file of this row is present under `base` (the dir that
    /// holds ggml files for whisper rows, or the parent of the model dir for
    /// parakeet rows). Always false for the OS-managed SpeechAnalyzer row —
    /// its "installed" is an OS capability query, not a disk check.
    pub fn installed_in(&self, base: &Path) -> bool {
        let dir = match self.engine {
            Engine::Whisper => base.to_path_buf(),
            Engine::Parakeet | Engine::Llm => base.join(self.id),
            Engine::SpeechAnalyzer => return false,
        };
        !self.files.is_empty() && self.files.iter().all(|(n, _)| dir.join(n).is_file())
    }

    /// The path handed to the engine under `base`: the ggml file for whisper,
    /// the model dir for parakeet, the first shard for the LLM row. None for
    /// SpeechAnalyzer (no path).
    pub fn engine_path(&self, base: &Path) -> Option<PathBuf> {
        match self.engine {
            Engine::SpeechAnalyzer => None,
            Engine::Llm => Some(base.join(self.id).join(self.files[0].0)),
            Engine::Whisper | Engine::Parakeet => Some(base.join(self.id)),
        }
    }

    /// Whisper size string (`"base.en"`) for `ggml-<size>.bin` rows.
    pub fn whisper_size(&self) -> Option<&'static str> {
        self.id.strip_prefix("ggml-")?.strip_suffix(".bin")
    }
}

pub fn row(id: &str) -> Option<&'static ModelRow> {
    REGISTRY.iter().find(|r| r.id == id)
}

/// Whisper rows in table order.
pub fn whisper_rows() -> impl Iterator<Item = &'static ModelRow> {
    REGISTRY.iter().filter(|r| r.engine == Engine::Whisper)
}

/// `"base.en"` → the `ggml-base.en.bin` row.
pub fn whisper_row_by_size(size: &str) -> Option<&'static ModelRow> {
    whisper_rows().find(|r| r.whisper_size() == Some(size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_rows_are_engine_typed() {
        assert_eq!(row("ggml-base.en.bin").unwrap().engine, Engine::Whisper);
        assert_eq!(row("ggml-large-v3-turbo.bin").unwrap().engine, Engine::Whisper);
    }

    #[test]
    fn parakeet_row_resolves() {
        assert_eq!(row("parakeet-tdt-0.6b-v2-int8").unwrap().engine, Engine::Parakeet);
    }

    #[test]
    fn unknown_id_is_none() {
        assert!(row("gigantic").is_none());
        assert!(row("not-a-model.bin").is_none());
    }

    #[test]
    fn all_file_pins_are_sha256_hex() {
        for r in REGISTRY {
            for (name, sha) in r.files {
                assert_eq!(sha.len(), 64, "{}/{name}", r.id);
                assert!(
                    sha.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                    "{}/{name}: {sha}",
                    r.id
                );
            }
        }
    }

    #[test]
    fn whisper_row_count_matches_legacy_known_models() {
        let n = REGISTRY.iter().filter(|r| r.engine == Engine::Whisper).count();
        assert_eq!(n, 10, "whisper rows silently dropped or grown");
    }

    #[test]
    fn parakeet_files_cover_loader_set() {
        let r = row("parakeet-tdt-0.6b-v2-int8").unwrap();
        for need in [
            "encoder.int8.onnx",
            "decoder.int8.onnx",
            "joint.pred.int8.onnx",
            "joint.enc.int8.onnx",
            "joint.joint_net.int8.onnx",
            "nemo128.onnx",
            "tokens.txt",
        ] {
            assert!(r.files.iter().any(|(n, _)| *n == need), "missing {need}");
        }
        assert_eq!(r.floor_on, &["windows", "linux"]);
    }

    #[test]
    fn speechanalyzer_row_is_os_managed() {
        let r = row("speechanalyzer").unwrap();
        assert_eq!(r.engine, Engine::SpeechAnalyzer);
        assert!(r.files.is_empty());
        assert_eq!(r.floor_on, &["macos"]);
    }

    #[test]
    fn llm_row_is_pinned_and_sharded() {
        let r = llm_row("gemma-4-e4b-it-q4").expect("row");
        assert_eq!(r.engine, Engine::Llm);
        assert_eq!(r.files.len(), 4);
        for (n, sha) in r.files {
            assert!(n.starts_with("gemma-4-e4b-it-q4-0000") && n.ends_with("-of-00004.gguf"), "{n}");
            assert_eq!(sha.len(), 64);
            assert!(sha.chars().all(|c| c.is_ascii_hexdigit()), "{n}: {sha}");
        }
        assert!(r.bytes > 5_000_000_000 && r.bytes < 5_300_000_000);
        assert!(LLM_RELEASE_BASE.starts_with("https://github.com/smallbricktory/daisy-local/releases/download/"));
    }

    #[test]
    fn llm_rows_are_absent_from_the_asr_registry() {
        assert!(REGISTRY.iter().all(|r| r.engine != Engine::Llm));
        assert!(row("gemma-4-e4b-it-q4").is_none());
    }

    #[test]
    fn llm_installed_in_and_engine_path_use_the_model_dir() {
        let dir = tempfile::tempdir().unwrap();
        let r = llm_row("gemma-4-e4b-it-q4").unwrap();
        assert!(!r.installed_in(dir.path()));
        let d = dir.path().join(r.id);
        std::fs::create_dir_all(&d).unwrap();
        for (n, _) in r.files {
            std::fs::write(d.join(n), b"x").unwrap();
        }
        assert!(r.installed_in(dir.path()));
        assert_eq!(r.engine_path(dir.path()).unwrap(), d.join(r.files[0].0));
    }
}
