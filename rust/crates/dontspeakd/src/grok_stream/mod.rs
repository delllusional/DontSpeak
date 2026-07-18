//! Grok mid-turn narration — tail interactive session `updates.jsonl` (ACP
//! `agent_message_chunk`). No app-server / MessageDisplay.
//!
//! Guarantees: session-keyed (registry from GreetSession/MarkActive with source=Grok);
//! witness on attach so Stop stays silent; never double-speak
//! ([`ds_narrate::deliver_batch`] HWM); ~12h idle eviction; SessionEnd forgets.
//! Config re-read per loop: `grok_stream` only.

mod proto;
mod tail;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ds_config::{NarrateKind, Paths, VoiceConfig};
use ds_narrate::{BatchPayload, NarrationUtterance, StreamBatch};

use proto::{batch_key, parse_agent_text_chunk};
use tail::JsonlTail;

// ── Session registry (hooks → supervisor) ────────────────────────────────────

/// Session ids reported by Grok hooks. Simpler than Codex: nudge / snapshot / forget /
/// idle eviction; no ensure_remote / app-server.
pub(crate) struct SessionRegistry {
    inner: Mutex<RegInner>,
    cv: Condvar,
}

struct RegInner {
    sessions: HashMap<String, Instant>,
    epoch: u64,
}

impl SessionRegistry {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(SessionRegistry {
            inner: Mutex::new(RegInner {
                sessions: HashMap::new(),
                epoch: 0,
            }),
            cv: Condvar::new(),
        })
    }

    /// SessionStart greet / UserPromptSubmit mark-active for a Grok session.
    pub(crate) fn nudge(&self, session: &str) {
        if session.trim().is_empty() {
            return;
        }
        let mut g = self.inner.lock().unwrap();
        g.sessions.insert(session.to_string(), Instant::now());
        g.epoch += 1;
        self.cv.notify_all();
    }

    /// SessionEnd: stop tailing this id.
    pub(crate) fn forget(&self, session: &str) {
        let mut g = self.inner.lock().unwrap();
        if g.sessions.remove(session).is_some() {
            g.epoch += 1;
            self.cv.notify_all();
        }
    }

    fn snapshot(&self) -> (Vec<String>, u64) {
        let g = self.inner.lock().unwrap();
        (g.sessions.keys().cloned().collect(), g.epoch)
    }

    fn wait_change(&self, seen: u64, timeout: Duration) -> u64 {
        let g = self.inner.lock().unwrap();
        let (g, _) = self
            .cv
            .wait_timeout_while(g, timeout, |g| g.epoch == seen)
            .unwrap();
        g.epoch
    }

    fn prune_older_than(&self, ttl: Duration) {
        let mut g = self.inner.lock().unwrap();
        let before = g.sessions.len();
        g.sessions.retain(|_, at| at.elapsed() <= ttl);
        if g.sessions.len() != before {
            g.epoch += 1;
            self.cv.notify_all();
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.inner.lock().unwrap().sessions.len()
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, session: &str) -> bool {
        self.inner.lock().unwrap().sessions.contains_key(session)
    }
}

// ── Delta coalescing (mirror codex_stream) ───────────────────────────────────

/// Per-(session, key) delta buffer: flush on newline or ~150 ms age.
struct Coalescer {
    bufs: HashMap<(String, String), ItemBuf>,
}

struct ItemBuf {
    pending: String,
    seq: u64,
    last_batch: Instant,
}

impl Coalescer {
    fn new() -> Self {
        Coalescer {
            bufs: HashMap::new(),
        }
    }

    fn on_delta(
        &mut self,
        session: &str,
        key: &str,
        delta: &str,
        now: Instant,
    ) -> Option<(String, StreamBatch)> {
        let buf = self
            .bufs
            .entry((session.to_string(), key.to_string()))
            .or_insert_with(|| ItemBuf {
                pending: String::new(),
                seq: 0,
                last_batch: now,
            });
        buf.pending.push_str(delta);
        if buf.pending.contains('\n') {
            return Self::flush_one(session, key, buf, now);
        }
        None
    }

    fn flush_aged(&mut self, now: Instant, max_age: Duration) -> Vec<(String, StreamBatch)> {
        let mut out = Vec::new();
        let keys: Vec<(String, String)> = self
            .bufs
            .iter()
            .filter(|(_, buf)| {
                !buf.pending.is_empty() && now.duration_since(buf.last_batch) >= max_age
            })
            .map(|(k, _)| k.clone())
            .collect();
        for (sess, key) in keys {
            if let Some(buf) = self.bufs.get_mut(&(sess.clone(), key.clone()))
                && let Some(b) = Self::flush_one(&sess, &key, buf, now)
            {
                out.push(b);
            }
        }
        out
    }

    fn flush_one(
        session: &str,
        key: &str,
        buf: &mut ItemBuf,
        now: Instant,
    ) -> Option<(String, StreamBatch)> {
        if buf.pending.is_empty() {
            return None;
        }
        let text = std::mem::take(&mut buf.pending);
        let seq = buf.seq;
        buf.seq += 1;
        buf.last_batch = now;
        Some((
            session.to_string(),
            StreamBatch {
                key: key.to_string(),
                payload: BatchPayload::Delta {
                    index: Some(seq),
                    text,
                },
                is_final: false,
            },
        ))
    }

    fn drop_session(&mut self, session: &str) {
        self.bufs.retain(|(sess, _), _| sess != session);
    }
}

// ── Supervisor ───────────────────────────────────────────────────────────────

const TICK: Duration = Duration::from_millis(120);
const FLUSH_AGE: Duration = Duration::from_millis(150);
const IDLE_TTL: Duration = Duration::from_secs(12 * 3600);

struct Attached {
    tail: JsonlTail,
    /// Path last resolved (re-resolve if missing / session dir moves).
    path: PathBuf,
}

/// Spawn the Grok file-tail supervisor. Parks while `grok_stream` is off, `~/.grok`
/// is absent, or no Grok session is registered.
pub(crate) fn spawn_supervisor(
    paths: Paths,
    running: Arc<AtomicBool>,
    registry: Arc<SessionRegistry>,
    mic: ds_platform::MicState,
    ttsq: Arc<crate::ttsq::TtsQueue>,
) {
    std::thread::Builder::new()
        .name("ds-grok-stream".into())
        .spawn(move || {
            let mic_active = move || mic.is_active();
            let mut speak = move |session: &str, utterance: &NarrationUtterance| {
                ttsq.enqueue_narration(
                    utterance.text.clone(),
                    ds_config::ClientSource::Grok,
                    Some(session.to_string()),
                    Some(utterance.id.clone()),
                )
            };
            supervise(&paths, &running, &registry, &mic_active, &mut speak);
        })
        .ok();
}

fn supervise(
    paths: &Paths,
    running: &AtomicBool,
    registry: &SessionRegistry,
    mic_active: &dyn Fn() -> bool,
    speak: &mut dyn FnMut(&str, &NarrationUtterance) -> Result<(), String>,
) {
    let mut attached: HashMap<String, Attached> = HashMap::new();
    let mut coalescer = Coalescer::new();

    while running.load(Ordering::Relaxed) {
        let cfg = VoiceConfig::load(paths);
        registry.prune_older_than(IDLE_TTL);
        let (sessions, epoch) = registry.snapshot();

        let park = !cfg.grok_stream || !paths.grok_dir.exists() || sessions.is_empty();
        if park {
            // Drop tails while parked so a later re-enable seeks EOF fresh.
            // Keep witness files so Stop stays silent across a brief config flap.
            for (session, _) in attached.drain() {
                coalescer.drop_session(&session);
            }
            let _ = registry.wait_change(epoch, Duration::from_secs(2));
            continue;
        }

        // Forget tails for sessions that left the registry (SessionEnd / idle prune).
        let wanted: std::collections::HashSet<&str> = sessions.iter().map(String::as_str).collect();
        attached.retain(|session, _| {
            if wanted.contains(session.as_str()) {
                true
            } else {
                coalescer.drop_session(session);
                false
            }
        });

        // Attach newly registered sessions (seek EOF + seed witness once).
        // File may appear after the first nudge — re-try each tick.
        for session in &sessions {
            if attached.contains_key(session) {
                continue;
            }
            let Some(path) = ds_config::resolve_grok_updates_jsonl(paths, session, None) else {
                continue;
            };
            match JsonlTail::attach_at_eof(path.clone()) {
                Ok(tail) => {
                    ds_narrate::seed_witness(paths, session);
                    log::info!(
                        target: "engine",
                        "grok-stream: attached to session {session} ({}) client=grok",
                        path.display()
                    );
                    attached.insert(session.clone(), Attached { tail, path });
                }
                Err(e) => {
                    log::debug!(target: "grok_stream", "attach failed for {session}: {e}");
                }
            }
        }

        // Poll each attached file.
        let now = Instant::now();
        let mut reattach: Vec<String> = Vec::new();
        for (session, att) in attached.iter_mut() {
            // If the path vanished, mark for re-resolve next tick.
            if !att.path.is_file() {
                reattach.push(session.clone());
                continue;
            }
            match att.tail.poll_lines() {
                Ok(lines) => {
                    for line in lines {
                        if let Some(chunk) = parse_agent_text_chunk(&line) {
                            let key = batch_key(&chunk, session);
                            if let Some((sess, batch)) =
                                coalescer.on_delta(session, &key, &chunk.text, now)
                            {
                                flush(paths, &cfg, mic_active, speak, &sess, &batch);
                            }
                        }
                    }
                }
                Err(e) => {
                    log::debug!(target: "grok_stream", "tail {session}: {e}");
                    reattach.push(session.clone());
                }
            }
        }
        for session in reattach {
            if let Some(att) = attached.remove(&session) {
                coalescer.drop_session(&session);
                let _ = att;
            }
        }

        for (sess, batch) in coalescer.flush_aged(Instant::now(), FLUSH_AGE) {
            flush(paths, &cfg, mic_active, speak, &sess, &batch);
        }

        for session in attached.keys() {
            if let Err(error) =
                ds_narrate::retry_pending(paths, session, |utterance| speak(session, utterance))
            {
                log::debug!(target: "grok_stream", "pending narration still blocked: {error}");
            }
        }

        let _ = registry.wait_change(epoch, TICK);
    }
}

fn flush(
    paths: &Paths,
    cfg: &VoiceConfig,
    mic_active: &dyn Fn() -> bool,
    speak: &mut dyn FnMut(&str, &NarrationUtterance) -> Result<(), String>,
    session: &str,
    batch: &StreamBatch,
) {
    let digests_on = cfg.narrates(NarrateKind::Digests);
    let shorts_on = cfg.narrates(NarrateKind::Shorts);
    if !digests_on && !shorts_on {
        return;
    }
    if let Err(error) = ds_narrate::deliver_batch(
        paths,
        session,
        batch,
        mic_active(),
        digests_on,
        shorts_on,
        |utterance| speak(session, utterance),
    ) {
        log::warn!(target: "grok_stream", "narration rejected: {error}");
    }
}

/// Drive one poll cycle of the supervisor logic for tests (no thread).
#[cfg(test)]
fn poll_once_for_test(
    paths: &Paths,
    registry: &SessionRegistry,
    attached: &mut HashMap<String, Attached>,
    coalescer: &mut Coalescer,
    cfg: &VoiceConfig,
    mic_active: &dyn Fn() -> bool,
    speak: &mut dyn FnMut(&str, &NarrationUtterance) -> Result<(), String>,
) {
    registry.prune_older_than(IDLE_TTL);
    let (sessions, _) = registry.snapshot();
    if !cfg.grok_stream || !paths.grok_dir.exists() || sessions.is_empty() {
        for (session, _) in attached.drain() {
            coalescer.drop_session(&session);
        }
        return;
    }
    let wanted: std::collections::HashSet<&str> = sessions.iter().map(String::as_str).collect();
    attached.retain(|session, _| {
        if wanted.contains(session.as_str()) {
            true
        } else {
            coalescer.drop_session(session);
            false
        }
    });
    for session in &sessions {
        if attached.contains_key(session) {
            continue;
        }
        if let Some(path) = ds_config::resolve_grok_updates_jsonl(paths, session, None)
            && let Ok(tail) = JsonlTail::attach_at_eof(path.clone())
        {
            ds_narrate::seed_witness(paths, session);
            attached.insert(session.clone(), Attached { tail, path });
        }
    }
    let now = Instant::now();
    for (session, att) in attached.iter_mut() {
        if let Ok(lines) = att.tail.poll_lines() {
            for line in lines {
                if let Some(chunk) = parse_agent_text_chunk(&line) {
                    let key = batch_key(&chunk, session);
                    if let Some((sess, batch)) = coalescer.on_delta(session, &key, &chunk.text, now)
                    {
                        flush(paths, cfg, mic_active, speak, &sess, &batch);
                    }
                }
            }
        }
    }
    for (sess, batch) in coalescer.flush_aged(Instant::now(), Duration::ZERO) {
        flush(paths, cfg, mic_active, speak, &sess, &batch);
    }
}
