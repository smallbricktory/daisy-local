// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Per-session flight recorder: append-only JSONL sidecar
//! (`<session>/metrics.jsonl`) of runtime decisions and state that cannot be
//! recomputed from the recorded audio — live AGC gain, AEC state, mic
//! switches, gate verdicts. Every line carries `t_ms` (stream
//! time, ms from session start; -1 for events outside stream time) and
//! `kind`.
//!
//! Part of debug logging: callers enable it from the same setting as
//! verbose file logs (no dedicated toggle). Emitters queue pre-serialized
//! lines on a [`JsonlSink`].
//!
//! Derivable signal (energies, peaks, spectra) is not recorded here.

use crate::jsonl_sink::JsonlSink;
use std::path::Path;

pub const METRICS_FILE: &str = "metrics.jsonl";

/// Dropping the recorder joins its writer: a recorder opened right after
/// (finalize) appends in order.
pub struct FlightRecorder {
    sink: JsonlSink,
}

impl FlightRecorder {
    /// Opens (appending) the session's metrics sidecar when `enabled`;
    /// otherwise an inert recorder that drops events. Never fails: on I/O
    /// error the recorder is inert with a log line.
    pub fn open_if(enabled: bool, session_dir: &Path) -> Self {
        if !enabled {
            return Self::disabled();
        }
        match JsonlSink::open(&session_dir.join(METRICS_FILE), "daisy-metrics") {
            Ok(sink) => Self { sink },
            Err(e) => {
                log::warn!("flight recorder open failed ({e}) — metrics disabled");
                Self::disabled()
            }
        }
    }

    /// Inert recorder (events dropped) for disabled/diagnostic-off paths.
    pub fn disabled() -> Self {
        Self { sink: JsonlSink::disabled() }
    }

    fn emit(&self, t_ms: i64, kind: &str, data: serde_json::Value) {
        if !self.sink.is_enabled() {
            return;
        }
        let mut line = serde_json::json!({ "t_ms": t_ms, "kind": kind });
        if let (Some(obj), serde_json::Value::Object(extra)) = (line.as_object_mut(), data) {
            obj.extend(extra);
        }
        self.sink.send(line.to_string());
    }

    // ── Typed events ────────────────────────────────────────────────────

    /// Session header: devices, the AGC guard seed in effect at start, and
    /// whether the mic arrived OS voice-processed (which capture backend the
    /// start ladder settled on).
    pub fn session(
        &self,
        mic: &str,
        mic_source_id: u32,
        system: &str,
        sample_rate: u32,
        seed: Option<f32>,
        voice_processed: bool,
    ) {
        self.emit(0, "session", serde_json::json!({
            "mic": mic,
            "mic_source_id": mic_source_id,
            "system": system,
            "sample_rate": sample_rate,
            "speech_env_min_seed": seed,
            "mic_voice_processed": voice_processed,
        }));
    }

    /// Periodic AGC sample: the live AGC gain computed for the last frame on
    /// the given track ("mic" / "system"). The engine feed applies it, except
    /// for an engine that takes the raw capture level (SpeechAnalyzer); the
    /// live transcript names the engine behind each segment.
    pub fn agc(&self, t_ms: u64, track: &str, gain: f32, env: f32, peak: f32, env_min: f32) {
        self.emit(t_ms as i64, "agc", serde_json::json!({
            "track": track, "gain": gain, "env": env, "peak": peak, "env_min": env_min,
        }));
    }

    /// Periodic AEC state sample.
    /// One acoustic alignment probe: the measured pairing offset candidate
    /// (samples), its GCC-PHAT confidence ratio, and whether it was applied.
    pub fn aec_probe(&self, t_ms: u64, offset: i64, ratio: f32, applied: bool) {
        self.emit(
            t_ms as i64,
            "aec_probe",
            serde_json::json!({ "offset": offset, "ratio": ratio, "applied": applied }),
        );
    }

    /// Periodic capture-clock sample: each stream's newest stamp and stream
    /// position, and the divergence between pairing by stamp and pairing by
    /// sample count (in samples). A healthy pair holds this near zero.
    pub fn clock(
        &self,
        t_ms: u64,
        mic_ns: u64,
        system_ns: u64,
        mic_sample: u64,
        system_sample: u64,
        base_samples: i64,
        divergence_samples: i64,
    ) {
        self.emit(t_ms as i64, "clock", serde_json::json!({
            "mic_ns": mic_ns, "system_ns": system_ns,
            "mic_sample": mic_sample, "system_sample": system_sample,
            "base_samples": base_samples,
            "divergence_samples": divergence_samples,
        }));
    }

    /// Periodic AEC state sample. `active`: routing wants software AEC and a
    /// canceller exists. `engaged`: the reference gate is cancelling (false
    /// while bypassed for lack of a far-end reference). `stats` is the
    /// canceller's own view of the echo path: its internal delay estimate
    /// (ms) and ERL/ERLE.
    pub fn aec(
        &self,
        t_ms: u64,
        active: bool,
        engaged: bool,
        underrun_frames: u64,
        stats: aec::echo_canceller::Stats,
    ) {
        self.emit(t_ms as i64, "aec", serde_json::json!({
            "active": active, "engaged": engaged, "underrun_frames": underrun_frames,
            "delay_ms": stats.delay_ms,
            "erl_db": stats.erl_db,
            "erle_db": stats.erle_db,
            "residual_echo": stats.residual_echo_likelihood,
        }));
    }

    /// Mid-call mic switch: the guard seed applied for the new device, and
    /// the capture backend it came up on.
    pub fn mic_switch(&self, to_source_id: u32, seed: Option<f32>, voice_processed: bool) {
        self.emit(-1, "mic_switch", serde_json::json!({
            "to_source_id": to_source_id,
            "speech_env_min_seed": seed,
            "mic_voice_processed": voice_processed,
        }));
    }

    /// Finalize gate summary (anchors, threshold, verdict counts).
    #[allow(clippy::too_many_arguments)]
    pub fn energy_gate(
        &self,
        gate_version: u32,
        speech_dbfs: Option<f32>,
        residue_dbfs: Option<f32>,
        threshold_dbfs: Option<f32>,
        speech_windows: usize,
        residue_windows: usize,
        applied: bool,
        dropped: usize,
    ) {
        self.emit(-1, "energy_gate", serde_json::json!({
            "gate_version": gate_version,
            "speech_dbfs": speech_dbfs,
            "residue_dbfs": residue_dbfs,
            "threshold_dbfs": threshold_dbfs,
            "speech_windows": speech_windows,
            "residue_windows": residue_windows,
            "applied": applied,
            "dropped": dropped,
        }));
    }

    /// One mic segment's measured peak + gate verdict, session-absolute ms.
    pub fn segment_gate(&self, t_ms: i64, end_ms: i64, peak_dbfs: Option<f32>, kept: bool) {
        self.emit(t_ms, "segment_gate", serde_json::json!({
            "end_ms": end_ms, "peak_dbfs": peak_dbfs, "kept": kept,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_jsonl_events_and_survives_reopen() {
        let dir = std::env::temp_dir().join(format!("daisy-fr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        syncsafe::create_dir_all(&dir).unwrap();

        let fr = FlightRecorder::open_if(true, &dir);
        fr.session("BRIO", 3, "loopback", 16_000, Some(0.02), true);
        fr.agc(5_000, "mic", 2.5, 0.1, 0.09, 0.02);
        fr.mic_switch(9, Some(0.03), false);
        drop(fr); // sender closes; writer thread flushes and exits
        // A later phase (finalize) appends to the same file.
        let fr = FlightRecorder::open_if(true, &dir);
        fr.energy_gate(2, Some(-12.0), Some(-40.0), Some(-34.0), 90, 120, true, 3);
        fr.segment_gate(1_000, 4_000, Some(-11.5), true);
        drop(fr);

        // Drop joins the writer thread, so the file is complete here.
        let text = syncsafe::read_to_string(dir.join(METRICS_FILE)).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0]["kind"], "session");
        assert_eq!(lines[0]["mic"], "BRIO");
        // The capture backend that served the mic is recorded at start and at every switch.
        assert_eq!(lines[0]["mic_voice_processed"], true);
        assert_eq!(lines[1]["t_ms"], 5_000);
        assert_eq!(lines[2]["kind"], "mic_switch");
        assert_eq!(lines[2]["mic_voice_processed"], false);
        assert_eq!(lines[3]["kind"], "energy_gate");
        assert_eq!(lines[3]["t_ms"], -1);
        assert_eq!(lines[4]["kind"], "segment_gate");
        assert_eq!(lines[4]["kept"], true);

        // Disabled recorder writes nothing.
        let off = FlightRecorder::open_if(false, &dir);
        off.agc(0, "system", 1.0, 0.0, 0.0, 0.02);
        drop(off);
        let text = syncsafe::read_to_string(dir.join(METRICS_FILE)).unwrap();
        assert_eq!(text.lines().count(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
