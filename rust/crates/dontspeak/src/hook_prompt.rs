//! The `prompt-context` subcommand — the Claude Code UserPromptSubmit hook. Lives
//! beside the other `hook_*` modules; invoked from the front-door dispatch in `main`.

use serde_json::{Value, json};

/// The narration-context QUERY (UserPromptSubmit `provide`): when "digests" narration is ON,
/// return the narration spec as `hookSpecificOutput.additionalContext` so Claude leads every
/// reply with spoken-line blockquotes the narrator reads verbatim. The spec is the built-in
/// [`ds_config::DEFAULT_NARRATION_SPEC`]; an optional `narration-spec.md` on disk overrides
/// it (empty file falls back to the default). `None` when "digests" is off → no blockquote
/// (silence, no wasted tokens). Re-reads config + file every call, so edits take effect next prompt.
///
/// The stdio `provide` subcommand prints this JSON to stdout.
pub(crate) fn narration_context() -> Option<Value> {
    let paths = ds_config::Paths::resolve()?;
    if !ds_config::VoiceConfig::load(&paths).narrates(ds_config::NarrateKind::Digests)
    {
        return None; // "digests" off → inject nothing so Claude stops emitting the blockquote
    }
    // Built-in default unless a non-empty override file exists.
    let spec = std::fs::read_to_string(&paths.narration_spec)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| ds_config::DEFAULT_NARRATION_SPEC.to_string());
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": spec,
        }
    }))
}
