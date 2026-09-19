// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Streaming acoustic echo canceller over WebRTC's audio processing module.
//!
//! Per frame: the far-end (reference) frame goes to the render side, then the
//! mic frame is processed in place by the capture side. AEC3's own delay
//! estimator aligns the two streams internally.

use crate::error::{Error, Result};
use webrtc_audio_processing::config::{Config, EchoCanceller, HighPassFilter};
use webrtc_audio_processing::Processor;

/// Sample rate the canceller runs at.
const SAMPLE_RATE: u32 = 16_000;

/// Samples consumed (and produced) per `process` call: 10 ms at 16 kHz,
/// WebRTC's fixed audio-processing frame.
const FRAME_SIZE: usize = 160;

/// The canceller's own view of the echo path, sampled at call time. Every
/// field is `None` until the far end has carried enough signal to estimate
/// from.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    /// The delay the internal estimator currently attributes to the
    /// mic↔reference path, in ms.
    pub delay_ms: Option<u32>,
    /// Echo return loss: 10·log10(P_far / P_echo).
    pub erl_db: Option<f64>,
    /// Echo return loss enhancement: 10·log10(P_echo / P_out).
    pub erle_db: Option<f64>,
    /// Residual-echo likelihood in [0, 1].
    pub residual_echo_likelihood: Option<f64>,
}

pub struct AcousticEchoCanceller {
    apm: Processor,
    render: [[f32; FRAME_SIZE]; 1],
    capture: [[f32; FRAME_SIZE]; 1],
}

fn build_apm() -> Result<Processor> {
    let apm = Processor::new(SAMPLE_RATE)?;
    apm.set_config(Config {
        high_pass_filter: Some(HighPassFilter::default()),
        echo_canceller: Some(EchoCanceller::Full { stream_delay_ms: None }),
        noise_suppression: None,
        gain_controller: None,
        ..Config::default()
    });
    Ok(apm)
}

impl AcousticEchoCanceller {
    /// The number of input samples consumed (and output samples produced) per
    /// `process` call (10 ms at 16 kHz).
    pub const FRAME_SIZE: usize = FRAME_SIZE;

    /// Build a canceller: echo cancellation plus the high-pass filter it
    /// requires — no noise suppression, no gain control, so the output stays
    /// comparable to the raw mic outside echo spans.
    pub fn new() -> Result<Self> {
        Ok(Self { apm: build_apm()?, render: [[0.0; FRAME_SIZE]], capture: [[0.0; FRAME_SIZE]] })
    }

    /// Return to exactly the freshly-built state. Call before processing each
    /// new independent stream. (A rebuilt processor, not `reinitialize()` —
    /// that keeps enough residue to make identical audio produce different
    /// output, which would break chunk independence at finalize.)
    pub fn reset(&mut self) {
        if let Ok(apm) = build_apm() {
            self.apm = apm;
        }
    }

    /// Snapshot the canceller's internal estimates — its own delay estimate
    /// against the reference, and how much echo it sees and removes.
    pub fn stats(&self) -> Stats {
        let s = self.apm.get_stats();
        Stats {
            delay_ms: s.delay_ms,
            erl_db: s.echo_return_loss,
            erle_db: s.echo_return_loss_enhancement,
            residual_echo_likelihood: s.residual_echo_likelihood,
        }
    }

    /// Process one FRAME_SIZE-sample int16 mono frame.
    /// Returns FRAME_SIZE samples of echo-cancelled near (int16).
    pub fn process(&mut self, near: &[i16], far: &[i16]) -> Result<Vec<i16>> {
        if near.len() != FRAME_SIZE {
            return Err(Error::InvalidFrame { expected: FRAME_SIZE, actual: near.len() });
        }
        if far.len() != FRAME_SIZE {
            return Err(Error::InvalidFrame { expected: FRAME_SIZE, actual: far.len() });
        }

        for (dst, &s) in self.render[0].iter_mut().zip(far) {
            *dst = s as f32 / 32768.0;
        }
        for (dst, &s) in self.capture[0].iter_mut().zip(near) {
            *dst = s as f32 / 32768.0;
        }
        // iter_mut, not the arrays by value: the arrays are Copy, and a
        // by-value pass would hand the APM a temporary — the processed
        // samples would be written to the copy and lost.
        self.apm.process_render_frame(self.render.iter_mut())?;
        self.apm.process_capture_frame(self.capture.iter_mut())?;

        Ok(self.capture[0]
            .iter()
            .map(|&s| (s * 32768.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16)
            .collect())
    }
}
