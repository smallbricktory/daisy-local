//! Download whisper.cpp GGML model files from Hugging Face.

use anyhow::{bail, Context};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const HF_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

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

/// Registry pin for `ggml-<size>.bin` (whisper rows are single-file).
fn expected_sha256(size: &str) -> Option<&'static str> {
    crate::registry::whisper_row_by_size(size).map(|r| r.files[0].1)
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
    if crate::registry::whisper_row_by_size(size).is_none() {
        bail!(
            "unknown whisper model size {size:?}; known sizes: {}",
            crate::registry::whisper_rows()
                .filter_map(|r| r.whisper_size())
                .collect::<Vec<_>>()
                .join(", ")
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

    let tmp_path = dest_dir.join(format!("{file_name}.part"));
    let url = format!("{HF_BASE}/{file_name}");
    let final_path = download_with_retry(&url, &tmp_path, &dest_path, &mut opts)?;

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

/// Fetch `url` into `dest_path` (via `tmp_path`), retrying transient errors
/// with backoff. The `.part` file is removed on every failure path.
fn download_with_retry(
    url: &str,
    tmp_path: &Path,
    dest_path: &Path,
    opts: &mut DownloadOpts,
) -> anyhow::Result<PathBuf> {
    let retries = 3;
    let mut attempt = 0;
    loop {
        attempt += 1;
        match download_attempt(url, tmp_path, dest_path, opts) {
            Ok(p) => return Ok(p),
            Err(e) if attempt < retries && is_retryable(&e) => {
                let backoff = std::time::Duration::from_millis(250 * (1u64 << (attempt - 1)));
                std::thread::sleep(backoff);
                let _ = std::fs::remove_file(tmp_path);
                continue;
            }
            Err(e) => {
                let _ = std::fs::remove_file(tmp_path);
                return Err(e);
            }
        }
    }
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

/// Download + install a parakeet model dir: fetch the upstream release
/// tarball, verify it, extract the registry-pinned files into a staging dir,
/// verify each pin, then atomically rename the staging dir into place — a
/// partial dir is never left at the destination.
/// `opts.verify_sha256` overrides the tarball pin (tests only).
pub fn download_parakeet_model_opts(
    id: &str,
    parakeet_parent: &Path,
    mut opts: DownloadOpts,
) -> anyhow::Result<PathBuf> {
    use crate::registry;
    let row = registry::row(id)
        .filter(|r| r.engine == registry::Engine::Parakeet)
        .ok_or_else(|| anyhow::anyhow!("unknown parakeet model {id:?}"))?;
    let lock = file_lock(id);
    let _guard = lock.lock().unwrap();
    let dest = parakeet_parent.join(id);
    if row.installed_in(parakeet_parent) {
        return Ok(dest);
    }
    std::fs::create_dir_all(parakeet_parent)?;
    let tar_part = parakeet_parent.join(format!("{id}.tar.gz.part"));
    let tar_path = parakeet_parent.join(format!("{id}.tar.gz"));
    let stage = parakeet_parent.join(format!("{id}.staging"));
    let result = (|| {
        download_with_retry(registry::PARAKEET_TARBALL_URL, &tar_part, &tar_path, &mut opts)?;
        let want = opts
            .verify_sha256
            .as_deref()
            .unwrap_or(registry::PARAKEET_TARBALL_SHA256);
        let got = file_sha256(&tar_path)?;
        if got != want {
            bail!("sha256 mismatch for {id} tarball: got {got}, want {want}");
        }
        let _ = std::fs::remove_dir_all(&stage);
        extract_pinned_files(&tar_path, registry::PARAKEET_TARBALL_ROOT, row.files, &stage)?;
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(&stage, &dest)?;
        Ok(dest.clone())
    })();
    let _ = std::fs::remove_file(&tar_path);
    let _ = std::fs::remove_file(&tar_part);
    let _ = std::fs::remove_dir_all(&stage);
    result
}

/// Extract `files` found under `root/` inside a tar.gz into `stage`,
/// verifying each sha256 pin. Fails if any pinned file is absent.
fn extract_pinned_files(
    tar_gz: &Path,
    root: &str,
    files: &[(&str, &str)],
    stage: &Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(stage)?;
    let f = std::fs::File::open(tar_gz)?;
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
    let mut found = std::collections::HashSet::new();
    for entry in ar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let Some(name) = path
            .strip_prefix(root)
            .ok()
            .and_then(|p| p.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let Some((_, want)) = files.iter().find(|(n, _)| *n == name) else {
            continue;
        };
        let out = stage.join(&name);
        entry.unpack(&out).with_context(|| format!("unpack {name}"))?;
        let got = file_sha256(&out)?;
        if got != *want {
            bail!("sha256 mismatch for {name}: got {got}, want {want}");
        }
        found.insert(name);
    }
    for (n, _) in files {
        if !found.contains(*n) {
            bail!("tarball is missing {n}");
        }
    }
    Ok(())
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
    url: &str,
    tmp_path: &Path,
    dest_path: &Path,
    opts: &mut DownloadOpts,
) -> anyhow::Result<PathBuf> {
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
                    .get(url)
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
mod extract_tests {
    use super::*;
    use std::io::Write;

    fn sha_of(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        format!("{:x}", h.finalize())
    }

    /// Build root/{name: content} as a tar.gz on disk.
    fn fixture_tarball(dir: &Path, root: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let p = dir.join("fixture.tar.gz");
        let gz = flate2::write::GzEncoder::new(
            std::fs::File::create(&p).unwrap(),
            flate2::Compression::fast(),
        );
        let mut ar = tar::Builder::new(gz);
        for (name, content) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(content.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            ar.append_data(&mut h, format!("{root}/{name}"), *content).unwrap();
        }
        ar.into_inner().unwrap().finish().unwrap().flush().unwrap();
        p
    }

    #[test]
    fn extracts_and_verifies_pinned_files() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = fixture_tarball(
            dir.path(),
            "root",
            &[("a.onnx", b"alpha"), ("b.txt", b"beta"), ("junk.bin", b"junk")],
        );
        let a_sha = sha_of(b"alpha");
        let b_sha = sha_of(b"beta");
        let pins: Vec<(&str, &str)> = vec![("a.onnx", a_sha.as_str()), ("b.txt", b_sha.as_str())];
        let stage = dir.path().join("stage");
        extract_pinned_files(&tarball, "root", &pins, &stage).unwrap();
        assert_eq!(std::fs::read(stage.join("a.onnx")).unwrap(), b"alpha");
        assert_eq!(std::fs::read(stage.join("b.txt")).unwrap(), b"beta");
        assert!(!stage.join("junk.bin").exists(), "unpinned members skipped");
    }

    #[test]
    fn pin_mismatch_errors() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = fixture_tarball(dir.path(), "root", &[("a.onnx", b"alpha")]);
        let pins: Vec<(&str, &str)> = vec![(
            "a.onnx",
            "0000000000000000000000000000000000000000000000000000000000000000",
        )];
        let err =
            extract_pinned_files(&tarball, "root", &pins, &dir.path().join("s")).unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err}");
    }

    #[test]
    fn missing_member_errors() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = fixture_tarball(dir.path(), "root", &[("a.onnx", b"alpha")]);
        let a_sha = sha_of(b"alpha");
        let pins: Vec<(&str, &str)> =
            vec![("a.onnx", a_sha.as_str()), ("gone.onnx", a_sha.as_str())];
        let err =
            extract_pinned_files(&tarball, "root", &pins, &dir.path().join("s")).unwrap_err();
        assert!(err.to_string().contains("missing gone.onnx"), "{err}");
    }
}

#[cfg(test)]
mod checksum_tests {
    use super::*;

    #[test]
    fn checksums_cover_first_run_offered_sizes() {
        // These sizes have pinned SHAs.
        for size in ["base.en", "base", "small.en", "small"] {
            assert!(expected_sha256(size).is_some(), "missing checksum for {size}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_expected_sizes() {
        assert!(crate::registry::whisper_row_by_size("base.en").is_some());
        assert!(crate::registry::whisper_row_by_size("large-v3-turbo").is_some());
        assert!(crate::registry::whisper_row_by_size("gigantic").is_none());
    }

    #[test]
    fn every_whisper_row_has_a_sha_pin() {
        for row in crate::registry::whisper_rows() {
            let size = row.whisper_size().expect("whisper row id shape");
            let pin = expected_sha256(size);
            assert!(pin.is_some(), "model {size} has no sha pin");
            assert_eq!(pin.unwrap().len(), 64, "model {size} pin is not a sha256 hex");
        }
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
        // Every model is SHA-pinned, so the stub bytes carry their own
        // checksum to pass verification.
        let sum = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(&bytes);
            format!("{:x}", h.finalize())
        };
        let opts = DownloadOpts {
            source: Some(DownloadSource::Stub {
                bytes,
                chunk_delay_ms: 0,
            }),
            progress: Some(cb),
            verify_sha256: Some(sum),
            ..Default::default()
        };
        download_ggml_model_opts("tiny", dir.path(), opts).unwrap();
        let v = observations.lock().unwrap().clone();
        assert!(!v.is_empty());
        assert!(v.windows(2).all(|w| w[0] <= w[1]));
    }
}
