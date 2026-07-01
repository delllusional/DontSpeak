//! Config serialization — `VoiceConfig` (de)serializes via its serde derive, so the
//! field set has ONE source of truth (no hand-maintained list to drift). Two consumers:
//!   • the config FILE — `write_settings` serializes the typed struct straight to TOML
//!     (`our config.toml`), merged over the existing table to keep sibling keys;
//!   • the IPC wire — `voice_to_value` / `merge_settings` produce the same JSON object
//!     the engine ⇄ app socket carries. A `write → load` round-trip is identity: each
//!     enum serializes to its `as_str()` token, exactly what its `parse()` accepts.
//! Claude Code's own `voice` block is the user's — DontSpeak never writes it (read-only).
//!
//! The atomic-write + timestamped-backup helpers also live here: they are the crash-safe
//! primitives shared by `write_settings` and the `wire <client>` orchestrator's hook + MCP writes.

use std::io::{self, Write};

use serde_json::{Map, Value};

use crate::voice::{read_config_table, write_config_table};
use crate::{Paths, VoiceConfig};

/// Set OUR `dontspeak` block on an existing document, PRESERVING every other root key —
/// including Claude Code's own `voice` block, which DontSpeak never writes (the
/// `claude_code` STT engine only reads it). The block is produced by serde from `VoiceConfig` (the
/// `Serialize` derive is the single source of truth — no hand-maintained field list to
/// drift), so adding a field can never silently drop it from the wire form. PURE — no
/// disk. A missing/garbage root (`Value::Null`, a string, …) coerces to `{}`.
///
/// This is now only the IPC wire shaper (via [`voice_to_value`]); the config FILE is
/// TOML and written by [`write_settings`] directly from the typed struct.
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

/// Our `dontspeak` block as a JSON object, in the EXACT shape `settings.json` uses
/// (so `serde_json::from_value::<VoiceConfig>` round-trips it). This is the IPC
/// wire form for config: reuses [`merge_settings`]'s field-by-field discipline,
/// then extracts just the `dontspeak` sub-object.
pub fn voice_to_value(voice: &VoiceConfig) -> Value {
    let root = merge_settings(Value::Null, voice);
    root.get("dontspeak")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()))
}

/// Parse a `voice` JSON object (as produced by [`voice_to_value`]) back into a
/// `VoiceConfig`. Fail-open: any non-object / bad value yields defaults, matching
/// [`VoiceConfig::load`]'s tolerance.
pub fn voice_from_value(v: Value) -> VoiceConfig {
    serde_json::from_value(v).unwrap_or_default()
}

/// Atomically write the `voice` settings to `our config.toml`, preserving the
/// file's other keys (the MCP-HTTP settings + anything hand-added). Serializes the
/// typed `VoiceConfig` to a TOML table and merges its keys over the existing table —
/// all at the `toml` layer, no JSON. Tolerates a missing/garbage file. Writes via a
/// temp file + atomic rename so the engine never reads a half-written file.
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

/// Atomically write a JSON value to `path`: pretty-print (+ trailing newline), write a
/// temp file in the SAME directory, then `rename` it onto the target (atomic on one
/// filesystem) so a reader never observes a half-written document. Shared by
/// [`write_settings`] and the `wire <client>` orchestrator's hook + MCP writes.
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

/// Atomically write arbitrary text to `path` (temp file in the same dir + rename),
/// the same crash-safe pattern as [`atomic_write_json`] but for non-JSON content —
/// used to write our own `config.toml` (already-serialized TOML).
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

/// Copy `path` to a timestamped sibling `…<suffix>.bak.<epoch-secs>` BEFORE an
/// overwrite, returning the backup path on success. CORR-3: the backup is the only
/// recovery if the about-to-happen write corrupts the user's own file (settings.json is
/// also Claude Code's), so its failure must NOT be
/// silently swallowed — the caller is expected to surface a clear warning (or abort)
/// when this returns `Err`, instead of proceeding to overwrite with no recoverable copy.
///
/// `Ok(None)` means the source does not exist yet (nothing to back up — a clean install,
/// not a failure). `suffix` is the on-disk extension to base the `.bak` name on, e.g.
/// `"json"` → `settings.json.bak.<secs>`, `"toml"` → `config.toml.bak.<secs>`.
pub fn backup_before_write(
    path: &std::path::Path,
    suffix: &str,
) -> io::Result<Option<std::path::PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bak = path.with_extension(format!("{suffix}.bak.{secs}"));
    std::fs::copy(path, &bak)?;
    Ok(Some(bak))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::tests::sample_voice;
    use crate::{SttEngine, TtsEngine};

    /// Test-only mirror of the on-disk/IPC config shape (`{ "dontspeak": {…} }`) that
    /// `merge_settings` / `voice_to_value` still emit. Production no longer reads this
    /// shape from settings.json (config lives in our config.toml), but the
    /// round-trip tests use it to prove `VoiceConfig`'s (de)serialize discipline.
    #[derive(Debug, Default, serde::Deserialize)]
    struct SettingsRoot {
        dontspeak: Option<VoiceConfig>,
    }

    #[test]
    fn ds_block_parsed_from_settings() {
        let r: SettingsRoot =
            serde_json::from_str(r#"{"dontspeak":{"tts_built_in_voices":["am_adam"]}}"#).unwrap();
        let v = r.dontspeak.unwrap();
        assert_eq!(v.current_voice(), "am_adam");
        assert_eq!(v.tts_rate, 1.0); // defaulted
    }

    // ── Atomic settings.json writer (PURE merge, NO disk, NO network) ────────

    #[test]
    fn merge_preserves_unrelated_keys_and_cc_voice() {
        // A realistic Claude Code settings.json with hooks/permissions/model + Claude
        // Code's OWN `voice` block: setting our `dontspeak` block must leave all of them
        // untouched (the dontspeak block itself is replaced wholesale from the typed config).
        let root = serde_json::json!({
            "model": "claude-opus-4",
            "permissions": { "allow": ["Bash(ls:*)"], "deny": [] },
            "hooks": { "PreToolUse": [ { "matcher": "Bash" } ] },
            "voice": { "enabled": true, "mode": "tap", "autoSubmit": true },
        });
        let merged = merge_settings(root, &sample_voice());

        // Every unrelated top-level key is byte-for-byte preserved.
        assert_eq!(merged["model"], serde_json::json!("claude-opus-4"));
        assert_eq!(
            merged["permissions"],
            serde_json::json!({ "allow": ["Bash(ls:*)"], "deny": [] })
        );
        assert_eq!(
            merged["hooks"],
            serde_json::json!({ "PreToolUse": [ { "matcher": "Bash" } ] })
        );
        // Claude Code's OWN `voice` block is NOT touched by merge_settings.
        assert_eq!(
            merged["voice"],
            serde_json::json!({ "enabled": true, "mode": "tap", "autoSubmit": true })
        );
        // Our managed fields reflect the config.
        assert_eq!(
            merged["dontspeak"]["tts_built_in_voices"][0],
            serde_json::json!("am_michael")
        );
        assert_eq!(
            merged["dontspeak"]["stt_engine"],
            serde_json::json!(["built_in"])
        );
        assert_eq!(
            merged["dontspeak"]["narrate"],
            serde_json::json!(["digests"])
        );
    }

    #[test]
    fn voice_value_roundtrips_the_three_toggles_and_enums() {
        // The IPC wire form: voice_to_value → voice_from_value must reproduce the
        // config exactly (including the new caps/stt/tts toggles and the enums).
        let v = sample_voice();
        let wire = voice_to_value(&v);
        // The toggles/engines are present in the wire object…
        assert_eq!(wire["caps_enabled"], serde_json::json!(false));
        assert_eq!(wire["tts_engine"], serde_json::json!(["system"]));
        // …and a full round-trip preserves every field we care about.
        let back = voice_from_value(wire);
        assert_eq!(back.caps_enabled, v.caps_enabled);
        assert_eq!(back.tts_engine, v.tts_engine);
        assert_eq!(back.stt_engine, v.stt_engine);
        assert_eq!(back.tts_built_in_voices, v.tts_built_in_voices);
        assert_eq!(back.tts_rate, v.tts_rate);
    }

    #[test]
    fn merge_on_missing_file_yields_populated_object() {
        // A missing file is fed as Value::Null → coerced to {} → fully populated,
        // no panic.
        let merged = merge_settings(Value::Null, &sample_voice());
        assert!(merged.is_object());
        assert!(merged.get("dontspeak").is_some());
        assert_eq!(
            merged["dontspeak"]["stt_engine"],
            serde_json::json!(["built_in"])
        );
    }

    #[test]
    fn merge_on_garbage_root_is_coerced() {
        // A string root and an array root both coerce to {} then populate.
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
        // A non-object `dontspeak` sub-value is replaced (cannot merge fields into a
        // scalar) while the rest of the root is preserved.
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
        // Serialize the merged doc, parse it back through the REAL load path
        // (SettingsRoot), and assert every enum + field round-trips — proving the
        // as_str() tokens are exactly what parse() accepts.
        let v = sample_voice();
        let merged = merge_settings(Value::Null, &v);
        let s = serde_json::to_string(&merged).unwrap();
        let root: SettingsRoot = serde_json::from_str(&s).unwrap();
        let lv = root.dontspeak.unwrap();

        assert_eq!(lv.tts_built_in_voices, v.tts_built_in_voices);
        assert_eq!(lv.stt_engine, v.stt_engine);
        assert_eq!(lv.tts_engine, v.tts_engine);
        assert_eq!(lv.tts_rate, v.tts_rate);
        assert_eq!(lv.narrate, v.narrate);
        assert_eq!(lv.long_press_ms, v.long_press_ms);
    }

    #[test]
    fn merge_emits_canonical_default_tokens() {
        // A defaulted config must emit the canonical Phase-1 tokens so a write of
        // the defaults round-trips to the defaults (no surprise enum drift).
        let merged = merge_settings(Value::Null, &VoiceConfig::default());
        assert_eq!(
            merged["dontspeak"]["stt_engine"],
            serde_json::json!(["built_in", "system", "claude_code"])
        );
        assert_eq!(
            merged["dontspeak"]["tts_engine"],
            serde_json::json!(["built_in", "system"])
        );
        assert_eq!(
            merged["dontspeak"]["narrate"],
            serde_json::json!(["shorts", "digests"])
        );
    }

    #[test]
    fn bad_enum_degrades_then_writes_back_canonical() {
        // A settings.json with a bogus stt_engine loads as the BuiltIn
        // default (fail-open); merging that loaded config writes back the
        // canonical "built_in" token (the degrade is persisted as a clean
        // default, never the bogus string).
        let on_disk = r#"{"dontspeak":{"stt_engine":"deepgram","tts_engine":"festival"}}"#;
        let root: SettingsRoot = serde_json::from_str(on_disk).unwrap();
        let loaded = root.dontspeak.unwrap();
        // A bogus scalar fails open to the DEFAULT ladder (not a single token).
        assert_eq!(
            loaded.stt_engine,
            vec![SttEngine::BuiltIn, SttEngine::System, SttEngine::ClaudeCode]
        );
        assert_eq!(
            loaded.tts_engine,
            vec![TtsEngine::Kokoro, TtsEngine::System]
        );
        let merged = merge_settings(Value::Null, &loaded);
        assert_eq!(
            merged["dontspeak"]["stt_engine"],
            serde_json::json!(["built_in", "system", "claude_code"])
        );
        assert_eq!(
            merged["dontspeak"]["tts_engine"],
            serde_json::json!(["built_in", "system"])
        );
    }

    #[test]
    fn backup_before_write_copies_and_signals_failure_visibly() {
        // CORR-3: present file → a timestamped copy is made (recoverable); absent file →
        // Ok(None) (clean install, nothing to back up). The copy's content matches.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("settings.json");

        // No file yet → Ok(None), nothing created.
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
