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

/// Appended to the two ggml Metal4 tensor-capability probe lines when they are
/// rewritten into the file sink.
const GGML_TENSOR_PROBE_NOTE: &str =
    "[tensor-capability probe only; standard Metal backend still engages]";

/// True for the two lines ggml's Metal4 tensor-capability probe emits when the
/// probe kernel fails to compile (an ERROR with no detail, then a WARN). The
/// standard Metal backend still initializes after them. Matched lines are kept
/// off the console and written to the file sink at INFO with
/// [`GGML_TENSOR_PROBE_NOTE`] appended. A real Metal library compile failure
/// logs the same prefix with an appended `: <detail>` and does not match.
fn is_ggml_tensor_probe_line(message: &str) -> bool {
    let m = message.trim();
    m == "ggml_metal_library_init_from_source: error compiling source"
        || m.contains("the tensor API is not supported in this environment")
}

/// Filesystem-safe, lowercased machine-name slug for the log filename.
/// Empty or unknown host → `"host"`.
fn log_host_slug() -> String {
    // Same machine identity as the per-machine settings key: the resolver's
    // host name changes with the network (macOS reports mymac-wifi.lan on
    // wifi, mymac-lan.lan on ethernet), which would split one machine's
    // logs across two filenames mid-investigation.
    let raw = crate::hardware::machine_name();
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
            if record.target() == "whisper_rs::ggml_logging_hook"
                && is_ggml_tensor_probe_line(&message.to_string())
            {
                return;
            }
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
        // The tract graph optimizer traces every model load at debug;
        // nothing below Warn is ours to act on.
        .level_for("tract_core", LevelFilter::Warn)
        .level_for("tract_hir", LevelFilter::Warn)
        .level_for("tract_pulse", LevelFilter::Warn)
        .level_for("tract_linalg", LevelFilter::Warn)
        .level_for("ort", LevelFilter::Warn)
        .format(|out, message, record| {
            if record.target() == "whisper_rs::ggml_logging_hook" {
                let msg = message.to_string();
                if is_ggml_tensor_probe_line(&msg) {
                    out.finish(format_args!(
                        "{} [{:<5} {}] {} {}",
                        Local::now().format("%Y-%m-%dT%H:%M:%S%.3f"),
                        log::Level::Info,
                        record.target(),
                        msg.trim(),
                        GGML_TENSOR_PROBE_NOTE
                    ));
                    return;
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tensor_probe_matches_the_two_probe_lines() {
        assert!(is_ggml_tensor_probe_line(
            "ggml_metal_library_init_from_source: error compiling source"
        ));
        assert!(is_ggml_tensor_probe_line(
            "ggml_metal_library_init_from_source: error compiling source\n"
        ));
        assert!(is_ggml_tensor_probe_line(
            "ggml_metal_device_init: - the tensor API is not supported in this environment - disabling"
        ));
    }

    #[test]
    fn tensor_probe_does_not_match_real_errors() {
        // Verbose compile failures carry a detail suffix and must surface.
        assert!(!is_ggml_tensor_probe_line(
            "ggml_metal_library_init_from_source: error compiling source: program_source:12: error"
        ));
        assert!(!is_ggml_tensor_probe_line(
            "ggml_metal_library_init: error: failed to create Metal library"
        ));
        assert!(!is_ggml_tensor_probe_line(
            "ggml_metal_device_init: - the tensor API does not support bfloat - disabling bfloat support"
        ));
        assert!(!is_ggml_tensor_probe_line("whisper_init_state: kv self size"));
    }
}
