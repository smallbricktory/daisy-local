// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! SpeechAnalyzer live backend: native volatile→final mapping, no window and
//! no committer — the OS owns the commit strategy (live output matches the
//! batch decode verbatim; finals never revise finals). The pure mapping
//! compiles and tests everywhere; only the FFI run loop is macOS.

use transcript::Segment;

#[cfg(any(target_os = "macos", test))]
use crate::streaming::agreement::StreamToken;
#[cfg(target_os = "macos")]
use crate::streaming::emit::emit_finals;
#[cfg(any(target_os = "macos", test))]
use crate::streaming::emit::{join_tokens, to_segment};

/// Live SpeechAnalyzer engine for one track. Constructed only when the
/// registry resolves the SA row (availability-gated at runtime); off-macOS
/// `run` errors.
pub struct SaRealtime {
    // Read by the macOS run loop; plain data elsewhere.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    locale: String,
}

impl SaRealtime {
    /// Empty `locale` = system locale (production always passes ""; the
    /// gated harness pins one for reproducibility).
    pub fn new(locale: impl Into<String>) -> Self {
        Self { locale: locale.into() }
    }
}

#[async_trait::async_trait]
impl providers_realtime::RealtimeTranscriber for SaRealtime {
    fn name(&self) -> &'static str {
        "speechanalyzer"
    }

    fn model(&self) -> &str {
        "Apple SpeechAnalyzer"
    }

    #[cfg(not(target_os = "macos"))]
    async fn run(
        &self,
        _sample_rate: u32,
        _audio_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<i16>>,
        _events_tx: tokio::sync::mpsc::Sender<providers_realtime::RealtimeEvent>,
    ) -> providers_realtime::Result<Vec<Segment>> {
        Err(providers_realtime::RealtimeError::Backend("SpeechAnalyzer is macOS-only".into()))
    }

    #[cfg(target_os = "macos")]
    async fn run(
        &self,
        _sample_rate: u32,
        mut audio_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<i16>>,
        events_tx: tokio::sync::mpsc::Sender<providers_realtime::RealtimeEvent>,
    ) -> providers_realtime::Result<Vec<Segment>> {
        use providers_realtime::{RealtimeError, RealtimeEvent};

        // The session is built on the first frame: a track that never carries
        // audio (the system track of a recording where nothing plays) then has
        // no analyzer to finalize when input closes.
        let mut session: Option<super::SaSession> = None;
        let mut batches_rx: Option<tokio::sync::mpsc::UnboundedReceiver<super::SaBatch>> = None;
        let mut accepted: Vec<Segment> = Vec::new();

        async fn emit_batch(
            words: Vec<StreamToken>,
            is_final: bool,
            events_tx: &tokio::sync::mpsc::Sender<RealtimeEvent>,
            accepted: &mut Vec<Segment>,
        ) {
            if is_final {
                emit_finals(&words, events_tx, accepted).await;
            } else if let Some(t) = join_tokens(&words) {
                let _ = events_tx.send(RealtimeEvent::Interim { segment: to_segment(&t) }).await;
            }
        }

        /// The batch channel only exists once the session does.
        async fn next_batch(
            rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<super::SaBatch>>,
        ) -> Option<super::SaBatch> {
            match rx {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        }

        loop {
            tokio::select! {
                frame = audio_rx.recv() => match frame {
                    Some(frame) => {
                        let started = match &session {
                            Some(s) => s,
                            None => {
                                // Init parks on shim semaphores (analyzer
                                // start + format probe) — blocking work, same
                                // as flush below.
                                let locale = self.locale.clone();
                                let (s, rx) = tokio::task::spawn_blocking(move || {
                                    super::SaSession::create(&locale)
                                })
                                .await
                                .map_err(|e| {
                                    RealtimeError::Backend(format!(
                                        "speechanalyzer init join: {e}"
                                    ))
                                })?
                                .map_err(|e| {
                                    RealtimeError::Backend(format!("speechanalyzer: {e}"))
                                })?;
                                batches_rx = Some(rx);
                                session.insert(s)
                            }
                        };
                        let pcm = crate::streaming::emit::samples_to_f32(frame);
                        if !started.feed(&pcm) {
                            let _ = events_tx
                                .send(RealtimeEvent::Error("speechanalyzer: feed rejected".into()))
                                .await;
                        }
                    }
                    None => break,
                },
                batch = next_batch(&mut batches_rx) => {
                    if let Some((words, is_final)) = batch {
                        emit_batch(words, is_final, &events_tx, &mut accepted).await;
                    }
                }
            }
        }

        // Input closed: flush blocks until every final callback has fired, so
        // it runs off the async threads; the session comes back for the drain.
        let Some(session) = session else { return Ok(accepted) };
        let session = tokio::task::spawn_blocking(move || {
            session.flush();
            session
        })
        .await
        .map_err(|e| RealtimeError::Backend(format!("speechanalyzer flush join: {e}")))?;
        if let Some(rx) = batches_rx.as_mut() {
            while let Ok((words, is_final)) = rx.try_recv() {
                emit_batch(words, is_final, &events_tx, &mut accepted).await;
            }
        }
        drop(session);
        Ok(accepted)
    }
}

#[cfg(test)]
mod tests {
    use super::super::SaBatch;
    use super::*;
    use crate::streaming::emit::finals_from;

    /// Fold a whole callback stream into (committed finals, trailing
    /// interim) — the run loop's per-batch emit, replayed over a recorded
    /// stream. A volatile batch carries the entire current hypothesis and
    /// replaces the previous one; a final batch commits and clears the
    /// interim.
    fn map_sa_stream(batches: &[SaBatch]) -> (Vec<Segment>, Option<Segment>) {
        let mut finals: Vec<Segment> = Vec::new();
        let mut interim: Option<Segment> = None;
        for (words, is_final) in batches {
            if *is_final {
                finals.extend(finals_from(words));
                interim = None;
            } else {
                interim = join_tokens(words).map(|t| to_segment(&t));
            }
        }
        (finals, interim)
    }

    fn tok(text: &str, start_ms: i64, end_ms: i64) -> StreamToken {
        StreamToken { text: text.into(), start_ms, end_ms }
    }

    /// The system track of a recording where nothing ever plays: input opens
    /// and closes without a frame. No analyzer is built, so the stop path has
    /// nothing to finalize and returns at once.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_track_that_carries_no_audio_starts_no_session() {
        use providers_realtime::RealtimeTranscriber;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(8);
        drop(audio_tx);
        let began = std::time::Instant::now();
        let out = rt.block_on(SaRealtime::new("en-US").run(16_000, audio_rx, events_tx)).unwrap();
        assert!(out.is_empty(), "no audio, no segments");
        assert!(began.elapsed() < std::time::Duration::from_secs(2), "{:?}", began.elapsed());
        assert!(events_rx.try_recv().is_err(), "and no events");
    }

    #[test]
    fn volatile_then_final_commits_once_with_no_interim_left() {
        let batches: Vec<SaBatch> = vec![
            (vec![tok("hello", 0, 400), tok("wor", 400, 700)], false),
            (vec![tok("hello", 0, 400), tok("world.", 400, 900)], true),
        ];
        let (finals, interim) = map_sa_stream(&batches);
        assert_eq!(finals.len(), 1, "one Final per committed utterance");
        assert_eq!(finals[0].text, "hello world.");
        assert_eq!((finals[0].start_ms, finals[0].end_ms), (0, 900));
        assert!(interim.is_none(), "final clears the volatile line");
    }

    #[test]
    fn one_final_batch_splits_at_a_real_word_gap() {
        // Two utterances 5 s apart committed by one final batch must emit as
        // two Finals, not one gap-spanning segment.
        let batches: Vec<SaBatch> = vec![(
            vec![tok("Okay.", 1_000, 1_500), tok("Right.", 6_500, 7_000)],
            true,
        )];
        let (finals, interim) = map_sa_stream(&batches);
        assert_eq!(finals.len(), 2, "gap-split into separate Finals");
        assert_eq!(finals[0].text, "Okay.");
        assert_eq!(finals[1].text, "Right.");
        assert!(interim.is_none());
    }

    #[test]
    fn trailing_volatile_survives_as_interim() {
        let batches: Vec<SaBatch> = vec![
            (vec![tok("hello world.", 0, 900)], true),
            (vec![tok("and", 1_000, 1_200), tok("then", 1_200, 1_500)], false),
        ];
        let (finals, interim) = map_sa_stream(&batches);
        assert_eq!(finals.len(), 1);
        let i = interim.expect("volatile after the final stays interim");
        assert_eq!(i.text, "and then");
        assert_eq!((i.start_ms, i.end_ms), (1_000, 1_500));
    }
}
