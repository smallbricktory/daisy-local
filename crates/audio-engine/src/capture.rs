//! PipeWire-driven audio capture.
//!
//! `capture_one(&Source, duration, &Path)` opens a single capture stream
//! against a PipeWire source — either a real input (`SourceKind::Mic`) or a
//! synthesized monitor entry produced by `source::list_sources` for an
//! `Audio/Sink` (`SourceKind::Monitor`). Mic streams connect via numeric
//! `target.object`; monitor streams connect via `node_name` (which ends in
//! `.monitor`) — the synthesized id is the sink's id, not a monitor source's.
//!
//! `capture_dual` runs two streams in parallel on one MainLoop, each writing
//! its own WAV file, with a JSON manifest.
//!
//! `run_dual_streaming` runs both streams until a `StreamingControl::Stop`
//! is received, with `OpenChunk`/`CloseChunk` commands that swap WAV writers
//! mid-flight without disturbing the PipeWire streams.
//!
//! # Wake mechanism
//!
//! Control uses `pipewire::channel::channel::<StreamingControl>()`, the
//! inter-thread channel for pipewire-rs 0.8. The `Sender` is `Send + Clone`
//! (wraps `Arc<Mutex<...>>`) and works as the waker. The `Receiver` is
//! attached to the loop via `add_io`; when the sender writes a message the
//! loop wakes and calls the registered callback on the loop thread.

use crate::error::Result;
use crate::source::Source;
#[cfg(target_os = "linux")]
use crate::source::SourceKind;
use std::path::Path;
use std::time::Duration;
#[cfg(target_os = "windows")]
use crate::wasapi as wasapi_impl;

/// Capture from a single PW source for `duration`, writing 16 kHz mono int16
/// WAV via `WavFrameWriter`.
pub fn capture_one(
    source: &Source,
    duration: Duration,
    out_path: &Path,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        pipewire_impl::capture_one_blocking(source, duration, out_path)
    }
    #[cfg(target_os = "windows")]
    {
        wasapi_impl::capture_one_blocking(source, duration, out_path)
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos::capture_one_blocking(source, duration, out_path)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = (source, duration, out_path);
        Err(crate::error::Error::NotSupported("capture_one"))
    }
}

/// Request parameters for a dual-source capture.
#[derive(Debug, Clone, Copy)]
pub struct DualCaptureRequest {
    pub mic_source_id: u32,
    pub system_source_id: u32,
    pub duration: Duration,
    /// Sample rate (16_000 for MVP).
    pub sample_rate: u32,
}

/// Paths produced by `capture_dual`.
pub struct DualCaptureOutputs {
    pub mic_wav: std::path::PathBuf,
    pub system_wav: std::path::PathBuf,
    pub manifest_json: std::path::PathBuf,
}

/// Capture mic + system loopback to two WAV files plus a JSON manifest.
///
/// Both streams run on a single PipeWire MainLoop — no extra threads.
pub fn capture_dual(
    req: DualCaptureRequest,
    out_dir: &Path,
) -> Result<DualCaptureOutputs> {
    #[cfg(target_os = "linux")]
    {
        pipewire_impl::capture_dual_blocking(req, out_dir)
    }
    #[cfg(target_os = "windows")]
    {
        wasapi_impl::capture_dual_blocking(req, out_dir)
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos::capture_dual_blocking(req, out_dir)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = (req, out_dir);
        Err(crate::error::Error::NotSupported("capture_dual"))
    }
}

// ─── Streaming API ────────────────────────────────────────────────────────────

/// Parameters for the streaming capture API.
#[derive(Debug, Clone)]
pub struct StreamingCaptureRequest {
    pub mic_source_id: u32,
    pub system_source_id: u32,
    pub sample_rate: u32,
}

/// A tee sender for live consumption of audio samples.
///
/// An **unbounded** `tokio::sync::mpsc::UnboundedSender<Vec<i16>>`: `send`
/// is non-blocking and never drops — safe to call from the realtime audio
/// callback (no `.await`, no block). A transient consumer stall grows the
/// queue and drains right after; nothing is silently lost.
pub type TeeSender = tokio::sync::mpsc::UnboundedSender<TeeFrame>;

/// One tee'd audio frame, stamped with its position in the stream.
///
/// `start_sample` is the absolute index (samples since the tee was first
/// attached, at the capture rate) of `samples[0]`, counted per stream over
/// everything DELIVERED to the tee. Consumers pair the two streams by this
/// clock instead of by arrival order, so an OS-side frame drop or a stall on
/// one stream shows up as an explicit, bounded gap rather than silently
/// shifting every later frame.
#[derive(Debug, Clone)]
pub struct TeeFrame {
    pub start_sample: u64,
    pub samples: Vec<i16>,
}

/// Commands sent from the controller thread to the PipeWire loop thread.
#[derive(Debug)]
pub enum StreamingControl {
    /// Open a new chunk: replace mic/system writers with the given paths.
    OpenChunk {
        mic_wav: std::path::PathBuf,
        system_wav: std::path::PathBuf,
    },
    /// Close current chunk: finalize the open writers; further audio is dropped.
    CloseChunk,
    /// Quit the MainLoop and return from `run_dual_streaming`.
    Stop,
    /// Replace the mic-stream tee. Pass `None` to detach.
    SetMicTee(Option<TeeSender>),
    /// Replace the system-stream tee. Pass `None` to detach.
    SetSystemTee(Option<TeeSender>),
    /// Mid-call mic switch: tear down the current mic stream and rebuild it on
    /// the given source, keeping the same writer + tee (the chunk continues).
    /// Fail-safe: if the rebuild fails the loop keeps running (mic may go silent,
    /// but recording + the system track are never interrupted). System track is
    /// untouched.
    SwitchMic(crate::source::Source),
    /// Software mute for the mic track: zeroes the captured mic frames in
    /// the callback, before they are written to the wav and forwarded live.
    /// On macOS the AVAudioEngine tap captures independent of the device
    /// input-volume property; this is the authoritative mute. System track
    /// is untouched.
    SetMicMuted(bool),
}

/// A cheaply-cloneable handle that lets any thread drive the streaming loop.
///
/// The inner transport is platform-conditional: on Linux it wraps a
/// `pipewire::channel::Sender`, on other targets it's an opaque stub that
/// fails every dispatch. The public API (`open_chunk`, `stop`, etc.) is
/// uniform across platforms.
#[derive(Clone)]
pub struct StreamingHandle {
    #[cfg(target_os = "linux")]
    sender: pipewire::channel::Sender<StreamingControl>,
    #[cfg(target_os = "windows")]
    sender: std::sync::mpsc::Sender<StreamingControl>,
    #[cfg(target_os = "macos")]
    sender: std::sync::mpsc::Sender<StreamingControl>,
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    _marker: std::marker::PhantomData<StreamingControl>,
}

#[cfg(target_os = "windows")]
impl StreamingHandle {
    pub(crate) fn new_windows(sender: std::sync::mpsc::Sender<StreamingControl>) -> Self {
        Self { sender }
    }
}

#[cfg(target_os = "macos")]
impl StreamingHandle {
    pub(crate) fn new_macos(sender: std::sync::mpsc::Sender<StreamingControl>) -> Self {
        Self { sender }
    }
}

impl StreamingHandle {
    /// Open (or replace) the current WAV chunk.
    pub fn open_chunk(&self, mic: &Path, system: &Path) -> Result<()> {
        self.send(StreamingControl::OpenChunk {
            mic_wav: mic.to_path_buf(),
            system_wav: system.to_path_buf(),
        })
    }

    /// Finalize the current WAV chunk; audio is silently discarded until the
    /// next `open_chunk`.
    pub fn close_chunk(&self) -> Result<()> {
        self.send(StreamingControl::CloseChunk)
    }

    /// Stop the streaming loop. `run_dual_streaming` will return after this.
    pub fn stop(&self) -> Result<()> {
        self.send(StreamingControl::Stop)
    }

    /// Attach (or replace) a tee for the mic stream. Pass `None` to detach.
    pub fn set_mic_tee(&self, tx: Option<TeeSender>) -> Result<()> {
        self.send(StreamingControl::SetMicTee(tx))
    }

    /// Attach (or replace) a tee for the system stream. Pass `None` to detach.
    pub fn set_system_tee(&self, tx: Option<TeeSender>) -> Result<()> {
        self.send(StreamingControl::SetSystemTee(tx))
    }

    /// Switch the mic to a different source mid-recording (fail-safe — see
    /// `StreamingControl::SwitchMic`).
    pub fn switch_mic(&self, source: crate::source::Source) -> Result<()> {
        self.send(StreamingControl::SwitchMic(source))
    }

    /// Software-mute (or unmute) the mic track in the capture callback.
    pub fn set_mic_muted(&self, muted: bool) -> Result<()> {
        self.send(StreamingControl::SetMicMuted(muted))
    }

    #[cfg(target_os = "linux")]
    fn send(&self, cmd: StreamingControl) -> Result<()> {
        self.sender
            .send(cmd)
            .map_err(|_| crate::error::Error::PipeWire("control channel closed".into()))
    }

    #[cfg(target_os = "windows")]
    fn send(&self, cmd: StreamingControl) -> Result<()> {
        self.sender
            .send(cmd)
            .map_err(|_| crate::error::Error::PipeWire("control channel closed".into()))
    }

    #[cfg(target_os = "macos")]
    fn send(&self, cmd: StreamingControl) -> Result<()> {
        self.sender
            .send(cmd)
            .map_err(|_| crate::error::Error::Macos("control channel closed".into()))
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    fn send(&self, _cmd: StreamingControl) -> Result<()> {
        Err(crate::error::Error::NotSupported("streaming_handle.send"))
    }
}

/// Blocking entry point. Connects both capture streams and runs the PipeWire
/// MainLoop until [`StreamingHandle::stop`] is called.
///
/// `on_ready` is invoked (on the calling thread, before `mainloop.run()`)
/// after the streams have been registered; the caller may immediately send
/// [`StreamingHandle::open_chunk`] commands.
pub fn run_dual_streaming(
    req: StreamingCaptureRequest,
    on_ready: impl FnOnce(StreamingHandle),
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        pipewire_impl::run_dual_streaming_impl(req, on_ready)
    }
    #[cfg(target_os = "windows")]
    {
        wasapi_impl::run_dual_streaming_impl(req, on_ready)
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos::run_dual_streaming_impl(req, on_ready)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = (req, on_ready);
        Err(crate::error::Error::NotSupported("run_dual_streaming"))
    }
}

/// Run a non-recording level meter on a mic source. Opens a PipeWire (Linux)
/// or WASAPI (Windows) capture stream against the given source id, computes
/// RMS in the process callback, and invokes `on_rms` at ~20 Hz with the
/// most-recent value (0.0–1.0, where 1.0 ≈ full-scale int16).
///
/// Runs until `stop_rx` receives any message (or its sender is dropped). All
/// audio data is discarded — no WAV is written.
///
/// Uses the same audio backend as recording. Web Audio's getUserMedia on
/// WebKitGTK silently produces zero-amplitude streams on some hosts; this is
/// the native-path meter.
pub fn run_level_meter(
    source_id: u32,
    on_rms: impl FnMut(f32) + Send + 'static,
    stop_rx: std::sync::mpsc::Receiver<()>,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        pipewire_impl::run_level_meter_impl(source_id, on_rms, stop_rx)
    }
    #[cfg(target_os = "windows")]
    {
        wasapi_impl::run_level_meter_impl(source_id, on_rms, stop_rx)
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos::run_level_meter_impl(source_id, on_rms, stop_rx)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = (source_id, on_rms, stop_rx);
        Err(crate::error::Error::NotSupported("run_level_meter"))
    }
}

// ─── PipeWire implementation ──────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod pipewire_impl {
    use super::{DualCaptureOutputs, DualCaptureRequest, Source, SourceKind, StreamingCaptureRequest, StreamingControl, StreamingHandle, TeeFrame, TeeSender};
    use crate::error::{Error, Result};
    use crate::manifest::{ChannelManifest, RecordingManifest};
    use crate::wav::{WavFrameWriter, FRAME_SAMPLES};
    use pipewire as pw;
    use pw::spa;
    use spa::param::audio::{AudioFormat, AudioInfoRaw};
    use spa::pod::Pod;
    use std::cell::RefCell;
    use std::path::Path;
    use std::rc::Rc;
    use std::time::Duration;

    /// Build a SPA format POD for S16LE at the given sample rate (mono).
    fn make_format_pod(sample_rate: u32) -> Result<Vec<u8>> {
        let mut audio_info = AudioInfoRaw::new();
        audio_info.set_format(AudioFormat::S16LE);
        audio_info.set_rate(sample_rate);
        audio_info.set_channels(1);
        let obj = pw::spa::pod::Object {
            type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: pw::spa::param::ParamType::EnumFormat.as_raw(),
            properties: audio_info.into(),
        };
        let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &pw::spa::pod::Value::Object(obj),
        )
        .map_err(|e| Error::PipeWire(format!("serialize format pod: {e}")))?
        .0
        .into_inner();
        Ok(values)
    }

    /// Build and connect a single capture stream backed by a concrete writer.
    ///
    /// Returns `(stream, listener)` — both must stay alive for the duration of
    /// the MainLoop run. Dropping the listener handle unsubscribes it in
    /// `pipewire-rs 0.8`.
    fn build_capture_stream(
        core: &pw::core::Core,
        source: &Source,
        sample_rate: u32,
        node_name: &str,
        writer: Rc<RefCell<WavFrameWriter>>,
    ) -> Result<(
        pw::stream::Stream,
        pw::stream::StreamListener<()>,
    )> {
        let target_str = match source.kind {
            SourceKind::Mic => format!("{}", source.id),
            SourceKind::Monitor => source.node_name.clone(),
        };

        let mut props = pw::properties::Properties::new();
        props.insert(*pw::keys::MEDIA_TYPE, "Audio");
        props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
        props.insert(*pw::keys::MEDIA_ROLE, "Music");
        props.insert(*pw::keys::NODE_NAME, node_name);
        props.insert("target.object", target_str.as_str());
        if source.kind == SourceKind::Monitor {
            props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
        }

        let stream = pw::stream::Stream::new(core, node_name, props)
            .map_err(|e| Error::PipeWire(format!("Stream::new ({node_name}): {e}")))?;

        let accum: Rc<RefCell<Vec<i16>>> =
            Rc::new(RefCell::new(Vec::with_capacity(FRAME_SAMPLES * 4)));
        let accum_for_cb = Rc::clone(&accum);
        let writer_for_cb = Rc::clone(&writer);

        let listener = stream
            .add_local_listener_with_user_data(())
            .process(move |stream, _| {
                let mut buf = match stream.dequeue_buffer() {
                    Some(b) => b,
                    None => return,
                };
                let datas = buf.datas_mut();
                let d = match datas.first_mut() {
                    Some(d) => d,
                    None => return,
                };
                let valid_bytes = d.chunk().size() as usize;
                if valid_bytes == 0 {
                    return;
                }
                let raw = match d.data() {
                    Some(b) => b,
                    None => return,
                };
                let bytes = &raw[..valid_bytes];
                let samples: &[i16] = bytemuck::cast_slice(bytes);
                let mut accum_borrow = accum_for_cb.borrow_mut();
                accum_borrow.extend_from_slice(samples);
                let mut writer_borrow = writer_for_cb.borrow_mut();
                while accum_borrow.len() >= FRAME_SAMPLES {
                    let frame: Vec<i16> = accum_borrow.drain(..FRAME_SAMPLES).collect();
                    if let Err(e) = writer_borrow.write_frame(&frame) {
                        log::warn!("frame write failed: {e}");
                    }
                }
            })
            .register()
            .map_err(|e| Error::PipeWire(format!("register listener ({node_name}): {e}")))?;

        let format_bytes = make_format_pod(sample_rate)?;
        let mut params = [Pod::from_bytes(&format_bytes)
            .ok_or_else(|| Error::PipeWire("invalid format pod bytes".into()))?];

        stream
            .connect(
                spa::utils::Direction::Input,
                None,
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut params,
            )
            .map_err(|e| Error::PipeWire(format!("Stream::connect ({node_name}): {e}")))?;

        Ok((stream, listener))
    }

    /// Variant of `build_capture_stream` where the writer is optional.
    ///
    /// The process callback skips writes when the slot holds `None` (paused).
    /// The same slot is shared with the command dispatcher, which swaps
    /// writers mid-flight on the loop thread.
    fn build_capture_stream_optional(
        core: &pw::core::Core,
        source: &Source,
        sample_rate: u32,
        node_name: &str,
        writer: Rc<RefCell<Option<WavFrameWriter>>>,
        tee: Rc<RefCell<Option<TeeSender>>>,
        sent: Rc<std::cell::Cell<u64>>,
    ) -> Result<(pw::stream::Stream, pw::stream::StreamListener<()>)> {
        let target_str = match source.kind {
            SourceKind::Mic => format!("{}", source.id),
            SourceKind::Monitor => source.node_name.clone(),
        };

        let mut props = pw::properties::Properties::new();
        props.insert(*pw::keys::MEDIA_TYPE, "Audio");
        props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
        props.insert(*pw::keys::MEDIA_ROLE, "Music");
        props.insert(*pw::keys::NODE_NAME, node_name);
        props.insert("target.object", target_str.as_str());
        if source.kind == SourceKind::Monitor {
            props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
        }

        let stream = pw::stream::Stream::new(core, node_name, props)
            .map_err(|e| Error::PipeWire(format!("Stream::new ({node_name}): {e}")))?;

        let accum: Rc<RefCell<Vec<i16>>> =
            Rc::new(RefCell::new(Vec::with_capacity(FRAME_SAMPLES * 4)));
        let accum_cb = Rc::clone(&accum);
        let writer_cb = Rc::clone(&writer);
        let tee_cb = Rc::clone(&tee);
        let sent_cb = Rc::clone(&sent);

        let listener = stream
            .add_local_listener_with_user_data(())
            .process(move |stream, _| {
                let mut buf = match stream.dequeue_buffer() {
                    Some(b) => b,
                    None => return,
                };
                let datas = buf.datas_mut();
                let d = match datas.first_mut() {
                    Some(d) => d,
                    None => return,
                };
                let valid = d.chunk().size() as usize;
                if valid == 0 {
                    return;
                }
                let raw = match d.data() {
                    Some(b) => b,
                    None => return,
                };
                let samples: &[i16] = bytemuck::cast_slice(&raw[..valid]);

                let mut accum_b = accum_cb.borrow_mut();
                accum_b.extend_from_slice(samples);

                let mut writer_b = writer_cb.borrow_mut();
                while accum_b.len() >= FRAME_SAMPLES {
                    let frame: Vec<i16> = accum_b.drain(..FRAME_SAMPLES).collect();
                    // Write to WAV (primary path).
                    if let Some(w) = writer_b.as_mut() {
                        if let Err(e) = w.write_frame(&frame) {
                            log::warn!("frame write failed: {e}");
                        }
                    }
                    // Tee to live consumer (unbounded: non-blocking, only fails
                    // if the receiver is gone — never drops under backpressure).
                    // The clock counts only what was delivered: pause detaches
                    // the tee and freezes it, matching the consumer's view.
                    let tee_borrow = tee_cb.borrow();
                    if let Some(tx) = tee_borrow.as_ref() {
                        let at = sent_cb.get();
                        let n = frame.len() as u64;
                        if tx.send(TeeFrame { start_sample: at, samples: frame }).is_ok() {
                            sent_cb.set(at + n);
                        }
                    }
                }
            })
            .register()
            .map_err(|e| Error::PipeWire(format!("register listener ({node_name}): {e}")))?;

        let format_bytes = make_format_pod(sample_rate)?;
        let mut params = [Pod::from_bytes(&format_bytes)
            .ok_or_else(|| Error::PipeWire("invalid format pod bytes".into()))?];

        stream
            .connect(
                spa::utils::Direction::Input,
                None,
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut params,
            )
            .map_err(|e| Error::PipeWire(format!("Stream::connect ({node_name}): {e}")))?;

        Ok((stream, listener))
    }

    pub(super) fn capture_one_blocking(
        source: &Source,
        duration: Duration,
        out_path: &Path,
    ) -> Result<()> {
        pw::init();

        let mainloop = pw::main_loop::MainLoop::new(None)
            .map_err(|e| Error::PipeWire(format!("MainLoop::new: {e}")))?;
        let context = pw::context::Context::new(&mainloop)
            .map_err(|e| Error::PipeWire(format!("Context::new: {e}")))?;
        let core = context
            .connect(None)
            .map_err(|e| Error::PipeWire(format!("connect: {e}")))?;

        let writer = Rc::new(RefCell::new(WavFrameWriter::create(out_path, 16_000)?));

        let (stream, _listener) =
            build_capture_stream(&core, source, 16_000, "daisy-capture-one", Rc::clone(&writer))?;

        let mainloop_for_timer = mainloop.clone();
        let timer = mainloop
            .loop_()
            .add_timer(move |_expirations| {
                mainloop_for_timer.quit();
            });
        timer
            .update_timer(Some(duration), None)
            .into_sync_result()
            .map_err(|e| Error::PipeWire(format!("update_timer: {e}")))?;

        mainloop.run();

        drop(_listener);
        drop(stream);

        let writer_owned = match Rc::try_unwrap(writer) {
            Ok(cell) => cell.into_inner(),
            Err(_) => {
                return Err(Error::PipeWire(
                    "writer Rc still held after stream drop".into(),
                ))
            }
        };
        writer_owned.close()?;
        Ok(())
    }

    pub(super) fn run_dual_streaming_impl(
        req: StreamingCaptureRequest,
        on_ready: impl FnOnce(StreamingHandle),
    ) -> Result<()> {
        pw::init();

        // Validate both source IDs before opening any PW resources.
        let all = crate::source::list_sources()?;
        let find = |id: u32| -> Result<crate::source::Source> {
            all.iter()
                .find(|s| s.id == id)
                .cloned()
                .ok_or_else(|| Error::SourceNotFound(format!("id={id}")))
        };
        let mic_source = find(req.mic_source_id)?;
        let system_source = find(req.system_source_id)?;

        let mainloop = pw::main_loop::MainLoop::new(None)
            .map_err(|e| Error::PipeWire(format!("MainLoop::new: {e}")))?;
        let context = pw::context::Context::new(&mainloop)
            .map_err(|e| Error::PipeWire(format!("Context::new: {e}")))?;
        let core = context
            .connect(None)
            .map_err(|e| Error::PipeWire(format!("connect: {e}")))?;

        // Shared optional writer slots — None means paused (audio discarded).
        let mic_slot: Rc<RefCell<Option<WavFrameWriter>>> = Rc::new(RefCell::new(None));
        let sys_slot: Rc<RefCell<Option<WavFrameWriter>>> = Rc::new(RefCell::new(None));

        // Per-stream tee slots — None means no live consumer attached.
        let mic_tee: Rc<RefCell<Option<TeeSender>>> = Rc::new(RefCell::new(None));
        let sys_tee: Rc<RefCell<Option<TeeSender>>> = Rc::new(RefCell::new(None));
        // Stream clocks (samples delivered to each tee). Live OUTSIDE the
        // streams so a mid-call mic rebuild keeps the clock monotonic.
        let mic_sent: Rc<std::cell::Cell<u64>> = Rc::new(std::cell::Cell::new(0));
        let sys_sent: Rc<std::cell::Cell<u64>> = Rc::new(std::cell::Cell::new(0));

        let (mic_stream, _mic_listener) = build_capture_stream_optional(
            &core,
            &mic_source,
            req.sample_rate,
            "daisy-stream-mic",
            Rc::clone(&mic_slot),
            Rc::clone(&mic_tee),
            Rc::clone(&mic_sent),
        )?;
        let (sys_stream, _sys_listener) = build_capture_stream_optional(
            &core,
            &system_source,
            req.sample_rate,
            "daisy-stream-system",
            Rc::clone(&sys_slot),
            Rc::clone(&sys_tee),
            Rc::clone(&sys_sent),
        )?;

        // pipewire::channel provides an inter-thread channel whose Sender is
        // Send+Clone and whose Receiver attaches to the loop via add_io.
        // Sending wakes the loop and the callback runs on the loop thread,
        // where borrowing Rc<RefCell<...>> is safe.
        let (pw_sender, pw_receiver) = pw::channel::channel::<StreamingControl>();

        let mic_slot_cmd = Rc::clone(&mic_slot);
        let sys_slot_cmd = Rc::clone(&sys_slot);
        let mic_tee_for_event = Rc::clone(&mic_tee);
        let sys_tee_for_event = Rc::clone(&sys_tee);
        let mic_sent_for_switch = Rc::clone(&mic_sent);
        let mainloop_for_cmd = mainloop.clone();
        let sample_rate = req.sample_rate;

        // The mic stream lives in a cell; SwitchMic replaces it on the loop
        // thread. Core is moved in (last use here) to rebuild the new stream.
        let mic_stream_cell = Rc::new(RefCell::new(Some((mic_stream, _mic_listener))));
        let mic_stream_for_cmd = Rc::clone(&mic_stream_cell);
        let mic_slot_for_switch = Rc::clone(&mic_slot);
        let mic_tee_for_switch = Rc::clone(&mic_tee);
        let core_for_cmd = core;

        let _receiver = pw_receiver.attach(mainloop.loop_(), move |cmd| {
            match cmd {
                StreamingControl::OpenChunk { mic_wav, system_wav } => {
                    // Finalize any existing writers before opening new ones.
                    if let Some(w) = mic_slot_cmd.borrow_mut().take() {
                        if let Err(e) = w.close() {
                            log::warn!("mic writer close on OpenChunk: {e}");
                        }
                    }
                    if let Some(w) = sys_slot_cmd.borrow_mut().take() {
                        if let Err(e) = w.close() {
                            log::warn!("system writer close on OpenChunk: {e}");
                        }
                    }

                    match WavFrameWriter::create(&mic_wav, sample_rate) {
                        Ok(w) => *mic_slot_cmd.borrow_mut() = Some(w),
                        Err(e) => log::error!("open mic chunk {:?}: {e}", mic_wav),
                    }
                    match WavFrameWriter::create(&system_wav, sample_rate) {
                        Ok(w) => *sys_slot_cmd.borrow_mut() = Some(w),
                        Err(e) => log::error!("open system chunk {:?}: {e}", system_wav),
                    }
                }
                StreamingControl::CloseChunk => {
                    if let Some(w) = mic_slot_cmd.borrow_mut().take() {
                        if let Err(e) = w.close() {
                            log::warn!("mic writer close on CloseChunk: {e}");
                        }
                    }
                    if let Some(w) = sys_slot_cmd.borrow_mut().take() {
                        if let Err(e) = w.close() {
                            log::warn!("system writer close on CloseChunk: {e}");
                        }
                    }
                }
                StreamingControl::Stop => {
                    // Close any open writers then quit.
                    if let Some(w) = mic_slot_cmd.borrow_mut().take() {
                        if let Err(e) = w.close() {
                            log::warn!("mic writer close on Stop: {e}");
                        }
                    }
                    if let Some(w) = sys_slot_cmd.borrow_mut().take() {
                        if let Err(e) = w.close() {
                            log::warn!("system writer close on Stop: {e}");
                        }
                    }
                    mainloop_for_cmd.quit();
                }
                StreamingControl::SetMicTee(tx) => {
                    *mic_tee_for_event.borrow_mut() = tx;
                }
                StreamingControl::SetSystemTee(tx) => {
                    *sys_tee_for_event.borrow_mut() = tx;
                }
                StreamingControl::SwitchMic(source) => {
                    // Drop the old mic stream (brief silence), then rebuild
                    // on the new source sharing the same writer + tee; the
                    // chunk continues. Fail-safe: on error the mic goes
                    // silent but the system track + recording keep running.
                    mic_stream_for_cmd.borrow_mut().take();
                    match build_capture_stream_optional(
                        &core_for_cmd,
                        &source,
                        sample_rate,
                        "daisy-stream-mic",
                        Rc::clone(&mic_slot_for_switch),
                        Rc::clone(&mic_tee_for_switch),
                        Rc::clone(&mic_sent_for_switch),
                    ) {
                        Ok(pair) => {
                            *mic_stream_for_cmd.borrow_mut() = Some(pair);
                            log::info!("SwitchMic: mic now {}", source.node_name);
                        }
                        Err(e) => log::error!(
                            "SwitchMic: rebuild on {} failed — mic silent, recording continues: {e}",
                            source.node_name
                        ),
                    }
                }
                StreamingControl::SetMicMuted(_) => {
                    // Linux mutes via the OS input gain (PipeWire honors it); the
                    // recorder's set_input_gain call handles it. No software mute
                    // needed in the callback here.
                }
            }
        });

        let handle = StreamingHandle { sender: pw_sender };

        // Notify the caller that the loop is ready to receive commands.
        on_ready(handle);

        mainloop.run();

        // Drop streams and the receiver before returning. The mic stream
        // lives in the cell captured by `_receiver`'s closure; dropping the
        // receiver drops it. The system stream is dropped directly.
        drop(_sys_listener);
        drop(sys_stream);
        drop(_receiver);

        Ok(())
    }

    pub(super) fn capture_dual_blocking(
        req: DualCaptureRequest,
        out_dir: &Path,
    ) -> Result<DualCaptureOutputs> {
        std::fs::create_dir_all(out_dir)?;

        let mic_wav = out_dir.join("mic.wav");
        let system_wav = out_dir.join("system.wav");
        let manifest_path = out_dir.join("manifest.json");

        let started = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Validate both source IDs before opening any PW resources.
        let all = crate::source::list_sources()?;
        let find = |id: u32| -> Result<crate::source::Source> {
            all.iter()
                .find(|s| s.id == id)
                .cloned()
                .ok_or_else(|| Error::SourceNotFound(format!("id={id}")))
        };
        let mic_source = find(req.mic_source_id)?;
        let system_source = find(req.system_source_id)?;

        let streaming_req = StreamingCaptureRequest {
            mic_source_id: req.mic_source_id,
            system_source_id: req.system_source_id,
            sample_rate: req.sample_rate,
        };

        let mic_path = mic_wav.clone();
        let sys_path = system_wav.clone();
        let duration = req.duration;

        super::run_dual_streaming(streaming_req, move |handle| {
            // Open one chunk pointing at the output paths, then schedule a
            // controller thread to stop after `duration`.
            let _ = handle.open_chunk(&mic_path, &sys_path);
            let h = handle.clone();
            std::thread::spawn(move || {
                std::thread::sleep(duration);
                let _ = h.stop();
            });
        })?;

        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let manifest = RecordingManifest {
            schema_version: RecordingManifest::SCHEMA,
            started_at_unix_seconds: started,
            duration_seconds: req.duration.as_secs(),
            sample_rate: req.sample_rate,
            channels: 1,
            mic: ChannelManifest {
                source_id: mic_source.id,
                source_node_name: mic_source.node_name.clone(),
                source_description: mic_source.description.clone(),
                wav_path: mic_wav.clone(),
                captured_at_unix_seconds: now_unix,
            },
            system: ChannelManifest {
                source_id: system_source.id,
                source_node_name: system_source.node_name.clone(),
                source_description: system_source.description.clone(),
                wav_path: system_wav.clone(),
                captured_at_unix_seconds: now_unix,
            },
        };
        manifest.write(&manifest_path)?;

        Ok(DualCaptureOutputs {
            mic_wav,
            system_wav,
            manifest_json: manifest_path,
        })
    }

    /// PipeWire level-meter impl. See the public `run_level_meter` wrapper for
    /// the contract. Lives on its own MainLoop and exits when `stop_rx`
    /// receives a message (or its sender disconnects).
    pub(super) fn run_level_meter_impl(
        source_id: u32,
        on_rms: impl FnMut(f32) + Send + 'static,
        stop_rx: std::sync::mpsc::Receiver<()>,
    ) -> Result<()> {
        // The timer callback signature is Fn, not FnMut; the user-supplied
        // FnMut sits behind a RefCell. The timer callback only ever runs on
        // the MainLoop thread.
        let on_rms = std::cell::RefCell::new(on_rms);
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        pw::init();

        // Resolve the source id once up front; "device unplugged" surfaces
        // before a PipeWire connection is opened.
        let all = crate::source::list_sources()?;
        let source = all
            .into_iter()
            .find(|s| s.id == source_id)
            .ok_or_else(|| Error::SourceNotFound(format!("id={source_id}")))?;

        let mainloop = pw::main_loop::MainLoop::new(None)
            .map_err(|e| Error::PipeWire(format!("MainLoop::new: {e}")))?;
        let context = pw::context::Context::new(&mainloop)
            .map_err(|e| Error::PipeWire(format!("Context::new: {e}")))?;
        let core = context
            .connect(None)
            .map_err(|e| Error::PipeWire(format!("connect: {e}")))?;

        // Latest RMS (f32, bit-cast into u32 for atomic transfer between the
        // realtime audio thread and the loop thread).
        let latest_bits = Arc::new(AtomicU32::new(0));
        let latest_for_cb = Arc::clone(&latest_bits);

        let target_str = match source.kind {
            SourceKind::Mic => format!("{}", source.id),
            SourceKind::Monitor => source.node_name.clone(),
        };

        let mut props = pw::properties::Properties::new();
        props.insert(*pw::keys::MEDIA_TYPE, "Audio");
        props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
        props.insert(*pw::keys::MEDIA_ROLE, "Music");
        props.insert(*pw::keys::NODE_NAME, "daisy-level-meter");
        props.insert("target.object", target_str.as_str());
        if source.kind == SourceKind::Monitor {
            props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
        }

        let stream = pw::stream::Stream::new(&core, "daisy-level-meter", props)
            .map_err(|e| Error::PipeWire(format!("Stream::new: {e}")))?;

        let _listener = stream
            .add_local_listener_with_user_data(())
            .process(move |stream, _| {
                let mut buf = match stream.dequeue_buffer() {
                    Some(b) => b,
                    None => return,
                };
                let datas = buf.datas_mut();
                let d = match datas.first_mut() {
                    Some(d) => d,
                    None => return,
                };
                let valid_bytes = d.chunk().size() as usize;
                if valid_bytes == 0 {
                    return;
                }
                let raw = match d.data() {
                    Some(b) => b,
                    None => return,
                };
                let bytes = &raw[..valid_bytes];
                let samples: &[i16] = bytemuck::cast_slice(bytes);
                if samples.is_empty() {
                    return;
                }
                // Peak amplitude normalised to [0, 1]. Peak, not RMS, is
                // reported; the frontend can apply a perceptual curve for a
                // more compressed display.
                let mut peak: i32 = 0;
                for &s in samples {
                    let a = (s as i32).abs();
                    if a > peak { peak = a; }
                }
                let level = (peak as f32) / (i16::MAX as f32);
                latest_for_cb.store(level.to_bits(), Ordering::Relaxed);
            })
            .register()
            .map_err(|e| Error::PipeWire(format!("register listener: {e}")))?;

        let format_bytes = make_format_pod(16_000)?;
        let mut params = [Pod::from_bytes(&format_bytes)
            .ok_or_else(|| Error::PipeWire("invalid format pod bytes".into()))?];

        stream
            .connect(
                spa::utils::Direction::Input,
                None,
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut params,
            )
            .map_err(|e| Error::PipeWire(format!("Stream::connect: {e}")))?;

        // Drive emission + stop polling from a 50ms loop timer (≈20 Hz).
        let mainloop_for_timer = mainloop.clone();
        let latest_for_timer = Arc::clone(&latest_bits);
        let timer = mainloop.loop_().add_timer(move |_| {
            // The stop signal is checked first.
            match stop_rx.try_recv() {
                Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    mainloop_for_timer.quit();
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            let bits = latest_for_timer.load(Ordering::Relaxed);
            let rms = f32::from_bits(bits);
            on_rms.borrow_mut()(rms);
        });
        timer
            .update_timer(Some(Duration::from_millis(50)), Some(Duration::from_millis(50)))
            .into_sync_result()
            .map_err(|e| Error::PipeWire(format!("update_timer: {e}")))?;

        mainloop.run();

        drop(_listener);
        drop(stream);
        Ok(())
    }
}
