// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! One continuous Nemotron stream: audio in, timed SentencePiece tokens
//! out. A token found at encoder frame `t` of chunk `c` is stamped
//! `560 c + 80 t` ms from the stream start. Emitted tokens are final.

use super::features::{MelStream, CHUNK_MS, CHUNK_SAMPLES};
use super::model::{DecoderState, EncoderCache, NemotronModel, BLANK, FRAME_MS};
use super::NemotronError;

/// Greedy RNNT symbols allowed per encoder frame.
const MAX_SYMBOLS_PER_FRAME: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct TimedToken {
    pub text: String,
    pub ms: i64,
}

pub fn token_ms(chunk: usize, frame: usize) -> i64 {
    chunk as i64 * CHUNK_MS + frame as i64 * FRAME_MS
}

/// Chunk `chunk`'s first new frame is centred on sample `chunk * 8960`; a
/// chunk starting at or past the last input sample carries only the zeros
/// `finish` flushes.
fn chunk_has_audio(chunk: usize, samples: usize) -> bool {
    chunk * CHUNK_SAMPLES < samples
}

#[derive(Default)]
pub struct NemotronStream {
    mel: MelStream,
    cache: EncoderCache,
    dec: Option<DecoderState>,
    chunks: usize,
    samples: usize,
}

impl NemotronStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed 16 kHz mono samples; returns the tokens of every chunk they
    /// completed.
    pub fn push(&mut self, model: &mut NemotronModel, samples: &[f32]) -> Result<Vec<TimedToken>, NemotronError> {
        self.samples += samples.len();
        self.mel.push(samples);
        self.drain(model)
    }

    /// End of input: flushes the last partial chunk. A trailing chunk that
    /// holds no input audio is discarded undecoded.
    pub fn finish(&mut self, model: &mut NemotronModel) -> Result<Vec<TimedToken>, NemotronError> {
        self.mel.finish();
        self.drain(model)
    }

    /// Audio the stream has decoded, in ms from its start.
    pub fn decoded_ms(&self) -> i64 {
        self.chunks as i64 * CHUNK_MS
    }

    fn drain(&mut self, model: &mut NemotronModel) -> Result<Vec<TimedToken>, NemotronError> {
        let mut out = Vec::new();
        while let Some(window) = self.mel.next_chunk() {
            if !chunk_has_audio(self.chunks, self.samples) {
                continue;
            }
            let enc = model.encode(&window, &mut self.cache)?;
            let mut dec = match self.dec.take() {
                Some(d) => d,
                None => model.predict(BLANK, &DecoderState::zeros())?,
            };
            for t in 0..enc.shape()[2] {
                for _ in 0..MAX_SYMBOLS_PER_FRAME {
                    let k = model.joint(&enc, t, &dec)?;
                    if k == BLANK {
                        break;
                    }
                    out.push(TimedToken { text: model.token(k).to_string(), ms: token_ms(self.chunks, t) });
                    dec = model.predict(k, &dec)?;
                }
            }
            self.dec = Some(dec);
            self.chunks += 1;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_time_is_chunk_plus_encoder_frame() {
        assert_eq!(token_ms(0, 0), 0);
        assert_eq!(token_ms(0, 6), 480);
        assert_eq!(token_ms(1, 0), 560);
        assert_eq!(token_ms(3, 2), 1_840);
    }

    #[test]
    fn a_chunk_holds_audio_while_it_starts_before_the_input_ends() {
        assert!(!chunk_has_audio(0, 0), "an empty stream decodes nothing");
        assert!(chunk_has_audio(0, 1));
        assert!(chunk_has_audio(1, CHUNK_SAMPLES + 1));
        assert!(!chunk_has_audio(1, CHUNK_SAMPLES), "the flushed tail is all zeros");
        assert!(!chunk_has_audio(2, CHUNK_SAMPLES + 1));
    }
}
