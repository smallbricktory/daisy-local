// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Measures what `AudioCategory_Communications` changes on a WASAPI capture
//! stream: which effects the OS attaches, whether the mix format shifts, how
//! much acoustic echo the OEM APO chain removes, and what it does to the
//! near-end voice while echo is present.
//!
//! One run is one pass over the mic, selected by `WASAPI_COMMS_PROBE_MODE`:
//!   default     — `IAudioClient::Initialize` with no client properties (the
//!                 path `crates/audio-engine/src/windows/mod.rs` ships today).
//!   comms       — `IAudioClient2::SetClientProperties` with
//!                 `eCategory = AudioCategory_Communications` before Initialize.
//!   comms-no-ns — communications with NS and DeepNS switched off, when the
//!                 driver allows it.
//!   duck-default / duck-comms — no mic measurement; opens a capture stream in
//!                 the named mode and records the render endpoint to show
//!                 whether audio another process is playing gets attenuated.
//!
//! A pass plays speech out the render endpoint and walks four windows:
//!   far        far-end speech only — what is left is echo residual
//!   double     far-end speech plus your voice — the window that decides this
//!   near       your voice alone, playback stopped — the no-echo baseline
//!   tail       silence — the processing noise floor
//!
//! Stationary noise is useless as a stimulus here: a noise suppressor removes
//! it whether or not the AEC is running, so speech is what separates them.
//!
//! Run on Windows, speakers ON, and read something aloud when prompted:
//!   cargo run -p audio-engine --example wasapi_comms_probe
//!
//! Env:
//!   WASAPI_COMMS_PROBE_MIC_INDEX     0-based index into the printed capture list
//!   WASAPI_COMMS_PROBE_RENDER_INDEX  0-based index into the printed render list;
//!                                    without it the default render endpoint is used
//!   WASAPI_COMMS_PROBE_DIR           where the per-pass WAVs are written (default .)
//!   WASAPI_COMMS_PROBE_LEVEL         render amplitude scale (default 1.0)
//!   WASAPI_COMMS_PROBE_STIMULUS      speech WAV to play (16-bit PCM mono)
//!   WASAPI_COMMS_PROBE_MODE          REQUIRED: default | comms | comms-no-ns |
//!                                    duck-default | duck-comms
//!   WASAPI_COMMS_PROBE_BARE          set to 1 for the control run: no playback,
//!                                    no loopback reference, just talk into the
//!                                    mic in each mode. Answers whether a
//!                                    communications stream captures at all.

#[cfg(target_os = "windows")]
mod imp {
    use std::ptr::{addr_of, read_unaligned};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use windows::core::{GUID, PCWSTR};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eRender, AudioCategory_Communications, AudioClientProperties, EDataFlow,
        IAcousticEchoCancellationControl, IAudioCaptureClient, IAudioClient2,
        IAudioEffectsManager, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
        AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
        AUDCLNT_STREAMOPTIONS_NONE, AUDIO_EFFECT, AUDIO_EFFECT_STATE_OFF, AUDIO_EFFECT_STATE_ON,
        DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    };
    use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
    use windows::Win32::Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED, STGM_READ,
    };
    use windows::Win32::System::Variant::VT_LPWSTR;

    type R<T> = Result<T, Box<dyn std::error::Error>>;

    /// `println!` leaves stdout block-buffered when it is not a terminal, which
    /// hides the live cues behind a pipe. Every line the operator needs during a
    /// run goes through here.
    macro_rules! say {
        ($($arg:tt)*) => {{
            let line = format!($($arg)*);
            println!("{line}");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            if let Ok(mut log) = LOG.lock() {
                log.push(line);
            }
        }};
    }

    /// Everything `say!` emitted, written to `summary.txt` beside the WAVs: a
    /// run whose console output never reaches the operator is still readable.
    static LOG: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    const FAR_SECS: f32 = 8.0;
    const DOUBLE_SECS: f32 = 6.0;
    const NEAR_SECS: f32 = 6.0;
    const TAIL_SECS: f32 = 3.0;
    const BARE_SECS: f32 = 6.0;
    const PASS_SECS: f32 = FAR_SECS + DOUBLE_SECS + NEAR_SECS + TAIL_SECS;
    const STIMULUS_RATE: u32 = 16_000;
    const DEFAULT_STIMULUS: &str = "crates/audio-engine/tests/fixtures/different_sources_30s.sys.wav";
    // Guard bands dropped from each measurement window, absorbing render
    // start-up and the tail of the last rendered buffer.
    const GUARD_SECS: f32 = 0.6;
    const BUFFER_HNS: i64 = 200 * 10_000;

    // ksmedia.h effect identifiers; windows-rs 0.59 does not export them.
    const FX_AEC: GUID = GUID::from_u128(0x6f64adbe_8211_11e2_8c70_2c27d7f001fa);
    const FX_NS: GUID = GUID::from_u128(0x6f64adbf_8211_11e2_8c70_2c27d7f001fa);
    const FX_AGC: GUID = GUID::from_u128(0x6f64adc0_8211_11e2_8c70_2c27d7f001fa);
    const FX_BEAMFORMING: GUID = GUID::from_u128(0x6f64adc1_8211_11e2_8c70_2c27d7f001fa);
    const FX_CONSTANT_TONE: GUID = GUID::from_u128(0x6f64adc2_8211_11e2_8c70_2c27d7f001fa);
    const FX_EQ: GUID = GUID::from_u128(0x6f64adc3_8211_11e2_8c70_2c27d7f001fa);
    const FX_DEEP_NS: GUID = GUID::from_u128(0x6f64add0_8211_11e2_8c70_2c27d7f001fa);

    const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID = GUID::from_values(
        0x0000_0003,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
    );
    const KSDATAFORMAT_SUBTYPE_PCM: GUID = GUID::from_values(
        0x0000_0001,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
    );

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Default,
        Communications,
        CommunicationsNoNs,
        /// Does opening our capture stream attenuate audio another process is
        /// already playing? Measured on the render endpoint, not the mic.
        DuckDefault,
        DuckCommunications,
    }

    impl Mode {
        fn label(self) -> &'static str {
            match self {
                Mode::Default => "A default",
                Mode::Communications => "B communications",
                Mode::CommunicationsNoNs => "C communications, NS+DeepNS off",
                Mode::DuckDefault => "ducking control, default capture",
                Mode::DuckCommunications => "ducking, communications capture",
            }
        }
        fn slug(self) -> &'static str {
            match self {
                Mode::Default => "default",
                Mode::Communications => "comms",
                Mode::CommunicationsNoNs => "comms_no_ns",
                Mode::DuckDefault => "duck_default",
                Mode::DuckCommunications => "duck_comms",
            }
        }
        fn communications(self) -> bool {
            !matches!(self, Mode::Default | Mode::DuckDefault)
        }
        fn ducking(self) -> bool {
            matches!(self, Mode::DuckDefault | Mode::DuckCommunications)
        }
    }

    struct Effect {
        id: GUID,
        on: bool,
        can_set: bool,
    }

    struct Measurement {
        format: Format,
        effects: Option<Vec<Effect>>,
        disabled: Vec<String>,
        aec_control: Result<(), String>,
        far_dbfs: f32,
        double_dbfs: f32,
        near_dbfs: f32,
        tail_dbfs: f32,
        reference_dbfs: f32,
    }

    pub fn run() -> R<()> {
        let mode = match std::env::var("WASAPI_COMMS_PROBE_MODE").as_deref() {
            Ok("default") => Mode::Default,
            Ok("comms") => Mode::Communications,
            Ok("comms-no-ns") => Mode::CommunicationsNoNs,
            Ok("duck-default") => Mode::DuckDefault,
            Ok("duck-comms") => Mode::DuckCommunications,
            Ok(other) => return Err(format!("unknown mode {other}").into()),
            Err(_) => {
                return Err("set WASAPI_COMMS_PROBE_MODE to default | comms | comms-no-ns \
                            | duck-default | duck-comms"
                    .into())
            }
        };

        let _com = ComGuard::init()?;
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };

        let mics = endpoints(&enumerator, eCapture)?;
        say!("Capture endpoints:");
        for (i, (_, name)) in mics.iter().enumerate() {
            say!("  [{i:2}] {name}");
        }
        let idx = match std::env::var("WASAPI_COMMS_PROBE_MIC_INDEX") {
            Ok(v) => v.parse::<usize>().map_err(|_| "MIC_INDEX is not a number")?,
            Err(_) => default_capture_index(&enumerator, &mics)?,
        };
        let (mic, mic_name) = mics.get(idx).ok_or("WASAPI_COMMS_PROBE_MIC_INDEX out of range")?;

        let renders = endpoints(&enumerator, eRender)?;
        say!("\nRender endpoints:");
        for (i, (dev, name)) in renders.iter().enumerate() {
            say!("  [{i:2}] {name}{}", volume_suffix(dev));
        }
        let render: &IMMDevice = match std::env::var("WASAPI_COMMS_PROBE_RENDER_INDEX") {
            Ok(v) => {
                let i: usize = v.parse().map_err(|_| "RENDER_INDEX is not a number")?;
                &renders.get(i).ok_or("RENDER_INDEX out of range")?.0
            }
            Err(_) => &unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole)? }.clone(),
        };
        let render_id = device_id_string(render)?;
        let render_name = friendly_name(render).unwrap_or_else(|| "<unnamed>".into());

        say!("\nMic    : [{idx}] {mic_name}");
        say!("Render : {render_name}{}", volume_suffix(render));
        let stimulus_path =
            std::env::var("WASAPI_COMMS_PROBE_STIMULUS").unwrap_or_else(|_| DEFAULT_STIMULUS.into());
        let stimulus = Arc::new(read_stimulus(&stimulus_path)?);
        say!("Stimulus: {stimulus_path}");
        if mode.ducking() {
            say!(
                "\nThis run measures another app's audio, not yours: keep something playing\
                 \nat a steady level and do not touch the volume. Nothing is asked of you.\n"
            );
        } else {
            say!(
                "\nEach pass runs {PASS_SECS:.0} s: {FAR_SECS:.0} s speech, {DOUBLE_SECS:.0} s speech\
                 \nwhile YOU talk, {NEAR_SECS:.0} s of you alone, {TAIL_SECS:.0} s silence.\
                 \nBeeps tell you what to do: LONG = stay quiet, 3 SHORT = start\
                 \ntalking, 2 SHORT = stop. Speakers on, normal speaking voice.\n"
            );
        }

        let level: f32 = std::env::var("WASAPI_COMMS_PROBE_LEVEL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0);
        let out_dir = std::path::PathBuf::from(
            std::env::var("WASAPI_COMMS_PROBE_DIR").unwrap_or_else(|_| ".".into()),
        );
        std::fs::create_dir_all(&out_dir)?;

        let bare = std::env::var("WASAPI_COMMS_PROBE_BARE").as_deref() == Ok("1");
        if bare {
            say!(
                "\nBARE control run: nothing is played. Talk continuously for the whole\
                 \n{BARE_SECS:.0} s of each pass — the beeps still mark start and stop.\n"
            );
        }

        say!("── {} ─────────────────────────────", mode.label());
        if mode.ducking() {
            measure_ducking(mic, render, &render_id, mode, &out_dir)?;
            return write_summary(&out_dir, mode);
        }

        let m = measure(mic, render, &render_id, mode, level, &stimulus, bare, &out_dir)?;
        report(&m);
        if m.reference_dbfs < -60.0 && m.reference_dbfs.is_finite() {
            say!(
                "Reference loopback is silent: nothing reached the render endpoint, so the\n\
                 mic numbers are room noise only."
            );
        }
        say!("\nWAVs written to {}", out_dir.display());
        write_summary(&out_dir, mode)
    }

    /// Everything `say!` emitted, beside the WAVs.
    fn write_summary(out_dir: &std::path::Path, mode: Mode) -> R<()> {
        if let Ok(log) = LOG.lock() {
            std::fs::write(out_dir.join(format!("summary_{}.txt", mode.slug())), log.join("\n"))?;
        }
        Ok(())
    }

    /// Holds a capture stream open while a loopback of the render endpoint runs
    /// underneath, so any attenuation Windows applies to another process's audio
    /// shows up as a level step between the phases.
    fn measure_ducking(
        mic: &IMMDevice,
        render: &IMMDevice,
        render_id: &str,
        mode: Mode,
        out_dir: &std::path::Path,
    ) -> R<()> {
        const BEFORE_SECS: f32 = 5.0;
        const DURING_SECS: f32 = 8.0;
        const AFTER_SECS: f32 = 5.0;

        let stop = Arc::new(AtomicBool::new(false));
        let reference = spawn_reference_capture(render, stop.clone())?;

        say!("  phase 1 · baseline — leave the audio playing, do not touch the volume");
        thread::sleep(Duration::from_secs_f32(BEFORE_SECS));

        say!("  phase 2 · capture stream OPEN");
        let capture_stop = Arc::new(AtomicBool::new(false));
        let holder = spawn_capture_holder(mic, render_id, mode, capture_stop.clone())?;
        thread::sleep(Duration::from_secs_f32(DURING_SECS));

        capture_stop.store(true, Ordering::SeqCst);
        let held = holder
            .join()
            .unwrap_or_else(|_| Err("holder panicked".to_string()));
        say!("  phase 3 · capture stream CLOSED");
        thread::sleep(Duration::from_secs_f32(AFTER_SECS));

        stop.store(true, Ordering::SeqCst);
        let (samples, rate) = reference.join().unwrap_or_default();
        if let Err(e) = held {
            return Err(e.into());
        }
        if rate == 0 {
            return Err("reference loopback produced nothing".into());
        }
        write_wav(
            &out_dir.join(format!("loopback_{}.wav", mode.slug())),
            &samples,
            rate,
        )?;

        let r = rate as f32;
        let before = rms_dbfs(&samples, r, 0.5, BEFORE_SECS - 0.3);
        let during = rms_dbfs(&samples, r, BEFORE_SECS + 0.5, BEFORE_SECS + DURING_SECS - 0.3);
        let after = rms_dbfs(
            &samples,
            r,
            BEFORE_SECS + DURING_SECS + 0.5,
            BEFORE_SECS + DURING_SECS + AFTER_SECS - 0.3,
        );

        say!("\n── result ─────────────────────────────────");
        say!("before {before:>7.1}   during {during:>7.1}   after {after:>7.1} dBFS");
        say!("drop while our stream was open : {:>6.1} dB", before - during);
        say!("recovery after we closed it    : {:>6.1} dB", after - during);
        if before < -60.0 {
            say!("\nNothing was playing: start audio in another app and run this again.");
        } else if before - during > 3.0 && after - during > 3.0 {
            say!("\nDUCKED — Windows attenuated the other app while our stream was open.");
        } else {
            say!("\nNo ducking: the other app's level did not move when our stream opened.");
        }
        Ok(())
    }

    /// Opens a capture stream in the requested mode and keeps it running until
    /// told to stop, draining packets so the stream stays live.
    fn spawn_capture_holder(
        mic: &IMMDevice,
        render_id: &str,
        mode: Mode,
        stop: Arc<AtomicBool>,
    ) -> R<thread::JoinHandle<Result<(), String>>> {
        let mic_id_string = device_id_string(mic)?;
        let render_id_string = render_id.to_string();

        Ok(thread::spawn(move || {
            hold_capture(&mic_id_string, &render_id_string, mode, &stop).map_err(|e| e.to_string())
        }))
    }

    fn hold_capture(
        mic_id_string: &str,
        render_id_string: &str,
        mode: Mode,
        stop: &AtomicBool,
    ) -> R<()> {
        let _com = ComGuard::init()?;
        let device = device_by_id(mic_id_string)?;
        let (client, fmt) = open_shared_client(&device, mode.communications(), 0)?;
        if mode.communications() {
            let _ = pin_render_endpoint(&client, render_id_string);
        }

        let capture: IAudioCaptureClient = unsafe { client.GetService::<IAudioCaptureClient>()? };
        unsafe { client.Start()? };
        let mut sink = Vec::new();
        while !stop.load(Ordering::SeqCst) {
            drain_packets(&capture, &fmt, &mut sink)?;
            sink.clear();
            thread::sleep(Duration::from_millis(5));
        }
        unsafe { client.Stop()? };
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn measure(
        mic: &IMMDevice,
        render: &IMMDevice,
        render_id: &str,
        mode: Mode,
        level: f32,
        stimulus: &Arc<Vec<f32>>,
        bare: bool,
        out_dir: &std::path::Path,
    ) -> R<Measurement> {
        let (client, format) = open_shared_client(mic, mode.communications(), 0)?;

        let disabled = if mode == Mode::CommunicationsNoNs {
            disable_effects(&client, &[FX_NS, FX_DEEP_NS])
        } else {
            Vec::new()
        };
        let effects = query_effects(&client);
        let aec_control = pin_render_endpoint(&client, render_id);

        let capture: IAudioCaptureClient = unsafe { client.GetService::<IAudioCaptureClient>()? };

        let stop = Arc::new(AtomicBool::new(false));
        let (renderer, reference, prompts) = if bare {
            (None, None, None)
        } else {
            (
                Some(spawn_renderer(render, level, stimulus.clone(), stop.clone())?),
                Some(spawn_reference_capture(render, stop.clone())?),
                Some(spawn_prompts(stop.clone())),
            )
        };
        let window = if bare { BARE_SECS } else { PASS_SECS };

        unsafe { client.Start()? };
        if bare {
            say!("  talk now — {window:.0} s");
        }
        let samples = capture_loop(&capture, &format, window)?;
        unsafe { client.Stop()? };
        stop.store(true, Ordering::SeqCst);
        if let Some(h) = renderer {
            let _ = h.join();
        }
        if let Some(h) = prompts {
            let _ = h.join();
        }
        let (reference_samples, reference_rate) =
            reference.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
        if bare {
            say!("  stop");
        }

        write_wav(
            &out_dir.join(format!("mic_{}.wav", mode.slug())),
            &samples,
            format.sample_rate,
        )?;
        if reference_rate > 0 {
            write_wav(
                &out_dir.join(format!("ref_{}.wav", mode.slug())),
                &reference_samples,
                reference_rate,
            )?;
        }

        let rate = format.sample_rate as f32;
        if bare {
            let level = rms_dbfs(&samples, rate, GUARD_SECS, BARE_SECS);
            return Ok(Measurement {
                format,
                effects,
                disabled,
                aec_control,
                far_dbfs: level,
                double_dbfs: f32::NEG_INFINITY,
                near_dbfs: level,
                tail_dbfs: f32::NEG_INFINITY,
                reference_dbfs: f32::NEG_INFINITY,
            });
        }
        let far_end = FAR_SECS;
        let double_end = far_end + DOUBLE_SECS;
        let near_end = double_end + NEAR_SECS;
        let reference_dbfs = if reference_rate > 0 {
            rms_dbfs(
                &reference_samples,
                reference_rate as f32,
                GUARD_SECS,
                far_end,
            )
        } else {
            f32::NEG_INFINITY
        };

        Ok(Measurement {
            format,
            effects,
            disabled,
            aec_control,
            far_dbfs: rms_dbfs(&samples, rate, GUARD_SECS, far_end),
            double_dbfs: rms_dbfs(&samples, rate, far_end + GUARD_SECS, double_end),
            near_dbfs: rms_dbfs(&samples, rate, double_end + GUARD_SECS, near_end),
            tail_dbfs: rms_dbfs(&samples, rate, near_end + GUARD_SECS, PASS_SECS),
            reference_dbfs,
        })
    }

    fn report(m: &Measurement) {
        say!(
            "format      : {} Hz, {} ch, {}",
            m.format.sample_rate,
            m.format.channels,
            if m.format.float { "f32" } else { "i16" }
        );
        match &m.effects {
            Some(list) if list.is_empty() => say!("effects     : none reported"),
            Some(list) => {
                let names: Vec<String> = list
                    .iter()
                    .map(|fx| {
                        format!(
                            "{}{}{}",
                            effect_name(&fx.id),
                            if fx.on { "" } else { "(off)" },
                            if fx.can_set { "" } else { "(locked)" }
                        )
                    })
                    .collect();
                say!("effects     : {}", names.join(", "));
            }
            None => say!("effects     : IAudioEffectsManager unavailable"),
        }
        if !m.disabled.is_empty() {
            say!("disabled    : {}", m.disabled.join(", "));
        }
        match &m.aec_control {
            Ok(()) => say!("aec control : render endpoint pinned"),
            Err(e) => say!("aec control : unavailable ({e})"),
        }
        say!(
            "reference   : {:>7.1} dBFS  (loopback of the render endpoint during stimulus)",
            m.reference_dbfs
        );
        say!(
            "mic RMS     : far {:>7.1}   double {:>7.1}   near {:>7.1}   tail {:>7.1} dBFS",
            m.far_dbfs, m.double_dbfs, m.near_dbfs, m.tail_dbfs
        );
    }

    // ─── capture ──────────────────────────────────────────────────────────────

    /// Activate + optionally mark communications + Initialize a shared-mode
    /// client at the endpoint's mix format.
    fn open_shared_client(
        device: &IMMDevice,
        comms: bool,
        stream_flags: u32,
    ) -> R<(IAudioClient2, Format)> {
        let client: IAudioClient2 = unsafe { device.Activate::<IAudioClient2>(CLSCTX_ALL, None)? };
        if comms {
            let props = AudioClientProperties {
                cbSize: std::mem::size_of::<AudioClientProperties>() as u32,
                bIsOffload: false.into(),
                eCategory: AudioCategory_Communications,
                Options: AUDCLNT_STREAMOPTIONS_NONE,
            };
            unsafe { client.SetClientProperties(&props)? };
        }
        let fmt_ptr = unsafe { client.GetMixFormat()? };
        let fmt = parse_format(fmt_ptr)?;
        let init = unsafe {
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, stream_flags, BUFFER_HNS, 0, fmt_ptr, None)
        };
        unsafe { CoTaskMemFree(Some(fmt_ptr as *const _)) };
        init?;
        Ok((client, fmt))
    }

    /// Pin the stream's echo-cancellation reference to the chosen render
    /// endpoint.
    fn pin_render_endpoint(client: &IAudioClient2, render_id: &str) -> Result<(), String> {
        let ctl = unsafe { client.GetService::<IAcousticEchoCancellationControl>() }
            .map_err(|e| e.to_string())?;
        let wide: Vec<u16> = render_id.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe { ctl.SetEchoCancellationRenderEndpoint(PCWSTR(wide.as_ptr())) }
            .map_err(|e| e.to_string())
    }

    /// One sweep of every packet currently queued on the capture client.
    fn drain_packets(capture: &IAudioCaptureClient, fmt: &Format, out: &mut Vec<f32>) -> R<()> {
        loop {
            let packet = unsafe { capture.GetNextPacketSize()? };
            if packet == 0 {
                return Ok(());
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames: u32 = 0;
            let mut flags: u32 = 0;
            unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)? };
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null();
            decode_into(fmt, data, frames, silent, out);
            unsafe { capture.ReleaseBuffer(frames)? };
        }
    }

    fn capture_loop(capture: &IAudioCaptureClient, fmt: &Format, secs: f32) -> R<Vec<f32>> {
        let mut out = Vec::with_capacity((fmt.sample_rate as f32 * secs) as usize);
        let deadline = Instant::now() + Duration::from_secs_f32(secs);
        while Instant::now() < deadline {
            drain_packets(capture, fmt, &mut out)?;
            thread::sleep(Duration::from_millis(5));
        }
        Ok(out)
    }

    fn decode_into(fmt: &Format, data: *const u8, frames: u32, silent: bool, out: &mut Vec<f32>) {
        let n = frames as usize;
        if silent {
            out.resize(out.len() + n, 0.0);
            return;
        }
        let chans = fmt.channels as usize;
        let inv = 1.0 / chans as f32;
        if fmt.float {
            let s = unsafe { std::slice::from_raw_parts(data as *const f32, n * chans) };
            for f in 0..n {
                let base = f * chans;
                out.push((0..chans).map(|c| s[base + c]).sum::<f32>() * inv);
            }
        } else {
            let s = unsafe { std::slice::from_raw_parts(data as *const i16, n * chans) };
            let denom = i16::MAX as f32;
            for f in 0..n {
                let base = f * chans;
                out.push((0..chans).map(|c| s[base + c] as f32 / denom).sum::<f32>() * inv);
            }
        }
    }

    fn rms_dbfs(samples: &[f32], rate: f32, from_secs: f32, to_secs: f32) -> f32 {
        let a = (from_secs * rate) as usize;
        let b = ((to_secs * rate) as usize).min(samples.len());
        if b <= a {
            return f32::NEG_INFINITY;
        }
        let window = &samples[a..b];
        let mean_sq = window.iter().map(|s| s * s).sum::<f32>() / window.len() as f32;
        if mean_sq <= 0.0 {
            return f32::NEG_INFINITY;
        }
        10.0 * mean_sq.log10()
    }

    // ─── render stimulus ──────────────────────────────────────────────────────

    fn spawn_renderer(
        device: &IMMDevice,
        level: f32,
        stimulus: Arc<Vec<f32>>,
        stop: Arc<AtomicBool>,
    ) -> R<thread::JoinHandle<()>> {
        // The device pointer is re-activated on the render thread; IMMDevice is
        // agile but the client and its COM apartment must live there.
        let id_string = device_id_string(device)?;

        Ok(thread::spawn(move || {
            if let Err(e) = render_stimulus(&id_string, level, &stimulus, &stop) {
                eprintln!("  renderer failed: {e} — echo numbers are meaningless");
            }
        }))
    }

    /// Counts the windows down on stdout while the render thread beeps the same
    /// boundaries into the speakers, so the operator can work by ear alone.
    fn spawn_prompts(stop: Arc<AtomicBool>) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let marks = [
                (0.0, FAR_SECS, "LONG beep · STAY QUIET — far end only"),
                (
                    FAR_SECS,
                    DOUBLE_SECS + NEAR_SECS,
                    "3 beeps   · TALK — keep going until the two beeps",
                ),
                (
                    FAR_SECS + DOUBLE_SECS + NEAR_SECS,
                    TAIL_SECS,
                    "2 beeps   · STOP TALKING — silence",
                ),
            ];
            let start = Instant::now();
            for (at, span, text) in marks {
                while start.elapsed() < Duration::from_secs_f32(at) {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                say!("  {text}");
                let mut left = span as i32;
                while left > 0 {
                    thread::sleep(Duration::from_secs(1));
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    left -= 1;
                    if left > 0 {
                        say!("    {left}");
                    }
                }
            }
        })
    }

    /// A 1 kHz cue burst at each window boundary. The bursts are inside the
    /// guard band every measurement window drops, so they colour no result.
    fn cue_sample(pos: u64, rate: u32) -> f32 {
        // (start, beep length, gap, count): one long tone opens the quiet
        // window, three short ones start the talking, two end it.
        let marks: [(f32, f32, f32, u32); 3] = [
            (0.0, 0.45, 0.10, 1),
            (FAR_SECS, 0.12, 0.10, 3),
            (FAR_SECS + DOUBLE_SECS + NEAR_SECS, 0.12, 0.10, 2),
        ];
        let t = pos as f32 / rate as f32;
        for (at, beep, gap, count) in marks {
            let span = (beep + gap) * count as f32;
            if t < at || t >= at + span {
                continue;
            }
            if (t - at) % (beep + gap) >= beep {
                return 0.0;
            }
            return (t * 1000.0 * std::f32::consts::TAU).sin() * 0.25;
        }
        0.0
    }

    fn render_stimulus(
        device_id: &str,
        level: f32,
        stimulus: &[f32],
        stop: &AtomicBool,
    ) -> R<()> {
        let _com = ComGuard::init()?;
        let device = device_by_id(device_id)?;
        let (client, fmt) = open_shared_client(&device, false, 0)?;

        let render: IAudioRenderClient = unsafe { client.GetService::<IAudioRenderClient>()? };
        let buffer_frames = unsafe { client.GetBufferSize()? };
        unsafe { client.Start()? };

        // The stimulus is 16 kHz mono; nearest-neighbour is enough to put it
        // in the room at the device rate.
        let step = STIMULUS_RATE as f32 / fmt.sample_rate as f32;
        let play_frames = ((FAR_SECS + DOUBLE_SECS) * fmt.sample_rate as f32) as u64;
        let mut written: u64 = 0;
        let chans = fmt.channels as usize;

        while !stop.load(Ordering::SeqCst) {
            let padding = unsafe { client.GetCurrentPadding()? };
            let free = buffer_frames.saturating_sub(padding);
            if free == 0 {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            let ptr = unsafe { render.GetBuffer(free)? };
            for f in 0..free as usize {
                let pos = written + f as u64;
                let far = if pos >= play_frames {
                    0.0
                } else {
                    let idx = (pos as f32 * step) as usize % stimulus.len();
                    stimulus[idx] * level
                };
                let s = (far + cue_sample(pos, fmt.sample_rate)).clamp(-1.0, 1.0);
                for c in 0..chans {
                    let idx = f * chans + c;
                    unsafe {
                        if fmt.float {
                            *(ptr as *mut f32).add(idx) = s;
                        } else {
                            *(ptr as *mut i16).add(idx) = (s * i16::MAX as f32) as i16;
                        }
                    }
                }
            }
            unsafe { render.ReleaseBuffer(free, 0)? };
            written += free as u64;
            thread::sleep(Duration::from_millis(5));
        }
        unsafe { client.Stop()? };
        Ok(())
    }

    // ─── device + format helpers ──────────────────────────────────────────────

    struct Format {
        sample_rate: u32,
        channels: u16,
        float: bool,
    }

    fn parse_format(ptr: *const WAVEFORMATEX) -> R<Format> {
        let tag = unsafe { read_unaligned(addr_of!((*ptr).wFormatTag)) } as u32;
        let channels = unsafe { read_unaligned(addr_of!((*ptr).nChannels)) };
        let sample_rate = unsafe { read_unaligned(addr_of!((*ptr).nSamplesPerSec)) };
        let bits = unsafe { read_unaligned(addr_of!((*ptr).wBitsPerSample)) };

        let float = if tag == WAVE_FORMAT_IEEE_FLOAT && bits == 32 {
            true
        } else if tag == 1 && bits == 16 {
            false
        } else if tag == WAVE_FORMAT_EXTENSIBLE {
            let ext = ptr as *const WAVEFORMATEXTENSIBLE;
            let sub: GUID = unsafe { read_unaligned(addr_of!((*ext).SubFormat)) };
            if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT && bits == 32 {
                true
            } else if sub == KSDATAFORMAT_SUBTYPE_PCM && bits == 16 {
                false
            } else {
                return Err(format!("unsupported SubFormat {sub:?} bits={bits}").into());
            }
        } else {
            return Err(format!("unsupported wave format tag=0x{tag:04x} bits={bits}").into());
        };

        Ok(Format {
            sample_rate,
            channels,
            float,
        })
    }

    /// Loopback-captures the render endpoint for the length of a pass, so the
    /// stimulus level is measured rather than assumed.
    fn spawn_reference_capture(
        device: &IMMDevice,
        stop: Arc<AtomicBool>,
    ) -> R<thread::JoinHandle<(Vec<f32>, u32)>> {
        let id_string = device_id_string(device)?;

        Ok(thread::spawn(move || {
            match capture_loopback(&id_string, &stop) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("  reference loopback failed: {e}");
                    (Vec::new(), 0)
                }
            }
        }))
    }

    fn capture_loopback(device_id: &str, stop: &AtomicBool) -> R<(Vec<f32>, u32)> {
        let _com = ComGuard::init()?;
        let device = device_by_id(device_id)?;
        let (client, fmt) = open_shared_client(&device, false, AUDCLNT_STREAMFLAGS_LOOPBACK)?;

        let capture: IAudioCaptureClient = unsafe { client.GetService::<IAudioCaptureClient>()? };
        unsafe { client.Start()? };
        let mut out = Vec::new();
        while !stop.load(Ordering::SeqCst) {
            drain_packets(&capture, &fmt, &mut out)?;
            thread::sleep(Duration::from_millis(5));
        }
        unsafe { client.Stop()? };
        Ok((out, fmt.sample_rate))
    }

    /// Switches the named effects off for this stream, returning what actually
    /// changed. An effect the driver marks `canSetState == false` stays on.
    fn disable_effects(client: &IAudioClient2, wanted: &[GUID]) -> Vec<String> {
        let Some(list) = query_effects(client) else {
            return Vec::new();
        };
        let Ok(mgr) = (unsafe { client.GetService::<IAudioEffectsManager>() }) else {
            return Vec::new();
        };
        let mut done = Vec::new();
        for fx in list {
            if !wanted.contains(&fx.id) || !fx.on {
                continue;
            }
            if !fx.can_set {
                done.push(format!("{} locked by driver", effect_name(&fx.id)));
                continue;
            }
            match unsafe { mgr.SetAudioEffectState(fx.id, AUDIO_EFFECT_STATE_OFF) } {
                Ok(()) => done.push(effect_name(&fx.id)),
                Err(e) => done.push(format!("{} REFUSED ({e})", effect_name(&fx.id))),
            }
        }
        done
    }

    fn query_effects(client: &IAudioClient2) -> Option<Vec<Effect>> {
        let mgr: IAudioEffectsManager = unsafe { client.GetService().ok()? };
        let mut ptr: *mut AUDIO_EFFECT = std::ptr::null_mut();
        let mut count: u32 = 0;
        unsafe { mgr.GetAudioEffects(&mut ptr, &mut count).ok()? };
        let list = unsafe { std::slice::from_raw_parts(ptr, count as usize) }
            .iter()
            .map(|e| Effect {
                id: e.id,
                on: e.state == AUDIO_EFFECT_STATE_ON,
                can_set: e.canSetState.as_bool(),
            })
            .collect();
        unsafe { CoTaskMemFree(Some(ptr as *const _)) };
        Some(list)
    }

    fn effect_name(id: &GUID) -> String {
        match *id {
            FX_AEC => "AEC".into(),
            FX_NS => "NS".into(),
            FX_DEEP_NS => "DeepNS".into(),
            FX_AGC => "AGC".into(),
            FX_BEAMFORMING => "Beamforming".into(),
            FX_CONSTANT_TONE => "ConstantToneRemoval".into(),
            FX_EQ => "EQ".into(),
            other => format!("{other:?}"),
        }
    }

    /// Master volume and mute of an endpoint, so a silent room can be told
    /// apart from a stimulus that never left the sound card.
    fn volume_suffix(device: &IMMDevice) -> String {
        let Ok(vol) = (unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) }) else {
            return String::new();
        };
        let level = unsafe { vol.GetMasterVolumeLevelScalar().unwrap_or(-1.0) };
        let muted = unsafe { vol.GetMute().map(|m| m.as_bool()).unwrap_or(false) };
        format!(
            "  [volume {:.0}%{}]",
            level * 100.0,
            if muted { ", MUTED" } else { "" }
        )
    }

    fn endpoints(
        enumerator: &IMMDeviceEnumerator,
        flow: EDataFlow,
    ) -> R<Vec<(IMMDevice, String)>> {
        let collection = unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)? };
        let count = unsafe { collection.GetCount()? };
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let dev = unsafe { collection.Item(i)? };
            let name = friendly_name(&dev).unwrap_or_else(|| format!("<device {i}>"));
            out.push((dev, name));
        }
        Ok(out)
    }

    fn default_capture_index(
        enumerator: &IMMDeviceEnumerator,
        mics: &[(IMMDevice, String)],
    ) -> R<usize> {
        let default = unsafe { enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)? };
        let wanted = device_id_string(&default)?;
        for (i, (dev, _)) in mics.iter().enumerate() {
            if device_id_string(dev)? == wanted {
                return Ok(i);
            }
        }
        Err("default capture endpoint is not in the active list".into())
    }

    /// The endpoint's device id, with the COM string freed.
    fn device_id_string(device: &IMMDevice) -> R<String> {
        let id = unsafe { device.GetId()? };
        let s = pcwstr_to_string(PCWSTR(id.0));
        unsafe { CoTaskMemFree(Some(id.0 as *const _)) };
        Ok(s)
    }

    /// Look an endpoint back up by id. COM must already be initialized on the
    /// calling thread.
    fn device_by_id(id: &str) -> R<IMMDevice> {
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
        Ok(unsafe { enumerator.GetDevice(PCWSTR(wide.as_ptr()))? })
    }

    fn friendly_name(device: &IMMDevice) -> Option<String> {
        let store = unsafe { device.OpenPropertyStore(STGM_READ).ok()? };
        let prop = unsafe { store.GetValue(&PKEY_Device_FriendlyName).ok()? };
        if unsafe { prop.Anonymous.Anonymous.vt } != VT_LPWSTR {
            return None;
        }
        let name = unsafe { prop.Anonymous.Anonymous.Anonymous.pwszVal };
        let s = pcwstr_to_string(PCWSTR(name.0 as *const u16));
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    fn pcwstr_to_string(p: PCWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        unsafe { p.to_string().unwrap_or_default() }
    }

    fn read_stimulus(path: &str) -> R<Vec<f32>> {
        let mut reader = hound::WavReader::open(path)
            .map_err(|e| format!("stimulus {path}: {e} — set WASAPI_COMMS_PROBE_STIMULUS"))?;
        let spec = reader.spec();
        if spec.channels != 1 || spec.sample_rate != STIMULUS_RATE {
            return Err(format!(
                "stimulus must be mono {STIMULUS_RATE} Hz; {path} is {} ch at {} Hz",
                spec.channels, spec.sample_rate
            )
            .into());
        }
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Int => reader
                .samples::<i16>()
                .filter_map(|s| s.ok())
                .map(|s| s as f32 / i16::MAX as f32)
                .collect(),
            hound::SampleFormat::Float => reader.samples::<f32>().filter_map(|s| s.ok()).collect(),
        };
        if samples.is_empty() {
            return Err(format!("stimulus {path} is empty").into());
        }
        Ok(samples)
    }

    fn write_wav(path: &std::path::Path, samples: &[f32], rate: u32) -> R<()> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(path, spec)?;
        for s in samples {
            w.write_sample(*s)?;
        }
        w.finalize()?;
        Ok(())
    }

    struct ComGuard;

    impl ComGuard {
        fn init() -> R<Self> {
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if hr.is_err() && hr.0 as u32 != 0x8001_0106 {
                return Err(format!("CoInitializeEx failed: 0x{:08x}", hr.0).into());
            }
            Ok(Self)
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
}

fn main() {
    #[cfg(target_os = "windows")]
    {
        if let Err(e) = imp::run() {
            eprintln!("wasapi_comms_probe FAILED: {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        println!("wasapi_comms_probe is a Windows-only example. Skipping on this target.");
    }
}
