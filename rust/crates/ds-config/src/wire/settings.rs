//! VoiceConfig (de)serialize — serde derive is the field single source.
//! File: TOML via `write_settings` (merge sibling keys). IPC: JSON via `voice_to_value`.
//! Round-trip identity via enum tokens. Never writes Claude Code's `voice` block.
//! Also: atomic-write + backup helpers for wire.

use std::io::{self, Write};

use serde_json::{Map, Value};

use crate::voice::{read_config_table, write_config_table};
use crate::{Paths, VoiceConfig};

/// Set OUR `dontspeak` block, preserving every other root key — including Claude Code's
/// `voice` block (we only *read* it via `claude_code` STT). Serde `Serialize` is the single
/// source of truth — no hand-maintained field list. PURE — no disk. Non-object root → `{}`.
///
/// IPC wire shaper (via [`voice_to_value`]); the config FILE is TOML via [`write_settings`].
pub fn merge_settings(mut root: Value, voice: &VoiceConfig) -> Value {
    if !root.is_object() {
        root = Value::Object(Map::new());
    }
    let obj = root.as_object_mut().expect("coerced to object above");
    obj.insert(
        "dontspeak".into(),
        serde_json::to_value(voice).unwrap_or_else(|_| Value::Object(Map::new())),
    );
    root
}

/// `VoiceConfig` as a JSON object for IPC — [`merge_settings`] then extract `dontspeak`.
pub fn voice_to_value(voice: &VoiceConfig) -> Value {
    let root = merge_settings(Value::Null, voice);
    root.get("dontspeak")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()))
}

/// Parse wire JSON back to `VoiceConfig`. Fail-open (defaults), matching [`VoiceConfig::load`].
pub fn voice_from_value(v: Value) -> VoiceConfig {
    serde_json::from_value(v).unwrap_or_default()
}

/// Atomically write `VoiceConfig` into our `config.toml`, preserving sibling keys (MCP-HTTP
/// etc.). Temp file + rename so the engine never reads half-written TOML.
pub fn write_settings(paths: &Paths, voice: &VoiceConfig) -> io::Result<()> {
    let mut table = read_config_table(paths);
    let voice_table = match toml::Value::try_from(voice) {
        Ok(toml::Value::Table(t)) => t,
        Ok(_) => {
            return Err(io::Error::other(
                "VoiceConfig did not serialize to a TOML table",
            ));
        }
        Err(e) => return Err(io::Error::other(e)),
    };
    for (k, v) in voice_table {
        table.insert(k, v);
    }
    write_config_table(paths, &table)
}

/// Atomic JSON write: pretty + trailing newline, temp in SAME dir, rename. Shared by wire
/// orchestrator hook + MCP writes.
pub fn atomic_write_json(path: &std::path::Path, value: &Value) -> io::Result<()> {
    let pretty = serde_json::to_string_pretty(value)? + "\n";
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(pretty.as_bytes())?;
    tmp.flush()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Same crash-safe pattern as [`atomic_write_json`] for non-JSON (e.g. already-serialized TOML).
pub fn atomic_write_str(path: &std::path::Path, contents: &str) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.flush()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Copy `path` → timestamped sibling `…<suffix>.bak.<epoch-nanos>` before overwrite.
/// CORR-3: backup is the only recovery if the write corrupts the user's file (settings.json
/// is also Claude Code's) — failure must NOT be swallowed; caller surfaces warning or aborts.
///
/// `Ok(None)` = source missing (clean install). `suffix` e.g. `"json"` / `"toml"`.
pub fn backup_before_write(
    path: &std::path::Path,
    suffix: &str,
) -> io::Result<Option<std::path::PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let bak = path.with_extension(format!("{suffix}.bak.{nanos}"));
    std::fs::copy(path, &bak)?;
    Ok(Some(bak))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::tests::sample_voice;
    use crate::{SttEngine, TtsEngine};

    /// Test-only mirror of the on-disk/IPC shape `{ "dontspeak": {…} }`. Production no longer
    /// reads this from settings.json; used to prove `VoiceConfig` (de)serialize discipline.
    #[derive(Debug, Default, serde::Deserialize)]
    struct SettingsRoot {
        dontspeak: Option<VoiceConfig>,
    }

    #[test]
    fn ds_block_parses_from_json_wrapper() {
        let r: SettingsRoot =
            serde_json::from_str(r#"{"dontspeak":{"tts_built_in_voices":["am_adam"]}}"#).unwrap();
        let v = r.dontspeak.unwrap();
        assert_eq!(v.tts_built_in_voices, vec!["am_adam"]);
        assert_eq!(v.tts_rate, 1.0);
    }

    #[test]
    fn merge_preserves_unrelated_keys_and_cc_voice() {
        let root = serde_json::json!({
            "model": "claude-opus-4",
            "permissions": { "allow": ["Bash(ls:*)"], "deny": [] },
            "hooks": { "PreToolUse": [ { "matcher": "Bash" } ] },
            "voice": { "enabled": true, "mode": "tap", "autoSubmit": true },
        });
        let merged = merge_settings(root, &sample_voice());

        assert_eq!(merged["model"], serde_json::json!("claude-opus-4"));
        assert_eq!(
            merged["permissions"],
            serde_json::json!({ "allow": ["Bash(ls:*)"], "deny": [] })
        );
        assert_eq!(
            merged["hooks"],
            serde_json::json!({ "PreToolUse": [ { "matcher": "Bash" } ] })
        );
        // Claude Code's OWN `voice` block is NOT touched.
        assert_eq!(
            merged["voice"],
            serde_json::json!({ "enabled": true, "mode": "tap", "autoSubmit": true })
        );
        assert_eq!(
            merged["dontspeak"]["tts_built_in_voices"][0],
            serde_json::json!("am_michael")
        );
        assert_eq!(
            merged["dontspeak"]["stt_engine_ladder"],
            serde_json::json!(["built_in"])
        );
        assert_eq!(
            merged["dontspeak"]["narrate"],
            serde_json::json!(["digests"])
        );
        // Unset prefs are `skip_serializing_if`'d — absent from wire.
        assert!(merged["dontspeak"].get("tts_engine").is_none());
        assert!(merged["dontspeak"].get("stt_engine").is_none());
    }

    #[test]
    fn voice_value_roundtrips_the_three_toggles_and_enums() {
        let v = sample_voice();
        let wire = voice_to_value(&v);
        assert_eq!(wire["caps_enabled"], serde_json::json!(false));
        assert_eq!(wire["tts_engine_ladder"], serde_json::json!(["system"]));
        let back = voice_from_value(wire);
        assert_eq!(back.caps_enabled, v.caps_enabled);
        assert_eq!(back.tts_engine, v.tts_engine);
        assert_eq!(back.tts_engine_ladder, v.tts_engine_ladder);
        assert_eq!(back.stt_engine, v.stt_engine);
        assert_eq!(back.stt_engine_ladder, v.stt_engine_ladder);
        assert_eq!(back.tts_built_in_voices, v.tts_built_in_voices);
        assert_eq!(back.tts_rate, v.tts_rate);
    }

    #[test]
    fn voice_preference_roundtrips_through_the_wire() {
        // Distinct from `sample_voice()` (prefs unset): all three preference states.
        let v = VoiceConfig {
            tts_engine: Some(vec![TtsEngine::System]),
            stt_engine: Some(Vec::new()), // explicit off
            ..sample_voice()
        };
        let wire = voice_to_value(&v);
        assert_eq!(wire["tts_engine"], serde_json::json!("system"));
        assert_eq!(wire["stt_engine"], serde_json::json!([]));
        let back = voice_from_value(wire);
        assert_eq!(back.tts_engine, v.tts_engine);
        assert_eq!(back.stt_engine, v.stt_engine);
    }

    #[test]
    fn merge_on_missing_file_yields_populated_object() {
        let merged = merge_settings(Value::Null, &sample_voice());
        assert!(merged.is_object());
        assert!(merged.get("dontspeak").is_some());
        assert_eq!(
            merged["dontspeak"]["stt_engine_ladder"],
            serde_json::json!(["built_in"])
        );
    }

    #[test]
    fn merge_on_garbage_root_is_coerced() {
        for garbage in [
            serde_json::json!("not an object"),
            serde_json::json!([1, 2, 3]),
        ] {
            let merged = merge_settings(garbage, &sample_voice());
            assert!(merged.is_object());
            assert_eq!(
                merged["dontspeak"]["tts_built_in_voices"][0],
                serde_json::json!("am_michael")
            );
        }
        // Non-object `dontspeak` is replaced; rest of root preserved.
        let root = serde_json::json!({ "keep": true, "dontspeak": "stringy" });
        let merged = merge_settings(root, &sample_voice());
        assert_eq!(merged["keep"], serde_json::json!(true));
        assert!(merged["dontspeak"].is_object());
        assert_eq!(
            merged["dontspeak"]["tts_built_in_voices"][0],
            serde_json::json!("am_michael")
        );
    }

    #[test]
    fn merge_then_load_roundtrip_is_identity() {
        // as_str() tokens must match what parse() accepts.
        let v = sample_voice();
        let merged = merge_settings(Value::Null, &v);
        let s = serde_json::to_string(&merged).unwrap();
        let root: SettingsRoot = serde_json::from_str(&s).unwrap();
        let lv = root.dontspeak.unwrap();

        assert_eq!(lv.tts_built_in_voices, v.tts_built_in_voices);
        assert_eq!(lv.stt_engine, v.stt_engine);
        assert_eq!(lv.stt_engine_ladder, v.stt_engine_ladder);
        assert_eq!(lv.tts_engine, v.tts_engine);
        assert_eq!(lv.tts_engine_ladder, v.tts_engine_ladder);
        assert_eq!(lv.tts_rate, v.tts_rate);
        assert_eq!(lv.narrate, v.narrate);
        assert_eq!(lv.long_press_ms, v.long_press_ms);
    }

    #[test]
    fn merge_emits_canonical_default_tokens() {
        let merged = merge_settings(Value::Null, &VoiceConfig::default());
        assert_eq!(
            merged["dontspeak"]["stt_engine_ladder"],
            serde_json::json!(["system", "built_in", "claude_code"])
        );
        assert_eq!(
            merged["dontspeak"]["tts_engine_ladder"],
            serde_json::json!(["built_in", "system"])
        );
        assert_eq!(
            merged["dontspeak"]["narrate"],
            serde_json::json!(["shorts", "digests"])
        );
        assert!(merged["dontspeak"].get("tts_engine").is_none());
        assert!(merged["dontspeak"].get("stt_engine").is_none());
    }

    #[test]
    fn bad_enum_degrades_then_writes_back_canonical() {
        // Bogus ladder fail-opens to default; merge persists clean tokens, never the bogus string.
        let on_disk =
            r#"{"dontspeak":{"stt_engine_ladder":"deepgram","tts_engine_ladder":"festival"}}"#;
        let root: SettingsRoot = serde_json::from_str(on_disk).unwrap();
        let loaded = root.dontspeak.unwrap();
        assert_eq!(
            loaded.stt_engine_ladder,
            vec![SttEngine::System, SttEngine::BuiltIn, SttEngine::ClaudeCode]
        );
        assert_eq!(
            loaded.tts_engine_ladder,
            vec![TtsEngine::Kokoro, TtsEngine::System]
        );
        let merged = merge_settings(Value::Null, &loaded);
        assert_eq!(
            merged["dontspeak"]["stt_engine_ladder"],
            serde_json::json!(["system", "built_in", "claude_code"])
        );
        assert_eq!(
            merged["dontspeak"]["tts_engine_ladder"],
            serde_json::json!(["built_in", "system"])
        );
    }

    #[test]
    fn bad_preference_degrades_to_unset_not_the_default_ladder() {
        // Bogus preference → `None` (unset), never a wrong single-engine choice.
        let on_disk = r#"{"dontspeak":{"stt_engine":"deepgram","tts_engine":"festival"}}"#;
        let root: SettingsRoot = serde_json::from_str(on_disk).unwrap();
        let loaded = root.dontspeak.unwrap();
        assert_eq!(loaded.stt_engine, None);
        assert_eq!(loaded.tts_engine, None);
    }

    #[test]
    fn backup_before_write_copies_and_signals_failure_visibly() {
        // CORR-3: present → timestamped copy; absent → Ok(None).
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("settings.json");

        assert!(backup_before_write(&target, "json").unwrap().is_none());

        std::fs::write(&target, "{\"a\":1}\n").unwrap();
        let bak = backup_before_write(&target, "json")
            .unwrap()
            .expect("backup made");
        assert!(bak.exists(), "backup file written");
        assert!(
            bak.to_string_lossy().contains(".bak."),
            "timestamped .bak name"
        );
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            "{\"a\":1}\n",
            "backup is a faithful copy"
        );
    }
}
