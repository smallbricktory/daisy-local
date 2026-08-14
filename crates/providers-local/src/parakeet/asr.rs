//! TDT greedy decode loop over the session pipeline, with token→word
//! aggregation and 80 ms-frame timestamps.

use super::error::ParakeetError;
use super::model::{ParakeetModel, ParakeetTokenizer, VOCAB_SIZE};
use super::ParakeetOpts;
use ndarray::{s, ArrayViewD, IxDyn};
use std::path::Path;

fn vmax(v: ndarray::ArrayView1<f32>) -> f32 {
    v.iter().cloned().fold(f32::NEG_INFINITY, f32::max)
}

fn vargmax(v: ndarray::ArrayView1<f32>) -> usize {
    let mut bi = 0;
    let mut bv = f32::NEG_INFINITY;
    for (i, &x) in v.iter().enumerate() {
        if x > bv {
            bv = x;
            bi = i;
        }
    }
    bi
}

pub struct ParakeetASR {
    tokenizer: ParakeetTokenizer,
    model: ParakeetModel,
}

impl ParakeetASR {
    pub fn new(model_dir: &Path, opts: &ParakeetOpts) -> Result<Self, ParakeetError> {
        let tokenizer = ParakeetTokenizer::new(model_dir)?;
        let model = ParakeetModel::new(model_dir, opts)?;
        Ok(Self { tokenizer, model })
    }

    pub fn infer_buffer(&mut self, buffer: &[f32]) -> Result<ASRResult, ParakeetError> {
        let shape = vec![1, buffer.len()];
        let audio: ArrayViewD<f32> = ArrayViewD::from_shape(IxDyn(&shape), buffer)?;

        let features = self.model.fbank_features(&audio, audio.len() as i64)?;

        let blank_id = self.tokenizer.blank_id();

        let encoder_output = self.model.encoder_infer(&features.view())?;
        let encoder_output_length = encoder_output.shape()[1];
        let encoder_output_projected = self.model.joint_enc_infer(&encoder_output.view())?;

        let (mut state0, mut state1) = self.model.get_init_decoder_state();

        let mut time_index = 0usize;
        let mut safe_time_index = 0;
        let mut time_index_current_labels;
        let last_timestamps = encoder_output_length - 1;

        let mut active = encoder_output_length > 0;
        let mut advance = false;
        let mut label = blank_id;
        let mut score;
        let mut duration;

        let mut asr_result = ASRResult::new();

        while active {
            // Prediction-network step, then joint scoring at the current
            // encoder frame.
            let (mut decoder_output, state0_next, state1_next) =
                self.model.decoder_infer(label, &state0.view(), &state1.view())?;
            decoder_output = self.model.joint_pred_infer(&decoder_output.view())?;
            let f_slice = encoder_output_projected.slice(s![0, safe_time_index, ..]);
            let f = f_slice.to_shape(IxDyn(&[1, 1, 640]))?;
            let logits = self.model.joint_net_infer(&f.view(), &decoder_output.view())?;

            score = vmax(logits.slice(s![0, 0, 0, ..VOCAB_SIZE]));
            label = vargmax(logits.slice(s![0, 0, 0, ..VOCAB_SIZE])) as i64;
            log::trace!(
                "score: {}, label: {}, token: {}, safe_time_index: {}",
                score,
                label,
                self.tokenizer.decode(label),
                safe_time_index
            );

            // TDT: the second logit block predicts how many 80 ms frames to
            // jump.
            let jump_duration_index = vargmax(logits.slice(s![0, 0, 0, VOCAB_SIZE..])) as i64;
            duration = jump_duration_index as i32;
            time_index_current_labels = time_index;

            if duration == 0 {
                duration = 1;
            }

            time_index += duration as usize;
            safe_time_index = time_index.min(last_timestamps);
            if time_index >= last_timestamps {
                active = false;
            }
            if active && label == blank_id {
                advance = true;
            }

            // Blank emitted mid-stream: keep advancing frames with the same
            // prediction-network state until a real token or the end.
            while advance {
                time_index_current_labels = time_index;

                let f_slice = encoder_output_projected.slice(s![0, safe_time_index, ..]);
                let f = f_slice.to_shape(IxDyn(&[1, 1, 640]))?;
                let logits = self.model.joint_net_infer(&f.view(), &decoder_output.view())?;
                score = vmax(logits.slice(s![0, 0, 0, ..VOCAB_SIZE]));
                label = vargmax(logits.slice(s![0, 0, 0, ..VOCAB_SIZE])) as i64;
                log::trace!(
                    "advance score: {}, label: {}, token: {}",
                    score,
                    label,
                    self.tokenizer.decode(label)
                );

                let jump_duration_index = vargmax(logits.slice(s![0, 0, 0, VOCAB_SIZE..])) as i64;
                duration = jump_duration_index as i32;

                if label == blank_id && duration == 0 {
                    duration = 1;
                }
                time_index += duration as usize;
                safe_time_index = time_index.min(last_timestamps);
                if time_index >= last_timestamps {
                    active = false;
                }
                advance = active && label == blank_id;
            }
            state0 = state0_next;
            state1 = state1_next;
            if label != blank_id {
                asr_result.add_token_record(
                    self.tokenizer.decode(label).to_string(),
                    duration,
                    time_index_current_labels as i32,
                );
            }
        }

        Ok(asr_result)
    }
}

#[derive(Debug, Clone)]
pub struct TokenRecognitionRecord {
    pub token: String,
    pub duration: i32,
    pub time_index: i32,
}

#[derive(Debug, Clone)]
pub struct WordRecognitionRecord {
    pub word: String,
    pub start_time: f32,
    pub end_time: f32,
}

const TOKEN_PREFIX: &str = "▁";
/// One encoder frame = 80 ms of audio.
const ASR_WINDOW: f32 = 0.08;

#[derive(Debug, Default)]
pub struct ASRResult {
    token_records: Vec<TokenRecognitionRecord>,
    word_records: Vec<WordRecognitionRecord>,
}

impl ASRResult {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_token_record(&mut self, token: String, duration: i32, time_index: i32) {
        self.token_records.push(TokenRecognitionRecord { token, duration, time_index });
    }

    /// Aggregate SentencePiece tokens into words with frame-derived spans.
    /// A word starts at a `▁`-prefixed token; every other token — including
    /// bare punctuation — appends to the current word, so "hello" + ","
    /// becomes one "hello," record. A standalone punctuation record would
    /// get a space joined in front of it downstream.
    pub fn generate_word_records(&mut self) {
        let mut word_start_time = 0.0;
        let mut word_buffer = String::new();
        let mut word_duration = 0.0;
        for token_record in &self.token_records {
            let is_starting_token = token_record.token.starts_with(TOKEN_PREFIX);
            if is_starting_token && !word_buffer.is_empty() {
                self.word_records.push(WordRecognitionRecord {
                    word: word_buffer.clone(),
                    start_time: word_start_time,
                    end_time: word_start_time + word_duration,
                });
                word_buffer.clear();
                word_duration = 0.0;
            }
            if word_buffer.is_empty() {
                word_start_time = token_record.time_index as f32 * ASR_WINDOW;
            }
            if is_starting_token {
                word_buffer.push_str(&token_record.token.replace(TOKEN_PREFIX, " "));
            } else {
                word_buffer.push_str(&token_record.token);
            }
            word_duration += token_record.duration as f32 * ASR_WINDOW;
        }
        if !word_buffer.is_empty() {
            self.word_records.push(WordRecognitionRecord {
                word: word_buffer.clone(),
                start_time: word_start_time,
                end_time: word_start_time + word_duration,
            });
        }
    }

    pub fn get_word_records(&self) -> &[WordRecognitionRecord] {
        &self.word_records
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(tokens: &[&str]) -> Vec<String> {
        let mut r = ASRResult::new();
        for (i, t) in tokens.iter().enumerate() {
            r.add_token_record(t.to_string(), 1, i as i32);
        }
        r.generate_word_records();
        r.get_word_records().iter().map(|w| w.word.trim().to_string()).collect()
    }

    #[test]
    fn punctuation_attaches_to_the_preceding_word() {
        // TDT emits punctuation as bare tokens; a standalone punctuation
        // "word" gets a space joined in front of it downstream ("hello ,").
        assert_eq!(words(&["▁hello", ",", "▁world", "."]), ["hello,", "world."]);
        assert_eq!(words(&["▁yes", "!", "▁o", "k", "?"]), ["yes!", "ok?"]);
    }

    #[test]
    fn abbreviation_periods_stay_inside_the_word() {
        assert_eq!(words(&["▁U", ".", "S", "."]), ["U.S."]);
    }

    #[test]
    fn leading_punctuation_still_emits() {
        assert_eq!(words(&[",", "▁next"]), [",", "next"]);
    }
}
