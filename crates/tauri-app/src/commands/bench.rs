//! In-app local-whisper speed benchmark. Runs a whisper throughput bench on
//! synthetic audio using the production model/backend, logs the result to
//! daisy.log (target `bench`), and returns a one-line summary.

use crate::error::{AppError, Result};
use crate::state::AppState;

/// Runs the bench, stores the measured batch xRT in this machine's
/// `live_captions_by_machine` entry, and returns the updated resolution.
pub fn run_live_captions_bench_impl(
    app: &AppState,
) -> Result<crate::hardware::LiveCaptionsResolution> {
    let report = run_bench_report(app)?;
    let path = app.profile.settings_path();
    let mut settings = crate::settings::Settings::load_or_default(&path);
    let entry = settings
        .live_captions_by_machine
        .entry(crate::hardware::machine_name())
        .or_default();
    entry.bench_xrt = Some(report.full_xrt);
    entry.benched_at_unix_seconds = Some(crate::now_unix());
    settings
        .save(&path)
        .map_err(|e| AppError::Config(format!("save settings: {e}")))?;
    Ok(crate::hardware::resolve_live_captions(&settings))
}

/// Resolves the production whisper model, runs the bench at cores-2 threads,
/// logs the summary, returns the report.
fn run_bench_report(app: &AppState) -> Result<providers_local::bench::BenchReport> {
    let settings = crate::settings::Settings::load_or_default(&app.profile.settings_path());
    let model_path = settings
        .whisper_model_path
        .clone()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("DAISY_WHISPER_MODEL_DIR")
                .map(|d| std::path::PathBuf::from(d).join("ggml-base.en.bin"))
        })
        .ok_or_else(|| AppError::Config("no local whisper model found to benchmark".into()))?;
    if !model_path.is_file() {
        return Err(AppError::Config(format!(
            "whisper model not found: {}",
            model_path.display()
        )));
    }

    let nproc = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4);
    let threads = (nproc - 2).max(1);

    // 60 s of synthetic speech-like audio.
    let audio = providers_local::bench::synthetic_test_audio(60);

    log::info!(
        target: "bench",
        "benchmark starting: {} thread(s), backend {}, model {}",
        threads,
        providers_local::bench::backend_label(),
        model_path.display()
    );

    let report = providers_local::bench::run_speed_bench(&model_path, &audio, threads)
        .map_err(|e| AppError::Config(format!("benchmark failed: {e}")))?;

    log::info!(target: "bench", "benchmark result: {}", report.summary());
    Ok(report)
}
