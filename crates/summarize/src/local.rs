// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The built-in model: Daisy's `daisy-llm` worker over a pipe. One instance
//! is shared by every call site, so chapters, summary, analysis, polish and
//! Q&A all reach one worker holding the weights. The worker is spawned on the
//! first call, killed when it has gone unused for the idle window, and
//! respawned by the next call.

use crate::chat::ChatCompleter;
use crate::error::{Result, SummarizeError};
use daisy_llm_protocol::{no_window, Message, Reply, Request};
use serde_json::Value;
use std::borrow::Cow;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Context the worker is started with; long meetings window and fold to it.
pub const LOCAL_CONTEXT_TOKENS: u32 = 20_480;
/// Registry id of the one built-in model.
pub const LOCAL_MODEL_ID: &str = "gemma-4-e4b-it-q4";

/// Ceiling on one request.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// How long a worker with no request outstanding stays resident before it is
/// killed and its weights released.
const DEFAULT_IDLE: Duration = Duration::from_secs(300);

/// One request at a time over the worker's pipe.
struct Pipe {
    stdin: ChildStdin,
    lines: Receiver<std::io::Result<String>>,
}

/// One worker process. A request holds `io` for its whole length; `kill` takes
/// only `child`, so the process can be ended while a request is in flight.
struct Worker {
    child: Mutex<Child>,
    io: Mutex<Pipe>,
}

impl Worker {
    fn kill(&self) {
        let _ = self.child.lock().unwrap_or_else(|e| e.into_inner()).kill();
    }

    /// Kills the process and waits for it to exit, so nothing maps the
    /// weights once this returns.
    fn end(&self) {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// The worker slot plus the clock the idle watcher reads. The slot is the one
/// handle to the process; it is never held across a request. Once `closed`
/// is set under the slot lock, no worker is spawned again.
struct Live {
    worker: Mutex<Option<Arc<Worker>>>,
    last_used: Mutex<Instant>,
    watching: AtomicBool,
    closed: AtomicBool,
}

impl Live {
    fn touch(&self) {
        *self.last_used.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    /// The resident worker, spawning one with `spawn` when none is. The bool
    /// is whether the caller must start the idle watcher. A closed slot
    /// refuses.
    fn acquire(&self, spawn: impl FnOnce() -> Result<Worker>) -> Result<(Arc<Worker>, bool)> {
        let mut slot = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if self.closed.load(Ordering::SeqCst) {
            return Err(SummarizeError::Worker("the built-in model was stopped".into()));
        }
        if let Some(w) = slot.as_ref() {
            return Ok((w.clone(), false));
        }
        let w = Arc::new(spawn()?);
        *slot = Some(w.clone());
        Ok((w, !self.watching.swap(true, Ordering::SeqCst)))
    }

    /// Empties the slot if `w` is still the worker in it. The process dies
    /// with the last handle to it.
    fn retire(&self, w: &Arc<Worker>) {
        let mut slot = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().is_some_and(|live| Arc::ptr_eq(live, w)) {
            *slot = None;
        }
    }

    /// Kills the worker when nothing has used it for `idle`. Returns whether a
    /// worker is still resident afterwards, clearing `watching` under the slot
    /// lock when none is.
    fn release_if_idle(&self, idle: Duration) -> bool {
        let mut slot = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        // A caller in `call` holds a second handle for its whole request.
        let resident = match slot.as_ref() {
            None => false,
            Some(w) => {
                Arc::strong_count(w) > 1
                    || self.last_used.lock().unwrap_or_else(|e| e.into_inner()).elapsed() < idle
            }
        };
        if !resident {
            *slot = None;
            self.watching.store(false, Ordering::SeqCst);
        }
        resident
    }
}

pub struct LocalChat {
    worker: PathBuf,
    model: PathBuf,
    n_ctx: u32,
    timeout: Duration,
    idle: Duration,
    live: Arc<Live>,
    next_id: Mutex<u64>,
}

impl LocalChat {
    pub fn new(worker: PathBuf, model: PathBuf, n_ctx: u32) -> Self {
        Self {
            worker,
            model,
            n_ctx,
            timeout: DEFAULT_TIMEOUT,
            idle: DEFAULT_IDLE,
            live: Arc::new(Live {
                worker: Mutex::new(None),
                last_used: Mutex::new(Instant::now()),
                watching: AtomicBool::new(false),
                closed: AtomicBool::new(false),
            }),
            next_id: Mutex::new(0),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets how long an unused worker stays resident.
    pub fn with_idle(mut self, idle: Duration) -> Self {
        self.idle = idle;
        self
    }

    /// The worker binary and model weights this instance was built for.
    pub fn paths(&self) -> (&Path, &Path) {
        (&self.worker, &self.model)
    }

    /// Whether a worker process is resident right now.
    pub fn has_worker(&self) -> bool {
        self.live.worker.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Ends the resident worker and waits for its process to exit. A request
    /// in flight ends with a worker error, and every later call is refused:
    /// this instance never spawns a worker again.
    pub fn close(&self) {
        let worker = {
            let mut slot = self.live.worker.lock().unwrap_or_else(|e| e.into_inner());
            self.live.closed.store(true, Ordering::SeqCst);
            slot.take()
        };
        if let Some(w) = worker {
            w.end();
        }
    }

    /// Starts the thread that kills the worker once it has been idle for
    /// `idle`; the thread exits with the worker it watches.
    fn watch_idle(&self) {
        let live = Arc::downgrade(&self.live);
        let idle = self.idle;
        let tick = (idle / 4).max(Duration::from_millis(20));
        std::thread::spawn(move || loop {
            std::thread::sleep(tick);
            let Some(live) = live.upgrade() else { return };
            if !live.release_if_idle(idle) {
                return;
            }
        });
    }

    /// Starts the worker process at normal priority.
    fn spawn(&self) -> Result<Worker> {
        let (worker, model, n_ctx) = (self.worker.clone(), self.model.clone(), self.n_ctx);
        transcript::cpu::run_at_normal_priority(move || Self::spawn_worker(&worker, &model, n_ctx))
    }

    fn spawn_worker(worker: &std::path::Path, model: &std::path::Path, n_ctx: u32) -> Result<Worker> {
        let mut child = no_window(Command::new(worker))
            .arg("--model")
            .arg(model)
            .arg("--ctx")
            .arg(n_ctx.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| SummarizeError::Worker(format!("start {}: {e}", worker.display())))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(|l| l.ok()) {
                if line.starts_with("daisy-llm:") {
                    log::info!("{line}");
                } else {
                    log::debug!("{line}");
                }
            }
        });
        Ok(Worker { child: Mutex::new(child), io: Mutex::new(Pipe { stdin, lines: rx }) })
    }

    fn call(&self, system: &str, messages: &[(String, String)], schema: Option<&Value>, max_tokens: u32) -> Result<Reply> {
        let (worker, watch) = self.live.acquire(|| self.spawn())?;
        self.live.touch();
        if watch {
            self.watch_idle();
        }
        let id = {
            let mut n = self.next_id.lock().unwrap_or_else(|e| e.into_inner());
            *n += 1;
            format!("q{}", *n)
        };
        let req = Request {
            id: Cow::Borrowed(&id),
            system: Cow::Borrowed(system),
            messages: messages.iter().map(|(r, c)| Message { role: Cow::Borrowed(r), content: Cow::Borrowed(c) }).collect(),
            schema: schema.map(Cow::Borrowed),
            max_tokens,
        };
        let line = serde_json::to_string(&req).map_err(|e| SummarizeError::Worker(format!("encode: {e}")))?;
        let (result, dead) = {
            let mut io = worker.io.lock().unwrap_or_else(|e| e.into_inner());
            let write = io.stdin.write_all(line.as_bytes()).and_then(|_| io.stdin.write_all(b"\n")).and_then(|_| io.stdin.flush());
            match write {
                Err(e) => (Err(SummarizeError::Worker(format!("worker went away: {e}"))), true),
                Ok(()) => loop {
                    match io.lines.recv_timeout(self.timeout) {
                        Ok(Ok(l)) => {
                            let r: Reply = match serde_json::from_str(&l) {
                                Ok(r) => r,
                                Err(e) => break (Err(SummarizeError::Worker(format!("bad reply line: {e}"))), false),
                            };
                            if r.id != id {
                                continue;
                            }
                            log::info!("daisy-llm reply {}: {} prompt, {} output tokens in {:.1}s", r.id, r.prompt_tokens, r.completion_tokens, r.seconds);
                            self.live.touch();
                            break (Ok(r), false);
                        }
                        Ok(Err(e)) => break (Err(SummarizeError::Worker(format!("read: {e}"))), true),
                        Err(RecvTimeoutError::Disconnected) => break (Err(SummarizeError::Worker("the worker exited before replying".into())), true),
                        Err(RecvTimeoutError::Timeout) => break (Err(SummarizeError::Worker(format!("no reply within {} s", self.timeout.as_secs()))), true),
                    }
                },
            }
        };
        if dead {
            worker.kill();
            self.live.retire(&worker);
        }
        result
    }

    /// Free-text completion (Q&A, live chat). `messages` are `(role, content)` turns.
    pub fn complete_text(&self, system: &str, messages: &[(String, String)], max_tokens: u32) -> Result<String> {
        let r = self.call(system, messages, None, max_tokens)?;
        match (r.text, r.error) {
            (_, Some(e)) => Err(SummarizeError::Worker(e)),
            (Some(t), None) => Ok(t),
            (None, None) => Err(SummarizeError::Worker("empty reply".into())),
        }
    }

    /// One schema-constrained completion, capped at `max_tokens` output.
    pub fn complete_schema(&self, system: &str, user: &str, schema: &Value, max_tokens: u32) -> Result<Value> {
        let msgs = [("user".to_string(), user.to_string())];
        let r = self.call(system, &msgs, Some(schema), max_tokens)?;
        if let Some(e) = r.error {
            return Err(SummarizeError::Worker(e));
        }
        if r.completion_tokens >= max_tokens {
            log::warn!("daisy-llm reply {} stopped at its {max_tokens}-token limit", r.id);
        }
        let text = r.text.unwrap_or_default();
        serde_json::from_str(&text).map_err(|e| SummarizeError::Decode(format!("worker json: {e}")))
    }
}

/// A [`ChatCompleter`] over the shared instance, carrying one caller's output
/// reserve. Cloning the `Arc` is what keeps every call site on one worker.
pub struct LocalCompleter {
    chat: Arc<LocalChat>,
    max_tokens: u32,
}

impl LocalCompleter {
    pub fn new(chat: Arc<LocalChat>, max_tokens: u32) -> Self {
        Self { chat, max_tokens }
    }
}

impl ChatCompleter for LocalCompleter {
    fn complete_json(&self, system: &str, user: &str, _tool_name: &str, schema: &Value) -> Result<Value> {
        self.chat.complete_schema(system, user, schema, self.max_tokens)
    }
}

#[cfg(test)]
mod watcher_tests {
    use super::*;

    /// A watcher that finds the slot empty clears `watching` before it drops
    /// the slot lock, so a spawn that follows is told to start a watcher.
    #[test]
    fn a_stopping_watcher_clears_the_flag_under_the_slot_lock() {
        let live = Live {
            worker: Mutex::new(None),
            last_used: Mutex::new(Instant::now()),
            watching: AtomicBool::new(true),
            closed: AtomicBool::new(false),
        };
        assert!(!live.release_if_idle(Duration::ZERO));
        assert!(
            !live.watching.load(Ordering::SeqCst),
            "a spawn racing the watcher's exit finds the flag still set, starts no watcher, and leaves the worker resident forever"
        );
    }
}
