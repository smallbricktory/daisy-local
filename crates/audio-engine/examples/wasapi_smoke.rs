//! End-to-end smoke test for the Windows WASAPI backend.
//!
//! Lists capture sources, captures 5 s of mic and 5 s of system loopback,
//! and asserts both WAV files exist and have >100 kB of content. Run via:
//!
//! ```sh
//! cargo run -p audio-engine --example wasapi_smoke
//! ```
//!
//! On non-Windows targets the example prints a notice and exits cleanly.

#[cfg(target_os = "windows")]
mod imp {
    use audio_engine::capture::capture_one;
    use audio_engine::source::{list_sources, Source, SourceKind};
    use std::path::PathBuf;
    use std::time::Duration;

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let sources = list_sources()?;
        println!("Enumerated {} sources:", sources.len());
        for s in &sources {
            println!(
                "  [{:?}] id={} {} :: {}",
                s.kind, s.id, s.node_name, s.description
            );
        }

        let mic = pick(&sources, SourceKind::Mic)
            .ok_or("no Mic sources — plug in a microphone or unmute the default input")?;

        // Loopback against the Windows *default* render endpoint, not whatever
        // Monitor source happens to be listed first. Picking sources[0] of
        // kind Monitor can land on a silent HDMI device; the default endpoint
        // is where SoundPlayer / browsers / system sounds all render. The
        // `"wasapi-loopback"` sentinel is the same hook the WASAPI VirtualSink
        // stub uses to mean "default render endpoint."
        let sys_default = Source {
            id: 0,
            node_name: "wasapi-loopback".to_string(),
            description: "Default render endpoint (loopback)".to_string(),
            kind: SourceKind::Monitor,
            default_sample_rate: 48_000,
            default_channels: 2,
        };

        let out_dir = PathBuf::from(env_or("WASAPI_SMOKE_DIR", "."));
        std::fs::create_dir_all(&out_dir)?;
        let mic_wav = out_dir.join("mic.wav");
        let sys_wav = out_dir.join("system.wav");

        println!("\nCapturing 5 s of mic → {}", mic_wav.display());
        capture_one(mic, Duration::from_secs(5), &mic_wav)?;
        println!(
            "Capturing 5 s of system loopback (default render endpoint) → {}",
            sys_wav.display()
        );
        println!("(play any audio through your default output right now to get a non-silent file)");
        capture_one(&sys_default, Duration::from_secs(5), &sys_wav)?;

        let mic_bytes = std::fs::metadata(&mic_wav)?.len();
        let sys_bytes = std::fs::metadata(&sys_wav)?.len();
        println!("\nmic.wav   = {mic_bytes} bytes");
        println!("system.wav = {sys_bytes} bytes");

        const MIN: u64 = 100_000;
        if mic_bytes < MIN {
            return Err(format!("mic.wav too small ({mic_bytes} < {MIN})").into());
        }
        if sys_bytes < MIN {
            eprintln!("\nNOTE: system.wav has no audio frames. This is the documented");
            eprintln!("WASAPI loopback behavior on a silent render endpoint — the stream");
            eprintln!("was opened and polled correctly, but no buffers were delivered.");
            eprintln!("Play media on your default output and re-run for a non-silent file.");
            return Err(format!("system.wav too small ({sys_bytes} < {MIN})").into());
        }

        println!("\nOK — both WAVs >{MIN} bytes. Listen to them in Sound Recorder / VLC.");
        Ok(())
    }

    fn pick(sources: &[Source], kind: SourceKind) -> Option<&Source> {
        sources.iter().find(|s| s.kind == kind)
    }

    fn env_or(key: &str, fallback: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| fallback.to_string())
    }
}

fn main() {
    #[cfg(target_os = "windows")]
    {
        if let Err(e) = imp::run() {
            eprintln!("wasapi_smoke FAILED: {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        println!("wasapi_smoke is a Windows-only example. Skipping on this target.");
    }
}
