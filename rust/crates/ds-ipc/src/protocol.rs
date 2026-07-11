//! The RPC wire protocol: one JSON [`Request`] per line in, one-or-more JSON
//! [`Response`] lines out. A streaming request (STT test-recognition) emits several
//! non-terminal `Response` lines and ends with a terminal one.
//!
//! Config is carried as a `serde_json::Value` — the `voice` object in the exact
//! shape `settings.json` uses (`ds_config::voice_to_value` / `voice_from_value`),
//! so neither side needs a parallel serializable mirror of `VoiceConfig`.

use ds_client::ClientSource;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A client → engine request. `#[serde(tag = "cmd")]` so each line is a small
/// self-describing object, e.g. `{"cmd":"ping"}` or `{"cmd":"speak","text":"hi"}`.
///
/// ## The `source` field
///
/// Every CLIENT-ORIGINATED request — one the `dontspeak` binary sends on a client's behalf and
/// the engine routes per-session or logs — carries a REQUIRED `source: ClientSource` naming
/// WHICH client it came from: the hook's `--client <token>` verb, or the MCP `initialize`
/// handshake's `clientInfo`. Those seven are [`Request::GreetSession`], [`Request::MarkActive`],
/// [`Request::SessionEnd`], [`Request::StopSpeech`], [`Request::Speak`],
/// [`Request::SpeakNarration`] and [`Request::Earcon`].
///
/// The rest DELIBERATELY do NOT carry it, and that is a decision, not an oversight:
/// [`Request::SetMuted`] is also sent by the app's own tray via `ds-core` (there is no client
/// there at all); the STT/diarization requests (`TestRecognitionStart`/`Stop`, `Diarize`,
/// `Enroll`, `ForgetSpeaker`, `ListSpeakers`) and every app/engine control request
/// (`SetProvider`, `Reload`, `ModelStatus`, `WaitModelStatus`, `EnsureKokoroVoices`,
/// `AuthorizeSystemStt`, `Shutdown`, `Ping`, `Status`) are the APP or the ENGINE talking to
/// itself, not a client. Keeping `source` off them is also what lets `ds-core/src/ffi.rs`
/// compile untouched — it constructs `SetMuted`/`ModelStatus`/`WaitModelStatus`/`SetProvider`
/// and none of the seven.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Liveness/health probe → [`Response::Pong`].
    Ping,
    /// Snapshot of the TTS queue's playback state → [`Response::Status`]. Read-only.
    Status,
    /// Ensure the Kokoro voices npz (~28 MB) is present, downloading it in the background
    /// via the single-flight download manager if absent. Returns immediately — does NOT
    /// wait for the download. → [`Response::Done`].
    EnsureKokoroVoices,
    /// Set global MUTE (the tray checkbox; the Caps-tap toggles it engine-side). Muting
    /// silences playback WITHOUT stopping it — the queue keeps draining. → [`Response::Done`].
    SetMuted { on: bool },
    /// A terminal/session just opened (SessionStart hook). If `greet_on_open` is set,
    /// the engine claims this session's pool voice and speaks a short greeting in it.
    /// No-op when greeting is off. `session` is ambient (the hook's `session_id`).
    /// → [`Response::Done`].
    GreetSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// WHICH client this request came from ([`ds_client::ClientSource`]) — the hook's
        /// `--client` verb, or the MCP `initialize` handshake's `clientInfo`.
        ///
        /// REQUIRED: every client-originated request names its source. No `#[serde(default)]`,
        /// no `Option`, no `skip_serializing_if`. Backward compatibility with a hook binary
        /// that predates the verb is explicitly OUT OF SCOPE — the CLI, the engine and the
        /// wiring ship together and the engine re-wires every client at boot, so there is no
        /// supported skew; a stale hook's line is REJECTED (`bad request: missing field
        /// `source``) rather than silently mis-attributed, which is the point.
        ///
        /// An UNRECOGNISED token still decodes to `ClientSource::Unknown` (fail-open
        /// `Deserialize`, mirroring [`Response::Unknown`]'s `#[serde(other)]`) — that is
        /// FORWARD robustness against a client we have not wired YET, not legacy support. An
        /// ABSENT `source` is a hard decode error, deliberately: see
        /// `request_without_source_is_a_hard_decode_error`, which exists to fail loudly if
        /// anyone ever reaches for `#[serde(default)]` here to make another test go green.
        source: ClientSource,
    },
    /// Mark this session as the ACTIVE terminal — the one you just submitted a prompt
    /// to (UserPromptSubmit hook). The TTS queue then speaks only this session's items
    /// and HOLDS the others (paused, not dropped) until they become active, so
    /// narration follows the terminal you're working in. `session` is ambient (the
    /// hook's `session_id`); absent ⇒ the default/global session. → [`Response::Done`].
    MarkActive {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// True when the hook classified the `UserPromptSubmit` prompt body as a
        /// harness-injected CONTINUATION rather than something a human typed and
        /// submitted — e.g. Claude Code auto-re-invoking the agent with a
        /// `<task-notification>` block after a background task finishes (issue #11).
        /// No human expressed "I've moved on" here, so a synthetic ping registers
        /// session-liveness bookkeeping ONLY: it must NOT claim active-terminal status
        /// and must NOT apply `input_clears`. See
        /// `dontspeak::hook_speak::is_synthetic_continuation` for the classifier that sets
        /// this on the wire.
        ///
        /// `#[serde(default)]` is a WIRE-COMPACTNESS affordance, nothing more: the hook omits
        /// the key when it's `false` (the common case), keeping the line short, and an omitted
        /// key decodes as `false` — the conservative default, since a synthetic ping must never
        /// be *assumed*. It is NOT a backward-compat shim for an old hook build: since `source`
        /// became required, such a build's line is rejected for the missing `source` before
        /// `synthetic` is ever considered, so this default can never fire for a stale client.
        #[serde(default)]
        synthetic: bool,
        /// WHICH client sent this (see [`Request::GreetSession::source`]). REQUIRED.
        source: ClientSource,
    },
    /// Speak `text` as a Reply on the engine's TTS queue (survives a record-barge when
    /// the resume policy is set). Used by the MCP `speak` tool for explicit, model-driven
    /// speech; assistant-reply narration goes through `SpeakNarration` instead.
    /// `voice`/`rate` override config.
    Speak {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rate: Option<f32>,
        /// The Claude session this reply belongs to (ambient; see [`Request::MarkActive`]).
        /// The engine tags the queued item with it so per-session playback routing (active
        /// window, pool voice) resolves correctly.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// WHICH client sent this (see [`Request::GreetSession::source`]). REQUIRED. For the
        /// MCP `speak` tool this is the `initialize` handshake's mapped `clientInfo.name`.
        source: ClientSource,
    },
    /// Enqueue `text` as mid-turn NARRATION on the engine's TTS queue (dropped
    /// first on a record-barge / skip-ahead). The engine splits it into
    /// sentences and plays them on the warm child — replaces the old cold
    /// per-block spawn so there is no model reload between blocks.
    SpeakNarration {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// WHICH client sent this (see [`Request::GreetSession::source`]). REQUIRED — it
        /// rides the wire even though the engine does not LOG this variant (it fires per
        /// blockquote and would spam the activity log).
        source: ClientSource,
    },
    /// Barge-in: stop in-flight speech. `session` scopes it to ONE window (Claude
    /// session): only that session's queued items are dropped, and the playing item
    /// is cancelled only if it belongs to that session — other windows keep talking.
    /// `None` (absent on the wire) is the GLOBAL hard barge: drop the whole queue and
    /// cancel whatever is playing (caps long-press / a non-session CLI caller).
    /// `session` is ambient (see [`Request::MarkActive`]), never a tool argument.
    StopSpeech {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// WHICH client sent this (see [`Request::GreetSession::source`]). REQUIRED.
        source: ClientSource,
    },
    /// A window/terminal closed for good (Claude Code `SessionEnd`). Like a per-window
    /// [`StopSpeech`](Request::StopSpeech) (drop this session's queued + in-flight speech),
    /// but ALSO reclaims the session's transient voice state — its preferred-pool
    /// assignment — so that map doesn't grow one entry per session for the engine's
    /// lifetime. `None` (no session id) is the global hard barge, same as
    /// `StopSpeech { None }`, and forgets nothing session-scoped.
    SessionEnd {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// WHICH client sent this (see [`Request::GreetSession::source`]). REQUIRED.
        source: ClientSource,
    },
    /// Start a live Parakeet "test recognition" session. The engine streams
    /// [`Response::Listening`], ending with [`Response::Transcript`] (terminal)
    /// when the session stops.
    TestRecognitionStart,
    /// Stop the active test-recognition session (sent on a SECOND connection,
    /// since the first is busy streaming). The session then runs its final pass
    /// and emits its terminal `Transcript` on the streaming connection.
    TestRecognitionStop,
    /// One-shot speaker diarization: record the mic for `seconds`, then return who
    /// spoke when. Unlike test-recognition this is NOT streamed — the engine records
    /// a fixed window on the warm helper, runs the diarizer, and replies with a single
    /// terminal [`Response::Diarization`]. On-demand (the `diarize` MCP tool).
    Diarize { seconds: u64 },
    /// Enroll a voiceprint: record the mic for `seconds`, extract a WeSpeaker embedding,
    /// and persist it under `name` so future [`Diarize`](Request::Diarize) labels that
    /// person by name. Replies [`Response::Enrolled`]. (the `manage_speakers` enroll action).
    Enroll { name: String, seconds: u64 },
    /// Remove an enrolled voiceprint by name → [`Response::Done`] (no-op if absent).
    ForgetSpeaker { name: String },
    /// List enrolled speaker names → [`Response::Speakers`].
    ListSpeakers,
    /// Ask the engine for model presence + removability. The engine is the
    /// authority because it knows what it has loaded: a model is `removable` only
    /// if present AND not currently running in the engine (e.g. the warm Kokoro
    /// child). Download/delete file IO stays in the app. → [`Response::ModelStatus`].
    ModelStatus,
    /// Like [`ModelStatus`](Request::ModelStatus) but BLOCKS until the engine's status
    /// sequence differs from `since`, or `timeout_ms` elapses — then replies with the
    /// current [`Response::ModelStatus`] (whose JSON carries the new `seq`). This is the
    /// PUSH transport for the dictation overlay: the app calls it on a dedicated thread
    /// and re-renders the instant a partial lands, instead of polling on a timer. Pass
    /// `since = 0` for the first call (replies immediately with the current state + seq).
    WaitModelStatus { since: u64, timeout_ms: u64 },
    /// Set the TTS execution provider for THIS session and RESTART the warm Kokoro
    /// child so the new ONNX session uses it; the engine resets its TTS stats so the
    /// app's range bars reflect only the new provider. `provider` = "cpu" | "cuda" |
    /// "coreml" | "ane" | "auto". Transient (not persisted). → [`Response::Done`].
    SetProvider { provider: String },
    /// Ask the engine to exit cleanly over IPC; replies [`Response::Done`] just
    /// before shutting down. NOTE: the engine runs in-process inside the native
    /// app, so the real shutdown on quit is the FFI `ds_engine_stop` (clears
    /// the run flag, joins the thread) — no current client sends this request; the
    /// handler is kept for an out-of-process / socket-driven stop.
    Shutdown,
    /// Apply `settings.json` NOW — the explicit "reload" nudge. The MCP/GUI writes
    /// settings.json (still the source of truth), then sends this so the engine
    /// reloads immediately and surgically via `Engine::reload` instead of waiting
    /// for the mtime poll. Same effect as an mtime-triggered reload; debounced with it.
    /// → [`Response::Done`].
    Reload,
    /// Play an audible EARCON now (fire-and-forget). `event` is `"reply_done"` (the Stop
    /// hook — Claude finished its turn) or `"needs_input"` (the Notification hook — a
    /// permission prompt / idle). The engine resolves the configured-or-introspected sound
    /// and plays it on the warm helper's audio output, honoring the `earcon_enabled` config
    /// and global mute. Unknown/disabled ⇒ silent no-op. → [`Response::Done`].
    Earcon {
        event: String,
        /// WHICH client sent this (see [`Request::GreetSession::source`]). REQUIRED.
        source: ClientSource,
    },
    /// Verify (and, if needed, REQUEST) authorization for the System STT engine
    /// (macOS on-device `SFSpeechRecognizer`). The engine prompts on first use — so the
    /// TCC prompt is attributed to DontSpeak.app — then re-checks on-device capability.
    /// `set_config stt_engine=system` sends this BEFORE persisting, and refuses to enable
    /// (no fallback) when it isn't usable. → [`Response::Done`] when usable, else
    /// [`Response::Error`] with the reason.
    AuthorizeSystemStt,
}

/// An engine → client response line. `#[serde(tag = "ok")]` keeps lines small and
/// unambiguous. Test-recognition emits `Listening`/`Partial` lines then a terminal
/// `Transcript` (or `Error`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "ok", rename_all = "snake_case")]
pub enum Response {
    /// Reply to [`Request::Ping`].
    Pong,
    /// Reply to [`Request::Status`] (TERMINAL): live TTS-queue playback state.
    /// `paused` is set during a record-barge hold; `muted` is the global mute
    /// (the same flag the tray checkbox / Caps-Lock toggle / `SetMuted` drive) —
    /// when true, output keeps queuing but plays SILENTLY.
    Status {
        tts_active: bool,
        queued: usize,
        paused: bool,
        muted: bool,
    },
    /// Generic success terminator for a request that returns no payload.
    Done,
    /// Test recognition: mic open, speak now (non-terminal).
    Listening,
    /// Test recognition: live partial transcript (non-terminal).
    Partial { text: String },
    /// Test recognition: final transcript (TERMINAL).
    Transcript { text: String },
    /// Diarization result (TERMINAL): `segments` is the JSON array
    /// `[{"speaker","start","end","name"?}, ...]` (seconds), in time order; `name` is
    /// the enrolled person a cluster matched, when present.
    Diarization { segments: Value },
    /// Enrollment succeeded (TERMINAL): echoes the enrolled `name`.
    Enrolled { name: String },
    /// Enrolled-speaker names (TERMINAL).
    Speakers { names: Vec<String> },
    /// Model presence + removability + per-subsystem running state (TERMINAL).
    /// `status` is a JSON object:
    /// `{ "kokoro": {"present":bool,"removable":bool}, "onnx": {...},
    ///    "parakeet": {"present":bool,"removable":bool},
    ///    "running": {"caps":bool,"kokoro":bool,"parakeet":bool} }`.
    ModelStatus { status: Value },
    /// Terminal error for any request.
    Error { message: String },
    /// Forward-compat fallback for version skew: chosen when the `ok` tag on
    /// the wire doesn't match any variant this build knows about (e.g. an
    /// older CLI/client talking to a newer daemon that has grown a new
    /// response). Without this, an unrecognized tag hard-errors
    /// [`crate::client::Client::recv`], which can make a caller abandon a
    /// streaming session (e.g. never sending `TestRecognitionStop`) instead of
    /// reacting to a well-formed-but-unknown terminal value. Treated as
    /// TERMINAL (see [`Response::is_terminal`]) for the same reason: it stops
    /// the read loop deterministically via `Ok`, so normal cleanup on the
    /// caller side still runs, rather than looping forever waiting for a
    /// variant that will never come. Never constructed deliberately by this
    /// crate's own encoder — `#[serde(other)]` only engages on decode.
    #[serde(other)]
    Unknown,
}

impl Response {
    /// Convenience constructor for an error terminator.
    pub fn error(msg: impl Into<String>) -> Self {
        Response::Error {
            message: msg.into(),
        }
    }

    /// Is this a terminal line (client may stop reading)? `Listening`/`Partial`
    /// are STREAMING (non-terminal); `Transcript` ends a recognition session.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Response::Pong
                | Response::Status { .. }
                | Response::Done
                | Response::Transcript { .. }
                | Response::Diarization { .. }
                | Response::Enrolled { .. }
                | Response::Speakers { .. }
                | Response::ModelStatus { .. }
                | Response::Error { .. }
                | Response::Unknown
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrips_through_json_lines() {
        let cases = [
            Request::Ping,
            Request::Status,
            Request::Diarize { seconds: 10 },
            Request::Enroll {
                name: "Alex".into(),
                seconds: 15,
            },
            Request::ForgetSpeaker {
                name: "Alex".into(),
            },
            Request::ListSpeakers,
            Request::Speak {
                text: "hello".into(),
                voice: Some("af_sarah".into()),
                rate: Some(1.5),
                session: Some("sess-1".into()),
                source: ClientSource::ClaudeCode,
            },
            Request::SpeakNarration {
                text: "working on it".into(),
                session: None,
                source: ClientSource::QwenCode,
            },
            Request::StopSpeech {
                session: None,
                source: ClientSource::Unknown,
            },
            Request::StopSpeech {
                session: Some("sess-1".into()),
                source: ClientSource::ClaudeCode,
            },
            Request::MarkActive {
                session: Some("sess-1".into()),
                synthetic: false,
                source: ClientSource::Codex,
            },
            Request::MarkActive {
                session: None,
                synthetic: true,
                source: ClientSource::ClaudeCode,
            },
            // The two client-originated variants the case list never covered before; all seven
            // that gained a required `source` are now here.
            Request::GreetSession {
                session: Some("sess-1".into()),
                source: ClientSource::Grok,
            },
            Request::SessionEnd {
                session: Some("sess-1".into()),
                source: ClientSource::QwenCode,
            },
            Request::TestRecognitionStart,
            Request::ModelStatus,
            Request::SetProvider {
                provider: "coreml".into(),
            },
            Request::AuthorizeSystemStt,
            Request::Earcon {
                event: "reply_done".into(),
                source: ClientSource::Grok,
            },
            Request::Shutdown,
        ];
        for req in cases {
            let line = serde_json::to_string(&req).unwrap();
            assert!(!line.contains('\n'), "a request must be a single line");
            let back: Request = serde_json::from_str(&line).unwrap();
            // Re-serializing the parsed value must be byte-identical.
            assert_eq!(serde_json::to_string(&back).unwrap(), line);
        }
    }

    #[test]
    fn ping_uses_the_compact_tagged_form() {
        assert_eq!(
            serde_json::to_string(&Request::Ping).unwrap(),
            r#"{"cmd":"ping"}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::Pong).unwrap(),
            r#"{"ok":"pong"}"#
        );
    }

    #[test]
    fn terminal_classification() {
        assert!(Response::Pong.is_terminal());
        assert!(
            Response::Status {
                tts_active: false,
                queued: 0,
                paused: false,
                muted: false,
            }
            .is_terminal()
        );
        assert!(Response::Done.is_terminal());
        assert!(Response::error("x").is_terminal());
        assert!(
            Response::ModelStatus {
                status: serde_json::Value::Null
            }
            .is_terminal()
        );
        assert!(!Response::Listening.is_terminal());
        assert!(!Response::Partial { text: "x".into() }.is_terminal());
    }

    /// Conservative-default guard (issue #11): the hook OMITS `synthetic` when it's false (the
    /// common case, keeping the line short), so an absent key must decode as `false` — a
    /// synthetic ping is never *assumed*. This is about wire compactness, NOT about an old hook
    /// build: since `source` became required, a stale hook's line is rejected for the missing
    /// `source` long before `synthetic` is considered (see
    /// `request_without_source_is_a_hard_decode_error`), which is why the line below carries one.
    #[test]
    fn mark_active_synthetic_defaults_to_false_when_absent_on_the_wire() {
        let line = r#"{"cmd":"mark_active","session":"sess-1","source":"claude_code"}"#;
        let req: Request = serde_json::from_str(line).unwrap();
        assert!(matches!(
            req,
            Request::MarkActive {
                session: Some(ref s),
                synthetic: false,
                source: ClientSource::ClaudeCode,
            } if s == "sess-1"
        ));
    }

    /// GUARD AGAINST A `#[serde(default)]` REGRESSION. `source` is REQUIRED on every
    /// client-originated request: a line that omits it must be a HARD DECODE ERROR, not a
    /// silent fallback to `Unknown`/`ClaudeCode` — the engine rejects the request outright
    /// rather than mis-attributing it. If someone adds `#[serde(default)]` to `source` to make
    /// some other test go green, THIS test is what fails and tells them not to.
    ///
    /// What that rejection is (and is NOT) visible as, since the whole "the three pieces
    /// deploy together" story leans on it: the engine replies `bad request: missing field
    /// `source`` on the socket AND writes a WARN to the activity log naming the rejected `cmd`
    /// (`ds_ipc::serve`'s `on_bad_request` sink, wired in `dontspeakd::ipc`). It is NOT visible
    /// at the terminal: every hook call site discards the reply and exits 0, so a stale CLI
    /// against a rebuilt engine drops the voice loop with nothing on screen — the activity-log
    /// WARN is the only place it surfaces. Deploy the CLI and the engine together
    /// (docs/BUILD-DEPLOY.md).
    ///
    /// Note the asymmetry, and it is deliberate: an unrecognised TOKEN fails OPEN to `Unknown`
    /// (forward robustness — see `unknown_client_token_decodes_to_unknown`), but an ABSENT
    /// FIELD fails CLOSED.
    #[test]
    fn request_without_source_is_a_hard_decode_error() {
        // Every one of the seven client-originated variants, each with a valid line for its
        // OTHER fields and `source` omitted.
        let cases = [
            r#"{"cmd":"greet_session"}"#,
            r#"{"cmd":"mark_active","session":"sess-1"}"#,
            r#"{"cmd":"session_end","session":"sess-1"}"#,
            r#"{"cmd":"stop_speech"}"#,
            r#"{"cmd":"speak","text":"hi"}"#,
            r#"{"cmd":"speak_narration","text":"hi"}"#,
            r#"{"cmd":"earcon","event":"reply_done"}"#,
        ];
        for line in cases {
            let err = serde_json::from_str::<Request>(line)
                .expect_err("a client-originated request with no `source` must NOT decode");
            assert!(
                err.to_string().contains("source"),
                "the error must name the missing field for {line}, got: {err}"
            );
        }
    }

    /// FORWARD-SKEW robustness (NOT backward compat): a client we have not wired YET sends a
    /// `source` token this build doesn't know. It must decode to `ClientSource::Unknown` rather
    /// than hard-erroring the whole line — the same idiom as `Response::Unknown`'s
    /// `#[serde(other)]`. Read this together with
    /// `request_without_source_is_a_hard_decode_error`: an unrecognised TOKEN fails open, an
    /// ABSENT FIELD does not.
    #[test]
    fn unknown_client_token_decodes_to_unknown() {
        let req: Request = serde_json::from_str(r#"{"cmd":"greet_session","source":"gemini_cli"}"#)
            .expect("an unrecognised client token must decode to Unknown, not error out");
        assert!(matches!(
            req,
            Request::GreetSession {
                source: ClientSource::Unknown,
                ..
            }
        ));
        // …and a KNOWN token still lands on its client (the positive half of the same decode).
        let req: Request =
            serde_json::from_str(r#"{"cmd":"earcon","event":"reply_done","source":"codex"}"#)
                .unwrap();
        assert!(matches!(
            req,
            Request::Earcon {
                source: ClientSource::Codex,
                ..
            }
        ));
    }

    /// Version-skew regression guard: a future response variant this build
    /// doesn't know about must deserialize into `Response::Unknown` (and be
    /// terminal), not fail with a hard parse error. See `Response::Unknown`'s
    /// doc comment for why a hard error here is dangerous (it made
    /// `Client::recv` abort a streaming session without cleanup).
    #[test]
    fn unrecognized_response_tag_falls_back_to_unknown_instead_of_erroring() {
        let future_line = r#"{"ok":"some_future_variant","extra":"field","n":42}"#;
        let resp: Response = serde_json::from_str(future_line)
            .expect("an unrecognized `ok` tag must decode to Response::Unknown, not error out");
        assert!(matches!(resp, Response::Unknown));
        assert!(
            resp.is_terminal(),
            "Unknown must be terminal so callers don't spin forever"
        );
    }
}
