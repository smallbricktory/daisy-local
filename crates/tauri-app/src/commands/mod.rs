pub mod bench;
pub mod binding;
pub mod bootstrap;
pub mod calendar;
pub mod analysis;
pub mod chapters;
pub mod contacts;
pub mod files;
pub mod gap_patch;
pub mod finalize;
pub mod history;
pub mod integrations;
pub mod integrity;
pub mod library;
pub mod license;
pub mod lifecycle;
pub mod live_chat;
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
pub mod settings;
pub mod prompts;
pub mod summary;
pub mod tags;
pub mod update;
pub mod whisper_models;

/// Energy-gate-relevant manifest fields, tolerant of partial/older
/// manifests: the mic device description and whether the mic carries a
/// single local speaker (absent = true, matching the manifest default).
pub(crate) struct ManifestGateProbe {
    pub mic_description: Option<String>,
    pub single_local_speaker: bool,
}

pub(crate) fn manifest_gate_probe(session_dir: &std::path::Path) -> ManifestGateProbe {
    #[derive(serde::Deserialize)]
    struct Probe {
        mic_source_description: Option<String>,
        single_local_speaker: Option<bool>,
    }
    let p = syncsafe::read(session_dir.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Probe>(&b).ok());
    ManifestGateProbe {
        mic_description: p.as_ref().and_then(|p| p.mic_source_description.clone()),
        single_local_speaker: p.and_then(|p| p.single_local_speaker).unwrap_or(true),
    }
}

/// Write mono 16 kHz signed-16-bit PCM to a WAV file.
pub(crate) fn write_wav_mono_16k(
    path: &std::path::Path,
    samples: &[i16],
) -> crate::error::Result<()> {
    use crate::error::AppError;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)
        .map_err(|e| AppError::Io(format!("create wav: {e}")))?;
    for &s in samples {
        w.write_sample(s).map_err(|e| AppError::Io(format!("write wav: {e}")))?;
    }
    w.finalize().map_err(|e| AppError::Io(format!("finalize wav: {e}")))?;
    Ok(())
}

/// Sample count of the silent mic placeholder: 0.1 s at 16 kHz. Written when a
/// meeting has no mic track.
pub(crate) const SILENT_MIC_PLACEHOLDER_SAMPLES: usize = 1600;
