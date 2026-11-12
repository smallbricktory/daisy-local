// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Replay WAVs through the recording's live pipeline (the same tasks, event
//! handling and telemetry a recording runs) and leave `live_transcript.jsonl`
//! and `telemetry.jsonl` in an output directory for
//! `scripts/clip-test-analyze.py`.
//!
//!   cargo run --release -p tauri-app --example clip_replay -- ENGINE SYSTEM_WAV OUT_DIR [MIC_WAV]
//!
//! ENGINE is a Nemotron model directory, or `sa` for SpeechAnalyzer (macOS).
//! SYSTEM_WAV feeds the system track; MIC_WAV feeds the mic track, which
//! otherwise carries silence. Both are 16 kHz mono, fed in 20 ms frames on a
//! real-time schedule. The mic arrives as OS voice-processed, so the
//! software echo canceller and the input-gain loop stay off, unless
//! REPLAY_AEC=1 marks it raw with echo cancellation wanted.
//!
//! Env: REPLAY_SPEED playback rate (default 1.0 = real time).
//!      REPLAY_AEC   1 = raw mic, software echo canceller on.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use audio_engine::capture::TeeFrame;
use providers_realtime::RealtimeTranscriber;
use recording::live_pipeline::{LiveMode, LivePipeline, LivePipelineEventKind, LiveStartConfig};
use recording::telemetry::{EngineId, Header, Host, Telemetry};

const FRAME: usize = 320;

fn read_pcm(path: &str) -> Vec<i16> {
    providers_local::wav::decode_wav_16k_mono_f32(std::path::Path::new(path))
        .unwrap_or_else(|e| panic!("decode {path}: {e}"))
        .iter()
        .map(|&s| (s * 32768.0).clamp(-32768.0, 32767.0) as i16)
        .collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(engine), Some(system_wav), Some(out)) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: clip_replay <nemotron-model-dir | sa> <system.wav> <out-dir> [mic.wav]");
        std::process::exit(2);
    };
    let mic_wav = args.next();
    let speed: f64 = std::env::var("REPLAY_SPEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &f64| *v > 0.0)
        .unwrap_or(1.0);
    let raw_mic = std::env::var("REPLAY_AEC").is_ok_and(|v| v == "1");
    let out = PathBuf::from(out);
    syncsafe::create_dir_all(&out).expect("create out dir");

    let system = read_pcm(&system_wav);
    let mic = mic_wav.as_deref().map(read_pcm).unwrap_or_else(|| vec![0; system.len()]);
    let frames = system.len().max(mic.len()) / FRAME;

    let client: Arc<dyn RealtimeTranscriber> = if engine == "sa" {
        Arc::new(providers_local::macos::realtime::SaRealtime::new(""))
    } else {
        Arc::new(providers_local::nemotron::realtime::NemotronRealtime::new(
            PathBuf::from(&engine),
            "Nemotron (replay)",
        ))
    };

    let session_id = out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let telemetry = Arc::new(Telemetry::open(&out));
    telemetry.header(
        &Host::detect(env!("CARGO_PKG_VERSION"), "clip-replay"),
        &Header {
            session_id,
            session_start_wall_ms: recording::telemetry::wall_ms(),
            live_mode: "realtime".into(),
            engine: Some(EngineId::of(client.as_ref())),
            fallbacks: Vec::new(),
            mic_device: mic_wav.clone().unwrap_or_else(|| "silence".into()),
            system_device: system_wav.clone(),
            sample_rate: 16_000,
        },
    );
    let sampler = recording::proc_stats::ProcSamplerThread::spawn(Arc::clone(&telemetry), Duration::from_secs(5));

    let (mic_tx, mic_rx) = tokio::sync::mpsc::unbounded_channel::<TeeFrame>();
    let (sys_tx, sys_rx) = tokio::sync::mpsc::unbounded_channel::<TeeFrame>();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(256);
    let (switch_tx, switch_rx) = tokio::sync::mpsc::unbounded_channel();
    let pipeline = LivePipeline::start(LiveStartConfig {
        mode: LiveMode::Realtime { client, fallbacks: Vec::new() },
        live_terms: Vec::new(),
        sample_rate: 16_000,
        mic_audio_rx: mic_rx,
        system_audio_rx: sys_rx,
        transcript_path: out.join("live_transcript.jsonl"),
        events_tx,
        mic_source_id: u32::MAX,
        speech_env_min: Arc::new(AtomicU32::new(recording::live_pipeline::speech_env_min_bits(None))),
        mic_switch_rx: switch_rx,
        needs_aec: Arc::new(AtomicBool::new(raw_mic)),
        mic_voice_processed: Arc::new(AtomicBool::new(!raw_mic)),
        paused: Arc::new(AtomicBool::new(false)),
        flight: Arc::new(recording::flight_recorder::FlightRecorder::disabled()),
        topology_tx: std::sync::mpsc::channel().0,
        mic_key: audio_engine::topology::DeviceKey("clip-replay".into()),
        snapshot: Arc::new(RwLock::new(audio_engine::topology::Snapshot::default())),
        self_writes: Arc::new(Mutex::new(recording::supervisor::SelfWrites::default())),
        epoch: Instant::now(),
        dead_air_floor: None,
        notices_tx: std::sync::mpsc::channel().0,
        telemetry: Arc::clone(&telemetry),
        live_aec_root: None,
    })
    .expect("start live pipeline");

    let printer = std::thread::spawn(move || {
        let mut finals = 0usize;
        while let Some(ev) = events_rx.blocking_recv() {
            match ev.kind {
                LivePipelineEventKind::Final { start_ms, end_ms, text, .. } => {
                    finals += 1;
                    println!("[{:?} {start_ms:>7}-{end_ms:>7}] {text}", ev.track);
                }
                LivePipelineEventKind::Error(e) => eprintln!("  !! {:?}: {e}", ev.track),
                _ => {}
            }
        }
        finals
    });

    println!(
        "replaying {:.1}s through the live pipeline at {speed}x into {}",
        frames as f64 * FRAME as f64 / 16_000.0,
        out.display()
    );
    let origin_ns = 1_000_000_000u64;
    let started = Instant::now();
    let frame_of = |pcm: &[i16], i: usize| -> Vec<i16> {
        let s = i * FRAME;
        (s..s + FRAME).map(|k| pcm.get(k).copied().unwrap_or(0)).collect()
    };
    for i in 0..frames {
        let due = started + Duration::from_secs_f64((i + 1) as f64 * 0.02 / speed);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let start_sample = (i * FRAME) as u64;
        let host_ns = origin_ns + start_sample * 1_000_000_000 / 16_000;
        let _ = mic_tx.send(TeeFrame { start_sample, host_ns, samples: frame_of(&mic, i), chunk: None });
        let _ = sys_tx.send(TeeFrame { start_sample, host_ns, samples: frame_of(&system, i), chunk: None });
    }
    drop(mic_tx);
    drop(sys_tx);
    drop(switch_tx);
    pipeline.shutdown();
    let finals = printer.join().unwrap_or(0);
    drop(sampler);
    telemetry.end();
    drop(telemetry);
    println!("{finals} finals; wrote {}", out.display());
}
