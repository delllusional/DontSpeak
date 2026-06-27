//! Claude Code's voice-dictation config, READ from its own files — never written.

use serde_json::Value;

use crate::Paths;

/// Claude Code's voice-dictation config, READ from its own files — never written.
/// The `claude_code` STT engine uses it to (a) synthesize the right key and (b) report
/// status (is CC voice on? which key?). All fields fail-open to Claude Code's documented
/// defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCodeVoice {
    /// `voice.enabled` in Claude Code's settings.json — is dictation turned on?
    pub enabled: bool,
    /// `voice.mode` — hold/tap; defaults to "hold" (Claude Code's default) when absent.
    pub mode: String,
    /// The key bound to `voice:pushToTalk` in keybindings.json, or "space" (Claude Code's
    /// default) when unbound. The verbatim CC token, e.g. "ctrl+g", "space", "meta+k".
    pub key: String,
}

impl Default for ClaudeCodeVoice {
    fn default() -> Self {
        ClaudeCodeVoice {
            enabled: false,
            mode: "hold".into(),
            key: "space".into(),
        }
    }
}

/// Read Claude Code's `voice` settings (`settings.json`) + the `voice:pushToTalk`
/// keybinding (`keybindings.json`). READ-ONLY. Fail-open: missing/garbage files yield the
/// defaults (voice off, "hold", "space"). `keybindings.json` is a SPARSE override file, so
/// an absent binding means the default `space` — exactly Claude Code's own semantics.
pub fn read_claude_code_voice(paths: &Paths) -> ClaudeCodeVoice {
    let mut v = ClaudeCodeVoice::default();
    // voice.{enabled,mode} from settings.json.
    if let Ok(text) = std::fs::read_to_string(&paths.settings_json)
        && let Ok(root) = serde_json::from_str::<Value>(&text)
        && let Some(voice) = root.get("voice").and_then(Value::as_object)
    {
        if let Some(b) = voice.get("enabled").and_then(Value::as_bool) {
            v.enabled = b;
        }
        if let Some(m) = voice.get("mode").and_then(Value::as_str) {
            v.mode = m.to_string();
        }
    }
    // The key bound to "voice:pushToTalk" (Chat context) in keybindings.json. Scan every
    // binding block; the last mapping wins. Absent ⇒ keep the default "space".
    if let Ok(text) = std::fs::read_to_string(&paths.keybindings_json)
        && let Ok(root) = serde_json::from_str::<Value>(&text)
        && let Some(blocks) = root.get("bindings").and_then(Value::as_array)
    {
        for block in blocks {
            // voice:pushToTalk lives in the Chat context; be lenient if it's omitted.
            let chat = block
                .get("context")
                .and_then(Value::as_str)
                .map(|c| c == "Chat")
                .unwrap_or(true);
            if !chat {
                continue;
            }
            if let Some(map) = block.get("bindings").and_then(Value::as_object) {
                for (key, action) in map {
                    if action.as_str() == Some("voice:pushToTalk") {
                        v.key = key.clone();
                    }
                }
            }
        }
    }
    v
}
