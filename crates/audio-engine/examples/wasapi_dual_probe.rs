//! Diagnostic probe for the Windows dual-stream WASAPI capture path.
//!
//! Drives `audio_engine::capture::run_dual_streaming` for ~20 seconds with
//! two 10-second chunks, exercising OpenChunk / CloseChunk rotation across
//! the mic and system streams. After the run, reads every WAV with hound
//! and prints per-chunk sample counts, showing whether mic and system stay
//! time-aligned.
//!
//! Recommended test pattern:
//!   1. Play media (Spotify / YouTube / SoundPlayer) on your default output.
//!   2. Run the probe.
//!   3. Stop audio at the ~10 s mark (between chunks).
//!   4. Compare chunk-0 (audio playing) vs chunk-1 (silent) sample counts.
//!
//! Expected silent-loopback bug signature: chunk-0 mic and system are
//! close in length (within a few ms); chunk-1 mic continues to ~10 s of
//! samples while system stays empty (WASAPI loopback delivers zero
//! buffers when nothing is rendering — unlike PipeWire which emits
//! silence frames).
//!
//! Run:
//!   cargo run -p audio-engine --example wasapi_dual_probe
//!
//! Override the system source if the heuristic picks wrong:
//!   $env:WASAPI_DUAL_PROBE_SYS_INDEX = "4"  # 0-based index into list_sources

#[cfg(target_os = "windows")]
mod imp {
    use audio_engine::capture::{run_dual_streaming, StreamingCaptureRequest};
    use audio_engine::source::{list_sources, Source, SourceKind};
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    const CHUNK_SECS: u64 = 10;
    const NUM_CHUNKS: usize = 2;
    const SAMPLE_RATE: u32 = 16_000;

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let sources = list_sources()?;
        println!("Enumerated {} sources:", sources.len());
        for (i, s) in sources.iter().enumerate() {
            println!(
                "  [{i:2}] {:?} id={} :: {}",
                s.kind, s.id, s.description
            );
        }

        let mic = pick_mic(&sources)?;
        let sys = pick_system(&sources)?;
        println!(
            "\nMic    : id={} {}\nSystem : id={} {}\n",
            mic.id, mic.description, sys.id, sys.description
        );

        let out_dir = PathBuf::from(env_or("WASAPI_DUAL_PROBE_DIR", "."));
        std::fs::create_dir_all(&out_dir)?;
        let chunk_paths: Vec<(PathBuf, PathBuf)> = (0..NUM_CHUNKS)
            .map(|i| {
                let m = out_dir.join(format!("mic_{i:02}.wav"));
                let s = out_dir.join(format!("sys_{i:02}.wav"));
                let _ = std::fs::remove_file(&m);
                let _ = std::fs::remove_file(&s);
                (m, s)
            })
            .collect();

        let req = StreamingCaptureRequest {
            mic_source_id: mic.id,
            system_source_id: sys.id,
            sample_rate: SAMPLE_RATE,
        };

        // A oneshot-ish channel signals when the controller thread is done;
        // the on_ready closure returns promptly.
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let chunk_paths_for_controller = chunk_paths.clone();
        let started = Instant::now();

        println!(
            "Starting capture: {NUM_CHUNKS} chunks of {CHUNK_SECS} s each, total {} s",
            (NUM_CHUNKS as u64) * CHUNK_SECS
        );
        println!("Play audio NOW (default output). Stop after ~{} s to expose the silent-loopback gap.\n", CHUNK_SECS);

        run_dual_streaming(req, move |handle| {
            let h = handle.clone();
            thread::spawn(move || {
                for (i, (mic_path, sys_path)) in chunk_paths_for_controller.iter().enumerate() {
                    println!(
                        "  [{elapsed:>5.1} s] open chunk {i} → {} + {}",
                        mic_path.file_name().unwrap().to_string_lossy(),
                        sys_path.file_name().unwrap().to_string_lossy(),
                        elapsed = started.elapsed().as_secs_f32()
                    );
                    let _ = h.open_chunk(mic_path, sys_path);
                    thread::sleep(Duration::from_secs(CHUNK_SECS));
                    println!(
                        "  [{:>5.1} s] close chunk {i}",
                        started.elapsed().as_secs_f32()
                    );
                    let _ = h.close_chunk();
                }
                println!(
                    "  [{:>5.1} s] stop",
                    started.elapsed().as_secs_f32()
                );
                let _ = h.stop();
                let _ = done_tx.send(());
            });
        })?;

        // Make sure the controller actually ran. (It exits before run_dual_streaming
        // returns, but channel drain protects us if not.)
        let _ = done_rx.recv_timeout(Duration::from_secs(2));

        println!(
            "\nCapture finished after {:.1} s.\n",
            started.elapsed().as_secs_f32()
        );
        println!("Per-chunk results:");
        println!(
            "{:<5} {:>14} {:>14} {:>14} {:>14} {:>10}",
            "chunk", "mic samples", "mic seconds", "sys samples", "sys seconds", "drift ms"
        );

        let mut had_anomaly = false;
        for (i, (mic_path, sys_path)) in chunk_paths.iter().enumerate() {
            let mic_samples = read_sample_count(mic_path);
            let sys_samples = read_sample_count(sys_path);
            let mic_secs = mic_samples as f64 / SAMPLE_RATE as f64;
            let sys_secs = sys_samples as f64 / SAMPLE_RATE as f64;
            let drift_ms = (mic_secs - sys_secs) * 1000.0;
            if drift_ms.abs() > 200.0 {
                had_anomaly = true;
            }
            println!(
                "{:<5} {:>14} {:>14.3} {:>14} {:>14.3} {:>10.1}",
                i, mic_samples, mic_secs, sys_samples, sys_secs, drift_ms
            );
        }

        if had_anomaly {
            println!("\nNOTE: at least one chunk has >200 ms mic/system drift.");
            println!("If the system stream went silent, this is the documented WASAPI");
            println!("loopback behavior — WASAPI delivers no buffers when nothing is");
            println!("rendering on the endpoint, so no frames hit the WAV.");
        } else {
            println!("\nAll chunks within 200 ms drift. Dual stream alignment looks fine.");
        }

        Ok(())
    }

    fn pick_mic(sources: &[Source]) -> Result<&Source, &'static str> {
        sources
            .iter()
            .find(|s| s.kind == SourceKind::Mic)
            .ok_or("no Mic sources")
    }

    fn pick_system(sources: &[Source]) -> Result<&Source, &'static str> {
        // Explicit override wins.
        if let Ok(idx) = std::env::var("WASAPI_DUAL_PROBE_SYS_INDEX") {
            if let Ok(i) = idx.parse::<usize>() {
                return sources
                    .get(i)
                    .filter(|s| s.kind == SourceKind::Monitor)
                    .ok_or("WASAPI_DUAL_PROBE_SYS_INDEX out of range or not a Monitor");
            }
        }

        // Heuristic: pick the Monitor whose description names a likely
        // default output (Realtek / Speakers / Headphones). Falls back to
        // the first Monitor if no heuristic match.
        sources
            .iter()
            .find(|s| {
                s.kind == SourceKind::Monitor
                    && (s.description.contains("Realtek")
                        || s.description.contains("Speakers")
                        || s.description.contains("Headphones"))
            })
            .or_else(|| sources.iter().find(|s| s.kind == SourceKind::Monitor))
            .ok_or("no Monitor sources — set WASAPI_DUAL_PROBE_SYS_INDEX")
    }

    fn read_sample_count(path: &PathBuf) -> u64 {
        match hound::WavReader::open(path) {
            Ok(r) => r.duration() as u64,
            Err(_) => 0,
        }
    }

    fn env_or(key: &str, fallback: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| fallback.to_string())
    }
}

fn main() {
    #[cfg(target_os = "windows")]
    {
        if let Err(e) = imp::run() {
            eprintln!("wasapi_dual_probe FAILED: {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        println!("wasapi_dual_probe is a Windows-only example. Skipping on this target.");
    }
}
