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
    Nemotron,
    SpeechAnalyzer,
    /// Built-in summary model (llama.cpp GGUF shards) — listed in
    /// [`LLM_REGISTRY`], never in [`REGISTRY`].
    Llm,
}

pub struct ModelRow {
    /// Stable id stored in `asr.active`: the on-disk file name (whisper) or
    /// model directory name (nemotron).
    pub id: &'static str,
    pub engine: Engine,
    pub display: &'static str,
    /// Approximate install size in bytes (drives UI and packaging asserts).
    pub bytes: u64,
    pub multilingual: bool,
    pub license: &'static str,
    /// Where the license text lives; empty for OS-managed models.
    pub license_url: &'static str,
    /// Files as (name, sha256 pin, exact byte size); empty for OS-managed
    /// models.
    pub files: &'static [(&'static str, &'static str, u64)],
    /// Platform floor: bundled, undeletable and preselected on these targets.
    pub floor_on: &'static [&'static str],
}

macro_rules! whisper_row {
    ($size:literal, $display:literal, $bytes:expr, $multi:expr, $sha:literal, $file_bytes:expr) => {
        ModelRow {
            id: concat!("ggml-", $size, ".bin"),
            engine: Engine::Whisper,
            display: $display,
            bytes: $bytes,
            multilingual: $multi,
            license: "MIT",
            license_url: "https://github.com/openai/whisper/blob/main/LICENSE",
            files: &[(concat!("ggml-", $size, ".bin"), $sha, $file_bytes)],
            floor_on: &[],
        }
    };
}

pub const REGISTRY: &[ModelRow] = &[
    whisper_row!("tiny", "Tiny", 78_000_000, true,
        "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21", 77_691_713),
    whisper_row!("tiny.en", "Tiny (English)", 78_000_000, false,
        "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f", 77_704_715),
    whisper_row!("base", "Base", 148_000_000, true,
        "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe", 147_951_465),
    whisper_row!("base.en", "Base (English)", 148_000_000, false,
        "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002", 147_964_211),
    whisper_row!("small", "Small", 488_000_000, true,
        "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b", 487_601_967),
    whisper_row!("small.en", "Small (English)", 488_000_000, false,
        "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d", 487_614_201),
    whisper_row!("medium", "Medium", 1_534_000_000, true,
        "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208", 1_533_763_059),
    whisper_row!("medium.en", "Medium (English)", 1_534_000_000, false,
        "cc37e93478338ec7700281a7ac30a10128929eb8f427dda2e865faa8f6da4356", 1_533_774_781),
    whisper_row!("large-v3", "Large v3", 3_095_000_000, true,
        "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2", 3_095_033_483),
    whisper_row!("large-v3-turbo", "Large v3 Turbo", 1_625_000_000, true,
        "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69", 1_624_555_275),
    ModelRow {
        id: NEMOTRON_ID,
        engine: Engine::Nemotron,
        display: "Nemotron Speech Streaming 0.6B (int8)",
        bytes: NEMOTRON_BYTES,
        multilingual: false,
        license: "NVIDIA Open Model License",
        license_url: "https://www.nvidia.com/en-us/agreements/enterprise-software/nvidia-open-model-license/",
        files: NEMOTRON_FILES,
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
            70_845_696,
        ),
        (
            "gemma-4-e4b-it-q4-00002-of-00004.gguf",
            "f1617594d6b099bf0cb3adc8caa4b547d0623dc8b3dc4e1bdf6a3f10e2484151",
            1_937_768_640,
        ),
        (
            "gemma-4-e4b-it-q4-00003-of-00004.gguf",
            "6a8098f3fb16c84160f01fec539fee970dd356ea682c74ce0e05c8b2385f6e23",
            1_885_292_544,
        ),
        (
            "gemma-4-e4b-it-q4-00004-of-00004.gguf",
            "e704a456b90ac92a86a23d11e08b9bf3eedd30d1679dd1e41738e0aeef99f244",
            1_232_400_480,
        ),
    ],
    floor_on: &[],
}];

/// Download base for LLM shards (daisy-local release assets).
pub const LLM_RELEASE_BASE: &str =
    "https://github.com/smallbricktory/daisy-local/releases/download/llm-gemma-4-e4b-q4-1";

/// Registry id and model dir name of the Nemotron streaming model.
pub const NEMOTRON_ID: &str = "nemotron-speech-streaming-en-0.6b-560ms-int8";
/// Pinned file set the Nemotron loader reads. `encoder.int8.onnx.data` holds
/// the encoder weights as external data next to the graph.
pub const NEMOTRON_FILES: &[(&str, &str, u64)] = &[
    ("encoder.int8.onnx", "48fdad330d1143f05940d43d4a090c14b18e3dfb98b7b4c430f5536df531934d", 3_843_353),
    ("encoder.int8.onnx.data", "7dc691f551ce9fc01ac97a87c8f72c85e9a348ab310ee57c033540b613915a12", 664_920_064),
    ("decoder.onnx", "809ad04d35a38030150c11f5fe982c319fc27cf4c703c2421eea49dca05e02fd", 28_883_664),
    ("joiner.onnx", "e7d0168dc1c76718c54d2afb63ee5ee1b68658cb68a5baf11b79de40378663a4", 6_894_757),
    ("tokens.txt", "dc0b4584ab2e4ddbf888425c076c61b736e7356a015250db7d307e6f1a8188ff", 8_952),
];
/// Summed size of [`NEMOTRON_FILES`].
pub const NEMOTRON_BYTES: u64 = 704_550_790;
/// Download base for [`NEMOTRON_FILES`] (daisy-local release assets).
pub const NEMOTRON_RELEASE_BASE: &str =
    "https://github.com/smallbricktory/daisy-local/releases/download/asr-nemotron-streaming-en-0.6b-560ms-int8-1";

pub fn llm_row(id: &str) -> Option<&'static ModelRow> {
    LLM_REGISTRY.iter().find(|r| r.id == id)
}

/// Default whisper row id: the fallback model and the dev-override file name.
pub const DEFAULT_WHISPER_ID: &str = "ggml-base.en.bin";

/// True when `files` is non-empty and each is a regular file under `dir` whose
/// length equals its pinned size.
pub fn files_intact_in(dir: &Path, files: &[(&str, &str, u64)]) -> bool {
    !files.is_empty()
        && files
            .iter()
            .all(|(n, _, size)| std::fs::metadata(dir.join(n)).is_ok_and(|m| m.is_file() && m.len() == *size))
}

/// What a disk check finds of a row's pinned files under one base.
#[derive(Debug)]
pub enum Presence {
    /// Every pinned file is a regular file at its pinned byte size.
    Installed,
    /// At least one pinned file is absent, not a regular file, or not at its
    /// pinned size, and none failed to stat.
    Missing,
    /// A pinned file's stat failed with an error other than not-found: the
    /// model may be on disk but can't be reached.
    Unreadable { path: PathBuf, error: std::io::Error },
}

impl ModelRow {
    /// Every pinned file of this row is present under `base` at its pinned
    /// byte size (the dir that
    /// holds ggml files for whisper rows, or the parent of the model dir for
    /// nemotron rows). A zero-byte or truncated file (an interrupted install)
    /// is not installed. Always false for the OS-managed SpeechAnalyzer row —
    /// its "installed" is an OS capability query, not a disk check.
    pub fn installed_in(&self, base: &Path) -> bool {
        matches!(self.presence_in(base), Presence::Installed)
    }

    /// [`Self::installed_in`] with the reason it is false: an unreadable
    /// file is reported as such, never as missing.
    pub fn presence_in(&self, base: &Path) -> Presence {
        let dir = match self.engine {
            Engine::Whisper => base.to_path_buf(),
            Engine::Nemotron | Engine::Llm => base.join(self.id),
            Engine::SpeechAnalyzer => return Presence::Missing,
        };
        if self.files.is_empty() {
            return Presence::Missing;
        }
        let mut missing = false;
        for (n, _, size) in self.files {
            let path = dir.join(n);
            match std::fs::metadata(&path) {
                Ok(m) if m.is_file() && m.len() == *size => {}
                Ok(_) => missing = true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing = true,
                Err(error) => return Presence::Unreadable { path, error },
            }
        }
        if missing {
            Presence::Missing
        } else {
            Presence::Installed
        }
    }

    /// The path handed to the engine under `base`: the ggml file for whisper,
    /// the model dir for nemotron, the first shard for the LLM row. None for
    /// SpeechAnalyzer (no path).
    pub fn engine_path(&self, base: &Path) -> Option<PathBuf> {
        match self.engine {
            Engine::SpeechAnalyzer => None,
            Engine::Llm => Some(base.join(self.id).join(self.files[0].0)),
            Engine::Whisper | Engine::Nemotron => Some(base.join(self.id)),
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
    fn nemotron_row_is_the_win_linux_floor() {
        let r = row(NEMOTRON_ID).unwrap();
        assert_eq!(r.engine, Engine::Nemotron);
        assert_eq!(r.files, NEMOTRON_FILES);
        assert_eq!(r.bytes, NEMOTRON_BYTES);
        assert_eq!(NEMOTRON_FILES.iter().map(|f| f.2).sum::<u64>(), NEMOTRON_BYTES);
        assert_eq!(r.floor_on, &["windows", "linux"]);
        assert_eq!(r.license, "NVIDIA Open Model License");
    }

    #[test]
    fn unknown_id_is_none() {
        assert!(row("gigantic").is_none());
        assert!(row("not-a-model.bin").is_none());
    }

    #[test]
    fn all_file_pins_are_sha256_hex() {
        for r in REGISTRY {
            for (name, sha, _) in r.files {
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
        for (n, sha, _) in r.files {
            assert!(n.starts_with("gemma-4-e4b-it-q4-0000") && n.ends_with("-of-00004.gguf"), "{n}");
            assert_eq!(sha.len(), 64);
            assert!(sha.chars().all(|c| c.is_ascii_hexdigit()), "{n}: {sha}");
        }
        assert_eq!(r.files.iter().map(|f| f.2).sum::<u64>(), r.bytes, "shard sizes sum to the row size");
        assert!(LLM_RELEASE_BASE.starts_with("https://github.com/smallbricktory/daisy-local/releases/download/"));
    }

    #[test]
    fn nemotron_pins_cover_the_loader_set_and_come_from_daisy_local() {
        let names: Vec<&str> = NEMOTRON_FILES.iter().map(|(n, _, _)| *n).collect();
        assert_eq!(
            names,
            ["encoder.int8.onnx", "encoder.int8.onnx.data", "decoder.onnx", "joiner.onnx", "tokens.txt"]
        );
        for (n, sha, _) in NEMOTRON_FILES {
            assert_eq!(sha.len(), 64, "{n}");
            assert!(sha.chars().all(|c| c.is_ascii_hexdigit()), "{n}: {sha}");
        }
        assert_eq!(NEMOTRON_BYTES, 704_550_790);
        assert_eq!(
            NEMOTRON_RELEASE_BASE,
            "https://github.com/smallbricktory/daisy-local/releases/download/asr-nemotron-streaming-en-0.6b-560ms-int8-1"
        );
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
        for (n, _, size) in r.files {
            std::fs::File::create(d.join(n)).unwrap().set_len(*size).unwrap();
        }
        assert!(r.installed_in(dir.path()));
        assert_eq!(r.engine_path(dir.path()).unwrap(), d.join(r.files[0].0));
    }

    #[test]
    fn a_file_the_stat_cannot_reach_is_unreadable_not_missing() {
        let dir = tempfile::tempdir().unwrap();
        let r = llm_row("gemma-4-e4b-it-q4").unwrap();
        assert!(matches!(r.presence_in(dir.path()), Presence::Missing));
        // The model dir is a regular file.
        #[cfg(unix)]
        {
            std::fs::write(dir.path().join(r.id), b"x").unwrap();
            match r.presence_in(dir.path()) {
                Presence::Unreadable { path, .. } => assert!(path.starts_with(dir.path().join(r.id))),
                other => panic!("expected unreadable, got {other:?}"),
            }
            assert!(!r.installed_in(dir.path()));
        }
    }

    #[test]
    fn installed_in_requires_the_pinned_size_for_every_engine() {
        for r in REGISTRY.iter().chain(LLM_REGISTRY).filter(|r| !r.files.is_empty()) {
            let dir = tempfile::tempdir().unwrap();
            let d = match r.engine {
                Engine::Whisper => dir.path().to_path_buf(),
                _ => dir.path().join(r.id),
            };
            std::fs::create_dir_all(&d).unwrap();
            let put = |name: &str, len: u64| {
                std::fs::File::create(d.join(name)).unwrap().set_len(len).unwrap();
            };
            for (n, _, size) in r.files {
                put(n, *size);
            }
            assert!(r.installed_in(dir.path()), "{}: full set", r.id);
            let (last, _, size) = r.files[r.files.len() - 1];
            put(last, 0);
            assert!(!r.installed_in(dir.path()), "{}: zero-byte file", r.id);
            put(last, size - 1);
            assert!(!r.installed_in(dir.path()), "{}: truncated file", r.id);
            put(last, size + 1);
            assert!(!r.installed_in(dir.path()), "{}: oversized file", r.id);
            std::fs::remove_file(d.join(last)).unwrap();
            std::fs::create_dir(d.join(last)).unwrap();
            assert!(!r.installed_in(dir.path()), "{}: directory in place of file", r.id);
        }
    }
}
