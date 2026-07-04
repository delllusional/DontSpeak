//! The `prompt-context` subcommand — the Claude Code UserPromptSubmit hook. Lives
//! beside the other `hook_*` modules; invoked from the front-door dispatch in `main`.

use ds_ipc::{Request, Response};
use serde_json::{Value, json};

/// Pushed to the model when narration is on AND the engine is muted: the ONE voice signal
/// it can't otherwise infer. The narrator speaks the model's blockquote, but while muted that
/// playback is SILENT — and no tool call sits on the speaking path to report it — so without
/// this the model narrates into a void and the user silently misses the reply. Injected as
/// context (not spoken), so it survives the mute it is warning about.
const MUTED_NOTICE: &str = "\n\n## Voice state\nThe app is currently MUTED: your spoken reply \
    and the narrator both play SILENTLY right now — the user will NOT hear them. Put anything \
    important in your TEXT response (not only in the spoken blockquote) until they unmute.";

/// The narration-context QUERY (UserPromptSubmit `provide`): when "digests" narration is ON,
/// return the narration spec as `hookSpecificOutput.additionalContext` so Claude leads every
/// reply with spoken-line blockquotes the narrator reads verbatim. The spec is the built-in
/// [`ds_config::DEFAULT_NARRATION_SPEC`]; an optional `narration-spec.md` on disk overrides
/// it (empty file falls back to the default). `None` when "digests" is off → no blockquote
/// (silence, no wasted tokens). Re-reads config + file every call, so edits take effect next prompt.
///
/// When narration is on we ALSO fold in a live voice-state notice (currently: muted) probed
/// from the engine — a PUSH of the one signal the model can't infer, since the speaking path
/// never calls a tool that could report it. The probe is read-only and best-effort: a down or
/// unreachable engine just omits the notice (never blocks the prompt).
///
/// The stdio `provide` subcommand prints this JSON to stdout.
pub(crate) fn narration_context() -> Option<Value> {
    let paths = ds_config::Paths::resolve()?;
    if !ds_config::VoiceConfig::load(&paths).narrates(ds_config::NarrateKind::Digests) {
        return None; // "digests" off → inject nothing so Claude stops emitting the blockquote
    }
    // Built-in default unless a non-empty override file exists.
    let spec = std::fs::read_to_string(&paths.narration_spec)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| ds_config::DEFAULT_NARRATION_SPEC.to_string());
    let context = with_voice_state(spec, engine_muted(&paths));
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": context,
        }
    }))
}

/// Read-only probe of the engine's global mute flag. `false` whenever the engine is down or
/// unreachable (no socket, timeout, unexpected reply) — we omit the notice rather than block
/// the prompt on a missing engine.
fn engine_muted(paths: &ds_config::Paths) -> bool {
    matches!(
        ds_ipc::request(&paths.engine_sock, &Request::Status),
        Ok(Response::Status { muted: true, .. })
    )
}

/// PURE: fold the live voice-state notice into the narration spec. Split from the engine
/// probe so the formatting is unit-testable without a running engine.
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
}
