//! Claude Code's voice-dictation config, READ from its own files — never written.

use serde_json::Value;

use crate::Paths;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Paths;

    fn write(paths: &Paths, settings: Option<&str>, keybindings: Option<&str>) {
        if let Some(s) = settings {
            std::fs::create_dir_all(paths.settings_json.parent().unwrap()).unwrap();
            std::fs::write(&paths.settings_json, s).unwrap();
        }
        if let Some(k) = keybindings {
            std::fs::create_dir_all(paths.keybindings_json.parent().unwrap()).unwrap();
            std::fs::write(&paths.keybindings_json, k).unwrap();
        }
    }

    #[test]
    fn both_files_missing_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let v = read_claude_code_voice(&paths);
        assert_eq!(v, ClaudeCodeVoice::default());
    }

    #[test]
    fn settings_json_enabled_and_mode_read() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        write(
            &paths,
            Some(r#"{"voice": {"enabled": true, "mode": "tap"}}"#),
            None,
        );
        let v = read_claude_code_voice(&paths);
        assert!(v.enabled);
        assert_eq!(v.mode, "tap");
        assert_eq!(v.key, "space"); // no keybindings.json ⇒ default key
    }

    #[test]
    fn settings_json_enabled_false_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        write(&paths, Some(r#"{"voice": {"enabled": false}}"#), None);
        let v = read_claude_code_voice(&paths);
        assert!(!v.enabled);
        assert_eq!(v.mode, "hold"); // mode absent ⇒ default
    }

    #[test]
    fn settings_json_missing_voice_object_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        write(&paths, Some(r#"{"other": true}"#), None);
        let v = read_claude_code_voice(&paths);
        assert_eq!(v, ClaudeCodeVoice::default());
    }

    #[test]
    fn settings_json_garbage_fails_open() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        write(&paths, Some("not json at all {{{"), None);
        let v = read_claude_code_voice(&paths);
        assert_eq!(v, ClaudeCodeVoice::default());
    }

    #[test]
    fn settings_json_wrong_typed_fields_fail_open() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        // enabled as a string, mode as a number: both wrong-typed ⇒ ignored, defaults kept.
        write(
            &paths,
            Some(r#"{"voice": {"enabled": "yes", "mode": 42}}"#),
            None,
        );
        let v = read_claude_code_voice(&paths);
        assert!(!v.enabled);
        assert_eq!(v.mode, "hold");
    }

    #[test]
    fn keybindings_chat_context_binding_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        write(
            &paths,
            None,
            Some(
                r#"{"bindings": [
                    {"context": "Chat", "bindings": {"ctrl+g": "voice:pushToTalk"}}
                ]}"#,
            ),
        );
        let v = read_claude_code_voice(&paths);
        assert_eq!(v.key, "ctrl+g");
    }

    #[test]
    fn keybindings_non_chat_context_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        write(
            &paths,
            None,
            Some(
                r#"{"bindings": [
                    {"context": "Editor", "bindings": {"ctrl+g": "voice:pushToTalk"}}
                ]}"#,
            ),
        );
        let v = read_claude_code_voice(&paths);
        assert_eq!(v.key, "space"); // Editor-scoped binding is not Chat ⇒ ignored
    }

    #[test]
    fn keybindings_block_without_context_field_is_lenient() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        write(
            &paths,
            None,
            Some(
                r#"{"bindings": [
                    {"bindings": {"meta+k": "voice:pushToTalk"}}
                ]}"#,
            ),
        );
        let v = read_claude_code_voice(&paths);
        assert_eq!(v.key, "meta+k"); // missing "context" ⇒ treated as matching
    }

    #[test]
    fn keybindings_last_matching_block_wins() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        write(
            &paths,
            None,
            Some(
                r#"{"bindings": [
                    {"context": "Chat", "bindings": {"ctrl+g": "voice:pushToTalk"}},
                    {"context": "Chat", "bindings": {"meta+k": "voice:pushToTalk"}}
                ]}"#,
            ),
        );
        let v = read_claude_code_voice(&paths);
        assert_eq!(v.key, "meta+k", "last matching block wins");
    }

    #[test]
    fn keybindings_non_pushtotalk_actions_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        write(
            &paths,
            None,
            Some(
                r#"{"bindings": [
                    {"context": "Chat", "bindings": {"ctrl+g": "some:otherAction"}}
                ]}"#,
            ),
        );
        let v = read_claude_code_voice(&paths);
        assert_eq!(v.key, "space");
    }

    #[test]
    fn keybindings_json_garbage_fails_open() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        write(&paths, None, Some("[[[ not valid json"));
        let v = read_claude_code_voice(&paths);
        assert_eq!(v.key, "space");
    }

    #[test]
    fn keybindings_wrong_typed_fields_fail_open() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        // "bindings" top-level is a string, not an array; and inside a block, "bindings" is
        // a string too. Neither should panic; both should just leave the default key.
        write(&paths, None, Some(r#"{"bindings": "not an array"}"#));
        let v1 = read_claude_code_voice(&paths);
        assert_eq!(v1.key, "space");

        write(
            &paths,
            None,
            Some(r#"{"bindings": [{"context": "Chat", "bindings": "nope"}]}"#),
        );
        let v2 = read_claude_code_voice(&paths);
        assert_eq!(v2.key, "space");
    }

    #[test]
    fn both_files_present_and_combined() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        write(
            &paths,
            Some(r#"{"voice": {"enabled": true, "mode": "tap"}}"#),
            Some(
                r#"{"bindings": [
                    {"context": "Chat", "bindings": {"ctrl+g": "voice:pushToTalk"}}
                ]}"#,
            ),
        );
        let v = read_claude_code_voice(&paths);
        assert!(v.enabled);
        assert_eq!(v.mode, "tap");
        assert_eq!(v.key, "ctrl+g");
    }
}
