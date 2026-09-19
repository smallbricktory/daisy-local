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
    let final_path = download_with_retry(&url, &tmp_path, &dest_path, &mut opts)?;

    // Verify sha256 (pinned or caller-supplied).
    if let Some(want) = opts.verify_sha256.as_deref().or_else(|| expected_sha256(size)) {
        let got = file_sha256(&final_path)?;
        if got != want {
            let _ = std::fs::remove_file(&final_path);
            bail!("{SHA256_MISMATCH} for {size}: got {got}, want {want}");
        }
    }
    Ok(final_path)
}

/// Fetch `url` into `dest_path` (via `tmp_path`), retrying transient errors
/// with backoff. A failed attempt — retried or not — leaves the `.part` file
/// on disk, so the next attempt (or the next launch) resumes it with a range
/// request instead of restarting from zero. `download_attempt` itself still
/// deletes the `.part` when the caller cancels.
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
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

fn is_retryable(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_lowercase();
    s.contains("timed out")
        || s.contains("timeout")
        || s.contains("connection")
        || s.contains("range not satisfiable")
        || s.contains("502")
        || s.contains("503")
        || s.contains("504")
}

/// The opening words of every pin-check failure in this module.
const SHA256_MISMATCH: &str = "sha256 mismatch";

/// Whether a failed download's bytes did not match their pinned sha256. The
/// pin is baked into this build, so the next attempt compares against the
/// same value and ends the same way.
pub fn is_integrity_failure(msg: &str) -> bool {
    msg.to_lowercase().contains(SHA256_MISMATCH)
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
    let _guard = install_guard(id);
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
            bail!("{SHA256_MISMATCH} for {id} tarball: got {got}, want {want}");
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

/// Fetch every `(name, sha256)` under `base_url` into `stage`, verifying each
/// pin as it lands. A file already present with the right pin is kept, so a
/// retry resumes after the shards that finished. Progress is reported as the
/// running byte count across files against `total_bytes`. `source_for` gives
/// tests a stub body per file; production returns `None` (HTTP).
pub fn download_pinned_files(
    base_url: &str,
    files: &[(&str, &str)],
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
    for (name, want) in files {
        let dest = stage.join(name);
        if dest.is_file() && file_sha256(&dest)? == *want {
            base.fetch_add(std::fs::metadata(&dest)?.len(), Ordering::SeqCst);
            continue;
        }
        let part = stage.join(format!("{name}.part"));
        opts.source = source_for(name);
        download_with_retry(&format!("{base_url}/{name}"), &part, &dest, opts)?;
        let got = file_sha256(&dest)?;
        if got != *want {
            let _ = std::fs::remove_file(&dest);
            bail!("{SHA256_MISMATCH} for {name}: got {got}, want {want}");
        }
        base.fetch_add(std::fs::metadata(&dest)?.len(), Ordering::SeqCst);
    }
    if let Some(cb) = outer.lock().unwrap().as_mut() {
        cb(base.load(Ordering::SeqCst), Some(total_bytes));
    }
    opts.progress = outer.lock().unwrap().take();
    Ok(())
}

/// Download + install one LLM row's shards from `base_url` under
/// `llm_parent/<row.id>/`: shards go to a staging dir, each pin verified,
/// then the dir is renamed into place. Returns the first shard's path (what
/// the worker loads). The staging dir survives a failure so the next attempt
/// resumes. `source_for` gives tests a stub body per file; production passes
/// `|_| None` (HTTP), via [`download_llm_model_opts`].
pub(crate) fn download_llm_row_opts(
    row: &crate::registry::ModelRow,
    base_url: &str,
    llm_parent: &Path,
    mut opts: DownloadOpts,
    source_for: impl FnMut(&str) -> Option<DownloadSource>,
) -> anyhow::Result<PathBuf> {
    let _guard = install_guard(row.id);
    if row.installed_in(llm_parent) {
        return Ok(row.engine_path(llm_parent).expect("llm rows have a path"));
    }
    std::fs::create_dir_all(llm_parent)?;
    let stage = llm_parent.join(format!("{}.staging", row.id));
    let dest = llm_parent.join(row.id);
    download_pinned_files(base_url, row.files, &stage, row.bytes, &mut opts, source_for)?;
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&stage, &dest)?;
    Ok(row.engine_path(llm_parent).expect("llm rows have a path"))
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
            bail!("{SHA256_MISMATCH} for {name}: got {got}, want {want}");
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

    let resume_from = std::fs::metadata(tmp_path).map(|m| m.len()).unwrap_or(0);
    let stub_status = opts.stub_status.unwrap_or(200);

    let (mut body, total, append): (BodyReader, Option<u64>, bool) =
        match opts.source.take().unwrap_or_default() {
            DownloadSource::Http => {
                let client = reqwest::blocking::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(30))
                    .timeout(std::time::Duration::from_secs(60 * 30))
                    .build()
                    .context("build HTTP client")?;
                let mut req = client.get(url);
                if resume_from > 0 {
                    req = req.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
                }
                let resp = req.send().with_context(|| format!("GET {url}"))?;
                if resume_from > 0 && resp.status().as_u16() == 416 {
                    let _ = std::fs::remove_file(tmp_path);
                    bail!("GET {url} returned 416 Range Not Satisfiable; retrying from byte 0");
                }
                let resp = resp
                    .error_for_status()
                    .with_context(|| format!("GET {url} returned an error status"))?;
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
        let files: Vec<(&str, &str)> = vec![("part-a.bin", sha_a.as_str()), ("part-b.bin", sha_b.as_str())];
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
        let files: Vec<(&str, &str)> = vec![("part-a.bin", "00".repeat(32).leak())];
        let mut opts = DownloadOpts::default();
        let err = download_pinned_files("http://unused", &files, &stage, 10, &mut opts, |_| {
            Some(DownloadSource::Stub { bytes: vec![9u8; 10], chunk_delay_ms: 0 })
        })
        .unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err}");
        assert!(!stage.join("part-a.bin").exists());
    }

    #[test]
    fn llm_download_installs_under_llm_dir_and_returns_the_first_shard() {
        // Real pins can't be satisfied by a stub; the row's file names are
        // exercised through a temp layout that `installed_in` accepts.
        let dir = tempfile::tempdir().unwrap();
        let row = crate::registry::llm_row("gemma-4-e4b-it-q4").unwrap();
        let d = dir.path().join(row.id);
        std::fs::create_dir_all(&d).unwrap();
        for (n, _) in row.files {
            std::fs::write(d.join(n), b"x").unwrap();
        }
        let p = download_llm_model_opts(row.id, dir.path(), DownloadOpts::default()).unwrap();
        assert_eq!(p, d.join(row.files[0].0));
    }

    /// A minimal LLM row for exercising the actual-download branch of
    /// `download_llm_row_opts` through a stub source (`download_llm_model_opts`
    /// always hits HTTP, so it can't reach this branch in a test).
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
            files: vec![("part-a.bin", sha_a), ("part-b.bin", sha_b)].leak(),
            floor_on: &[],
        }
    }

    #[test]
    fn llm_row_download_fetches_shards_replaces_a_stale_dest_and_leaves_no_staging_dir() {
        let dir = tempfile::tempdir().unwrap();
        let a = vec![1u8; 1000];
        let b = vec![2u8; 500];
        let row = fake_llm_row(&a, &b);

        // A stale dest dir from a previous, differently-shaped install.
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
    /// the retry resumes it with a range request instead of starting over.
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
        let got = download_with_retry(&url, &tmp, &dest, &mut opts).unwrap();
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
        let got = download_with_retry(&url, &tmp, &dest, &mut opts).unwrap();
        server.join().unwrap();

        assert_eq!(std::fs::read(&got).unwrap(), full);
        let seen = ranges_seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "the 416 must not exhaust the retry budget: {seen:?}");
        assert!(seen[0].as_deref().unwrap_or("").to_lowercase().contains("bytes=500-"), "{seen:?}");
        assert!(seen[1].is_none(), "the restart must carry no Range header: {seen:?}");
    }

    /// A URL that happens to contain the digits "416" but answers a fatal,
    /// non-retryable status must not be treated as a range restart: matching
    /// the literal status, not the substring "416", is what makes that safe.
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
        let err = download_with_retry(&url, &tmp, &dest, &mut opts).unwrap_err();
        server.join().unwrap();

        assert!(!is_retryable(&err), "a URL digit run must not read as a 416: {err}");
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
