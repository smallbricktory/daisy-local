//! Offline (local) transcription via whisper.cpp, wrapping the `whisper-rs` crate.
//!
//! [`WhisperLocalTranscriber`] implements [`providers_http::Transcriber`] and
//! drops into the same code paths as the HTTP providers. It loads a GGML model
//! file once (`ggml-*.bin`) and runs CPU inference per call.
//!
//! [`download_ggml_model`] fetches model files from Hugging Face.

pub mod bench;
mod download;
pub mod streaming;
pub mod wav;

pub use download::{download_ggml_model, download_ggml_model_opts, DownloadOpts, KNOWN_MODELS};

use providers_http::{ProviderError, Result as ProviderResult, Transcriber};
use std::path::Path;
use transcript::Segment;

/// Hard cap on inference threads.
const MAX_THREADS: i32 = 8;

/// Anti-hallucination / repetition-loop guards for Whisper decoding
/// (whisper.cpp's standard mitigations):
///  - temperature fallback: a window is re-decoded hotter when a quality
///    check trips;
///  - entropy/logprob thresholds: repetitive (low-entropy) or low-confidence
///    output triggers that fallback;
///  - no-speech threshold: windows the model flags as non-speech are skipped;
///  - `no_context`: each window is not seeded with the previous window's
///    text.
fn set_decode_guards(p: &mut whisper_rs::FullParams<'_, '_>) {
    let envf = |k: &str, d: f32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    p.set_temperature(0.0);
    p.set_temperature_inc(envf("DAISY_WHISPER_TEMP_INC", 0.2));
    // entropy_thold: a window whose token entropy falls below this triggers
    // the temperature fallback. Env-tunable.
    p.set_entropy_thold(envf("DAISY_WHISPER_ENTROPY_THOLD", 3.2));
    p.set_logprob_thold(envf("DAISY_WHISPER_LOGPROB_THOLD", -1.0));
    p.set_no_speech_thold(0.6);
    p.set_no_context(true);
    // Suppress non-speech tokens ("[coughs]", "(typing)", musical notes) at
    // decode time; the is_nonspeech_only filter catches whatever still slips
    // through (e.g. asterisk-wrapped "*scoff*").
    p.set_suppress_nst(true);
}

/// Cap on the Whisper `initial_prompt` length (~200 tokens).
const MAX_PROMPT_CHARS: usize = 800;

/// Local whisper.cpp transcriber. Holds one loaded model context for the
/// lifetime of the struct; a fresh inference state is created per
/// `transcribe` call.
pub struct WhisperLocalTranscriber {
    ctx: whisper_rs::WhisperContext,
    /// Model file stem, e.g. `"ggml-base.en"`.
    model_label: String,
    /// Number of threads passed to whisper.cpp.
    n_threads: i32,
    /// Optional vocabulary hint (names/jargon) biasing decoding, set per
    /// session by the caller. `None` = no prompt. Sanitized + capped.
    initial_prompt: Option<String>,
}

impl WhisperLocalTranscriber {
    /// Load a GGML model from `model_path` (a `ggml-*.bin` file), using a
    /// default thread count derived from available parallelism (capped).
    pub fn new(model_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::with_threads(model_path, default_thread_count())
    }

    /// Load a GGML model and pin the inference thread count.
    pub fn with_threads(model_path: impl AsRef<Path>, n: i32) -> anyhow::Result<Self> {
        let path = model_path.as_ref();
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("model path is not valid UTF-8: {}", path.display()))?;
        if !path.is_file() {
            anyhow::bail!(
                "whisper model file not found: {} (run `daisy download-model base.en`)",
                path.display()
            );
        }

        // All whisper context creation is serialized process-wide.
        // whisper.cpp's ggml GPU-backend registration (Vulkan device
        // enumeration + buffer-type setup) has global state that is not safe
        // to initialize from two threads at once.
        static WHISPER_INIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let ctx = {
            let _guard = WHISPER_INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            whisper_rs::WhisperContext::new_with_params(
                path_str,
                whisper_rs::WhisperContextParameters::default(),
            )
            .map_err(|e| anyhow::anyhow!("load whisper model {}: {e}", path.display()))?
        };

        let model_label = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "whisper".to_string());

        let n_threads = n.clamp(1, MAX_THREADS);

        Ok(Self {
            ctx,
            model_label,
            n_threads,
            initial_prompt: None,
        })
    }

    /// Attach a Whisper `initial_prompt` — a short vocabulary hint (names,
    /// jargon, expected spellings) to bias decoding. Null bytes are stripped
    /// (`set_initial_prompt` panics on them) and the text is capped to
    /// [`MAX_PROMPT_CHARS`]; an empty/whitespace hint becomes `None`.
    pub fn with_initial_prompt(mut self, prompt: Option<String>) -> Self {
        self.initial_prompt = sanitize_initial_prompt(prompt);
        self
    }
}

/// Strip null bytes (whisper.cpp's `set_initial_prompt` panics on them), cap
/// to [`MAX_PROMPT_CHARS`], and collapse empty/whitespace to `None`.
fn sanitize_initial_prompt(prompt: Option<String>) -> Option<String> {
    prompt
        .map(|p| p.replace('\0', " "))
        .map(|p| p.chars().take(MAX_PROMPT_CHARS).collect::<String>())
        .filter(|p| !p.trim().is_empty())
}

impl WhisperLocalTranscriber {
    /// Decode 16 kHz mono f32 samples already in memory into window-relative
    /// tokens (whisper segments). No file I/O and no speech-snap; live keeps
    /// raw segment spans.
    pub fn transcribe_samples(
        &self,
        samples: &[f32],
    ) -> anyhow::Result<Vec<crate::streaming::agreement::StreamToken>> {
        use crate::streaming::agreement::StreamToken;
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| anyhow::anyhow!("whisper create_state: {e}"))?;

        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.n_threads);
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_timestamps(false);
        params.set_token_timestamps(false);
        params.set_suppress_blank(true);
        set_decode_guards(&mut params);
        params.set_language(None); // base.en ignores it; auto for multilingual.
        if let Some(p) = &self.initial_prompt {
            params.set_initial_prompt(p);
        }

        state
            .full(params, samples)
            .map_err(|e| anyhow::anyhow!("whisper inference failed: {e}"))?;

        let n = state.full_n_segments();
        let mut out = Vec::with_capacity(n.max(0) as usize);
        for i in 0..n {
            let Some(seg) = state.get_segment(i) else { continue };
            // A multilingual model can split a multi-byte char at a segment
            // boundary; such a segment is recovered lossily instead of
            // failing the whole decode.
            let text = match seg.to_str() {
                Ok(s) => s.trim().to_string(),
                Err(e) => {
                    log::warn!("whisper live segment {i} not valid UTF-8 ({e}); recovering lossily");
                    match seg.to_str_lossy() {
                        Ok(lossy) => sanitize_lossy(lossy.as_ref()),
                        Err(e2) => {
                            log::warn!("whisper live segment {i} lossy decode failed ({e2}); skipping");
                            continue;
                        }
                    }
                }
            };
            if text.is_empty() || is_nonspeech_only(&text) {
                continue;
            }
            out.push(StreamToken {
                text,
                start_ms: (seg.start_timestamp() * 10).max(0),
                end_ms: (seg.end_timestamp() * 10).max(0),
            });
        }
        Ok(out)
    }
}

/// True when a segment is purely a non-speech annotation Whisper emits — e.g.
/// `[BLANK_AUDIO]`, `[Silence]`, `[Birds chirping]`, `(applause)`, `♪ music ♪`:
/// a whole-string bracketed/parenthesized tag, or text with no alphanumerics
/// at all (music notes / pure punctuation).
fn is_nonspeech_only(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    // Whole-segment sound/non-speech annotations wrapped in brackets, parens,
    // or asterisks: "[coughs]", "(typing)", "*scoff*", "** laughter **".
    if (t.starts_with('[') && t.ends_with(']'))
        || (t.starts_with('(') && t.ends_with(')'))
        || (t.starts_with('*') && t.ends_with('*'))
    {
        return true;
    }
    !t.chars().any(|c| c.is_alphanumeric())
}

fn default_thread_count() -> i32 {
    // available_parallelism minus 2 cores of headroom, at least 1 thread.
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
        .saturating_sub(2)
        .clamp(1, MAX_THREADS)
}

impl Transcriber for WhisperLocalTranscriber {
    fn name(&self) -> &'static str {
        "whisper_local"
    }

    fn model(&self) -> &str {
        &self.model_label
    }

    fn transcribe(
        &self,
        wav_path: &Path,
        language_hint: Option<&str>,
    ) -> ProviderResult<Vec<Segment>> {
        // 1. Decode WAV -> 16 kHz mono f32 PCM.
        let samples = wav::decode_wav_16k_mono_f32(wav_path).map_err(ProviderError::Other)?;
        if samples.is_empty() {
            return Ok(Vec::new());
        }

        // 2. Run inference on a fresh state.
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| ProviderError::Other(format!("whisper create_state: {e}")))?;

        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.n_threads);
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_timestamps(false);
        params.set_token_timestamps(false);
        params.set_suppress_blank(true);
        set_decode_guards(&mut params);
        // `Some("en")` etc. through; `None` lets whisper auto-detect the language.
        params.set_language(language_hint);
        if let Some(p) = &self.initial_prompt {
            params.set_initial_prompt(p);
        }

        state
            .full(params, &samples)
            .map_err(|e| ProviderError::Other(format!("whisper inference failed: {e}")))?;

        // 3. Collect segments. t0/t1 are centiseconds (1/100 s) -> *10 for ms;
        // segments are snapped to real speech onsets after the loop.
        let n_segments = state.full_n_segments();

        let mut out = Vec::with_capacity(n_segments.max(0) as usize);
        for i in 0..n_segments {
            let Some(segment) = state.get_segment(i) else {
                continue;
            };
            // whisper.cpp can split a multibyte UTF-8 sequence at a token
            // boundary, handing back bytes that are not valid UTF-8. Such a
            // segment is logged, decoded lossily, and stripped of the U+FFFD
            // replacement markers; legitimate non-Latin text (accents, CJK,
            // emoji) survives untouched.
            let text = match segment.to_str() {
                Ok(s) => s.trim().to_string(),
                Err(e) => {
                    log::warn!(
                        "whisper segment {i} of {wav_path:?} not valid UTF-8 ({e}); recovering lossily"
                    );
                    match segment.to_str_lossy() {
                        Ok(lossy) => sanitize_lossy(lossy.as_ref()),
                        Err(e2) => {
                            log::warn!("whisper segment {i} lossy decode failed ({e2}); skipping");
                            continue;
                        }
                    }
                }
            };
            if text.is_empty() || is_nonspeech_only(&text) {
                continue;
            }
            let t0 = segment.start_timestamp();
            let t1 = segment.end_timestamp();
            out.push(Segment {
                start_ms: (t0 * 10).max(0) as u32,
                end_ms: (t1 * 10).max(0) as u32,
                text,
                confidence: None,
                speaker_id: None,
            });
        }

        // 4. Snap each segment's [start,end] onto the real speech inside its
        // own span. whisper.cpp (greedy, token_timestamps off) tiles segments
        // back-to-back from t=0; a segment's transcript start can sit several
        // seconds before the speaker begins. Only the output timestamps are
        // corrected, using the same decoded samples; the audio fed to whisper
        // is not re-cut.
        snap_segments_to_speech(&samples, 16_000, &mut out);
        Ok(out)
    }
}

/// Strip the U+FFFD replacement markers that `to_str_lossy` inserts for
/// un-decodable bytes, plus any control characters, then trim. Every
/// legitimate Unicode scalar (accents, CJK, emoji) is kept.
fn sanitize_lossy(s: &str) -> String {
    s.chars()
        .filter(|&c| c != '\u{FFFD}' && !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// RMS over `[start, end)` of normalized f32 mono PCM. 0.0 for empty.
fn frame_rms(samples: &[f32], start: usize, end: usize) -> f32 {
    let end = end.min(samples.len());
    if start >= end {
        return 0.0;
    }
    let slice = &samples[start..end];
    let sum_sq: f64 = slice.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum_sq / slice.len() as f64).sqrt() as f32
}

/// Tighten each whisper segment's `[start_ms, end_ms)` onto the speech
/// actually present in that span: advance `start_ms` to the first ~20ms frame
/// above the speech floor, pull `end_ms` back to just after the last. Only
/// ever shrinks a segment inward; segments stay in order and non-overlapping.
/// A segment with no audible frame is left untouched.
///
/// `sample_rate` is the decoded PCM rate (whisper decodes to 16 kHz mono f32).
fn snap_segments_to_speech(samples: &[f32], sample_rate: u32, segs: &mut [Segment]) {
    if samples.is_empty() || sample_rate == 0 {
        return;
    }
    // 20ms analysis frames. Speech floor in normalized amplitude (i16/32768):
    // ~ -45 dBFS.
    const FRAME_MS: u32 = 20;
    const SPEECH_FLOOR: f32 = 0.006;
    let sr = sample_rate as u64;
    let frame_len = (sr * FRAME_MS as u64 / 1000) as usize;
    if frame_len == 0 {
        return;
    }
    let total_ms = (samples.len() as u64 * 1000 / sr) as u32;
    let ms_to_idx = |ms: u32| -> usize { ((ms as u64) * sr / 1000) as usize };

    for s in segs.iter_mut() {
        let lo_ms = s.start_ms.min(total_ms);
        let hi_ms = s.end_ms.min(total_ms);
        if hi_ms <= lo_ms {
            continue;
        }
        let lo = ms_to_idx(lo_ms);
        let hi = ms_to_idx(hi_ms);

        // First speech frame at/after lo.
        let mut first: Option<usize> = None;
        let mut idx = lo;
        while idx < hi {
            if frame_rms(samples, idx, idx + frame_len) >= SPEECH_FLOOR {
                first = Some(idx);
                break;
            }
            idx += frame_len;
        }
        let Some(first) = first else {
            // No audible frame in this span; timestamps are left as-is.
            continue;
        };

        // Last speech frame before hi (scan from the end).
        let mut last_end = hi;
        let mut probe = hi.saturating_sub(frame_len);
        while probe > first {
            if frame_rms(samples, probe, probe + frame_len) >= SPEECH_FLOOR {
                last_end = (probe + frame_len).min(hi);
                break;
            }
            probe = probe.saturating_sub(frame_len);
        }

        let new_start = (first as u64 * 1000 / sr) as u32;
        let new_end = (last_end as u64 * 1000 / sr) as u32;
        if new_end > new_start {
            s.start_ms = new_start;
            s.end_ms = new_end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_ms: u32, end_ms: u32) -> Segment {
        Segment { start_ms, end_ms, text: "x".into(), confidence: None, speaker_id: None }
    }

    #[test]
    fn initial_prompt_sanitized() {
        // None / empty / whitespace → None.
        assert_eq!(sanitize_initial_prompt(None), None);
        assert_eq!(sanitize_initial_prompt(Some("".into())), None);
        assert_eq!(sanitize_initial_prompt(Some("   ".into())), None);
        // Null bytes stripped (set_initial_prompt panics on them).
        assert_eq!(
            sanitize_initial_prompt(Some("Ac\0me Corp".into())).as_deref(),
            Some("Ac me Corp")
        );
        // Capped to MAX_PROMPT_CHARS.
        let long = "a".repeat(MAX_PROMPT_CHARS + 50);
        assert_eq!(
            sanitize_initial_prompt(Some(long)).map(|p| p.chars().count()),
            Some(MAX_PROMPT_CHARS)
        );
    }

    #[test]
    fn nonspeech_only_drops_annotations_keeps_speech() {
        for s in ["[BLANK_AUDIO]", "[Silence]", "[Birds chirping]", "(applause)", "(laughter)", "♪", "♪♫", "  [music]  ", "...", "***", "*scoff*", "*laughs*", "** coughs **", "  *typing*  "] {
            assert!(is_nonspeech_only(s), "should drop: {s:?}");
        }
        for s in ["hello there", "yeah", "100", "It's [redacted] here", "see you (Friday)", "this is *important*"] {
            assert!(!is_nonspeech_only(s), "should keep: {s:?}");
        }
    }

    #[test]
    fn sanitize_lossy_drops_replacement_markers_keeps_unicode() {
        // Replacement markers (from invalid UTF-8) are dropped...
        assert_eq!(sanitize_lossy("ab\u{FFFD}cd"), "abcd");
        // ...but legitimate non-Latin text survives untouched.
        assert_eq!(sanitize_lossy("café 你好 🌼"), "café 你好 🌼");
        // Control chars stripped; surrounding text trimmed.
        assert_eq!(sanitize_lossy("  hi\u{0007}there  "), "hithere");
        // All-garble collapses to empty (caller then skips the segment).
        assert_eq!(sanitize_lossy("\u{FFFD}\u{FFFD}"), "");
    }

    #[test]
    fn snaps_segment_start_past_leading_silence() {
        // 5s silence + 2s tone. whisper tiles the segment from t=0 even
        // though speech starts ~5000ms.
        let sr = 16_000u32;
        let mut samples = vec![0f32; sr as usize * 5];
        samples.extend(std::iter::repeat(0.3f32).take(sr as usize * 2));
        let mut segs = vec![seg(0, 7000)];
        snap_segments_to_speech(&samples, sr, &mut segs);
        assert!(
            (4500..=5200).contains(&segs[0].start_ms),
            "start snapped to {} (expected ~5000)",
            segs[0].start_ms
        );
        assert!(segs[0].end_ms <= 7000 && segs[0].end_ms > segs[0].start_ms);
    }

    #[test]
    fn silent_track_leaves_timestamps_untouched() {
        let sr = 16_000u32;
        let samples = vec![0f32; sr as usize * 3];
        let mut segs = vec![seg(0, 3000)];
        snap_segments_to_speech(&samples, sr, &mut segs);
        assert_eq!(segs[0].start_ms, 0);
        assert_eq!(segs[0].end_ms, 3000);
    }

    #[test]
    fn aligned_speech_segment_is_left_alone() {
        // Tone throughout → speech from frame 0 → start stays 0.
        let sr = 16_000u32;
        let samples = vec![0.3f32; sr as usize * 3];
        let mut segs = vec![seg(0, 3000)];
        snap_segments_to_speech(&samples, sr, &mut segs);
        assert_eq!(segs[0].start_ms, 0);
    }

    #[test]
    fn preserves_monotonic_order_across_segments() {
        // silence(0-2s) tone(2-4s) silence(4-5s) tone(5-7s)
        let sr = 16_000usize;
        let mut samples = vec![0f32; sr * 2];
        samples.extend(std::iter::repeat(0.3f32).take(sr * 2));
        samples.extend(std::iter::repeat(0f32).take(sr));
        samples.extend(std::iter::repeat(0.3f32).take(sr * 2));
        let mut segs = vec![seg(0, 4000), seg(4000, 7000)];
        snap_segments_to_speech(&samples, 16_000, &mut segs);
        assert!((1800..=2600).contains(&segs[0].start_ms), "seg0 start {}", segs[0].start_ms);
        assert!((4800..=5400).contains(&segs[1].start_ms), "seg1 start {}", segs[1].start_ms);
        assert!(segs[0].end_ms <= segs[1].start_ms, "overlap: {:?}", segs);
    }

    #[test]
    fn known_models_reexported() {
        assert!(KNOWN_MODELS.contains(&"base.en"));
    }

    #[test]
    fn default_thread_count_is_in_range() {
        let n = default_thread_count();
        assert!((1..=MAX_THREADS).contains(&n));
    }

    #[test]
    fn new_errors_on_missing_model_file() {
        match WhisperLocalTranscriber::new("/no/such/ggml-model.bin") {
            Ok(_) => panic!("expected an error for a missing model file"),
            Err(e) => assert!(e.to_string().contains("not found"), "unexpected: {e}"),
        }
    }

    // Optional end-to-end smoke test against a real model. Set
    // WHISPER_TEST_MODEL=/path/to/ggml-tiny.en.bin and WHISPER_TEST_WAV=/path/to/16k.wav
    // then run `cargo test -p providers-local -- --ignored`.
    #[test]
    #[ignore = "needs a real GGML model + WAV via env vars; slow"]
    fn transcribes_real_audio() {
        let model = std::env::var("WHISPER_TEST_MODEL").expect("WHISPER_TEST_MODEL");
        let wav = std::env::var("WHISPER_TEST_WAV").expect("WHISPER_TEST_WAV");
        let t = WhisperLocalTranscriber::new(&model).unwrap();
        let segs = t.transcribe(Path::new(&wav), Some("en")).unwrap();
        assert!(!segs.is_empty(), "expected at least one segment");
        for s in &segs {
            assert!(s.end_ms >= s.start_ms);
        }
    }
}
