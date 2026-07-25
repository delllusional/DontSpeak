//! UserPromptSubmit `provide`: inject narration context when digests are on.

use ds_config::WiredAgent;
use ds_ipc::{Request, Response};
use serde_json::{Value, json};

/// Pushed when narration is on AND the engine is muted: the one voice signal the model can't
/// infer. Narrator speaks blockquotes, but muted playback is silent and no tool sits on that
/// path to report it — without this the model narrates into a void. Injected as context
/// (not spoken), so it survives the mute it warns about.
const MUTED_NOTICE: &str = "\n\n## Voice state\nMUTED: speech and narration play silently. \
    Put anything important in text until unmuted.";

/// Desktop's ordinary text surface must never receive the visible blockquote contract.
/// Its hook child carries this exact origin marker; the CLI does not and retains streaming
/// digests. Keep the comparison exact so unrelated Codex environments are unaffected.
const CODEX_DESKTOP_ORIGIN: &str = "Codex Desktop";

pub(crate) fn is_codex_desktop(client: WiredAgent) -> bool {
    is_codex_desktop_with(client, |name| std::env::var(name).ok())
}

fn is_codex_desktop_with(client: WiredAgent, get: impl FnOnce(&str) -> Option<String>) -> bool {
    client == WiredAgent::Codex
        && get("CODEX_INTERNAL_ORIGINATOR_OVERRIDE")
            .is_some_and(|origin| origin.trim() == CODEX_DESKTOP_ORIGIN)
}

/// When digests are ON, return the narration spec. Claude-shape clients get
/// `hookSpecificOutput.additionalContext`; Hermes wants flat `{"context":…}`.
/// `None` when digests off (no instruction tokens). Also folds a best-effort mute notice
/// from the engine — read-only; down/unreachable engine omits the notice, never blocks.
pub(crate) fn narration_context(client: WiredAgent) -> Option<Value> {
    if is_codex_desktop(client) {
        return None;
    }
    let paths = ds_config::Paths::resolve()?;
    if !ds_config::VoiceConfig::load(&paths).narrates(ds_config::NarrateKind::Digests) {
        return None;
    }
    let spec = ds_config::DEFAULT_NARRATION_SPEC.to_string();
    let context = with_voice_state(spec, engine_muted(&paths));
    Some(provide_shape(client, context))
}

/// Pure dual-shape for unit tests (no engine / Paths).
fn provide_shape(client: WiredAgent, context: String) -> Value {
    if client == WiredAgent::Hermes {
        json!({ "context": context })
    } else {
        json!({
            "hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": context,
            }
        })
    }
}

/// Read-only mute probe. `false` when engine down/unreachable — omit notice, don't block.
fn engine_muted(paths: &ds_config::Paths) -> bool {
    matches!(
        ds_ipc::request(&paths.engine_sock, &Request::ModelStatus),
        Ok(Response::ModelStatus { status })
            if status.pointer("/activity/muted") == Some(&Value::Bool(true))
    )
}

/// Pure: fold mute notice into the narration spec (unit-testable without a running engine).
fn with_voice_state(spec: String, muted: bool) -> String {
    if muted { spec + MUTED_NOTICE } else { spec }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmuted_leaves_spec_untouched() {
        let spec = "SPEC".to_string();
        assert_eq!(with_voice_state(spec.clone(), false), spec);
    }

    #[test]
    fn muted_appends_notice_once() {
        let out = with_voice_state("SPEC".to_string(), true);
        assert!(out.starts_with("SPEC"), "spec is preserved verbatim");
        assert!(out.contains("MUTED"), "notice warns about the mute");
        assert_eq!(
            out.matches("## Voice state").count(),
            1,
            "exactly one notice"
        );
    }

    #[test]
    fn hermes_provide_is_flat_context_others_keep_hook_specific_output() {
        let hermes = provide_shape(WiredAgent::Hermes, "SPEC".into());
        assert_eq!(hermes["context"], "SPEC");
        assert!(hermes.get("hookSpecificOutput").is_none());

        let claude = provide_shape(WiredAgent::ClaudeCode, "SPEC".into());
        assert_eq!(claude["hookSpecificOutput"]["additionalContext"], "SPEC");
        assert!(claude.get("context").is_none());
    }

    #[test]
    fn codex_desktop_suppresses_only_the_model_facing_digest_instruction() {
        assert!(is_codex_desktop_with(WiredAgent::Codex, |name| {
            (name == "CODEX_INTERNAL_ORIGINATOR_OVERRIDE").then(|| CODEX_DESKTOP_ORIGIN.to_string())
        }));
        assert!(!is_codex_desktop_with(WiredAgent::Codex, |_| None));
        assert!(!is_codex_desktop_with(WiredAgent::ClaudeCode, |_| {
            Some(CODEX_DESKTOP_ORIGIN.to_string())
        }));
    }
}
