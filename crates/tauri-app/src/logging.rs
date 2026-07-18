//! Dual-sink logging.
//!
//!   - Console (stderr): WARN+, single-line.
//!   - File: INFO+ (DEBUG+ when `settings.debug_logging = true`), rotated
//!     daily into `<profile>/logs/daisy-<host>-YYYY-MM-DD.log`; files older
//!     than `KEEP_DAYS` are pruned at startup.
//!
//! Whisper.cpp + ggml output is routed through Rust's `log` crate via
//! `whisper_rs::install_logging_hooks()`.
//!
//! Init is safe to call once; subsequent calls return Err without panicking.

use chrono::Local;
use log::LevelFilter;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Number of days of rotated log files kept; older ones are deleted at startup.
const KEEP_DAYS: u64 = 7;

/// Filesystem-safe, lowercased machine-name slug for the log filename.
/// Empty or unknown host → `"host"`.
fn log_host_slug() -> String {
    let raw = sysinfo::System::host_name().unwrap_or_default();
    let slug: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let slug: String = slug.trim_matches('-').chars().take(24).collect();
    if slug.is_empty() {
        "host".to_string()
    } else {
        slug
    }
}

/// Initializes the dual-sink logger. Console at WARN+, file at INFO+ (or
/// DEBUG+ when `debug` is true), rotated daily as
/// `<logs_dir>/daisy-<host>-YYYY-MM-DD.log`. Returns the active log file
/// path for the lifetime of this process.
pub fn init(logs_dir: &Path, debug: bool) -> Result<PathBuf, String> {
    syncsafe::create_dir_all(logs_dir).map_err(|e| format!("create logs dir: {e}"))?;

    let file_level = if debug { LevelFilter::Debug } else { LevelFilter::Info };
    let host = log_host_slug();
    let log_path = logs_dir.join(format!("daisy-{host}-{}.log", Local::now().format("%Y-%m-%d")));

    let console = fern::Dispatch::new()
        .level(LevelFilter::Warn)
        .format(|out, message, record| {
            out.finish(format_args!(
                "[{} {}] {}",
                record.level(),
                record.target(),
                message
            ))
        })
        .chain(std::io::stderr());

    let file = fern::Dispatch::new()
        .level(file_level)
        // Third-party crates are capped at INFO even when debug logging is on.
        .level_for("hyper", LevelFilter::Info)
        .level_for("hyper_util", LevelFilter::Info)
        .level_for("reqwest", LevelFilter::Info)
        .level_for("tao", LevelFilter::Info)
        .level_for("tracing", LevelFilter::Info)
        .level_for("wry", LevelFilter::Info)
        // whisper/ggml logging hooks are capped at WARN.
        .level_for("whisper_rs::whisper_logging_hook", LevelFilter::Warn)
        .level_for("whisper_rs::ggml_logging_hook", LevelFilter::Warn)
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} [{:<5} {}] {}",
                Local::now().format("%Y-%m-%dT%H:%M:%S%.3f"),
                record.level(),
                record.target(),
                message
            ))
        })
        .chain(fern::DateBased::new(
            logs_dir.join(format!("daisy-{host}-")),
            "%Y-%m-%d.log",
        ));

    fern::Dispatch::new()
        // The outer dispatcher accepts everything; each sink applies its own level.
        .level(LevelFilter::Trace)
        .chain(console)
        .chain(file)
        .apply()
        .map_err(|e| format!("install logger: {e}"))?;

    // Routes whisper.cpp + ggml output through Rust's `log` crate.
    whisper_rs::install_logging_hooks();

    prune_old_logs(logs_dir);

    Ok(log_path)
}

/// Deletes rotated log files older than KEEP_DAYS days. Errors are ignored.
fn prune_old_logs(logs_dir: &Path) {
    let cutoff = SystemTime::now()
        - Duration::from_secs(KEEP_DAYS * 24 * 60 * 60);
    let Ok(entries) = fs::read_dir(logs_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // Only files matching `daisy-*.log` or `daisy.log.*` are deleted.
        let is_current = name.starts_with("daisy-") && name.ends_with(".log");
        let is_legacy = name.starts_with("daisy.log.");
        if !is_current && !is_legacy {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        if mtime < cutoff {
            let _ = syncsafe::remove_file(&path);
        }
    }
}
