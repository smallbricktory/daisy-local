//! Download whisper.cpp GGML model files from Hugging Face.

use anyhow::{bail, Context};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Known GGML model sizes hosted at huggingface.co/ggerganov/whisper.cpp.
pub const KNOWN_MODELS: &[&str] = &[
    "tiny",
    "tiny.en",
    "base",
    "base.en",
    "small",
    "small.en",
    "medium",
    "medium.en",
    "large-v3",
    "large-v3-turbo",
];

const HF_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Pinned SHA-256 of upstream `ggml-{size}.bin` files. A download whose hash
/// does not match is rejected.
pub const EXPECTED_SHA256: &[(&str, &str)] = &[
    ("base.en", "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002"),
    ("base",    "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe"),
    ("small.en","c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d"),
    ("small",   "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b"),
];

pub type ProgressCb = Box<dyn FnMut(u64, Option<u64>) + Send + 'static>;

pub enum DownloadSource {
    /// Default: the HTTP GET against huggingface.co.
    Http,
    /// Tests only — feed bytes through the same pipeline.
    #[allow(dead_code)]
    Stub { bytes: Vec<u8>, chunk_delay_ms: u64 },
}

impl Default for DownloadSource {
    fn default() -> Self {
        DownloadSource::Http
    }
}

#[derive(Default)]
pub struct DownloadOpts {
    pub source: Option<DownloadSource>,
    pub progress: Option<ProgressCb>,
    pub cancel: Option<Arc<AtomicBool>>,
    /// Override the built-in per-model SHA-256 (hex-lowercase). `None` falls
    /// back to `expected_sha256(size)`.
    pub verify_sha256: Option<String>,
}

/// Per-filename mutex; concurrent calls for the same model serialize.
fn file_lock(name: &str) -> Arc<Mutex<()>> {
    static MAP: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let map = MAP.get_or_init(|| Mutex::new(HashMap::new()));
    let mut g = map.lock().unwrap();
    g.entry(name.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn expected_sha256(size: &str) -> Option<&'static str> {
    EXPECTED_SHA256.iter().find(|(s, _)| *s == size).map(|(_, h)| *h)
}

/// Entry point with default options.
pub fn download_ggml_model(size: &str, dest_dir: &Path) -> anyhow::Result<PathBuf> {
    download_ggml_model_opts(size, dest_dir, DownloadOpts::default())
}

/// Full entry point with progress / cancel / sha256 / preflight / retry.
pub fn download_ggml_model_opts(
    size: &str,
    dest_dir: &Path,
    mut opts: DownloadOpts,
) -> anyhow::Result<PathBuf> {
    if !KNOWN_MODELS.contains(&size) {
        bail!(
            "unknown whisper model size {size:?}; known sizes: {}",
            KNOWN_MODELS.join(", ")
        );
    }

    let file_name = format!("ggml-{size}.bin");
    let dest_path = dest_dir.join(&file_name);
    let lock = file_lock(&file_name);
    let _guard = lock.lock().unwrap();

    // Skip-if-present (verified, if a sha is pinned).
    if let Ok(meta) = std::fs::metadata(&dest_path) {
        if meta.is_file() && meta.len() > 0 {
            if let Some(want) = opts.verify_sha256.as_deref().or_else(|| expected_sha256(size)) {
                if file_sha256(&dest_path)? == want {
                    return Ok(dest_path);
                }
                // Hash mismatch on disk: redownload.
                let _ = std::fs::remove_file(&dest_path);
            } else {
                return Ok(dest_path);
            }
        }
    }

    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("create models dir {}", dest_dir.display()))?;

    let retries = 3;
    let tmp_path = dest_dir.join(format!("{file_name}.part"));

    let mut attempt = 0;
    let final_path = loop {
        attempt += 1;
        match download_attempt(size, &tmp_path, &dest_path, &mut opts) {
            Ok(p) => break p,
            Err(e) if attempt < retries && is_retryable(&e) => {
                let backoff = std::time::Duration::from_millis(250 * (1u64 << (attempt - 1)));
                std::thread::sleep(backoff);
                let _ = std::fs::remove_file(&tmp_path);
                continue;
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(e);
            }
        }
    };

    // Verify sha256 (pinned or caller-supplied).
    if let Some(want) = opts.verify_sha256.as_deref().or_else(|| expected_sha256(size)) {
        let got = file_sha256(&final_path)?;
        if got != want {
            let _ = std::fs::remove_file(&final_path);
            bail!("sha256 mismatch for {size}: got {got}, want {want}");
        }
    }
    Ok(final_path)
}

fn is_retryable(e: &anyhow::Error) -> bool {
    let s = e.to_string().to_lowercase();
    s.contains("timed out")
        || s.contains("timeout")
        || s.contains("connection")
        || s.contains("502")
        || s.contains("503")
        || s.contains("504")
}

fn file_sha256(p: &Path) -> anyhow::Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

fn download_attempt(
    size: &str,
    tmp_path: &Path,
    dest_path: &Path,
    opts: &mut DownloadOpts,
) -> anyhow::Result<PathBuf> {
    let file_name = format!("ggml-{size}.bin");
    let url = format!("{HF_BASE}/{file_name}");

    enum BodyReader {
        Http(reqwest::blocking::Response),
        Stub {
            bytes: Vec<u8>,
            pos: usize,
            chunk_delay_ms: u64,
        },
    }
    impl std::io::Read for BodyReader {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            match self {
                BodyReader::Http(r) => r.read(b),
                BodyReader::Stub {
                    bytes,
                    pos,
                    chunk_delay_ms,
                } => {
                    if *pos >= bytes.len() {
                        return Ok(0);
                    }
                    let chunk = (bytes.len() - *pos).min(b.len()).min(64 * 1024);
                    b[..chunk].copy_from_slice(&bytes[*pos..*pos + chunk]);
                    *pos += chunk;
                    if *chunk_delay_ms > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(*chunk_delay_ms));
                    }
                    Ok(chunk)
                }
            }
        }
    }

    let (mut body, total): (BodyReader, Option<u64>) =
        match opts.source.take().unwrap_or_default() {
            DownloadSource::Http => {
                let client = reqwest::blocking::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(30))
                    .timeout(std::time::Duration::from_secs(60 * 30))
                    .build()
                    .context("build HTTP client")?;
                let resp = client
                    .get(&url)
                    .send()
                    .with_context(|| format!("GET {url}"))?
                    .error_for_status()
                    .with_context(|| format!("GET {url} returned an error status"))?;
                let total = resp.content_length();
                (BodyReader::Http(resp), total)
            }
            DownloadSource::Stub { bytes, chunk_delay_ms } => {
                let total = Some(bytes.len() as u64);
                (
                    BodyReader::Stub {
                        bytes,
                        pos: 0,
                        chunk_delay_ms,
                    },
                    total,
                )
            }
        };

    let mut file = std::fs::File::create(tmp_path)
        .with_context(|| format!("create {}", tmp_path.display()))?;

    let mut buf = [0u8; 64 * 1024];
    let mut written: u64 = 0;
    let mut last_reported: u64 = 0;
    const REPORT_EVERY: u64 = 512 * 1024;
    loop {
        if let Some(c) = opts.cancel.as_ref() {
            if c.load(Ordering::SeqCst) {
                drop(file);
                let _ = std::fs::remove_file(tmp_path);
                bail!("download cancelled by user");
            }
        }
        let n = body.read(&mut buf).context("read response body")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("write tmp file")?;
        written += n as u64;
        if let Some(cb) = opts.progress.as_mut() {
            if written - last_reported >= REPORT_EVERY || n == 0 {
                cb(written, total);
                last_reported = written;
            }
        }
    }
    // The final byte count is always reported.
    if let Some(cb) = opts.progress.as_mut() {
        if written > last_reported {
            cb(written, total);
        }
    }
    file.flush().ok();
    drop(file);
    std::fs::rename(tmp_path, dest_path).with_context(|| {
        format!(
            "rename {} -> {}",
            tmp_path.display(),
            dest_path.display()
        )
    })?;
    Ok(dest_path.to_path_buf())
}

#[cfg(test)]
mod checksum_tests {
    use super::*;

    #[test]
    fn checksums_cover_first_run_offered_sizes() {
        // These sizes have pinned SHAs.
        for size in ["base.en", "base", "small.en", "small"] {
            assert!(
                EXPECTED_SHA256.iter().any(|(s, _)| *s == size),
                "missing checksum for {size}"
            );
        }
    }

    #[test]
    fn checksum_strings_are_64_lowercase_hex() {
        for (_, sum) in EXPECTED_SHA256 {
            assert_eq!(sum.len(), 64, "{sum}");
            assert!(sum.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')), "{sum}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_models_contains_expected_sizes() {
        assert!(KNOWN_MODELS.contains(&"base.en"));
        assert!(KNOWN_MODELS.contains(&"large-v3-turbo"));
        assert!(!KNOWN_MODELS.contains(&"gigantic"));
    }

    #[test]
    fn rejects_unknown_size_before_any_request() {
        // The dir does not exist; an early Err with no side effects is
        // expected.
        let dir = std::path::Path::new("/nonexistent-providers-local-test-dir");
        let err = download_ggml_model("definitely-not-a-model", dir).unwrap_err();
        assert!(
            err.to_string().contains("unknown whisper model size"),
            "unexpected error: {err}"
        );
        assert!(!dir.exists(), "validation must run before any filesystem work");
    }
}

#[cfg(test)]
mod refactor_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn cancel_signal_deletes_part_file() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            cancel_c.store(true, Ordering::SeqCst);
        });
        let opts = DownloadOpts {
            source: Some(DownloadSource::Stub {
                bytes: vec![0u8; 4 * 1024 * 1024],
                chunk_delay_ms: 5,
            }),
            cancel: Some(cancel),
            ..Default::default()
        };
        let err = download_ggml_model_opts("base.en", dir.path(), opts).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("cancel"), "{err}");
        let part = dir.path().join("ggml-base.en.bin.part");
        assert!(!part.exists(), ".part should be deleted on cancel");
    }

    #[test]
    fn sha256_mismatch_deletes_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let opts = DownloadOpts {
            source: Some(DownloadSource::Stub {
                bytes: b"hello world".to_vec(),
                chunk_delay_ms: 0,
            }),
            verify_sha256: Some(
                "0000000000000000000000000000000000000000000000000000000000000000".into(),
            ),
            ..Default::default()
        };
        let err = download_ggml_model_opts("base.en", dir.path(), opts).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("sha"), "{err}");
        assert!(!dir.path().join("ggml-base.en.bin").exists());
    }

    #[test]
    fn concurrent_downloads_for_same_file_serialize() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"deterministic-fixture".to_vec();
        let sum = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(&bytes);
            format!("{:x}", h.finalize())
        };
        let p1 = dir.path().to_path_buf();
        let p2 = dir.path().to_path_buf();
        let bytes1 = bytes.clone();
        let sum1 = sum.clone();
        let sum2 = sum.clone();
        let t1 = std::thread::spawn(move || {
            download_ggml_model_opts(
                "base.en",
                &p1,
                DownloadOpts {
                    source: Some(DownloadSource::Stub {
                        bytes: bytes1,
                        chunk_delay_ms: 1,
                    }),
                    verify_sha256: Some(sum1),
                    ..Default::default()
                },
            )
            .unwrap()
        });
        let t2 = std::thread::spawn(move || {
            download_ggml_model_opts(
                "base.en",
                &p2,
                DownloadOpts {
                    source: Some(DownloadSource::Stub {
                        bytes,
                        chunk_delay_ms: 1,
                    }),
                    verify_sha256: Some(sum2),
                    ..Default::default()
                },
            )
            .unwrap()
        });
        let a = t1.join().unwrap();
        let b = t2.join().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn progress_callback_called_with_monotonic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = vec![0u8; 1024 * 1024];
        let observations: Arc<std::sync::Mutex<Vec<u64>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let obs_c = observations.clone();
        let cb: ProgressCb = Box::new(move |downloaded: u64, _total: Option<u64>| {
            obs_c.lock().unwrap().push(downloaded);
        });
        let opts = DownloadOpts {
            source: Some(DownloadSource::Stub {
                bytes,
                chunk_delay_ms: 0,
            }),
            progress: Some(cb),
            // "tiny" has no pinned SHA; no post-download checksum rejection.
            verify_sha256: None,
            ..Default::default()
        };
        download_ggml_model_opts("tiny", dir.path(), opts).unwrap();
        let v = observations.lock().unwrap().clone();
        assert!(!v.is_empty());
        assert!(v.windows(2).all(|w| w[0] <= w[1]));
    }
}
