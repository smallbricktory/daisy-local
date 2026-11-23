// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Model lifecycle and one completion at a time. One context per process,
//! sized to `n_ctx`; its KV cache is cleared at the top of every request.

use daisy_llm_protocol::{Message, Reply, Request};
use anyhow::{anyhow, Context};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::data::LlamaTokenData;
use llama_cpp_2::token::data_array::LlamaTokenDataArray;
use llama_cpp_2::token::LlamaToken;
use std::io::{BufRead, Write};
use std::num::NonZeroU32;
use std::path::Path;
use std::time::Instant;

const N_BATCH: u32 = 2048;
const N_UBATCH: u32 = 512;

/// Gemma 4's turn markers. A chat template containing `TURN_OPEN` is rendered
/// by `turn_marker_prompt`.
const TURN_OPEN: &str = "<|turn>";
const TURN_CLOSE: &str = "<turn|>";

/// One `<|turn>role\n…<turn|>\n` block per message, ending with an open model
/// turn. BOS comes from the tokenizer, so no `<bos>` is written here.
fn turn_marker_prompt(system: &str, messages: &[Message<'_>]) -> String {
    fn turn(s: &mut String, role: &str, content: &str) {
        s.push_str(TURN_OPEN);
        s.push_str(role);
        s.push('\n');
        s.push_str(content.trim());
        s.push_str(TURN_CLOSE);
        s.push('\n');
    }
    let mut s = String::new();
    if !system.is_empty() {
        turn(&mut s, "system", system);
    }
    for m in messages {
        let role = if m.role == "assistant" {
            "model"
        } else {
            m.role.as_ref()
        };
        turn(&mut s, role, &m.content);
    }
    s.push_str(TURN_OPEN);
    s.push_str("model\n");
    s
}

pub struct Engine {
    backend: LlamaBackend,
    model: LlamaModel,
    n_ctx: u32,
}

/// llama.cpp's own schema-to-GBNF conversion (common/json-schema-to-grammar).
pub fn grammar_for(schema: &serde_json::Value) -> anyhow::Result<String> {
    llama_cpp_2::json_schema_to_grammar(&schema.to_string())
        .map_err(|e| anyhow!("schema to grammar: {e}"))
}

/// Samples one token: greedy over logits the repetition penalty has already
/// lowered, honouring `grammar` when there is one. The emitted token is
/// accepted into both before returning.
fn sample_constrained(
    ctx: &LlamaContext,
    repeat: &mut LlamaSampler,
    grammar: Option<&mut LlamaSampler>,
    idx: i32,
) -> LlamaToken {
    let pick = |grammar: Option<&LlamaSampler>| {
        let mut all = ctx.token_data_array_ith(idx);
        if let Some(g) = grammar {
            g.apply(&mut all);
        }
        repeat.apply(&mut all);
        all.sample_token_greedy()
    };
    let tok = pick(None);
    let tok = match grammar {
        None => tok,
        Some(grammar) => {
            let mut one = LlamaTokenDataArray::new(vec![LlamaTokenData::new(tok, 1.0, 0.0)], false);
            grammar.apply(&mut one);
            let tok = if one.data[0].logit() == f32::NEG_INFINITY { pick(Some(grammar)) } else { tok };
            grammar.accept(tok);
            tok
        }
    };
    repeat.accept(tok);
    tok
}

/// llama.cpp's DRY defaults over the whole reply: a run of output that
/// repeats an earlier run is penalised, more steeply the longer it gets.
/// Newlines, colons, quotes and asterisks end a run, so JSON keys and
/// punctuation repeat freely. The window has to be positive; a negative one
/// switches the penalty off.
fn repeat_penalty(model: &LlamaModel, window: u32) -> LlamaSampler {
    LlamaSampler::dry(model, 0.8, 1.75, 2, window as i32, ["\n", ":", "\"", "*"])
}

impl Engine {
    pub fn load(path: &Path, n_ctx: u32) -> anyhow::Result<Self> {
        let t = Instant::now();
        let backend = LlamaBackend::init()?;
        let params = LlamaModelParams::default().with_n_gpu_layers(1000);
        let model = LlamaModel::load_from_file(&backend, path, &params)
            .with_context(|| format!("load {}", path.display()))?;
        eprintln!(
            "daisy-llm: loaded {} in {:.1}s",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("model"),
            t.elapsed().as_secs_f64()
        );
        Ok(Self {
            backend,
            model,
            n_ctx,
        })
    }

    fn prompt(&self, system: &str, messages: &[Message<'_>]) -> anyhow::Result<String> {
        let tmpl = self
            .model
            .chat_template(None)
            .map_err(|e| anyhow!("chat template: {e}"))?;
        let tmpl_text = tmpl.to_str().map_err(|e| anyhow!("chat template: {e}"))?;
        if tmpl_text.contains(TURN_OPEN) {
            return Ok(turn_marker_prompt(system, messages));
        }
        let mut chat = Vec::with_capacity(messages.len() + 1);
        if !system.is_empty() {
            chat.push(LlamaChatMessage::new("system".into(), system.to_string())?);
        }
        for m in messages {
            chat.push(LlamaChatMessage::new(m.role.to_string(), m.content.to_string())?);
        }
        self.model
            .apply_chat_template(&tmpl, &chat, true)
            .map_err(|e| anyhow!("apply chat template: {e}"))
    }

    /// The one context every request runs in, sized to `n_ctx`.
    pub fn context(&self) -> anyhow::Result<LlamaContext<'_>> {
        let params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(self.n_ctx))
            .with_n_batch(N_BATCH)
            .with_n_ubatch(N_UBATCH);
        self.model
            .new_context(&self.backend, params)
            .map_err(|e| anyhow!("context: {e}"))
    }

    pub fn complete(&self, ctx: &mut LlamaContext<'_>, req: &Request<'_>) -> anyhow::Result<Reply> {
        ctx.clear_kv_cache();
        let prompt = self.prompt(&req.system, &req.messages)?;
        let tokens = self
            .model
            .str_to_token(&prompt, AddBos::Always)
            .map_err(|e| anyhow!("tokenize: {e}"))?;
        let n_prompt = tokens.len() as u32;
        if n_prompt.saturating_add(req.max_tokens) > self.n_ctx {
            anyhow::bail!(
                "prompt is {n_prompt} tokens; with {} output tokens it exceeds the {} context",
                req.max_tokens,
                self.n_ctx
            );
        }
        // The grammar is built before the prefill and applied in `sample_constrained`.
        let mut grammar = match &req.schema {
            Some(schema) => {
                let gbnf = grammar_for(schema)?;
                Some(
                    LlamaSampler::grammar(&self.model, &gbnf, "root")
                        .map_err(|e| anyhow!("grammar sampler: {e}"))?,
                )
            }
            None => None,
        };
        let mut repeat = repeat_penalty(&self.model, req.max_tokens.max(1));

        // Prefill in n_batch-sized chunks; logits only for the last token.
        let t0 = Instant::now();
        let mut batch = LlamaBatch::new(N_BATCH as usize, 1);
        let last = tokens.len() - 1;
        for (i, tok) in tokens.iter().enumerate() {
            batch.add(*tok, i as i32, &[0], i == last)?;
            if batch.n_tokens() as u32 == N_BATCH || i == last {
                ctx.decode(&mut batch)
                    .map_err(|e| anyhow!("prefill decode: {e}"))?;
                batch.clear();
            }
        }
        let prefill_s = t0.elapsed().as_secs_f64();

        let t1 = Instant::now();
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut text = String::new();
        let mut n_cur = tokens.len() as i32;
        let mut n_out: u32 = 0;
        // The prompt's last logits are at index n_tokens-1 of the final batch.
        let mut idx = (tokens.len() as i32 - 1) % N_BATCH as i32;
        while n_out < req.max_tokens {
            // Sampling accepts the token into the repeat penalty and the grammar.
            let tok = sample_constrained(ctx, &mut repeat, grammar.as_mut(), idx);
            if self.model.is_eog_token(tok) {
                break;
            }
            text.push_str(&self.model.token_to_piece(tok, &mut decoder, false, None)?);
            n_out += 1;
            batch.clear();
            batch.add(tok, n_cur, &[0], true)?;
            n_cur += 1;
            ctx.decode(&mut batch).map_err(|e| anyhow!("decode: {e}"))?;
            idx = 0;
        }
        let decode_s = t1.elapsed().as_secs_f64();
        eprintln!(
            "daisy-llm: prefill {prefill_s:.2}s ({n_prompt} tok, {:.0} tok/s) decode {decode_s:.2}s ({n_out} tok, {:.1} tok/s)",
            n_prompt as f64 / prefill_s.max(1e-9),
            n_out as f64 / decode_s.max(1e-9)
        );
        Ok(Reply {
            id: req.id.to_string(),
            text: Some(text),
            error: None,
            prompt_tokens: n_prompt,
            completion_tokens: n_out,
            seconds: prefill_s + decode_s,
        })
    }
}

/// One request per stdin line, one reply per stdout line, in order. A bad
/// line answers with an error reply carrying the id when one parses.
pub fn serve(path: &Path, n_ctx: u32) -> anyhow::Result<()> {
    let engine = Engine::load(path, n_ctx)?;
    let mut ctx = engine.context()?;
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(req) => engine
                .complete(&mut ctx, &req)
                .unwrap_or_else(|e| Reply::error(&req.id, format!("{e:#}"))),
            Err(e) => {
                let id = serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_string))
                    .unwrap_or_default();
                Reply::error(&id, format!("bad request line: {e}"))
            }
        };
        serde_json::to_writer(&mut out, &reply)?;
        out.write_all(b"\n")?;
        out.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{turn_marker_prompt, Message};

    fn msg<'a>(role: &'a str, content: &'a str) -> Message<'a> {
        Message { role: role.into(), content: content.into() }
    }

    #[test]
    fn turn_markers_wrap_each_message_and_leave_the_model_turn_open() {
        let p = turn_marker_prompt(
            "Be brief. ",
            &[msg("user", " hello "), msg("assistant", "hi"), msg("user", "again")],
        );
        assert_eq!(
            p,
            "<|turn>system\nBe brief.<turn|>\n\
             <|turn>user\nhello<turn|>\n\
             <|turn>model\nhi<turn|>\n\
             <|turn>user\nagain<turn|>\n\
             <|turn>model\n"
        );
    }

    #[test]
    fn an_empty_system_adds_no_system_turn() {
        let p = turn_marker_prompt("", &[msg("user", "hello")]);
        assert_eq!(p, "<|turn>user\nhello<turn|>\n<|turn>model\n");
    }

    #[test]
    fn every_production_schema_converts_to_a_grammar() {
        for (name, schema) in summarize::schemas::all() {
            let g = super::grammar_for(&schema).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(g.contains("root ::="), "{name}: {g}");
        }
    }

    // Schema patterns spell their classes out (`[0-9]`), with no regex escapes.
    #[test]
    fn no_production_grammar_carries_a_regex_escape() {
        for (name, schema) in summarize::schemas::all() {
            let g = super::grammar_for(&schema).unwrap_or_else(|e| panic!("{name}: {e}"));
            for esc in ["\\d", "\\w", "\\s", "\\D", "\\W", "\\S"] {
                assert!(
                    !g.contains(esc),
                    "{name}: grammar carries {esc}, which GBNF has no escape for — \
                     write the class out in the schema pattern:\n{g}"
                );
            }
        }
    }
}
