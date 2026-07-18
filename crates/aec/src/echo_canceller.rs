//! Streaming acoustic echo canceller.
//!
//! Workflow per frame:
//!   1. Slide near + far rolling buffers (BLOCK_SIZE wide, hop BLOCK_SHIFT)
//!   2. RFFT both
//!   3. Stage 1: ONNX (mic_mag, state1, far_mag) -> (mask, state1')
//!   4. Apply real mask to complex mic spectrum, IRFFT to time domain
//!   5. Stage 2: ONNX (near_estimate, state2, far_buf) -> (clean, state2')
//!   6. Overlap-add Stage 2 output into out_buf; emit first BLOCK_SHIFT samples

use crate::constants::{BLOCK_SHIFT, BLOCK_SIZE, FFT_BINS, STATE_SHAPE};
use crate::error::{Error, Result};
use crate::fft::{ifft_real, rfft};
use crate::onnx::{Stage1Session, Stage2Session};
use ndarray::{Array3, Array4};
use ort::value::TensorRef;
use std::path::Path;

pub struct AcousticEchoCanceller {
    stage1: Stage1Session,
    stage2: Stage2Session,
    near_buf: Vec<f32>,
    far_buf: Vec<f32>,
    state1: Array4<f32>,
    state2: Array4<f32>,
    /// Overlap-add accumulation buffer (BLOCK_SIZE samples).
    out_buf: Vec<f32>,
}

impl AcousticEchoCanceller {
    /// Load both stage models from `model_dir` and return a fresh AEC with
    /// zero-initialized buffers + LSTM state.
    pub fn load(model_dir: &Path) -> Result<Self> {
        let stage1 = Stage1Session::load(&model_dir.join("model_256_1.onnx"))?;
        let stage2 = Stage2Session::load(&model_dir.join("model_256_2.onnx"))?;
        Ok(Self {
            stage1,
            stage2,
            near_buf: vec![0.0; BLOCK_SIZE],
            far_buf: vec![0.0; BLOCK_SIZE],
            state1: Array4::zeros(STATE_SHAPE),
            state2: Array4::zeros(STATE_SHAPE),
            out_buf: vec![0.0; BLOCK_SIZE],
        })
    }

    /// Zero the rolling buffers and LSTM state back to a freshly-loaded state
    /// without rebuilding the ONNX sessions. Call before processing each new
    /// independent stream.
    pub fn reset(&mut self) {
        self.near_buf.fill(0.0);
        self.far_buf.fill(0.0);
        self.out_buf.fill(0.0);
        self.state1.fill(0.0);
        self.state2.fill(0.0);
    }

    /// The number of input samples consumed (and output samples produced) per
    /// `process` call. Equals `BLOCK_SHIFT` (128 at 16 kHz = 8 ms).
    pub const FRAME_SIZE: usize = BLOCK_SHIFT;

    /// Process one BLOCK_SHIFT-sample int16 mono frame.
    /// Returns BLOCK_SHIFT samples of echo-cancelled near (int16).
    pub fn process(&mut self, near: &[i16], far: &[i16]) -> Result<Vec<i16>> {
        if near.len() != BLOCK_SHIFT {
            return Err(Error::InvalidFrame {
                expected: BLOCK_SHIFT,
                actual: near.len(),
                dtype: "i16",
            });
        }
        if far.len() != BLOCK_SHIFT {
            return Err(Error::InvalidFrame {
                expected: BLOCK_SHIFT,
                actual: far.len(),
                dtype: "i16",
            });
        }

        // ── 1. Slide rolling buffers ──────────────────────────────────────────────
        // Drop oldest BLOCK_SHIFT samples, append new frame (normalised to [-1, 1]).
        self.near_buf.drain(..BLOCK_SHIFT);
        self.far_buf.drain(..BLOCK_SHIFT);
        for &s in near {
            self.near_buf.push(s as f32 / 32768.0);
        }
        for &s in far {
            self.far_buf.push(s as f32 / 32768.0);
        }

        // ── 2. RFFT both buffers ──────────────────────────────────────────────────
        let mic_spectrum = rfft(&self.near_buf);
        let far_spectrum = rfft(&self.far_buf);

        // ── 3. Magnitude spectra for Stage 1 ─────────────────────────────────────
        let mic_mag: Vec<f32> = mic_spectrum.iter().map(|c| c.norm()).collect();
        let far_mag: Vec<f32> = far_spectrum.iter().map(|c| c.norm()).collect();

        // ── 4. Stage 1 inference ──────────────────────────────────────────────────
        let mic_mag_arr =
            Array3::from_shape_vec((1, 1, FFT_BINS), mic_mag).map_err(|e| Error::Onnx(e.to_string()))?;
        let far_mag_arr =
            Array3::from_shape_vec((1, 1, FFT_BINS), far_mag).map_err(|e| Error::Onnx(e.to_string()))?;

        let stage1_outputs = {
            let mic_ref = TensorRef::<f32>::from_array_view(mic_mag_arr.view())?;
            let state_ref = TensorRef::<f32>::from_array_view(self.state1.view())?;
            let far_ref = TensorRef::<f32>::from_array_view(far_mag_arr.view())?;
            // Positional order: slot 0 = mic, slot 1 = state, slot 2 = far.
            self.stage1.inner_mut().run(ort::inputs![mic_ref, state_ref, far_ref])?
        };

        // Extract mask (1, 1, FFT_BINS) and new state
        let mask_data: Vec<f32> = {
            let (_shape, data) = stage1_outputs[0].try_extract_tensor::<f32>()?;
            data.to_vec()
        };
        {
            let (_shape, state_data) = stage1_outputs[1].try_extract_tensor::<f32>()?;
            self.state1
                .as_slice_mut()
                .ok_or_else(|| Error::Onnx("state1 not contiguous".into()))?
                .copy_from_slice(state_data);
        }

        // ── 5. Apply mask to mic spectrum → IRFFT ────────────────────────────────
        // ifft_real normalizes by 1/BLOCK_SIZE.
        let masked_spectrum: Vec<_> = mic_spectrum
            .iter()
            .zip(mask_data.iter())
            .map(|(c, &m)| c * m)
            .collect();
        let near_estimate = ifft_real(&masked_spectrum);

        // ── 6. Stage 2 inference ──────────────────────────────────────────────────
        let near_est_arr =
            Array3::from_shape_vec((1, 1, BLOCK_SIZE), near_estimate).map_err(|e| Error::Onnx(e.to_string()))?;
        let far_time_arr = Array3::from_shape_vec((1, 1, BLOCK_SIZE), self.far_buf.clone())
            .map_err(|e| Error::Onnx(e.to_string()))?;

        let stage2_outputs = {
            let near_ref = TensorRef::<f32>::from_array_view(near_est_arr.view())?;
            let state_ref = TensorRef::<f32>::from_array_view(self.state2.view())?;
            let far_ref = TensorRef::<f32>::from_array_view(far_time_arr.view())?;
            // Positional order: slot 0 = near estimate, slot 1 = state, slot 2 = far.
            self.stage2.inner_mut().run(ort::inputs![near_ref, state_ref, far_ref])?
        };

        // Extract clean (1, 1, BLOCK_SIZE) and new state
        let clean_data: Vec<f32> = {
            let (_shape, data) = stage2_outputs[0].try_extract_tensor::<f32>()?;
            data.to_vec()
        };
        {
            let (_shape, state_data) = stage2_outputs[1].try_extract_tensor::<f32>()?;
            self.state2
                .as_slice_mut()
                .ok_or_else(|| Error::Onnx("state2 not contiguous".into()))?
                .copy_from_slice(state_data);
        }

        // ── 7. Overlap-add and emit first BLOCK_SHIFT samples ────────────────────
        //   1. Shift left by BLOCK_SHIFT (discard oldest).
        //   2. Zero the newly exposed trailing BLOCK_SHIFT samples.
        //   3. Accumulate Stage 2's full BLOCK_SIZE output.
        //   4. Emit the first BLOCK_SHIFT samples.
        self.out_buf.copy_within(BLOCK_SHIFT.., 0);
        for s in &mut self.out_buf[BLOCK_SIZE - BLOCK_SHIFT..] {
            *s = 0.0;
        }
        for (acc, &val) in self.out_buf.iter_mut().zip(clean_data.iter()) {
            *acc += val;
        }

        let output_frame: Vec<i16> = self.out_buf[..BLOCK_SHIFT]
            .iter()
            .map(|&s| {
                let scaled = s * 32768.0;
                scaled.clamp(i16::MIN as f32, i16::MAX as f32) as i16
            })
            .collect();

        Ok(output_frame)
    }
}
