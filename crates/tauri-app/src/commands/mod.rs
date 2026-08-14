/// Atomic write of the rendered transcript.md so a reader never sees a
/// half-written render.
pub(crate) fn write_transcript_md(
    session_dir: &std::path::Path,
    md: &str,
) -> std::io::Result<()> {
    syncsafe::write_atomic(session_dir.join("transcript.md"), md.as_bytes())
}

pub mod binding;
pub mod bootstrap;
pub mod calendar;
pub mod analysis;
pub mod chapters;
pub mod contacts;
pub mod file_cache;
pub mod files;
pub mod gap_patch;
pub mod finalize;
pub mod history;
pub mod integrations;
pub mod integrity;
pub mod jsonl_store;
pub mod library;
pub mod license;
pub mod lifecycle;
pub mod live_chat;
pub mod llm;
pub mod llm_stream;
pub mod llm_text;
pub mod transcribe_priming;
pub mod meeting;
pub mod migrate;
pub mod pipeline;
pub mod playback;
pub mod qa;
pub mod voiceprints;
pub mod workflow_engine;
pub mod workflow_history;
pub mod workflows;
pub mod recording;
pub mod recordings;
pub mod search;
pub mod session;
pub mod session_store;
pub mod settings;
pub mod prompts;
pub mod summary;
pub mod tags;
pub mod update;
pub mod asr_models;

/// Write mono 16 kHz signed-16-bit PCM to a WAV file.
pub(crate) fn write_wav_mono_16k(
    path: &std::path::Path,
    samples: &[i16],
) -> crate::error::Result<()> {
    ::recording::write_wav_mono_i16(path, samples, 16_000)
        .map_err(|e| crate::error::AppError::Io(format!("write wav: {e}")))
}

/// Sample count of the silent mic placeholder: 0.1 s at 16 kHz. Written when a
/// meeting has no mic track.
pub(crate) const SILENT_MIC_PLACEHOLDER_SAMPLES: usize = 1600;
