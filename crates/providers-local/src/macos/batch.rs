// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! SpeechAnalyzer batch transcriber (re-transcribe + single-track imports):
//! whole-file feed → flush → mapped finals. No windowing and no committer —
//! SA has no long-audio cap and owns its own segmentation.

use std::path::Path;

use providers_http::{ProviderError, Result, Transcriber};
use transcript::Segment;

/// Batch SpeechAnalyzer engine. Constructed only when the registry resolves
/// the SA row (availability-gated at runtime); off-macOS `transcribe` errors.
pub struct SaTranscriber {
    // Read by the macOS transcribe body; plain data elsewhere.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    locale: String,
}

impl SaTranscriber {
    /// Empty `locale` = system locale (production always passes ""; the
    /// gated harness pins one for reproducibility).
    pub fn new(locale: impl Into<String>) -> Self {
        Self { locale: locale.into() }
    }
}

impl Transcriber for SaTranscriber {
    fn name(&self) -> &'static str {
        "speechanalyzer_local"
    }

    fn model(&self) -> &str {
        "Apple SpeechAnalyzer"
    }

    /// `language_hint` is ignored; the session locale applies.
    fn transcribe(&self, wav_path: &Path, _language_hint: Option<&str>) -> Result<Vec<Segment>> {
        #[cfg(target_os = "macos")]
        {
            let samples =
                crate::wav::decode_wav_16k_mono_f32(wav_path).map_err(ProviderError::Other)?;
            let (session, mut rx) = super::SaSession::create(&self.locale, false)
                .map_err(|e| ProviderError::Other(format!("speechanalyzer: {e}")))?;
            for chunk in samples.chunks(16_000) {
                if !session.feed(chunk) {
                    return Err(ProviderError::Other("speechanalyzer: feed rejected".into()));
                }
            }
            // Blocks until every final callback has fired.
            session.flush();
            let mut segs: Vec<Segment> = Vec::new();
            while let Ok((words, is_final)) = rx.try_recv() {
                if is_final {
                    segs.extend(crate::streaming::emit::finals_from(&words));
                }
            }
            Ok(segs)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = wav_path;
            Err(ProviderError::Other("SpeechAnalyzer is macOS-only".into()))
        }
    }
}
