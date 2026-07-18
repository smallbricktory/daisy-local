//! Worker thread that hosts the PipeWire MainLoop via audio_engine::run_dual_streaming.

use crate::error::{RecordingError, Result};
use audio_engine::capture::{run_dual_streaming, StreamingCaptureRequest, StreamingHandle, TeeSender};
use std::path::Path;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

/// Thin adapter that spawns and manages the audio capture worker thread.
/// Owns the streaming handle and join handle for the MainLoop thread.
pub struct CaptureWorker {
    handle: StreamingHandle,
    join: Option<JoinHandle<Result<()>>>,
}

impl CaptureWorker {
    /// Spawn the worker thread and block until streams are connected.
    ///
    /// The caller provides a `StreamingCaptureRequest` which is passed to
    /// `audio_engine::run_dual_streaming`. Once the MainLoop is ready, it
    /// calls the on_ready callback, which delivers the StreamingHandle via an
    /// mpsc channel.
    ///
    /// Returns an error if the worker thread exits before handing back the
    /// handle (e.g. PipeWire initialization failure).
    pub fn spawn(req: StreamingCaptureRequest) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<StreamingHandle>();
        let join = thread::spawn(move || -> Result<()> {
            run_dual_streaming(req, move |h| {
                let _ = tx.send(h);
            })
            .map_err(RecordingError::from)
        });
        let handle = rx
            .recv()
            .map_err(|_| RecordingError::WorkerGone)?;
        Ok(Self {
            handle,
            join: Some(join),
        })
    }

    /// Open (or replace) the current WAV chunk for both mic and system audio.
    pub fn open_chunk(&self, mic_wav: &Path, system_wav: &Path) -> Result<()> {
        self.handle
            .open_chunk(mic_wav, system_wav)
            .map_err(RecordingError::from)
    }

    /// Finalize the current WAV chunk. Audio is silently discarded until the
    /// next `open_chunk` call.
    pub fn close_chunk(&self) -> Result<()> {
        self.handle.close_chunk().map_err(RecordingError::from)
    }

    /// Attach (or detach) the mic audio tee. Pass `None` to detach.
    pub fn set_mic_tee(&self, tx: Option<TeeSender>) -> Result<()> {
        self.handle.set_mic_tee(tx).map_err(RecordingError::from)
    }

    /// Attach (or detach) the system audio tee. Pass `None` to detach.
    pub fn set_system_tee(&self, tx: Option<TeeSender>) -> Result<()> {
        self.handle.set_system_tee(tx).map_err(RecordingError::from)
    }

    /// Switch the mic to a different source mid-recording (fail-safe).
    pub fn switch_mic(&self, source: audio_engine::source::Source) -> Result<()> {
        self.handle.switch_mic(source).map_err(RecordingError::from)
    }

    /// Software-mute the mic track in the capture callback (zeros mic frames).
    pub fn set_mic_muted(&self, muted: bool) -> Result<()> {
        self.handle.set_mic_muted(muted).map_err(RecordingError::from)
    }

    /// Stop the worker thread and wait for it to exit gracefully.
    ///
    /// Sends the stop command to the MainLoop, then joins the thread.
    /// Returns an error if the thread panicked or if the stop command failed.
    pub fn shutdown(mut self) -> Result<()> {
        self.handle.stop().map_err(RecordingError::from)?;
        if let Some(j) = self.join.take() {
            match j.join() {
                Ok(r) => r?,
                Err(_) => return Err(RecordingError::WorkerGone),
            }
        }
        Ok(())
    }
}

impl Drop for CaptureWorker {
    // Best-effort cleanup if the Recorder fails partway through start() or is
    // dropped without an explicit shutdown(). After a successful shutdown(),
    // `join` is None and the stop() send fails harmlessly.
    fn drop(&mut self) {
        let _ = self.handle.stop();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}
