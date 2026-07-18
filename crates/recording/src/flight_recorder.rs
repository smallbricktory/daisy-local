//! Per-session flight recorder: append-only JSONL sidecar
//! (`<session>/metrics.jsonl`) of runtime decisions and state that cannot be
//! recomputed from the recorded audio — AGC gain actually applied, AEC
//! state, mic switches, gate verdicts. Every line carries `t_ms` (stream
//! time, ms from session start; -1 for events outside stream time) and
//! `kind`.
//!
//! Part of debug logging: callers enable it from the same setting as
//! verbose file logs (no dedicated toggle). Emitters send pre-serialized
//! lines over a channel; a dedicated writer thread owns the file, so the
//! audio path never blocks on disk.
//!
//! Derivable signal (energies, peaks, spectra) stays out of the periodic
//! stream: the WAVs are that record, and any pass can recompute it with
//! current code. Finalize-time verdicts are the exception — they capture
//! which *decision* was made, not the audio.

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

pub const METRICS_FILE: &str = "metrics.jsonl";

pub struct FlightRecorder {
    tx: Option<std::sync::mpsc::Sender<String>>,
    writer: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Drop for FlightRecorder {
    // Deterministic flush: close the channel, then wait for the writer to
    // drain — a recorder opened right after (finalize) appends in order.
    fn drop(&mut self) {
        self.tx = None;
        if let Some(h) = self.writer.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

impl FlightRecorder {
    /// Opens (appending) the session's metrics sidecar when `enabled`;
    /// otherwise an inert recorder that drops events. Never fails: on I/O
    /// error the recorder is inert with a log line.
    pub fn open_if(enabled: bool, session_dir: &Path) -> Self {
        if !enabled {
            return Self::disabled();
        }
        let metrics_path = session_dir.join(METRICS_FILE);
        let file = match syncsafe::retry(|| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&metrics_path)
        }) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("flight recorder open failed ({e}) — metrics disabled");
                return Self::disabled();
            }
        };
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let writer = std::thread::spawn(move || {
            let mut w = std::io::BufWriter::new(file);
            while let Ok(line) = rx.recv() {
                if w.write_all(line.as_bytes()).and_then(|_| w.flush()).is_err() {
                    break; // sender keeps queueing harmlessly; nothing reads
                }
            }
            let _ = w.flush();
        });
        Self { tx: Some(tx), writer: Mutex::new(Some(writer)) }
    }

    /// Inert recorder (events dropped) for disabled/diagnostic-off paths.
    pub fn disabled() -> Self {
        Self { tx: None, writer: Mutex::new(None) }
    }

    fn emit(&self, t_ms: i64, kind: &str, data: serde_json::Value) {
        let Some(tx) = &self.tx else { return };
        let mut line = serde_json::json!({ "t_ms": t_ms, "kind": kind });
        if let (Some(obj), serde_json::Value::Object(extra)) = (line.as_object_mut(), data) {
            obj.extend(extra);
        }
        let mut buf = line.to_string();
        buf.push('\n');
        let _ = tx.send(buf);
    }

    // ── Typed events ────────────────────────────────────────────────────

    /// Session header: devices + the AGC guard seed in effect at start.
    pub fn session(&self, mic: &str, mic_source_id: u32, system: &str, sample_rate: u32, seed: Option<f32>) {
        self.emit(0, "session", serde_json::json!({
            "mic": mic,
            "mic_source_id": mic_source_id,
            "system": system,
            "sample_rate": sample_rate,
            "speech_env_min_seed": seed,
        }));
    }

    /// Periodic AGC sample: the gain actually applied to the last frame on
    /// the given track ("mic" / "system").
    pub fn agc(&self, t_ms: u64, track: &str, gain: f32, env: f32, peak: f32, env_min: f32) {
        self.emit(t_ms as i64, "agc", serde_json::json!({
            "track": track, "gain": gain, "env": env, "peak": peak, "env_min": env_min,
        }));
    }

    /// Periodic AEC state sample.
    pub fn aec(&self, t_ms: u64, active: bool, underrun_frames: u64) {
        self.emit(t_ms as i64, "aec", serde_json::json!({
            "active": active, "underrun_frames": underrun_frames,
        }));
    }

    /// Mid-call mic switch and the guard seed applied for the new device.
    pub fn mic_switch(&self, to_source_id: u32, seed: Option<f32>) {
        self.emit(-1, "mic_switch", serde_json::json!({
            "to_source_id": to_source_id,
            "speech_env_min_seed": seed,
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
        fr.session("BRIO", 3, "loopback", 16_000, Some(0.02));
        fr.agc(5_000, "mic", 2.5, 0.1, 0.09, 0.02);
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
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0]["kind"], "session");
        assert_eq!(lines[0]["mic"], "BRIO");
        assert_eq!(lines[1]["t_ms"], 5_000);
        assert_eq!(lines[2]["kind"], "energy_gate");
        assert_eq!(lines[2]["t_ms"], -1);
        assert_eq!(lines[3]["kind"], "segment_gate");
        assert_eq!(lines[3]["kept"], true);

        // Disabled recorder writes nothing.
        let off = FlightRecorder::open_if(false, &dir);
        off.agc(0, "system", 1.0, 0.0, 0.0, 0.02);
        drop(off);
        let text = syncsafe::read_to_string(dir.join(METRICS_FILE)).unwrap();
        assert_eq!(text.lines().count(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
