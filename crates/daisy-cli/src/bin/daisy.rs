use audio_engine::{list_sources, SourceKind};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "daisy", about = "Daisy audio engine CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print version and exit.
    Version,
    /// List capture-capable PipeWire sources.
    Sources,
    /// Capture from a single source to a WAV file (debug helper).
    CaptureOne {
        #[arg(long)]
        source: u32,
        #[arg(long, default_value_t = 5)]
        duration_secs: u64,
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Run AEC offline against existing mic + far WAV files.
    Aec {
        /// Mic / near-end WAV (16 kHz mono int16).
        #[arg(long)]
        mic: std::path::PathBuf,
        /// Far / loopback WAV (16 kHz mono int16).
        #[arg(long)]
        far: std::path::PathBuf,
        /// Output WAV path for echo-cancelled mic.
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Capture mic + system loopback to two WAV files plus a manifest.
    Capture {
        /// Use a daisy-managed virtual sink for the system channel.
        /// When set, --system is ignored: the app creates a temporary
        /// daisy-capture null sink and captures from its monitor.
        #[arg(long)]
        virtual_sink: bool,
        /// (Requires --virtual-sink) Move currently-playing output streams into
        /// the daisy sink for the duration of the recording, restore on stop.
        #[arg(long, requires = "virtual_sink")]
        auto_route: bool,
        /// Run AEC after capture, producing mic_aec.wav alongside mic.wav.
        #[arg(long)]
        run_aec: bool,
        #[arg(long)]
        mic: u32,
        /// PW source ID for the system loopback. Required unless --virtual-sink.
        #[arg(long)]
        system: Option<u32>,
        #[arg(long, default_value_t = 30)]
        duration_secs: u64,
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Repair an orphaned session (process died mid-recording). Refuses if heartbeat is fresh.
    SessionFinalize {
        /// Path to the session directory.
        #[arg(long)]
        path: PathBuf,
        /// Heartbeat staleness threshold in seconds (default: 30).
        #[arg(long, default_value_t = 30)]
        heartbeat_max_age_secs: u64,
    },
    /// Run a controllable recording session: pause/resume/stop on stdin lines, Ctrl-C to stop.
    Record {
        /// Numeric mic source id (from `daisy sources`).
        #[arg(long)]
        mic: u32,
        /// Numeric system source id (from `daisy sources`). Use --virtual-sink to create one.
        #[arg(long)]
        system: Option<u32>,
        /// Use a daisy-owned virtual sink as the system source.
        #[arg(long)]
        virtual_sink: bool,
        /// Output session directory (must not exist).
        #[arg(long)]
        out: PathBuf,
        /// Friendly session id stamped in the manifest. Defaults to `daisy-<unix_ts>`.
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Transcribe an existing session with on-device Whisper. Reads its
    /// manifest, transcribes each chunk-track, writes transcript.json next
    /// to manifest.json.
    Transcribe {
        /// Path to the session directory.
        #[arg(long)]
        session: PathBuf,
        /// Path to a ggml-*.bin whisper model file (see `daisy download-model`).
        #[arg(long)]
        model: String,
        /// Language hint passed to the provider (e.g. "en"). Pass empty string
        /// to disable and let the model auto-detect.
        #[arg(long, default_value = "en")]
        language: String,
    },
    /// Summarize a session. Reads transcript.md, notes.md and manifest.json;
    /// writes summary.json + summary.md next to manifest.json.
    Summarize {
        /// Path to the session directory (the one containing manifest.json).
        #[arg(long)]
        session: PathBuf,
        /// Provider name. One of: anthropic, openai, lm_studio, ollama.
        #[arg(long, default_value = "anthropic")]
        provider: String,
        /// Override the provider default model. Leave unset for provider defaults.
        #[arg(long)]
        model: Option<String>,
        /// Tag prompt as `name=text`. Repeatable. Split on the first `=`.
        #[arg(long = "tag-prompt")]
        tag_prompt: Vec<String>,
    },
    /// Cross-track dedup + unified markdown render. Reads transcript.json,
    /// writes transcript.dedup.json, transcript.md.
    Dedup {
        /// Path to the session directory.
        #[arg(long)]
        session: PathBuf,
        /// Bigram Jaccard threshold (inclusive). Higher = stricter (fewer drops).
        #[arg(long, default_value_t = 0.6)]
        jaccard_threshold: f32,
        /// Mic RMS dBFS ceiling. Mic must be quieter than this to count as bleed.
        #[arg(long, default_value_t = -35.0)]
        mic_quiet_dbfs: f32,
        /// Time-overlap slack in ms.
        #[arg(long, default_value_t = 500)]
        overlap_slack_ms: u32,
        /// Keep mic backchannels ("yeah", "mm-hmm", "oh", etc.) instead of dropping them.
        #[arg(long, default_value_t = false)]
        keep_backchannels: bool,
    },
    /// Download a whisper.cpp GGML model for offline transcription.
    DownloadModel {
        /// Model size, e.g. base.en, small.en, medium.en, large-v3. Default: base.en.
        #[arg(long, default_value = "base.en")]
        size: String,
        /// Directory to put the .bin in. Default: <profile>/models, where the
        /// profile comes from $DAISY_PROFILE_DIR or bootstrap.json; the
        /// platform data dir when neither resolves.
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
    },
    /// Store a provider API key into the app's encrypted vault. A running
    /// Daisy app overwrites this value on its next vault write.
    SetKey {
        /// Provider name, e.g. groq, openai, anthropic.
        #[arg(long)]
        provider: String,
        /// Read the key from this file (trailing whitespace/newline trimmed). If omitted, reads the key from stdin.
        #[arg(long)]
        key_file: Option<std::path::PathBuf>,
        /// Path to keys.vault.json. Default: <profile>/keys.vault.json, where
        /// the profile comes from $DAISY_PROFILE_DIR or bootstrap.json.
        #[arg(long)]
        vault: Option<std::path::PathBuf>,
        /// Read the vault passphrase from stdin instead of prompting interactively.
        #[arg(long)]
        passphrase_stdin: bool,
    },
    /// Vendor tool: generate an Ed25519 license keypair. Writes the private
    /// key to `--out` and prints only the public key.
    LicenseKeygen {
        /// File to write the base64 private key to (e.g. ~/.daisy/license.key).
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Vendor tool: sign a license for a buyer. Prints the license token.
    LicenseSign {
        /// Path to the private-key file from license-keygen.
        #[arg(long)]
        priv_file: std::path::PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long)]
        email: String,
        /// Days until expiry; omit for a perpetual license.
        #[arg(long)]
        days: Option<i64>,
    },
}

/// Decrypted vault payload. The CLI edits only `providers`; every other
/// top-level field round-trips untouched through `extra`. A vault without
/// `schema_version` fails to parse rather than being rewritten with a
/// guessed version.
#[derive(serde::Serialize, serde::Deserialize)]
struct VaultPayload {
    schema_version: u32,
    #[serde(default)]
    providers: std::collections::BTreeMap<String, ProviderEntry>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct ProviderEntry {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    base_url: Option<String>,
}

#[derive(serde::Deserialize)]
struct Bootstrap {
    profile_dir: PathBuf,
}

/// Expand a leading `~/` or `~` in a path to the user's home directory.
/// Returns the input unchanged otherwise.
fn expand_home(s: &str) -> PathBuf {
    if s == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(s)
}

/// `DAISY_PROFILE_DIR` override — first in the profile resolution order,
/// same precedence as the app (env → bootstrap.json → platform default).
/// Empty or unset = None.
fn env_profile_dir() -> Option<PathBuf> {
    match std::env::var("DAISY_PROFILE_DIR") {
        Ok(s) if !s.is_empty() => Some(expand_home(&s)),
        _ => None,
    }
}

/// Profile dir from bootstrap.json (written by the Daisy app's first run).
fn bootstrap_profile_dir() -> anyhow::Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("ai", "daisy", "Daisy")
        .ok_or_else(|| anyhow::anyhow!("could not determine the Daisy config directory"))?;
    let bootstrap_path = dirs.config_dir().join("bootstrap.json");
    let bytes = std::fs::read(&bootstrap_path).map_err(|_| {
        anyhow::anyhow!(
            "no bootstrap.json at {} — run the Daisy app once, or pass --vault /path/to/keys.vault.json",
            bootstrap_path.display()
        )
    })?;
    let bootstrap: Bootstrap = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("parse {}: {e}", bootstrap_path.display()))?;
    Ok(bootstrap.profile_dir)
}

fn cmd_record(
    mic: u32,
    system_arg: Option<u32>,
    virtual_sink: bool,
    out: PathBuf,
    session_id: Option<String>,
) -> anyhow::Result<()> {
    use audio_engine::virtual_sink::VirtualSink;
    use recording::{Recorder, RecorderConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    // Optional virtual sink (RAII; stays alive until end of function).
    let _vs: Option<VirtualSink> = if virtual_sink {
        Some(VirtualSink::create("daisy-capture")?)
    } else {
        None
    };

    let sources = list_sources()?;
    let mic_src = sources.iter().find(|s| s.id == mic)
        .ok_or_else(|| anyhow::anyhow!("mic source id {} not found", mic))?;
    let system_id = match system_arg {
        Some(id) => id,
        None => {
            // No --system: use the virtual-sink monitor. The monitor name
            // differs by platform (`daisy-capture.monitor` on Linux/PipeWire,
            // `wasapi-loopback` on Windows) and comes from VirtualSink.
            let monitor_name = _vs
                .as_ref()
                .map(|vs| vs.monitor_source_name())
                .ok_or_else(|| anyhow::anyhow!(
                    "no --system id and no --virtual-sink (pass one of them)"
                ))?;
            let m = sources.iter().find(|s| s.node_name == monitor_name)
                .ok_or_else(|| anyhow::anyhow!(
                    "no --system id and '{monitor_name}' not found (pass --virtual-sink or --system)"
                ))?;
            m.id
        }
    };
    let sys_src = sources.iter().find(|s| s.id == system_id)
        .ok_or_else(|| anyhow::anyhow!("system source id {} not found", system_id))?;

    let session_id = session_id.unwrap_or_else(|| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("daisy-{ts}")
    });

    let cfg = RecorderConfig {
        // The CLI records without a meeting, so there are no names to prime with.
        live_terms: Vec::new(),
        session_root: out,
        mic_source_id: mic_src.id,
        mic_source_node_name: mic_src.node_name.clone(),
        mic_source_key: mic_src.key.clone(),
        mic_source_description: mic_src.description.clone(),
        system_source_id: sys_src.id,
        system_source_node_name: sys_src.node_name.clone(),
        system_source_description: sys_src.description.clone(),
        sample_rate: 16_000,
        session_id,
        live_mode: recording::LiveMode::Off,
        mic_levels_lookup: None,
        flight_recorder: false,
        aec_override: recording::AecOverride::Auto,
        // The CLI keeps sink ownership itself (`_vs` above) — plumbing
        // reactions are a supervised-app concern.
        virtual_sink: None,
        topology_hub: std::sync::Arc::new(audio_engine::topology::TopologyHub::start()),
    };

    eprintln!("Recording (AEC will run automatically if speaker output is detected)...");
    let mut rec = Recorder::start(cfg)?;
    eprintln!(
        "Recording started. Type 'pause', 'resume', 'stop' (or Ctrl-C to stop). Session: {}",
        rec.session_root().display()
    );

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_for_handler = Arc::clone(&stop_flag);
    ctrlc::set_handler(move || {
        stop_for_handler.store(true, Ordering::SeqCst);
    })?;

    // Stdin is read on a dedicated thread and forwarded via crossbeam; the
    // main loop polls Ctrl-C and stdin together at a 200 ms interval.
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<String>(8);
    std::thread::spawn(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            if cmd_tx.send(line).is_err() {
                break;
            }
        }
    });

    let poll = std::time::Duration::from_millis(200);
    loop {
        if stop_flag.load(Ordering::SeqCst) {
            eprintln!("\n[ctrl-c received] stopping...");
            break;
        }
        let line = match cmd_rx.recv_timeout(poll) {
            Ok(l) => l,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };
        match line.trim() {
            "pause" => {
                if let Err(e) = rec.pause() {
                    eprintln!("pause failed: {e}");
                } else {
                    eprintln!("[paused]");
                }
            }
            "resume" => {
                if let Err(e) = rec.resume() {
                    eprintln!("resume failed: {e}");
                } else {
                    eprintln!("[recording]");
                }
            }
            "stop" => break,
            "" => {}
            other => eprintln!("unknown: '{other}' — expected pause/resume/stop"),
        }
    }
    let final_root = rec.stop()?;
    eprintln!("[stopped] session: {}", final_root.display());
    Ok(())
}

fn cmd_transcribe(session: PathBuf, model: String, language: String) -> anyhow::Result<()> {
    let provider: Box<dyn providers_http::Transcriber> =
        Box::new(providers_local::WhisperLocalTranscriber::new(&model)?);
    transcribe_with(session, provider, language)
}

/// Any installed model for recovery runs, in registry-preference order (the
/// default whisper row first): the profile models dir, then the bundled
/// resource dirs named by the model-dir env vars.
fn find_recovery_model() -> Option<(&'static providers_local::registry::ModelRow, PathBuf)> {
    use providers_local::registry::{self, Engine};
    let profile_models = env_profile_dir()
        .or_else(|| bootstrap_profile_dir().ok())?
        .join("models");
    let env_dir = |v: &str| std::env::var_os(v).map(PathBuf::from);
    let ordered = registry::row(registry::DEFAULT_WHISPER_ID)
        .into_iter()
        .chain(registry::REGISTRY.iter());
    for row in ordered {
        let bases: [Option<PathBuf>; 2] = match row.engine {
            Engine::Whisper => {
                [Some(profile_models.clone()), env_dir("DAISY_WHISPER_MODEL_DIR")]
            }
            Engine::Parakeet => {
                [Some(profile_models.join("parakeet")), env_dir("DAISY_PARAKEET_MODEL_DIR")]
            }
            Engine::SpeechAnalyzer => [None, None],
        };
        for b in bases.into_iter().flatten() {
            if row.installed_in(&b) {
                if let Some(p) = row.engine_path(&b) {
                    return Some((row, p));
                }
            }
        }
    }
    None
}

fn transcribe_with(
    session: PathBuf,
    provider: Box<dyn providers_http::Transcriber>,
    language: String,
) -> anyhow::Result<()> {
    use providers_http::transcribe_session;

    let manifest_path = session.join("manifest.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", manifest_path.display()))?;

    let language_hint = if language.is_empty() {
        None
    } else {
        Some(language.as_str())
    };

    eprintln!(
        "transcribing session {} via {} ({})",
        session.display(),
        provider.name(),
        provider.model()
    );
    let st = transcribe_session(&*provider, &session, &manifest_bytes, language_hint, &|_, _| {})
        .map_err(|e| anyhow::anyhow!("transcribe: {e}"))?;

    let out_path = session.join("transcript.json");
    let json = serde_json::to_vec_pretty(&st)?;
    syncsafe::write_atomic(&out_path, json)?;
    providers_http::clear_chunk_transcript_checkpoints(&session);
    let total_segments: usize = st
        .chunks
        .iter()
        .flat_map(|c| c.tracks.iter())
        .map(|t| t.segments.len())
        .sum();
    eprintln!(
        "wrote transcript: {} ({} chunks, {} segments)",
        out_path.display(),
        st.chunks.len(),
        total_segments
    );
    Ok(())
}

fn cmd_download_model(size: String, dir: Option<PathBuf>) -> anyhow::Result<()> {
    // Default dir = <profile>/models, where the profile resolves like the
    // app's (DAISY_PROFILE_DIR env, then bootstrap.json). Platform data dir
    // only when no profile resolves.
    let dir = dir.unwrap_or_else(|| {
        env_profile_dir()
            .or_else(|| bootstrap_profile_dir().ok())
            .map(|p| p.join("models"))
            .or_else(|| {
                directories::ProjectDirs::from("ai", "daisy", "Daisy")
                    .map(|p| p.data_dir().join("models"))
            })
            .unwrap_or_else(|| std::path::PathBuf::from("./models"))
    });
    let path = providers_local::download_ggml_model(&size, &dir)?;
    println!("model ready: {}", path.display());
    println!(
        "use it: daisy transcribe --model {} --session <dir>",
        path.display()
    );
    Ok(())
}

fn cmd_dedup(
    session: PathBuf,
    jaccard_threshold: f32,
    mic_quiet_dbfs: f32,
    overlap_slack_ms: u32,
    keep_backchannels: bool,
) -> anyhow::Result<()> {
    use transcript::dedup::{dedup_session, DedupParams};
    use transcript::render::render_markdown_with_speakers;
    use transcript::SessionTranscript;

    let in_path = session.join("transcript.json");
    let bytes = std::fs::read(&in_path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", in_path.display()))?;
    let mut st: SessionTranscript = serde_json::from_slice(&bytes)?;

    // Same cleanup order as the app's dedup path: profanity mask, then
    // hallucination filter, then energy gate, then dedup (minus the
    // per-device store write — the CLI operates on a bare session
    // directory).
    for chunk in &mut st.chunks {
        for track in &mut chunk.tracks {
            for seg in &mut track.segments {
                seg.text = transcript::text::mask_profanity(&seg.text);
            }
        }
    }
    let hallucinated = transcript::hallucination::filter_session(&mut st);
    if hallucinated > 0 {
        eprintln!("hallucination filter: dropped {hallucinated} segment(s)");
    }
    let single_local = recording::manifest::manifest_gate_probe(&session).single_local_speaker;
    let out = transcript::energy_gate::gate_session(&mut st, &session, single_local);
    eprintln!(
        "energy gate: speech={:?} residue={:?} threshold={:?} ({} speech / {} residue windows), apply={single_local}, dropped {}",
        out.anchors.speech_dbfs, out.anchors.residue_dbfs, out.anchors.threshold_dbfs,
        out.anchors.speech_windows, out.anchors.residue_windows, out.dropped
    );

    let params = DedupParams {
        jaccard_threshold,
        mic_quiet_dbfs,
        sys_quiet_dbfs: DedupParams::default().sys_quiet_dbfs,
        overlap_slack_ms,
        drop_backchannels: !keep_backchannels,
    };
    let result = dedup_session(&st, &session, &params)
        .map_err(|e| anyhow::anyhow!("dedup: {e}"))?;

    let dedup_path = session.join("transcript.dedup.json");
    syncsafe::write_atomic(&dedup_path, serde_json::to_vec_pretty(&result.deduped)?)?;

    // Render like the app: timestamps offset to the meeting start and
    // diarized clusters labelled from the manifest's speaker_map.
    let (offsets, speakers) = std::fs::read(session.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<recording::SessionManifest>(&b).ok())
        .map(|m| (m.chunk_offsets_ms(), m.speaker_display_map()))
        .unwrap_or_default();
    let md = render_markdown_with_speakers(&result.deduped, &offsets, &speakers);
    let md_path = session.join("transcript.md");
    syncsafe::write_atomic(&md_path, md.as_bytes())?;

    eprintln!("dedup done:");
    eprintln!("  deduped:  {}", dedup_path.display());
    eprintln!("  markdown: {}", md_path.display());
    eprintln!(
        "  dropped {} bleed segments, kept {} segments total",
        result.report.dropped,
        result.report.kept_count
    );
    Ok(())
}

fn cmd_summarize(
    session: PathBuf,
    provider_name: String,
    model_override: Option<String>,
    tag_prompts_raw: Vec<String>,
) -> anyhow::Result<()> {
    use recording::manifest::{AttendeeRole, SessionManifest};
    use summarize::anthropic::AnthropicSummarizer;
    use summarize::openai_compat::OpenAICompatSummarizer;
    use summarize::{AttendeeRef, SpeakerRole, SummaryInput, Summarizer, TagPromptRef};

    // 1. transcript.md (required).
    let transcript_path = session.join("transcript.md");
    let transcript_md = std::fs::read_to_string(&transcript_path).map_err(|_| {
        anyhow::anyhow!(
            "no transcript.md at {}; run `daisy transcribe` + `daisy dedup` first",
            transcript_path.display()
        )
    })?;

    // 2. notes.md (optional).
    let notes_md = std::fs::read_to_string(session.join("notes.md")).unwrap_or_default();

    // 3. manifest.json (best-effort: warn and proceed on failure).
    let (manifest_title, attendees_owned): (Option<String>, Vec<(String, SpeakerRole)>) =
        match std::fs::read(session.join("manifest.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<SessionManifest>(&b).ok())
        {
            Some(m) => {
                let att = m
                    .attendees
                    .into_iter()
                    .map(|a| {
                        let role = match a.role {
                            AttendeeRole::Self_ => SpeakerRole::Me,
                            AttendeeRole::Other => SpeakerRole::Them,
                        };
                        (a.display_name, role)
                    })
                    .collect();
                (m.title, att)
            }
            None => {
                eprintln!("warning: could not read/parse manifest.json; proceeding without title/attendees");
                (None, Vec::new())
            }
        };
    let att_refs: Vec<AttendeeRef> = attendees_owned
        .iter()
        .map(|(name, role)| AttendeeRef { display_name: name, role: *role })
        .collect();

    // 4. tag prompts: split each `name=text` on the first `=`.
    let tag_prompts_owned: Vec<(String, String)> = tag_prompts_raw
        .iter()
        .map(|s| match s.split_once('=') {
            Some((name, text)) => Ok((name.to_string(), text.to_string())),
            None => Err(anyhow::anyhow!("--tag-prompt must be `name=text`; got {s:?}")),
        })
        .collect::<anyhow::Result<_>>()?;
    let tag_refs: Vec<TagPromptRef> = tag_prompts_owned
        .iter()
        .map(|(name, prompt)| TagPromptRef { name, prompt_md: prompt, terms: "" })
        .collect();

    // 5. provider. Base URLs + default models come from the one defaults
    // table (`summarize::defaults`); keys stay env-only in the CLI.
    let d = summarize::defaults::for_provider(&provider_name).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown provider: {provider_name}. Try one of: anthropic, openai, groq, lm_studio, ollama"
        )
    })?;
    let model = model_override.unwrap_or_else(|| d.model.into());
    let require_env = |var: &'static str| {
        std::env::var(var).map_err(|_| anyhow::anyhow!("{var} not set"))
    };
    let summarizer: Box<dyn Summarizer> = match provider_name.as_str() {
        "anthropic" => Box::new(AnthropicSummarizer::with_config(
            require_env(d.env_key)?,
            d.base_url.into(),
            model,
        )),
        "openai" => Box::new(OpenAICompatSummarizer::new(
            "openai",
            d.base_url.into(),
            Some(require_env(d.env_key)?),
            model,
        )),
        "lm_studio" => Box::new(OpenAICompatSummarizer::new(
            "lm_studio",
            d.base_url.into(),
            std::env::var(d.env_key).ok(),
            model,
        )),
        "ollama" => Box::new(OpenAICompatSummarizer::new(
            "ollama",
            d.base_url.into(),
            None,
            model,
        )),
        "groq" => Box::new(OpenAICompatSummarizer::new(
            "groq",
            d.base_url.into(),
            Some(require_env(d.env_key)?),
            model,
        )),
        _ => unreachable!("for_provider covers exactly these names"),
    };

    let session_id = session
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("session");

    let input = SummaryInput {
        session_id,
        title: manifest_title.as_deref(),
        attendees: &att_refs,
        user_notes_md: &notes_md,
        transcript_md: &transcript_md,
        tag_prompts: &tag_refs,
        // The CLI summarize path always uses the default Daisy Summarizer shape.
        style_envelope: summarize::Envelope::Classic,
        style_directive: "",
    };

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    eprintln!(
        "summarizing session {} via {} ({})",
        session.display(),
        summarizer.name(),
        summarizer.model()
    );
    let summary = summarizer
        .summarize(&input, now_unix)
        .map_err(|e| anyhow::anyhow!("summarize: {e}"))?;

    let json_path = session.join("summary.json");
    syncsafe::write_atomic(&json_path, serde_json::to_vec_pretty(&summary)?)?;
    let md_path = session.join("summary.md");
    syncsafe::write_atomic(&md_path, summary.markdown.as_bytes())?;

    println!(
        "wrote summary.json + summary.md ({} action items, {} decisions)",
        summary.structured.action_items.len(),
        summary.structured.decisions.len()
    );
    Ok(())
}

fn resolve_vault_path(vault_arg: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(p) = vault_arg {
        return Ok(p);
    }
    if let Some(profile) = env_profile_dir() {
        return Ok(profile.join("keys.vault.json"));
    }
    Ok(bootstrap_profile_dir()?.join("keys.vault.json"))
}

fn read_stdin_trimmed() -> anyhow::Result<String> {
    use std::io::Read;
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s.trim().to_string())
}

fn cmd_set_key(
    provider: String,
    key_file: Option<PathBuf>,
    vault_arg: Option<PathBuf>,
    passphrase_stdin: bool,
) -> anyhow::Result<()> {
    // 1. Resolve the key.
    let key = match &key_file {
        Some(p) => std::fs::read_to_string(p)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", p.display()))?
            .trim()
            .to_string(),
        None => {
            eprintln!("reading key from stdin...");
            read_stdin_trimmed()?
        }
    };
    if key.is_empty() {
        anyhow::bail!("the API key is empty");
    }

    // 2. Resolve the vault path.
    let vault_path = resolve_vault_path(vault_arg)?;
    if !vault_path.exists() {
        anyhow::bail!(
            "no vault at {} — has the app's vault been created? (open the Daisy app's Settings once)",
            vault_path.display()
        );
    }

    // 3. Get the passphrase.
    let passphrase = if passphrase_stdin {
        use std::io::BufRead;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        line.trim_end_matches(['\n', '\r']).to_string()
    } else {
        rpassword::prompt_password("Vault passphrase: ")?
    };

    // 4. Decrypt.
    let bytes = std::fs::read(&vault_path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", vault_path.display()))?;
    let envelope: vault::Envelope = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("parse vault {}: {e}", vault_path.display()))?;
    let plaintext = vault::decrypt(&envelope, &passphrase)
        .map_err(|_| anyhow::anyhow!("could not decrypt the vault — wrong passphrase?"))?;
    let mut payload: VaultPayload = serde_json::from_slice(&plaintext)
        .map_err(|e| anyhow::anyhow!("vault contents are not the expected JSON: {e}"))?;

    // 5. Update — set api_key, leave model/base_url as they were.
    payload
        .providers
        .entry(provider.clone())
        .or_default()
        .api_key = Some(key);

    // 6. Re-encrypt + atomic write.
    let new_plaintext = serde_json::to_vec(&payload)?;
    let new_envelope = vault::encrypt(&new_plaintext, &passphrase)
        .map_err(|e| anyhow::anyhow!("re-encrypt vault: {e}"))?;
    let serialized = serde_json::to_vec_pretty(&new_envelope)?;
    syncsafe::write_atomic(&vault_path, &serialized)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", vault_path.display()))?;

    // 7. Report.
    println!("stored api_key for {:?} in {}", provider, vault_path.display());
    println!("if the Daisy app is open, quit & relaunch it so it re-reads the vault.");
    Ok(())
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Version => {
            println!("daisy {}", env!("CARGO_PKG_VERSION"));
        }
        Cmd::LicenseKeygen { out } => {
            use base64::{engine::general_purpose::STANDARD as B64, Engine};
            let seed: [u8; 32] = rand::random();
            let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
            let verifying = signing.verifying_key();
            std::fs::write(&out, B64.encode(signing.to_bytes()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o600));
            }
            println!("PUBLIC (paste into LICENSE_PUBKEY_B64): {}", B64.encode(verifying.to_bytes()));
            println!("PRIVATE written to: {} (keep secret; do not commit)", out.display());
        }
        Cmd::LicenseSign { priv_file, name, email, days } => {
            use base64::{engine::general_purpose::STANDARD as B64, Engine};
            use ed25519_dalek::Signer;
            let sk_b64 = std::fs::read_to_string(&priv_file)?;
            let sk_bytes = B64.decode(sk_b64.trim())?;
            let sk = ed25519_dalek::SigningKey::from_bytes(
                sk_bytes.as_slice().try_into().map_err(|_| anyhow::anyhow!("bad private key length"))?,
            );
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let expires = days.map(|d| now + d * 86_400);
            let payload = serde_json::json!({ "name": name, "email": email, "issued": now, "expires": expires });
            let payload_bytes = serde_json::to_vec(&payload)?;
            let sig = sk.sign(&payload_bytes);
            println!("{}.{}", B64.encode(&payload_bytes), B64.encode(sig.to_bytes()));
        }
        Cmd::Sources => {
            let sources = list_sources()?;
            if sources.is_empty() {
                println!("(no sources found — is PipeWire running?)");
            }
            for s in &sources {
                let kind = match s.kind {
                    SourceKind::Mic => "mic     ",
                    SourceKind::Monitor => "monitor ",
                };
                println!(
                    "  [{}] id={:<5} {} ({} Hz, {} ch)",
                    kind, s.id, s.description, s.default_sample_rate, s.default_channels
                );
                println!("            node.name = {}", s.node_name);
            }
        }
        Cmd::Aec { mic, far, out } => {
            let (mic_pcm, mic_sr) = recording::read_wav_mono_i16(&mic)?;
            let (far_pcm, far_sr) = recording::read_wav_mono_i16(&far)?;
            anyhow::ensure!(mic_sr == 16_000 && far_sr == 16_000, "expected 16 kHz audio; got {mic_sr}/{far_sr} Hz");
            let n = mic_pcm.len().min(far_pcm.len());
            println!("AEC: {} samples ({} s of audio)", n, n / 16_000);
            let mut aec_engine = aec::AcousticEchoCanceller::new()?;
            let frame_size = aec::AcousticEchoCanceller::FRAME_SIZE;
            let mut output = Vec::with_capacity(n);
            let mut i = 0;
            while i + frame_size <= n {
                let frame = aec_engine
                    .process(&mic_pcm[i..i + frame_size], &far_pcm[i..i + frame_size])?;
                output.extend_from_slice(&frame);
                i += frame_size;
            }
            recording::write_wav_mono_i16(&out, &output, 16_000)?;
            println!("wrote {}", out.display());
        }
        Cmd::CaptureOne {
            source,
            duration_secs,
            out,
        } => {
            let sources = audio_engine::list_sources()?;
            let s = sources
                .iter()
                .find(|s| s.id == source)
                .ok_or_else(|| anyhow::anyhow!("source id {source} not found"))?;
            audio_engine::capture_one(s, std::time::Duration::from_secs(duration_secs), &out)?;
            println!("wrote {}", out.display());
        }
        Cmd::Capture {
            virtual_sink,
            auto_route,
            run_aec,
            mic,
            system,
            duration_secs,
            out,
        } => {
            let outputs = if virtual_sink {
                let vs = audio_engine::VirtualSink::create("daisy-capture")?;
                // Wait for PipeWire to register the new sink.
                std::thread::sleep(std::time::Duration::from_millis(300));
                let monitor_name = vs.monitor_source_name();
                let sources = audio_engine::list_sources()?;
                let sys_id = sources
                    .iter()
                    .find(|s| s.node_name == monitor_name)
                    .ok_or_else(|| {
                        anyhow::anyhow!("virtual-sink monitor '{monitor_name}' not found in source list")
                    })?
                    .id;
                let moved_streams = if auto_route {
                    let m = vs.route_playing_streams()?;
                    if !m.is_empty() {
                        println!("(auto-route) moved {} stream(s) into daisy-capture", m.len());
                        for (id, src) in &m {
                            println!("    stream {} from {}", id, src);
                        }
                    } else {
                        println!("(auto-route) no playing output streams to move");
                    }
                    m
                } else {
                    Vec::new()
                };
                let outputs = audio_engine::capture_dual(
                    audio_engine::DualCaptureRequest {
                        mic_source_id: mic,
                        system_source_id: sys_id,
                        duration: std::time::Duration::from_secs(duration_secs),
                        sample_rate: 16_000,
                    },
                    &out,
                )?;
                if !moved_streams.is_empty() {
                    audio_engine::VirtualSink::restore_routing(&moved_streams);
                    println!("(auto-route) restored {} stream(s)", moved_streams.len());
                }
                println!("(virtual sink) mic:      {}", outputs.mic_wav.display());
                println!("(virtual sink) system:   {}", outputs.system_wav.display());
                println!(
                    "(virtual sink) manifest: {}",
                    outputs.manifest_json.display()
                );
                // Dropping vs tears down both pactl modules.
                drop(vs);
                outputs
            } else {
                let sys = system.ok_or_else(|| {
                    anyhow::anyhow!("--system required (or use --virtual-sink)")
                })?;
                let outputs = audio_engine::capture_dual(
                    audio_engine::DualCaptureRequest {
                        mic_source_id: mic,
                        system_source_id: sys,
                        duration: std::time::Duration::from_secs(duration_secs),
                        sample_rate: 16_000,
                    },
                    &out,
                )?;
                println!("mic:      {}", outputs.mic_wav.display());
                println!("system:   {}", outputs.system_wav.display());
                println!("manifest: {}", outputs.manifest_json.display());
                outputs
            };
            if run_aec {
                let mic_aec_path = out.join("mic_aec.wav");
                let (mic_pcm, mic_sr) = recording::read_wav_mono_i16(&outputs.mic_wav)?;
                let (sys_pcm, sys_sr) = recording::read_wav_mono_i16(&outputs.system_wav)?;
                anyhow::ensure!(mic_sr == 16_000 && sys_sr == 16_000, "expected 16 kHz audio; got {mic_sr}/{sys_sr} Hz");
                let n = mic_pcm.len().min(sys_pcm.len());
                let mut aec_engine = aec::AcousticEchoCanceller::new()?;
                let frame_size = aec::AcousticEchoCanceller::FRAME_SIZE;
                let mut output = Vec::with_capacity(n);
                let mut i = 0;
                while i + frame_size <= n {
                    let frame = aec_engine
                        .process(&mic_pcm[i..i + frame_size], &sys_pcm[i..i + frame_size])?;
                    output.extend_from_slice(&frame);
                    i += frame_size;
                }
                recording::write_wav_mono_i16(&mic_aec_path, &output, 16_000)?;
                println!("AEC:      {}", mic_aec_path.display());
            }
        }
        Cmd::SessionFinalize { path, heartbeat_max_age_secs } => {
            // Step 1: refuse a session whose heartbeat is fresher than
            // `heartbeat_max_age_secs` — the recorder is still alive.
            let hb_path = path.join("heartbeat");
            if hb_path.exists()
                && recording::heartbeat::Heartbeat::is_alive(&hb_path, heartbeat_max_age_secs)
            {
                let snap = recording::heartbeat::Heartbeat::read(&hb_path)?;
                anyhow::bail!(
                    "session is still live (heartbeat from pid {} within {}s)",
                    snap.pid,
                    snap.age_seconds()
                );
            }

            // Step 2: mark the session interrupted (recording ended without a
            // clean stop; a successful finalize below clears it) — unless it
            // is already finalized. Backfill chunk durations from WAV
            // lengths, and run AEC if the manifest's aec_mode is Always.
            // Idempotent.
            {
                let mut session = recording::session::Session::load(&path)?;
                session.update_manifest(|m| {
                    if m.finalized_at_unix_seconds.is_none() {
                        m.interrupted = true;
                    }
                })?;
                recording::manifest_ops::backfill_chunk_durations(&mut session)?;
            }
            recording::apply_aec(&path)?;

            // Step 3: transcribe with any installed model (registry order,
            // profile models dir or the model-dir env vars), only if
            // transcript.json is missing.
            let transcript_path = path.join("transcript.json");
            if !transcript_path.is_file() {
                use providers_local::registry::Engine;
                match find_recovery_model() {
                    Some((row, model_path)) => {
                        let provider: Box<dyn providers_http::Transcriber> = match row.engine {
                            Engine::Parakeet => Box::new(
                                providers_local::parakeet::batch::ParakeetLocalTranscriber::new(
                                    model_path,
                                    Default::default(),
                                    row.display,
                                ),
                            ),
                            _ => Box::new(providers_local::WhisperLocalTranscriber::new(
                                &model_path,
                            )?),
                        };
                        match transcribe_with(path.clone(), provider, "en".into()) {
                            Ok(_) => eprintln!("session-finalize: transcribe done"),
                            Err(e) => eprintln!("session-finalize: transcribe failed: {e}"),
                        }
                    }
                    None => eprintln!(
                        "session-finalize: no installed model found (profile models dir or the \
                         DAISY_*_MODEL_DIR env vars); download one in the app under Settings → \
                         Providers, then rerun"
                    ),
                }
            } else {
                eprintln!("session-finalize: transcript.json already on disk — skipping transcribe");
            }

            // Step 4: dedup — only if transcript.dedup.json missing AND
            // transcript.json present (the latter is the input).
            let dedup_path = path.join("transcript.dedup.json");
            if !dedup_path.is_file() && transcript_path.is_file() {
                let defaults = transcript::dedup::DedupParams::default();
                match cmd_dedup(
                    path.clone(),
                    defaults.jaccard_threshold,
                    defaults.mic_quiet_dbfs,
                    defaults.overlap_slack_ms,
                    !defaults.drop_backchannels,
                ) {
                    Ok(_) => eprintln!("session-finalize: dedup done"),
                    Err(e) => eprintln!("session-finalize: dedup failed: {e}"),
                }
            } else if dedup_path.is_file() {
                eprintln!("session-finalize: transcript.dedup.json already on disk — skipping dedup");
            }

            // Step 5: build meeting.opus. Skipped when meeting.opus is
            // already on disk (idempotent).
            let opus_path = path.join(recording::mixdown::MEETING_AUDIO_NAME);
            if !opus_path.is_file() {
                if let Ok(bytes) = std::fs::read(path.join("manifest.json")) {
                    if let Ok(m) =
                        serde_json::from_slice::<recording::manifest::SessionManifest>(&bytes)
                    {
                        match recording::mixdown::build_meeting_audio(
                            &path,
                            &m,
                            &recording::compress::CompressParams::default(),
                        ) {
                            Ok(n) => eprintln!(
                                "session-finalize: built meeting.opus ({} bytes)",
                                n
                            ),
                            Err(e) => eprintln!("session-finalize: meeting.opus build failed: {e}"),
                        }
                    }
                }
            } else {
                eprintln!("session-finalize: meeting.opus already on disk — skipping mixdown");
            }

            // Step 6: stamp finalized_at_unix_seconds and clear the
            // interrupted flag — but only when a transcript exists; a run
            // where transcription was skipped or failed leaves the manifest
            // interrupted so the app offers recovery. Removes the stale
            // heartbeat on success.
            if !transcript_path.is_file() {
                anyhow::bail!(
                    "transcription did not run (missing DAISY_WHISPER_MODEL_DIR or transcribe failed); \
                     session left marked interrupted — the app will offer recovery for {}",
                    path.display()
                );
            }
            let mut session = recording::session::Session::load(&path)?;
            recording::manifest_ops::stamp_finalized(&mut session, false)?;
            let _ = syncsafe::remove_file(&hb_path);

            println!("finalized: {}", path.display());
        }
        Cmd::Record { mic, system, virtual_sink, out, session_id } => {
            cmd_record(mic, system, virtual_sink, out, session_id)?;
        }
        Cmd::Transcribe { session, model, language } => {
            cmd_transcribe(session, model, language)?;
        }
        Cmd::Dedup { session, jaccard_threshold, mic_quiet_dbfs, overlap_slack_ms, keep_backchannels } => {
            cmd_dedup(session, jaccard_threshold, mic_quiet_dbfs, overlap_slack_ms, keep_backchannels)?;
        }
        Cmd::Summarize { session, provider, model, tag_prompt } => {
            cmd_summarize(session, provider, model, tag_prompt)?;
        }
        Cmd::DownloadModel { size, dir } => {
            cmd_download_model(size, dir)?;
        }
        Cmd::SetKey { provider, key_file, vault, passphrase_stdin } => {
            cmd_set_key(provider, key_file, vault, passphrase_stdin)?;
        }
    }
    Ok(())
}
