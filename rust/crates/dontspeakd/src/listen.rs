//! Always-listening (hands-free) turn logic — the PURE core (no audio I/O, no
//! platform, no clock). See docs/ALWAYS-LISTENING.md.
//!
//! Three pieces, all unit-tested with synthetic inputs:
//!   * [`Endpointer`] — energy-VAD segmentation: per-frame energy + frame duration
//!     → `SpeechOnset` / `SegmentClosed` events.
//!   * [`match_submit_word`] — does a transcript END with the stopword (on a word
//!     boundary), and what is the content before it.
//!   * [`TurnLogic`] — consumes closed segments + onset + ticks → `SubmitText`/`Cancel`
//!     actions, implementing the stopword + trailing-silence confirmation.
//!
//! The daemon glue (dontspeakd::lib) owns the audio buffer, the Parakeet
//! transcribe call, and the platform paste/Enter — it just feeds these state
//! machines and executes their actions.

/// Default energy gate (RMS of f32 PCM in [-1, 1]). Speech RMS is typically
/// ~0.02–0.1; ambient silence ~0.001–0.005, so ~0.01 separates them with margin.
pub const DEFAULT_ENERGY_THRESHOLD: f32 = 0.01;

/// Root-mean-square energy of a mono f32 frame. 0.0 for an empty frame. PURE.
pub fn frame_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

/// What the endpointer observed on a frame. At most one per frame (onset and
/// close cannot both happen in the same frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointEvent {
    /// Energy crossed the gate after silence — the user started talking.
    SpeechOnset,
    /// `endpoint_silence_ms` of continuous sub-gate silence after speech — the
    /// utterance is over; the daemon transcribes the buffered samples.
    SegmentClosed,
}

/// Energy-VAD segmentation state machine (PURE — fed frame energy + duration).
#[derive(Debug, Clone)]
pub struct Endpointer {
    threshold: f32,
    endpoint_silence_ms: u64,
    in_speech: bool,
    silence_ms: u64,
}

impl Endpointer {
    pub fn new(threshold: f32, endpoint_silence_ms: u64) -> Self {
        Self {
            threshold,
            endpoint_silence_ms,
            in_speech: false,
            silence_ms: 0,
        }
    }

    /// Whether an utterance is currently in progress (the daemon buffers samples
    /// while this is true).
    pub fn in_speech(&self) -> bool {
        self.in_speech
    }

    /// Force back to idle WITHOUT emitting an event — used when the mic is gated
    /// off (TTS playing) so resumed listening starts from a clean silence state.
    pub fn reset(&mut self) {
        self.in_speech = false;
        self.silence_ms = 0;
    }

    /// Advance by one frame of `energy` lasting `frame_ms`. Returns an event on a
    /// speech-onset or segment-close edge, else `None`.
    pub fn step(&mut self, energy: f32, frame_ms: u64) -> Option<EndpointEvent> {
        let voiced = energy >= self.threshold;
        if !self.in_speech {
            if voiced {
                self.in_speech = true;
                self.silence_ms = 0;
                return Some(EndpointEvent::SpeechOnset);
            }
            None
        } else if voiced {
            self.silence_ms = 0;
            None
        } else {
            self.silence_ms = self.silence_ms.saturating_add(frame_ms);
            if self.silence_ms >= self.endpoint_silence_ms {
                self.in_speech = false;
                self.silence_ms = 0;
                Some(EndpointEvent::SegmentClosed)
            } else {
                None
            }
        }
    }
}

/// Result of [`match_submit_word`]: whether the stopword is the trailing token(s)
/// of the transcript, and the content with that suffix removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitMatch {
    pub matched: bool,
    /// The transcript with the trailing stopword removed (trimmed). Equals the
    /// trimmed input when `!matched`.
    pub content_before: String,
}

/// Split a string into (byte-start, normalized) tokens: maximal non-whitespace
/// runs, each lowercased and stripped of leading/trailing non-alphanumerics
/// (so "submit." and "Submit" both normalize to "submit"). Empty normalized
/// tokens (pure punctuation) keep their position but compare as "".
fn tokens(s: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            if let Some(st) = start.take() {
                out.push((st, normalize_token(&s[st..i])));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(st) = start {
        out.push((st, normalize_token(&s[st..])));
    }
    out
}

fn normalize_token(t: &str) -> String {
    t.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Does `transcript` end with `word` (which may be multiple words) on a token
/// boundary? Case-insensitive, punctuation-tolerant. The match must be the FINAL
/// token(s): "submit the report" does NOT match "submit" (it is not final), while
/// "okay, submit" and "okay submit." do. Returns the content before the matched
/// suffix (trimmed). PURE.
pub fn match_submit_word(transcript: &str, word: &str) -> SubmitMatch {
    let trimmed = transcript.trim();
    let word_tokens: Vec<String> = word
        .split_whitespace()
        .map(normalize_token)
        .filter(|t| !t.is_empty())
        .collect();
    let no_match = || SubmitMatch {
        matched: false,
        content_before: trimmed.to_string(),
    };
    if word_tokens.is_empty() {
        return no_match();
    }
    let toks = tokens(trimmed);
    if toks.len() < word_tokens.len() {
        return no_match();
    }
    let tail = &toks[toks.len() - word_tokens.len()..];
    let suffix_matches = tail
        .iter()
        .zip(&word_tokens)
        .all(|((_, got), want)| got == want);
    if !suffix_matches {
        return no_match();
    }
    // Content is everything before the first matched token's byte offset.
    let cut = tail[0].0;
    SubmitMatch {
        matched: true,
        content_before: trimmed[..cut].trim_end().to_string(),
    }
}

/// Levenshtein edit distance (PURE) — for FUZZY start-word matching, since the STT
/// mangles a wake name (e.g. "computer" → "computor" / "computa"). Iterative two-row DP.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Find the START word anywhere in `transcript` (case-insensitive, punctuation- and
/// fuzz-tolerant within Levenshtein ≤ 2, so STT manglings of the wake name still fire)
/// and return the content AFTER it ("hey computer add a button" → "add a button"). `None`
/// when no token matches. PURE.
pub fn match_start_word(transcript: &str, word: &str) -> Option<String> {
    let trimmed = transcript.trim();
    let want = normalize_token(word);
    if want.is_empty() {
        return None;
    }
    let tol = (want.chars().count() / 2).clamp(1, 2);
    let toks = tokens(trimmed);
    for (i, (_, got)) in toks.iter().enumerate() {
        if !got.is_empty() && (*got == want || levenshtein(got, &want) <= tol) {
            let next = i + 1;
            let start = if next < toks.len() {
                toks[next].0
            } else {
                trimmed.len()
            };
            return Some(trimmed[start..].trim().to_string());
        }
    }
    None
}

/// An action the daemon executes when a hands-free turn ends. The pill shows the
/// accumulated text live (`TurnLogic::buffer`); these fire only on a stop word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnAction {
    /// Paste this whole captured text into the prompt, then press Enter ("submit").
    SubmitText(String),
    /// Discard the in-flight capture without pasting ("cancel").
    Cancel,
}

/// Which stop word armed the pending confirm window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingKind {
    Submit,
    Cancel,
}

/// In-flight stop-word confirmation: a "submit"/"cancel" was the final token of a
/// segment; we wait for `confirm_ms` of continued silence before acting on it.
#[derive(Debug, Clone)]
struct Pending {
    kind: PendingKind,
    /// The stop word, re-attached to the buffer if the user keeps talking.
    word: String,
    elapsed_ms: u64,
}

/// Hands-free turn state machine (PURE). Idle until the START word fires (opening the
/// pill); then ACCUMULATES the dictation (shown live) until a "submit" or "cancel" stop
/// word + trailing silence — submit pastes the buffer + Enter, cancel discards it. The
/// start word and the stop word are stripped from the captured text.
#[derive(Debug, Clone)]
pub struct TurnLogic {
    start_word: String,
    submit_word: String,
    cancel_word: String,
    confirm_ms: u64,
    /// True once the start word fired — the pill is open and we're accumulating.
    capturing: bool,
    /// Accumulated dictation (what the pill shows; pasted on submit).
    buffer: String,
    pending: Option<Pending>,
}

impl TurnLogic {
    pub fn new(
        start_word: impl Into<String>,
        submit_word: impl Into<String>,
        cancel_word: impl Into<String>,
        confirm_ms: u64,
    ) -> Self {
        Self {
            start_word: start_word.into(),
            submit_word: submit_word.into(),
            cancel_word: cancel_word.into(),
            confirm_ms,
            capturing: false,
            buffer: String::new(),
            pending: None,
        }
    }

    /// Whether the pill is open (start word fired, awaiting submit/cancel).
    pub fn capturing(&self) -> bool {
        self.capturing
    }

    /// The accumulated dictation so far (what the pill shows).
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    fn append(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if !self.buffer.is_empty() {
            self.buffer.push(' ');
        }
        self.buffer.push_str(text);
    }

    fn reset(&mut self) {
        self.capturing = false;
        self.buffer.clear();
        self.pending = None;
    }

    /// A closed utterance was transcribed. IDLE: fire on the start word and begin
    /// capturing whatever follows it. CAPTURING: append, or arm a submit/cancel when
    /// the utterance ends with that stop word.
    pub fn on_segment(&mut self, transcript: &str) -> Vec<TurnAction> {
        // A segment followed a SpeechOnset, which already cleared a pending stop word.
        self.pending = None;

        let text = transcript.trim();
        if text.is_empty() {
            return vec![];
        }

        if !self.capturing {
            if let Some(rest) = match_start_word(text, &self.start_word) {
                self.capturing = true;
                self.buffer.clear();
                self.append(&rest);
            }
            return vec![];
        }

        let sub = match_submit_word(text, &self.submit_word);
        if sub.matched {
            self.append(&sub.content_before);
            self.pending = Some(Pending {
                kind: PendingKind::Submit,
                word: self.submit_word.clone(),
                elapsed_ms: 0,
            });
            return vec![];
        }
        let can = match_submit_word(text, &self.cancel_word);
        if can.matched {
            self.append(&can.content_before);
            self.pending = Some(Pending {
                kind: PendingKind::Cancel,
                word: self.cancel_word.clone(),
                elapsed_ms: 0,
            });
            return vec![];
        }
        self.append(text);
        vec![]
    }

    /// The user kept talking: a pending stop word was content, not a command —
    /// re-attach it to the buffer and keep capturing.
    pub fn on_speech_onset(&mut self) -> Vec<TurnAction> {
        if let Some(p) = self.pending.take() {
            let word = p.word;
            self.append(&word);
        }
        vec![]
    }

    /// `dt_ms` of silence elapsed. Once a pending stop word's confirm window closes,
    /// submit (paste the buffer + Enter) or cancel (discard), then reset to idle.
    pub fn on_tick(&mut self, dt_ms: u64) -> Vec<TurnAction> {
        let Some(p) = self.pending.as_mut() else {
            return vec![];
        };
        p.elapsed_ms = p.elapsed_ms.saturating_add(dt_ms);
        if p.elapsed_ms < self.confirm_ms {
            return vec![];
        }
        let kind = p.kind;
        let text = self.buffer.trim().to_string();
        self.reset();
        match kind {
            PendingKind::Submit if !text.is_empty() => vec![TurnAction::SubmitText(text)],
            PendingKind::Submit => vec![], // nothing captured → nothing to submit
            PendingKind::Cancel => vec![TurnAction::Cancel],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── frame_rms ────────────────────────────────────────────────────────────
    #[test]
    fn rms_basics() {
        assert_eq!(frame_rms(&[]), 0.0);
        assert_eq!(frame_rms(&[0.0, 0.0]), 0.0);
        // RMS of [0.5,-0.5] = sqrt((0.25+0.25)/2) = 0.5.
        assert!((frame_rms(&[0.5, -0.5]) - 0.5).abs() < 1e-6);
    }

    // ── Endpointer ──────────────────────────────────────────────────────────
    #[test]
    fn endpointer_onset_then_close_after_silence() {
        let mut ep = Endpointer::new(0.01, 700);
        // Silence before speech → nothing.
        assert_eq!(ep.step(0.001, 100), None);
        // First voiced frame → onset.
        assert_eq!(ep.step(0.05, 100), Some(EndpointEvent::SpeechOnset));
        assert!(ep.in_speech());
        // More speech, no events.
        assert_eq!(ep.step(0.04, 100), None);
        // Silence accumulates; closes exactly at the threshold (700 ms = 7×100).
        for _ in 0..6 {
            assert_eq!(ep.step(0.0, 100), None);
        }
        assert_eq!(ep.step(0.0, 100), Some(EndpointEvent::SegmentClosed));
        assert!(!ep.in_speech());
    }

    #[test]
    fn endpointer_brief_pause_does_not_close() {
        let mut ep = Endpointer::new(0.01, 700);
        ep.step(0.05, 100); // onset
        // 300 ms pause, then speech again → silence counter resets, no close.
        for _ in 0..3 {
            assert_eq!(ep.step(0.0, 100), None);
        }
        assert_eq!(ep.step(0.05, 100), None); // re-voiced, still one segment
        // Now a full 700 ms silence closes it.
        for _ in 0..6 {
            ep.step(0.0, 100);
        }
        assert_eq!(ep.step(0.0, 100), Some(EndpointEvent::SegmentClosed));
    }

    #[test]
    fn endpointer_reset_clears_speech() {
        let mut ep = Endpointer::new(0.01, 700);
        ep.step(0.05, 100);
        assert!(ep.in_speech());
        ep.reset();
        assert!(!ep.in_speech());
        // After reset the next voiced frame is a fresh onset.
        assert_eq!(ep.step(0.05, 100), Some(EndpointEvent::SpeechOnset));
    }

    // ── match_submit_word ─────────────────────────────────────────────────────
    #[test]
    fn stopword_final_token_matches() {
        let m = match_submit_word("okay add a login button submit", "submit");
        assert!(m.matched);
        assert_eq!(m.content_before, "okay add a login button");
    }

    #[test]
    fn stopword_tolerates_trailing_punctuation_and_case() {
        let m = match_submit_word("do the thing Submit.", "submit");
        assert!(m.matched);
        assert_eq!(m.content_before, "do the thing");
    }

    #[test]
    fn stopword_midsentence_does_not_match() {
        // The user's example: "submit" is not the final token → never fires.
        let m = match_submit_word("I want to submit the message to a client", "submit");
        assert!(!m.matched);
        assert_eq!(m.content_before, "I want to submit the message to a client");
    }

    #[test]
    fn stopword_not_a_substring_false_positive() {
        // "resubmit" must not match "submit" (token boundary required).
        let m = match_submit_word("please resubmit", "submit");
        assert!(!m.matched);
    }

    #[test]
    fn stopword_only_segment_has_empty_content() {
        let m = match_submit_word("submit", "submit");
        assert!(m.matched);
        assert_eq!(m.content_before, "");
    }

    #[test]
    fn stopword_multiword() {
        let m = match_submit_word("change the title go ahead", "go ahead");
        assert!(m.matched);
        assert_eq!(m.content_before, "change the title");
        // Only the final tokens count.
        let m2 = match_submit_word("go ahead and change the title", "go ahead");
        assert!(!m2.matched);
    }

    #[test]
    fn empty_stopword_never_matches() {
        let m = match_submit_word("anything at all", "");
        assert!(!m.matched);
    }

    // ── match_start_word ───────────────────────────────────────────────────────
    #[test]
    fn start_word_fires_and_returns_content_after() {
        assert_eq!(
            match_start_word("hey computer add a login button", "computer").as_deref(),
            Some("add a login button")
        );
        // Fuzzy: STT manglings of the name still fire (Levenshtein ≤ 2).
        assert_eq!(
            match_start_word("hey computor write the readme", "computer").as_deref(),
            Some("write the readme")
        );
        assert_eq!(
            match_start_word("okay computa commit it", "computer").as_deref(),
            Some("commit it")
        );
        // Wake word at the very end → empty content (capture begins, nothing yet).
        assert_eq!(
            match_start_word("right then, computer", "computer").as_deref(),
            Some("")
        );
        // Absent → None.
        assert!(match_start_word("just some other words", "computer").is_none());
    }

    // ── TurnLogic (hands-free: start → accumulate → submit/cancel) ──────────────
    fn turn() -> TurnLogic {
        TurnLogic::new("computer", "submit", "cancel", 1000)
    }

    #[test]
    fn idle_until_start_word() {
        let mut t = turn();
        // Pre-start chatter is discarded; pill stays closed.
        assert_eq!(t.on_segment("just talking to myself"), vec![]);
        assert!(!t.capturing());
        assert_eq!(t.buffer(), "");
        // Start word opens the pill and captures what follows.
        assert_eq!(t.on_segment("hey computer add a button"), vec![]);
        assert!(t.capturing());
        assert_eq!(t.buffer(), "add a button");
    }

    #[test]
    fn accumulates_then_submits_on_silence() {
        let mut t = turn();
        t.on_segment("computer write the readme");
        t.on_segment("and a changelog"); // appended with a separator
        assert_eq!(t.buffer(), "write the readme and a changelog");
        // "submit" as the final token arms; no action yet.
        assert_eq!(t.on_segment("then commit it submit"), vec![]);
        assert_eq!(
            t.buffer(),
            "write the readme and a changelog then commit it"
        );
        assert_eq!(t.on_tick(500), vec![]); // not enough silence
        assert_eq!(
            t.on_tick(600),
            vec![TurnAction::SubmitText(
                "write the readme and a changelog then commit it".into()
            )]
        );
        // Reset to idle for the next turn.
        assert!(!t.capturing());
        assert_eq!(t.buffer(), "");
    }

    #[test]
    fn cancel_discards_the_buffer() {
        let mut t = turn();
        t.on_segment("computer delete everything");
        assert_eq!(t.on_segment("cancel"), vec![]); // armed
        assert_eq!(t.on_tick(1000), vec![TurnAction::Cancel]);
        assert!(!t.capturing());
        assert_eq!(t.buffer(), "");
    }

    #[test]
    fn speech_during_confirm_reattaches_stop_word() {
        let mut t = turn();
        t.on_segment("computer fix the bug");
        t.on_segment("submit"); // stop word only → armed
        // User keeps talking → "submit" was content, not a command.
        assert_eq!(t.on_speech_onset(), vec![]);
        assert_eq!(t.buffer(), "fix the bug submit");
        t.on_segment("in the parser");
        assert_eq!(t.buffer(), "fix the bug submit in the parser");
        assert!(t.capturing()); // no spurious submit
    }

    #[test]
    fn submit_with_empty_buffer_is_noop() {
        let mut t = turn();
        t.on_segment("computer"); // start, nothing captured
        assert_eq!(t.buffer(), "");
        t.on_segment("submit"); // armed, empty
        assert_eq!(t.on_tick(1000), vec![]); // nothing to submit
        assert!(!t.capturing());
    }
}
