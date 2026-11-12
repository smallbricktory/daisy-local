// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Download whisper.cpp GGML model files from Hugging Face.

use anyhow::{bail, Context};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

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
    /// Tests only — the status a [`DownloadSource::Stub`] answers a range
    /// request with. `None` reads as `200`.
    pub stub_status: Option<u16>,
    /// Tests only — receives the byte offset the attempt resumed from.
    pub stub_offset: Option<Arc<AtomicU64>>,
}

/// What holds a model id: a download writing its files, or a delete removing
/// them.
#[derive(Clone, Copy, PartialEq)]
enum Holder {
    Download,
    Delete,
}

/// The model ids held right now, and the signal a release sends.
fn installing() -> &'static (Mutex<HashMap<String, Holder>>, Condvar) {
    static INSTALLING: OnceLock<(Mutex<HashMap<String, Holder>>, Condvar)> = OnceLock::new();
    INSTALLING.get_or_init(|| (Mutex::new(HashMap::new()), Condvar::new()))
}

/// Holds one model id against every other install path in this process — the
/// download that writes its files, the delete that removes them. Dropping it
/// releases the id.
pub struct InstallGuard(String);

impl Drop for InstallGuard {
    fn drop(&mut self) {
        let (held, released) = installing();
        held.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
        released.notify_all();
    }
}

/// Waits until `id` is free, then holds it for `holder`. Waiting out a
/// download takes as long as the download; `refuse_download` returns `None`
/// instead.
fn hold(id: &str, holder: Holder, refuse_download: bool) -> Option<InstallGuard> {
    let (held, released) = installing();
    let mut held = held.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        match held.get(id) {
            None => {
                held.insert(id.to_string(), holder);
                return Some(InstallGuard(id.to_string()));
            }
            Some(Holder::Download) if refuse_download => return None,
            Some(_) => held = released.wait(held).unwrap_or_else(|e| e.into_inner()),
        }
    }
}

/// Waits for `id` to be free, then holds it for a download.
pub fn install_guard(id: &str) -> InstallGuard {
    hold(id, Holder::Download, false).expect("a waiting hold always gets the id")
}

/// Holds `id` for a delete, waiting out another delete. `None` while a
/// download holds it.
pub fn delete_guard(id: &str) -> Option<InstallGuard> {
    hold(id, Holder::Delete, true)
}

/// Registry pin for `ggml-<size>.bin` (whisper rows are single-file).
fn expected_sha256(size: &str) -> Option<&'static str> {
    crate::registry::whisper_row_by_size(size).map(|r| r.files[0].1)
}

/// Registry byte size of `ggml-<size>.bin`.
fn expected_bytes(size: &str) -> Option<u64> {
    crate::registry::whisper_row_by_size(size).map(|r| r.files[0].2)
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
    let _guard = install_guard(&file_name);

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
    let max_bytes = if opts.verify_sha256.is_some() { None } else { expected_bytes(size) };
    let want = opts.verify_sha256.clone().or_else(|| expected_sha256(size).map(str::to_owned));
    download_with_retry(&url, &tmp_path, &dest_path, max_bytes, want.as_deref(), &mut opts)
}

/// Fetch `url` into `dest_path` (via `tmp_path`), retrying transient errors
/// with backoff. A failed attempt leaves the `.part` file on disk for the
/// next attempt to resume with a range request. The `.part` is deleted on
/// cancel, when the body outgrows `max_bytes`, or when its bytes miss the
/// `want` sha256, which is checked before the rename.
fn download_with_retry(
    url: &str,
    tmp_path: &Path,
    dest_path: &Path,
    max_bytes: Option<u64>,
    want: Option<&str>,
    opts: &mut DownloadOpts,
) -> anyhow::Result<PathBuf> {
    let retries = 3;
    let mut attempt = 0;
    loop {
        attempt += 1;
        match download_attempt(url, tmp_path, dest_path, max_bytes, want, opts) {
            Ok(p) => return Ok(p),
            Err(e) if attempt < retries && is_retryable(&e) => {
                let backoff = std::time::Duration::from_millis(250 * (1u64 << (attempt - 1)));
                std::thread::sleep(backoff);
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

#[derive(Debug)]
struct Transient(String);

impl std::fmt::Display for Transient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Transient {}

/// Builds an error the retry loop will try again.
fn transient(msg: impl std::fmt::Display) -> anyhow::Error {
    anyhow::Error::new(Transient(msg.to_string()))
}

fn is_retryable(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<Transient>())
}

/// The opening words of every pin-check failure in this module.
const SHA256_MISMATCH: &str = "sha256 mismatch";
/// The opening words of a body that outgrew its pinned byte size.
const SIZE_MISMATCH: &str = "size mismatch";

/// Whether a failed download's bytes did not match their pinned sha256 or
/// byte size.
pub fn is_integrity_failure(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains(SHA256_MISMATCH) || m.contains(SIZE_MISMATCH)
}

/// Fetch every `(name, sha256, bytes)` under `base_url` into `stage`,
/// verifying each pin as it lands; a body longer than its pinned size stops
/// at the limit. A file already present with the right pin is kept, so a
/// retry resumes after the shards that finished. Progress is reported as the
/// running byte count across files against `total_bytes`. `source_for` gives
/// tests a stub body per file; production returns `None` (HTTP).
pub fn download_pinned_files(
    base_url: &str,
    files: &[(&str, &str, u64)],
    stage: &Path,
    total_bytes: u64,
    opts: &mut DownloadOpts,
    mut source_for: impl FnMut(&str) -> Option<DownloadSource>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(stage)?;
    let outer = Arc::new(Mutex::new(opts.progress.take()));
    let base = Arc::new(AtomicU64::new(0));
    let (outer_c, base_c) = (outer.clone(), base.clone());
    opts.progress = Some(Box::new(move |d, _| {
        if let Some(cb) = outer_c.lock().unwrap().as_mut() {
            cb(base_c.load(Ordering::SeqCst) + d, Some(total_bytes));
        }
    }));
    let cancel = opts.cancel.clone();
    for (name, want, bytes) in files {
        let dest = stage.join(name);
        if dest.is_file() && file_sha256_until(&dest, cancel.as_deref())? == *want {
            base.fetch_add(std::fs::metadata(&dest)?.len(), Ordering::SeqCst);
            continue;
        }
        let part = stage.join(format!("{name}.part"));
        opts.source = source_for(name);
        download_with_retry(&format!("{base_url}/{name}"), &part, &dest, Some(*bytes), Some(want), opts)?;
        base.fetch_add(std::fs::metadata(&dest)?.len(), Ordering::SeqCst);
    }
    if let Some(cb) = outer.lock().unwrap().as_mut() {
        cb(base.load(Ordering::SeqCst), Some(total_bytes));
    }
    opts.progress = outer.lock().unwrap().take();
    Ok(())
}

/// Download + install a pinned file set from `base_url` under
/// `parent/<id>/`: files go to a staging dir, each pin verified, then the dir
/// is renamed into place. The staging dir survives a failure so the next
/// attempt resumes. A cancel seen before the rename installs nothing.
/// Returns the installed dir. `source_for` gives tests a stub body per file;
/// production passes `|_| None` (HTTP).
pub(crate) fn download_pinned_dir_opts(
    id: &str,
    files: &[(&str, &str, u64)],
    total_bytes: u64,
    base_url: &str,
    parent: &Path,
    mut opts: DownloadOpts,
    source_for: impl FnMut(&str) -> Option<DownloadSource>,
) -> anyhow::Result<PathBuf> {
    let dest = parent.join(id);
    if crate::registry::files_intact_in(&dest, files) {
        return Ok(dest);
    }
    std::fs::create_dir_all(parent)?;
    let stage = parent.join(format!("{id}.staging"));
    download_pinned_files(base_url, files, &stage, total_bytes, &mut opts, source_for)?;
    if is_cancelled(opts.cancel.as_deref()) {
        bail!("{CANCELLED}");
    }
    let _ = syncsafe::remove_dir_all(&dest);
    syncsafe::rename(&stage, &dest)
        .with_context(|| format!("rename {} -> {}", stage.display(), dest.display()))?;
    Ok(dest)
}

/// Download + install one LLM row's shards from `base_url` under
/// `llm_parent/<row.id>/`. Returns the first shard's path (what the worker
/// loads), via [`download_pinned_dir_opts`]; production enters through
/// [`download_llm_model_opts`].
pub(crate) fn download_llm_row_opts(
    row: &crate::registry::ModelRow,
    base_url: &str,
    llm_parent: &Path,
    opts: DownloadOpts,
    source_for: impl FnMut(&str) -> Option<DownloadSource>,
) -> anyhow::Result<PathBuf> {
    download_pinned_dir_opts(row.id, row.files, row.bytes, base_url, llm_parent, opts, source_for)?;
    Ok(row.engine_path(llm_parent).expect("llm rows have a path"))
}

/// Download + install the Nemotron model dir from
/// [`registry::NEMOTRON_RELEASE_BASE`](crate::registry::NEMOTRON_RELEASE_BASE)
/// under `nemotron_parent/<NEMOTRON_ID>/`; returns that dir.
pub fn download_nemotron_model_opts(
    nemotron_parent: &Path,
    opts: DownloadOpts,
) -> anyhow::Result<PathBuf> {
    use crate::registry::{NEMOTRON_BYTES, NEMOTRON_FILES, NEMOTRON_ID, NEMOTRON_RELEASE_BASE};
    let _guard = install_guard(NEMOTRON_ID);
    download_pinned_dir_opts(
        NEMOTRON_ID,
        NEMOTRON_FILES,
        NEMOTRON_BYTES,
        NEMOTRON_RELEASE_BASE,
        nemotron_parent,
        opts,
        |_| None,
    )
}

/// Download + install a built-in LLM row by id: the production entry point,
/// resolving the row from the registry and fetching its shards over HTTP
/// from [`registry::LLM_RELEASE_BASE`].
pub fn download_llm_model_opts(
    id: &str,
    llm_parent: &Path,
    opts: DownloadOpts,
) -> anyhow::Result<PathBuf> {
    use crate::registry;
    let row = registry::llm_row(id).ok_or_else(|| anyhow::anyhow!("unknown LLM model {id:?}"))?;
    download_llm_row_opts(row, registry::LLM_RELEASE_BASE, llm_parent, opts, |_| None)
}

fn file_sha256(p: &Path) -> anyhow::Result<String> {
    file_sha256_until(p, None)
}

/// The message a download stopped by its cancel flag ends with.
const CANCELLED: &str = "download cancelled by user";

fn is_cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(Ordering::SeqCst))
}

/// [`file_sha256`] that stops with [`CANCELLED`] once `cancel` is set.
fn file_sha256_until(p: &Path, cancel: Option<&AtomicBool>) -> anyhow::Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        if is_cancelled(cancel) {
            bail!("{CANCELLED}");
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// Redirects stay on https: a hop to any other scheme ends the request.
fn https_redirects_only() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.url().scheme() != "https" {
            let to = attempt.url().to_string();
            attempt.error(format!("refused a redirect off https to {to}"))
        } else if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else {
            attempt.follow()
        }
    })
}

fn download_attempt(
    url: &str,
    tmp_path: &Path,
    dest_path: &Path,
    max_bytes: Option<u64>,
    want: Option<&str>,
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

    let mut resume_from = std::fs::metadata(tmp_path).map(|m| m.len()).unwrap_or(0);
    // A `.part` holding every pinned byte is renamed into place once it
    // matches its pin; one that misses the pin is dropped and the file
    // fetched from byte 0.
    if resume_from > 0 && max_bytes == Some(resume_from) {
        if verify_part(tmp_path, want, opts.cancel.as_deref())?.is_ok() {
            return install_part(tmp_path, dest_path);
        }
        let _ = std::fs::remove_file(tmp_path);
        resume_from = 0;
    }
    let stub_status = opts.stub_status.unwrap_or(200);

    let (mut body, total, append): (BodyReader, Option<u64>, bool) =
        match opts.source.take().unwrap_or_default() {
            DownloadSource::Http => {
                let client = reqwest::blocking::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(30))
                    .timeout(std::time::Duration::from_secs(60 * 30))
                    .redirect(https_redirects_only())
                    .build()
                    .context("build HTTP client")?;
                let mut req = client.get(url);
                if resume_from > 0 {
                    req = req.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
                }
                let resp = req.send().map_err(|e| {
                    if e.is_timeout() || e.is_connect() || e.is_request() {
                        transient(format!("GET {url}: {e}"))
                    } else {
                        anyhow::Error::new(e).context(format!("GET {url}"))
                    }
                })?;
                if resume_from > 0 && resp.status().as_u16() == 416 {
                    let _ = std::fs::remove_file(tmp_path);
                    return Err(transient(format!(
                        "GET {url} returned 416 Range Not Satisfiable; retrying from byte 0"
                    )));
                }
                let status = resp.status();
                let resp = resp.error_for_status().map_err(|e| {
                    if status.is_server_error() {
                        transient(format!("GET {url} returned {status}"))
                    } else {
                        anyhow::Error::new(e).context(format!("GET {url} returned an error status"))
                    }
                })?;
                let partial = resp.status().as_u16() == 206 && resume_from > 0;
                let total = resp
                    .content_length()
                    .map(|n| if partial { n + resume_from } else { n });
                (BodyReader::Http(resp), total, partial)
            }
            DownloadSource::Stub { bytes, chunk_delay_ms } => {
                let partial = stub_status == 206 && resume_from > 0;
                let pos = if partial { resume_from as usize } else { 0 };
                let total = Some(bytes.len() as u64);
                (BodyReader::Stub { bytes, pos, chunk_delay_ms }, total, partial)
            }
        };

    if let Some(seen) = opts.stub_offset.as_ref() {
        seen.store(if append { resume_from } else { 0 }, Ordering::SeqCst);
    }

    let mut file = if append {
        std::fs::OpenOptions::new()
            .append(true)
            .open(tmp_path)
            .with_context(|| format!("append {}", tmp_path.display()))?
    } else {
        std::fs::File::create(tmp_path)
            .with_context(|| format!("create {}", tmp_path.display()))?
    };

    let mut buf = [0u8; 64 * 1024];
    let mut written: u64 = if append { resume_from } else { 0 };
    let mut last_reported: u64 = written;
    const REPORT_EVERY: u64 = 512 * 1024;
    loop {
        if is_cancelled(opts.cancel.as_deref()) {
            drop(file);
            let _ = std::fs::remove_file(tmp_path);
            bail!("{CANCELLED}");
        }
        let n = body
            .read(&mut buf)
            .map_err(|e| transient(format!("read response body: {e}")))?;
        if n == 0 {
            break;
        }
        if let Some(max) = max_bytes.filter(|max| written + n as u64 > *max) {
            drop(file);
            let _ = std::fs::remove_file(tmp_path);
            bail!("{SIZE_MISMATCH}: {url} sent more than its pinned {max} bytes");
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
    if let Err(got) = verify_part(tmp_path, want, opts.cancel.as_deref())? {
        let _ = std::fs::remove_file(tmp_path);
        let name = dest_path.file_name().unwrap_or_default().to_string_lossy();
        if append {
            return Err(transient(format!(
                "{name} resumed from byte {resume_from} did not match its pin; retrying from byte 0"
            )));
        }
        bail!("{SHA256_MISMATCH} for {name}: got {got}, want {}", want.unwrap_or_default());
    }
    install_part(tmp_path, dest_path)
}

/// Whether the `.part` at `tmp_path` matches the `want` sha256: `Ok(())`
/// when it does or no pin is given, `Err(got)` with its digest when it
/// misses. Hashing stops with [`CANCELLED`] once `cancel` is set.
fn verify_part(
    tmp_path: &Path,
    want: Option<&str>,
    cancel: Option<&AtomicBool>,
) -> anyhow::Result<Result<(), String>> {
    let Some(want) = want else { return Ok(Ok(())) };
    let got = file_sha256_until(tmp_path, cancel)?;
    Ok(if got == want { Ok(()) } else { Err(got) })
}

/// Renames a finished `.part` into place. A failed rename leaves the `.part`
/// on disk for the next attempt to install.
fn install_part(tmp_path: &Path, dest_path: &Path) -> anyhow::Result<PathBuf> {
    syncsafe::rename(tmp_path, dest_path).with_context(|| {
        format!(
            "rename {} -> {}",
            tmp_path.display(),
            dest_path.display()
        )
    })?;
    Ok(dest_path.to_path_buf())
}

#[cfg(test)]
mod install_guard_tests {
    use super::*;

    #[test]
    fn a_download_refuses_a_delete_until_it_releases() {
        let downloading = install_guard("guard-test-download");
        assert!(delete_guard("guard-test-download").is_none());
        assert!(delete_guard("guard-test-other").is_some(), "another id is free");
        drop(downloading);
        assert!(delete_guard("guard-test-download").is_some());
    }

    #[test]
    fn a_delete_waits_out_another_delete() {
        let first = delete_guard("guard-test-delete").unwrap();
        let second = std::thread::spawn(|| {
            let _g = delete_guard("guard-test-delete").expect("a delete waits, it does not refuse");
            std::time::Instant::now()
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        let released = std::time::Instant::now();
        drop(first);
        assert!(
            second.join().unwrap() >= released,
            "the second delete ran before the first released"
        );
    }

    #[test]
    fn a_download_waits_out_the_id_it_needs() {
        let first = install_guard("guard-test-wait");
        let second = std::thread::spawn(|| {
            let _g = install_guard("guard-test-wait");
            std::time::Instant::now()
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        let released = std::time::Instant::now();
        drop(first);
        assert!(
            second.join().unwrap() >= released,
            "the second download was handed the id before the release"
        );
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
    use std::sync::{Arc, Mutex};

    /// The io error a failed write produced is the last link of the chain:
    /// `{e}` prints the outermost context alone, `{e:#}` prints the chain.
    #[cfg(unix)]
    #[test]
    fn a_write_failure_names_the_io_error_only_in_the_alternate_form() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("models");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o555)).unwrap();
        let opts = DownloadOpts {
            source: Some(DownloadSource::Stub { bytes: vec![0u8; 16], chunk_delay_ms: 0 }),
            ..Default::default()
        };
        let err = download_ggml_model_opts("base.en", &dest, opts).unwrap_err();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            !err.to_string().contains("Permission denied"),
            "plain Display carries the outermost context alone: {err}"
        );
        assert!(
            format!("{err:#}").contains("Permission denied"),
            "the alternate form must carry the io error: {err:#}"
        );
    }

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
        // The stub bytes carry their own checksum.
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

    #[test]
    fn pinned_files_download_in_order_verify_each_pin_and_report_aggregate_progress() {
        use sha2::Digest;
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        let a = vec![1u8; 300_000];
        let b = vec![2u8; 200_000];
        let sha = |v: &[u8]| format!("{:x}", Sha256::digest(v));
        let sha_a = sha(&a);
        let sha_b = sha(&b);
        let files: Vec<(&str, &str, u64)> =
            vec![("part-a.bin", sha_a.as_str(), 300_000), ("part-b.bin", sha_b.as_str(), 200_000)];
        let seen = Arc::new(Mutex::new(Vec::<(u64, Option<u64>)>::new()));
        let seen_c = seen.clone();
        let mut opts = DownloadOpts {
            progress: Some(Box::new(move |d, t| seen_c.lock().unwrap().push((d, t)))),
            ..Default::default()
        };
        let stubs = std::collections::HashMap::from([("part-a.bin", a.clone()), ("part-b.bin", b.clone())]);
        download_pinned_files("http://unused", &files, &stage, 500_000, &mut opts, |name| {
            stubs.get(name).map(|bytes| DownloadSource::Stub { bytes: bytes.clone(), chunk_delay_ms: 0 })
        })
        .unwrap();
        assert_eq!(std::fs::read(stage.join("part-a.bin")).unwrap(), a);
        assert_eq!(std::fs::read(stage.join("part-b.bin")).unwrap(), b);
        let s = seen.lock().unwrap();
        assert!(s.iter().all(|(_, t)| *t == Some(500_000)), "{s:?}");
        assert_eq!(s.last().unwrap().0, 500_000, "{s:?}");
        assert!(s.windows(2).all(|w| w[0].0 <= w[1].0), "monotone: {s:?}");
    }

    #[test]
    fn pinned_files_pin_mismatch_removes_that_file_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        let files: Vec<(&str, &str, u64)> = vec![("part-a.bin", "00".repeat(32).leak(), 10)];
        let mut opts = DownloadOpts::default();
        let err = download_pinned_files("http://unused", &files, &stage, 10, &mut opts, |_| {
            Some(DownloadSource::Stub { bytes: vec![9u8; 10], chunk_delay_ms: 0 })
        })
        .unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err}");
        assert!(!stage.join("part-a.bin").exists());
    }

    #[test]
    fn a_body_longer_than_its_pinned_size_stops_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        let body = vec![5u8; 200_000];
        let sha: &'static str = format!("{:x}", Sha256::digest(&body)).leak();
        let files: Vec<(&str, &str, u64)> = vec![("part-a.bin", sha, 100_000)];
        let mut opts = DownloadOpts::default();
        let err = download_pinned_files("http://unused", &files, &stage, 100_000, &mut opts, |_| {
            Some(DownloadSource::Stub { bytes: body.clone(), chunk_delay_ms: 0 })
        })
        .unwrap_err();
        assert!(err.to_string().contains("size mismatch"), "{err}");
        assert!(is_integrity_failure(&err.to_string()), "the queue does not retry it");
        assert!(!stage.join("part-a.bin").exists());
        assert!(!stage.join("part-a.bin.part").exists(), "the oversized part is removed");
    }

    #[test]
    fn a_body_of_exactly_its_pinned_size_installs() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        let body = vec![6u8; 100_000];
        let sha: &'static str = format!("{:x}", Sha256::digest(&body)).leak();
        let files: Vec<(&str, &str, u64)> = vec![("part-a.bin", sha, 100_000)];
        let mut opts = DownloadOpts::default();
        download_pinned_files("http://unused", &files, &stage, 100_000, &mut opts, |_| {
            Some(DownloadSource::Stub { bytes: body.clone(), chunk_delay_ms: 0 })
        })
        .unwrap();
        assert_eq!(std::fs::read(stage.join("part-a.bin")).unwrap(), body);
    }

    #[test]
    fn a_redirect_off_https_is_refused() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("model.bin.part");
        let dest = dir.path().join("model.bin");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = sock.read(&mut chunk).unwrap();
                req.extend_from_slice(&chunk[..n]);
                if n == 0 || req.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let resp = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/elsewhere.bin\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            sock.write_all(resp.as_bytes()).unwrap();
        });

        let url = format!("http://127.0.0.1:{port}/model.bin");
        let mut opts = DownloadOpts::default();
        let err = download_with_retry(&url, &tmp, &dest, None, None, &mut opts).unwrap_err();
        server.join().unwrap();
        assert!(format!("{err:#}").contains("refused a redirect off https"), "{err:#}");
        assert!(!dest.exists());
    }

    #[test]
    fn llm_download_installs_under_llm_dir_and_returns_the_first_shard() {
        // The row's file names are exercised through a temp layout that
        // `installed_in` accepts.
        let dir = tempfile::tempdir().unwrap();
        let row = crate::registry::llm_row("gemma-4-e4b-it-q4").unwrap();
        let d = dir.path().join(row.id);
        std::fs::create_dir_all(&d).unwrap();
        for (n, _, size) in row.files {
            std::fs::File::create(d.join(n)).unwrap().set_len(*size).unwrap();
        }
        let p = download_llm_model_opts(row.id, dir.path(), DownloadOpts::default()).unwrap();
        assert_eq!(p, d.join(row.files[0].0));
    }

    /// A minimal LLM row for exercising the actual-download branch of
    /// `download_llm_row_opts` through a stub source.
    fn fake_llm_row(a: &[u8], b: &[u8]) -> crate::registry::ModelRow {
        use crate::registry::{Engine, ModelRow};
        let sha_a: &'static str = format!("{:x}", Sha256::digest(a)).leak();
        let sha_b: &'static str = format!("{:x}", Sha256::digest(b)).leak();
        ModelRow {
            id: "fake-llm",
            engine: Engine::Llm,
            display: "Fake LLM",
            bytes: (a.len() + b.len()) as u64,
            multilingual: false,
            license: "test",
            license_url: "",
            files: vec![("part-a.bin", sha_a, a.len() as u64), ("part-b.bin", sha_b, b.len() as u64)].leak(),
            floor_on: &[],
        }
    }

    #[test]
    fn llm_row_download_fetches_shards_replaces_a_stale_dest_and_leaves_no_staging_dir() {
        let dir = tempfile::tempdir().unwrap();
        let a = vec![1u8; 1000];
        let b = vec![2u8; 500];
        let row = fake_llm_row(&a, &b);

        // A dest dir from an earlier install.
        let dest = dir.path().join(row.id);
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("stale.txt"), b"old").unwrap();

        let stubs = std::collections::HashMap::from([
            ("part-a.bin", a.clone()),
            ("part-b.bin", b.clone()),
        ]);
        let p = download_llm_row_opts(&row, "http://unused", dir.path(), DownloadOpts::default(), |name| {
            stubs.get(name).map(|bytes| DownloadSource::Stub { bytes: bytes.clone(), chunk_delay_ms: 0 })
        })
        .unwrap();

        assert_eq!(p, dest.join("part-a.bin"));
        assert_eq!(std::fs::read(dest.join("part-a.bin")).unwrap(), a);
        assert_eq!(std::fs::read(dest.join("part-b.bin")).unwrap(), b);
        assert!(!dest.join("stale.txt").exists(), "stale dest contents were not replaced");
        assert!(!dir.path().join("fake-llm.staging").exists(), "staging dir left behind");
    }

    #[test]
    fn a_cancel_after_the_last_byte_installs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let a = vec![1u8; 1000];
        let b = vec![2u8; 500];
        let row = fake_llm_row(&a, &b);
        let total = row.bytes;
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let opts = DownloadOpts {
            cancel: Some(cancel.clone()),
            progress: Some(Box::new(move |done, _| {
                if done >= total {
                    flag.store(true, Ordering::SeqCst);
                }
            })),
            ..Default::default()
        };
        let stubs = std::collections::HashMap::from([
            ("part-a.bin", a.clone()),
            ("part-b.bin", b.clone()),
        ]);
        let r = download_llm_row_opts(&row, "http://unused", dir.path(), opts, |name| {
            stubs.get(name).map(|bytes| DownloadSource::Stub { bytes: bytes.clone(), chunk_delay_ms: 0 })
        });
        assert!(cancel.load(Ordering::SeqCst), "the cancel landed");
        assert!(r.is_err(), "a cancelled download reports it: {r:?}");
        assert!(!row.installed_in(dir.path()), "a cancelled download must not install");
    }

    #[test]
    fn llm_row_download_pin_mismatch_keeps_the_verified_shard_staged_for_resume() {
        let dir = tempfile::tempdir().unwrap();
        let a = vec![1u8; 1000];
        let b = vec![2u8; 500];
        let row = fake_llm_row(&a, &b);
        let wrong_b = vec![9u8; 500]; // does not match row's pin for part-b.bin

        let stubs = std::collections::HashMap::from([
            ("part-a.bin", a.clone()),
            ("part-b.bin", wrong_b),
        ]);
        let err = download_llm_row_opts(&row, "http://unused", dir.path(), DownloadOpts::default(), |name| {
            stubs.get(name).map(|bytes| DownloadSource::Stub { bytes: bytes.clone(), chunk_delay_ms: 0 })
        })
        .unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err}");

        let stage = dir.path().join("fake-llm.staging");
        assert_eq!(std::fs::read(stage.join("part-a.bin")).unwrap(), a, "verified shard not kept for resume");
        assert!(!stage.join("part-b.bin").exists(), "mismatched shard should be removed");
        assert!(!dir.path().join("fake-llm").exists(), "dest must not appear until every pin verifies");
    }

    #[test]
    fn pinned_dir_download_fetches_every_file_and_leaves_no_staging_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (vec![3u8; 700], vec![4u8; 300]);
        let sha_a = format!("{:x}", Sha256::digest(&a));
        let sha_b = format!("{:x}", Sha256::digest(&b));
        let files = [("encoder.int8.onnx", sha_a.as_str(), 700), ("tokens.txt", sha_b.as_str(), 300)];
        let stubs = std::collections::HashMap::from([
            ("encoder.int8.onnx", a.clone()),
            ("tokens.txt", b.clone()),
        ]);
        let dest = download_pinned_dir_opts(
            "fake-asr",
            &files,
            1_000,
            "http://unused",
            dir.path(),
            DownloadOpts::default(),
            |name| stubs.get(name).map(|bytes| DownloadSource::Stub { bytes: bytes.clone(), chunk_delay_ms: 0 }),
        )
        .unwrap();
        assert_eq!(dest, dir.path().join("fake-asr"));
        assert_eq!(std::fs::read(dest.join("encoder.int8.onnx")).unwrap(), a);
        assert_eq!(std::fs::read(dest.join("tokens.txt")).unwrap(), b);
        assert!(!dir.path().join("fake-asr.staging").exists());
    }

    #[test]
    fn nemotron_download_returns_the_installed_dir_without_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join(crate::registry::NEMOTRON_ID);
        std::fs::create_dir_all(&d).unwrap();
        for (n, _, size) in crate::registry::NEMOTRON_FILES {
            std::fs::File::create(d.join(n)).unwrap().set_len(*size).unwrap();
        }
        let p = download_nemotron_model_opts(dir.path(), DownloadOpts::default()).unwrap();
        assert_eq!(p, d);
    }

    #[test]
    fn pinned_dir_with_a_zero_byte_file_is_fetched_again() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (vec![1u8; 1000], vec![2u8; 500]);
        let row = fake_llm_row(&a, &b);
        let d = dir.path().join(row.id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("part-a.bin"), &a).unwrap();
        std::fs::write(d.join("part-b.bin"), b"").unwrap();
        let stubs = std::collections::HashMap::from([("part-a.bin", a.clone()), ("part-b.bin", b.clone())]);
        download_llm_row_opts(&row, "http://unused", dir.path(), DownloadOpts::default(), |n| {
            stubs.get(n).map(|bytes| DownloadSource::Stub { bytes: bytes.clone(), chunk_delay_ms: 0 })
        })
        .unwrap();
        assert!(row.installed_in(dir.path()));
        assert_eq!(std::fs::read(d.join("part-b.bin")).unwrap(), b);
    }

    /// A `.part` left by a killed app is resumed: the request carries the
    /// byte offset and a 206 body is appended to what is already there.
    #[test]
    fn range_resume_appends_after_206() {
        use std::sync::atomic::AtomicU64;
        let dir = tempfile::tempdir().unwrap();
        let head = vec![7u8; 1000];
        let tail = vec![9u8; 1000];
        let mut full = head.clone();
        full.extend_from_slice(&tail);
        std::fs::write(dir.path().join("ggml-base.en.bin.part"), &head).unwrap();
        let sum = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(&full);
            format!("{:x}", h.finalize())
        };
        let seen = Arc::new(AtomicU64::new(u64::MAX));
        let opts = DownloadOpts {
            source: Some(DownloadSource::Stub { bytes: full.clone(), chunk_delay_ms: 0 }),
            verify_sha256: Some(sum),
            stub_status: Some(206),
            stub_offset: Some(seen.clone()),
            ..Default::default()
        };
        let got = download_ggml_model_opts("base.en", dir.path(), opts).unwrap();
        assert_eq!(seen.load(Ordering::SeqCst), 1000, "the request must name the resume offset");
        assert_eq!(std::fs::read(&got).unwrap(), full);
    }

    /// A retryable failure on the first attempt leaves the `.part` alone, so
    /// the retry resumes it with a range request.
    #[test]
    fn retryable_failure_keeps_the_part_for_the_retrys_range_resume() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let dir = tempfile::tempdir().unwrap();
        let head = vec![7u8; 500];
        let tail = vec![9u8; 500];
        let mut full = head.clone();
        full.extend_from_slice(&tail);
        let tmp = dir.path().join("ggml-base.en.bin.part");
        std::fs::write(&tmp, &head).unwrap();
        let dest = dir.path().join("ggml-base.en.bin");

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let range_seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let range_seen_c = range_seen.clone();
        let tail_c = tail.clone();
        let server = std::thread::spawn(move || {
            // First connection: read whatever the client sent, then drop the
            // socket without a response — a connection reset.
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf);
            drop(sock);

            // Second connection: capture the Range header, answer 206 with
            // the remaining bytes.
            let (mut sock, _) = listener.accept().unwrap();
            let mut req = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = sock.read(&mut chunk).unwrap();
                req.extend_from_slice(&chunk[..n]);
                if n == 0 || req.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&req);
            *range_seen_c.lock().unwrap() = text
                .lines()
                .find(|l| l.to_lowercase().starts_with("range:"))
                .map(|l| l.trim().to_string());
            let resp = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes 500-999/1000\r\nConnection: close\r\n\r\n",
                tail_c.len()
            );
            sock.write_all(resp.as_bytes()).unwrap();
            sock.write_all(&tail_c).unwrap();
        });

        let url = format!("http://127.0.0.1:{port}/ggml-base.en.bin");
        let mut opts = DownloadOpts::default();
        let got = download_with_retry(&url, &tmp, &dest, None, None, &mut opts).unwrap();
        server.join().unwrap();

        assert_eq!(std::fs::read(&got).unwrap(), full, "the retry must resume, not restart");
        let range = range_seen.lock().unwrap().clone().expect("no request reached the server");
        assert!(range.to_lowercase().contains("bytes=500-"), "range not resumed: {range}");
    }

    /// A `.part` past the object's length draws a 416 on the range request.
    /// The retry drops the `.part` and asks for the whole object again, with
    /// no `Range` header.
    #[test]
    fn a_416_response_restarts_from_byte_zero() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let dir = tempfile::tempdir().unwrap();
        let head = vec![7u8; 500];
        let mut full = head.clone();
        full.extend_from_slice(&vec![9u8; 500]);
        let tmp = dir.path().join("ggml-base.en.bin.part");
        std::fs::write(&tmp, &head).unwrap();
        let dest = dir.path().join("ggml-base.en.bin");

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let ranges_seen: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let ranges_seen_c = ranges_seen.clone();
        let full_c = full.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut sock, _) = listener.accept().unwrap();
                let mut req = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = sock.read(&mut chunk).unwrap();
                    req.extend_from_slice(&chunk[..n]);
                    if n == 0 || req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&req);
                let range = text
                    .lines()
                    .find(|l| l.to_lowercase().starts_with("range:"))
                    .map(|l| l.trim().to_string());
                let has_range = range.is_some();
                ranges_seen_c.lock().unwrap().push(range);
                if has_range {
                    let resp = "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    sock.write_all(resp.as_bytes()).unwrap();
                } else {
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        full_c.len()
                    );
                    sock.write_all(resp.as_bytes()).unwrap();
                    sock.write_all(&full_c).unwrap();
                }
            }
        });

        let url = format!("http://127.0.0.1:{port}/ggml-base.en.bin");
        let mut opts = DownloadOpts::default();
        let got = download_with_retry(&url, &tmp, &dest, None, None, &mut opts).unwrap();
        server.join().unwrap();

        assert_eq!(std::fs::read(&got).unwrap(), full);
        let seen = ranges_seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "the 416 must not exhaust the retry budget: {seen:?}");
        assert!(seen[0].as_deref().unwrap_or("").to_lowercase().contains("bytes=500-"), "{seen:?}");
        assert!(seen[1].is_none(), "the restart must carry no Range header: {seen:?}");
    }

    /// Serves `body` the way the release CDN does: 200 for a plain GET, 206
    /// for a range inside the object, 416 with `Content-Range: bytes */<len>`
    /// for a range at or past its end. Returns the port and the body bytes
    /// sent so far. The server runs until the test process exits.
    fn serve_like_the_release_cdn(body: Vec<u8>) -> (u16, Arc<std::sync::atomic::AtomicU64>) {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let sent_c = sent.clone();
        std::thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(mut sock) = sock else { continue };
                let mut req = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = sock.read(&mut chunk).unwrap_or(0);
                    req.extend_from_slice(&chunk[..n]);
                    if n == 0 || req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&req).to_lowercase();
                let from = text
                    .lines()
                    .find_map(|l| l.strip_prefix("range: bytes="))
                    .and_then(|r| r.trim().trim_end_matches('-').parse::<usize>().ok());
                let len = body.len();
                let (head, slice) = match from {
                    Some(s) if s >= len => (
                        format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{len}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
                        &body[..0],
                    ),
                    Some(s) => (
                        format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {s}-{}/{len}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            len - 1,
                            len - s
                        ),
                        &body[s..],
                    ),
                    None => (
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"),
                        &body[..],
                    ),
                };
                let _ = sock.write_all(head.as_bytes());
                if sock.write_all(slice).is_ok() {
                    sent_c.fetch_add(slice.len() as u64, Ordering::SeqCst);
                }
            }
        });
        (port, sent)
    }

    /// A file whose rename into place failed is installed by the next
    /// attempt from the `.part` on disk, without a second fetch.
    #[test]
    fn a_finished_part_whose_rename_failed_installs_without_a_second_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let sha: &'static str = format!("{:x}", Sha256::digest(&body)).leak();
        let name = "encoder.int8.onnx.data";
        let files: Vec<(&str, &str, u64)> = vec![(name, sha, body.len() as u64)];
        let (port, sent) = serve_like_the_release_cdn(body.clone());
        let base = format!("http://127.0.0.1:{port}");

        // A directory where the file belongs makes the rename fail.
        let blocker = stage.join(name);
        std::fs::create_dir_all(blocker.join("held")).unwrap();
        let mut opts = DownloadOpts::default();
        let err = download_pinned_files(&base, &files, &stage, body.len() as u64, &mut opts, |_| None)
            .unwrap_err();
        assert!(format!("{err:#}").contains("rename"), "{err:#}");
        let part = stage.join(format!("{name}.part"));
        assert_eq!(std::fs::metadata(&part).unwrap().len(), body.len() as u64, "the body landed");
        assert_eq!(sent.load(Ordering::SeqCst), body.len() as u64);

        std::fs::remove_dir_all(&blocker).unwrap();
        let mut opts = DownloadOpts::default();
        download_pinned_files(&base, &files, &stage, body.len() as u64, &mut opts, |_| None).unwrap();
        assert_eq!(std::fs::read(stage.join(name)).unwrap(), body);
        assert!(!part.exists());
        assert_eq!(
            sent.load(Ordering::SeqCst),
            body.len() as u64,
            "the finished .part was fetched again"
        );
    }

    /// A `.part` of the pinned size whose bytes miss the pin is dropped and
    /// the whole file fetched again in the same run.
    #[test]
    fn a_finished_part_that_misses_its_pin_is_fetched_again_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let sha: &'static str = format!("{:x}", Sha256::digest(&body)).leak();
        let name = "encoder.int8.onnx.data";
        let files: Vec<(&str, &str, u64)> = vec![(name, sha, body.len() as u64)];
        let (port, sent) = serve_like_the_release_cdn(body.clone());
        std::fs::create_dir_all(&stage).unwrap();
        let part = stage.join(format!("{name}.part"));
        std::fs::write(&part, vec![0u8; body.len()]).unwrap();

        let mut opts = DownloadOpts::default();
        download_pinned_files(&format!("http://127.0.0.1:{port}"), &files, &stage, body.len() as u64, &mut opts, |_| None)
            .unwrap();
        assert_eq!(std::fs::read(stage.join(name)).unwrap(), body);
        assert!(!part.exists());
        assert_eq!(sent.load(Ordering::SeqCst), body.len() as u64, "one whole fetch");
    }

    /// A `.part` whose earlier bytes are wrong misses the pin once the
    /// resumed tail lands. The retry drops it and fetches from byte 0.
    #[test]
    fn a_resumed_part_that_misses_its_pin_restarts_from_byte_zero() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let sha: &'static str = format!("{:x}", Sha256::digest(&body)).leak();
        let name = "encoder.int8.onnx.data";
        let files: Vec<(&str, &str, u64)> = vec![(name, sha, body.len() as u64)];
        let (port, sent) = serve_like_the_release_cdn(body.clone());
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join(format!("{name}.part")), vec![0u8; 100_000]).unwrap();

        let mut opts = DownloadOpts::default();
        download_pinned_files(&format!("http://127.0.0.1:{port}"), &files, &stage, body.len() as u64, &mut opts, |_| None)
            .unwrap();
        assert_eq!(std::fs::read(stage.join(name)).unwrap(), body);
        assert_eq!(
            sent.load(Ordering::SeqCst),
            (body.len() - 100_000 + body.len()) as u64,
            "the resumed tail, then the whole file"
        );
    }

    /// The pin is checked on the `.part` before the rename: a body that
    /// misses it fails as a pin mismatch with its destination untouched.
    #[test]
    fn a_body_that_misses_its_pin_never_reaches_its_destination() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        let name = "part-a.bin";
        let files: Vec<(&str, &str, u64)> = vec![(name, "00".repeat(32).leak(), 10)];
        std::fs::create_dir_all(stage.join(name).join("held")).unwrap();
        let mut opts = DownloadOpts::default();
        let err = download_pinned_files("http://unused", &files, &stage, 10, &mut opts, |_| {
            Some(DownloadSource::Stub { bytes: vec![9u8; 10], chunk_delay_ms: 0 })
        })
        .unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err:#}");
        assert!(is_integrity_failure(&err.to_string()));
        assert!(stage.join(name).join("held").is_dir(), "the destination was touched");
        assert!(!stage.join(format!("{name}.part")).exists(), "the bad .part is removed");
    }

    #[test]
    fn digits_in_a_message_do_not_make_a_failure_retryable() {
        let fatal = anyhow::anyhow!("GET http://127.0.0.1:51504/model-416.bin returned 404");
        assert!(!is_retryable(&fatal));

        let server_side = transient("GET http://example.test/m.bin returned 503 Service Unavailable");
        assert!(is_retryable(&server_side));

        // The mark survives the context a caller adds on the way out.
        let wrapped = server_side.context("download ggml-base.en.bin");
        assert!(is_retryable(&wrapped));
    }

    /// A URL that happens to contain the digits "416" but answers a fatal,
    /// non-retryable status is not treated as a range restart.
    #[test]
    fn a_416_looking_url_with_a_different_status_is_not_retried() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("model-416.bin.part");
        let dest = dir.path().join("model-416.bin");

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let connections_c = connections.clone();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            connections_c.fetch_add(1, Ordering::SeqCst);
            let mut chunk = [0u8; 4096];
            let mut req = Vec::new();
            loop {
                let n = sock.read(&mut chunk).unwrap();
                req.extend_from_slice(&chunk[..n]);
                if n == 0 || req.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            sock.write_all(resp.as_bytes()).unwrap();
        });

        let url = format!("http://127.0.0.1:{port}/model-416.bin");
        let mut opts = DownloadOpts::default();
        let err = download_with_retry(&url, &tmp, &dest, None, None, &mut opts).unwrap_err();
        server.join().unwrap();

        assert!(
            !is_retryable(&err),
            "a digit run in the url or its port must not read as a status: {err}"
        );
        assert_eq!(connections.load(Ordering::SeqCst), 1, "no retry should have been attempted");
    }

    /// A server that ignores the range answers 200: the `.part` is truncated
    /// and the whole file is fetched again.
    #[test]
    fn range_ignored_restarts_on_200() {
        use std::sync::atomic::AtomicU64;
        let dir = tempfile::tempdir().unwrap();
        let full = vec![3u8; 2000];
        std::fs::write(dir.path().join("ggml-base.en.bin.part"), vec![0xAAu8; 1000]).unwrap();
        let sum = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(&full);
            format!("{:x}", h.finalize())
        };
        let seen = Arc::new(AtomicU64::new(u64::MAX));
        let opts = DownloadOpts {
            source: Some(DownloadSource::Stub { bytes: full.clone(), chunk_delay_ms: 0 }),
            verify_sha256: Some(sum),
            stub_status: Some(200),
            stub_offset: Some(seen.clone()),
            ..Default::default()
        };
        let got = download_ggml_model_opts("base.en", dir.path(), opts).unwrap();
        assert_eq!(seen.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(&got).unwrap(), full);
    }
}
