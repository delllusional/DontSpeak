//! Hook writers ([`claude_json_hooks`], TOML, Grok, [`seed_config`]).
//! Registry target; merges in `ds-config`; IO in `super::io`.
//! Additive, idempotent, backed-up. Unmergeable → leave untouched.
//! Print-only seed/capture: #30 / `wire_surfaces_print_only`.

use super::io::{self, WriteBody};
use crate::PreviewDoc;
use ds_config::{ClientSource, HookCommandStyle, HookSpec, Paths};

/// Seed missing `config.toml`. Interactive wire only (not engine reconcile).
pub(crate) fn seed_config(paths: &Paths) {
    if !paths.config_toml.exists() {
        if let Err(e) = ds_config::write_settings(paths, &ds_config::VoiceConfig::default()) {
            let msg = format!("wire: could not seed {}: {e}", paths.config_toml.display());
            eprintln!("{msg}");
            ds_log::log(&paths.log_file, ds_log::LogLevel::Error, "hook", &msg);
        } else {
            let msg = format!("wire: seeded {}", paths.config_toml.display());
            eprintln!("{msg}");
            ds_log::log(&paths.log_file, ds_log::LogLevel::Info, "hook", &msg);
        }
    }
}

/// Claude-contract JSON hooks. Malformed/unmergeable → leave untouched (exit 0).
/// `seed`/`capture`: print-only grouping (issue #30).
#[allow(clippy::too_many_arguments)]
pub(crate) fn claude_json_hooks(
    cfg: &std::path::Path,
    streaming: bool,
    command_style: HookCommandStyle,
    client: ClientSource,
    remove: bool,
    print_only: bool,
    paths: &Paths,
    seed: Option<PreviewDoc>,
    capture: Option<&mut Option<PreviewDoc>>,
) -> i32 {
    let existing = match seed {
        Some(PreviewDoc::Json(v)) => v,
        Some(PreviewDoc::Toml(_) | PreviewDoc::Yaml(_)) => {
            panic!("claude_json_hooks: seed must be PreviewDoc::Json for a JSON mechanism")
        }
        None => {
            let Ok(v) = io::read_json_or_bail("wire", cfg) else {
                return 0; // user file: leave, don't fail (match toml writer)
            };
            v
        }
    };
    let before = existing.clone();

    let merged = if remove {
        Ok(ds_config::strip_hooks(existing))
    } else {
        let Some(bin) = io::resolve_dontspeak_bin_at(Some(paths)) else {
            eprintln!("wire: could not resolve the dontspeak binary path");
            return 1;
        };
        let notif_channel = if cfg!(target_os = "macos") {
            Some("iterm2_with_bell")
        } else {
            None
        };
        let spec = HookSpec {
            bin: &bin,
            notif_channel,
            streaming,
            command_style,
            client,
        };
        ds_config::merge_hooks(existing, &spec)
    };

    let merged = match merged {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wire: {} left unchanged ({e})", cfg.display());
            return 0;
        }
    };

    if print_only {
        match capture {
            Some(slot) => {
                *slot = Some(PreviewDoc::Json(merged));
                0
            }
            None => match serde_json::to_string_pretty(&merged) {
                Ok(s) => {
                    println!("// {}\n{s}", cfg.display());
                    0
                }
                Err(e) => {
                    eprintln!("wire: serialize failed: {e}");
                    1
                }
            },
        }
    } else {
        // Load-bearing: every-boot reconcile zero-write when unchanged.
        if merged == before {
            return 0;
        }
        io::backup_then_write(
            "wire",
            cfg,
            "json",
            &WriteBody::Json(&merged),
            hook_action(remove),
        )
    }
}

fn hook_action(remove: bool) -> &'static str {
    if remove {
        "removed DontSpeak hooks from"
    } else {
        "wired DontSpeak hooks ->"
    }
}

/// Shared TOML hook body: format-preserving; malformed/unmergeable → exit 0; zero-write when
/// unchanged; backup before write; print-only seed/capture. Bin resolve only on merge path.
#[allow(clippy::too_many_arguments)]
fn toml_hooks_body<E: std::fmt::Display>(
    cfg: &std::path::Path,
    remove: bool,
    print_only: bool,
    seed: Option<PreviewDoc>,
    capture: Option<&mut Option<PreviewDoc>>,
    paths: &Paths,
    panic_ctx: &'static str,
    merge: impl FnOnce(&str, &str) -> Result<String, E>,
    strip: impl FnOnce(&str) -> Result<String, E>,
) -> i32 {
    let existing = match seed {
        Some(PreviewDoc::Toml(s)) => s,
        Some(PreviewDoc::Json(_) | PreviewDoc::Yaml(_)) => {
            panic!("{panic_ctx}: seed must be PreviewDoc::Toml for a TOML mechanism")
        }
        None => match std::fs::read_to_string(cfg) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                eprintln!("wire: could not read {} ({e})", cfg.display());
                return 1;
            }
        },
    };
    let result = if remove {
        strip(&existing)
    } else {
        let Some(bin) = io::resolve_dontspeak_bin_at(Some(paths)) else {
            eprintln!("wire: could not resolve the dontspeak binary path");
            return 1;
        };
        merge(&existing, &bin)
    };
    match result {
        Ok(merged) if print_only => match capture {
            Some(slot) => {
                *slot = Some(PreviewDoc::Toml(merged));
                0
            }
            None => {
                println!("// {}\n{merged}", cfg.display());
                0
            }
        },
        Ok(merged) if merged != existing => io::backup_then_write(
            "wire",
            cfg,
            "toml",
            &WriteBody::Str(&merged),
            hook_action(remove),
        ),
        Ok(_) => 0,
        Err(e) => {
            eprintln!("wire: {} left unchanged ({e})", cfg.display());
            0
        }
    }
}

/// Claude-contract TOML hooks (e.g. Codex). Same writer contract as JSON path.
pub(crate) fn claude_toml_hooks(
    cfg: &std::path::Path,
    client: ClientSource,
    remove: bool,
    print_only: bool,
    paths: &Paths,
    seed: Option<PreviewDoc>,
    capture: Option<&mut Option<PreviewDoc>>,
) -> i32 {
    toml_hooks_body(
        cfg,
        remove,
        print_only,
        seed,
        capture,
        paths,
        "claude_toml_hooks",
        |existing, bin| ds_config::merge_codex_hooks(existing, bin, client),
        ds_config::strip_codex_hooks,
    )
}

/// Kimi flat-`[[hooks]]` TOML. Same writer contract as [`claude_toml_hooks`].
pub(crate) fn kimi_toml_hooks(
    cfg: &std::path::Path,
    client: ClientSource,
    remove: bool,
    print_only: bool,
    paths: &Paths,
    seed: Option<PreviewDoc>,
    capture: Option<&mut Option<PreviewDoc>>,
) -> i32 {
    toml_hooks_body(
        cfg,
        remove,
        print_only,
        seed,
        capture,
        paths,
        "kimi_toml_hooks",
        |existing, bin| ds_config::merge_kimi_hooks(existing, bin, client),
        ds_config::strip_kimi_hooks,
    )
}

/// Shared YAML text body (Hermes config.yaml): same contract as [`toml_hooks_body`].
///
/// `write_action(remove)` labels the backup write; `load_hint` prints after a
/// successful non-remove write (MCP); `fatal_err` makes merge/strip errors
/// exit 1 (MCP) vs leave-unchanged exit 0 (hooks).
#[allow(clippy::too_many_arguments)]
fn yaml_text_body<E: std::fmt::Display>(
    cfg: &std::path::Path,
    remove: bool,
    print_only: bool,
    seed: Option<PreviewDoc>,
    capture: Option<&mut Option<PreviewDoc>>,
    paths: &Paths,
    panic_ctx: &'static str,
    merge: impl FnOnce(&str, &str) -> Result<String, E>,
    strip: impl FnOnce(&str) -> Result<String, E>,
    write_action: impl FnOnce(bool) -> &'static str,
    load_hint: Option<&str>,
    fatal_err: bool,
) -> i32 {
    let existing = match seed {
        Some(PreviewDoc::Yaml(s)) => s,
        Some(PreviewDoc::Json(_) | PreviewDoc::Toml(_)) => {
            panic!("{panic_ctx}: seed must be PreviewDoc::Yaml for a YAML mechanism")
        }
        None => match std::fs::read_to_string(cfg) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                eprintln!("wire: could not read {} ({e})", cfg.display());
                return 1;
            }
        },
    };
    let result = if remove {
        strip(&existing)
    } else {
        let Some(bin) = io::resolve_dontspeak_bin_at(Some(paths)) else {
            eprintln!("wire: could not resolve the dontspeak binary path");
            return 1;
        };
        merge(&existing, &bin)
    };
    match result {
        Ok(merged) if print_only => match capture {
            Some(slot) => {
                *slot = Some(PreviewDoc::Yaml(merged));
                0
            }
            None => {
                println!("// {}\n{merged}", cfg.display());
                0
            }
        },
        Ok(merged) if merged != existing => {
            let code = io::backup_then_write(
                "wire",
                cfg,
                "yaml",
                &WriteBody::Str(&merged),
                write_action(remove),
            );
            if code == 0
                && !remove
                && let Some(hint) = load_hint
            {
                eprintln!("wire: {hint}");
            }
            code
        }
        Ok(_) => 0,
        Err(e) => {
            if fatal_err {
                eprintln!("wire: {e}");
                1
            } else {
                eprintln!("wire: {} left unchanged ({e})", cfg.display());
                0
            }
        }
    }
}

/// Hermes nested `hooks.<event>` YAML. Same writer contract as TOML path.
pub(crate) fn hermes_yaml_hooks(
    cfg: &std::path::Path,
    client: ClientSource,
    remove: bool,
    print_only: bool,
    paths: &Paths,
    seed: Option<PreviewDoc>,
    capture: Option<&mut Option<PreviewDoc>>,
) -> i32 {
    yaml_text_body(
        cfg,
        remove,
        print_only,
        seed,
        capture,
        paths,
        "hermes_yaml_hooks",
        |existing, bin| ds_config::merge_hermes_hooks(existing, bin, client),
        ds_config::strip_hermes_hooks,
        hook_action,
        None,
        false,
    )
}

/// Hermes `mcp_servers.DontSpeak` in config.yaml (shares file with hooks).
pub(crate) fn hermes_yaml_mcp(
    cfg: &std::path::Path,
    remove: bool,
    print_only: bool,
    paths: &Paths,
    seed: Option<PreviewDoc>,
    capture: Option<&mut Option<PreviewDoc>>,
    load_hint: &str,
) -> i32 {
    yaml_text_body(
        cfg,
        remove,
        print_only,
        seed,
        capture,
        paths,
        "hermes_yaml_mcp",
        |existing, bin| ds_config::merge_hermes_mcp(existing, crate::SERVER_NAME, bin, &[]),
        |existing| ds_config::strip_hermes_mcp(existing, crate::SERVER_NAME),
        |rm| {
            if rm {
                "removed dontspeak MCP server from"
            } else {
                "registered dontspeak MCP server ->"
            }
        },
        Some(load_hint),
        true,
    )
}

/// Hermes shell-hooks allowlist JSON (consent for every wired `(event, command)`).
pub(crate) fn hermes_shell_allowlist(
    cfg: &std::path::Path,
    client: ClientSource,
    remove: bool,
    print_only: bool,
    paths: &Paths,
) -> i32 {
    if remove && !print_only && !cfg.exists() {
        return 0;
    }
    let existing = match io::read_json_or_bail("wire", cfg) {
        Ok(v) => v,
        Err(()) => return 0,
    };
    let before = existing.clone();
    let merged = if remove {
        ds_config::strip_hermes_allowlist(&existing)
    } else {
        let Some(bin) = io::resolve_dontspeak_bin_at(Some(paths)) else {
            eprintln!("wire: could not resolve the dontspeak binary path");
            return 1;
        };
        ds_config::merge_hermes_allowlist(&existing, &bin, client)
    };
    if print_only {
        match serde_json::to_string_pretty(&merged) {
            Ok(s) => {
                println!("// {}\n{s}", cfg.display());
                0
            }
            Err(e) => {
                eprintln!("wire: serialize failed: {e}");
                1
            }
        }
    } else if merged == before {
        0
    } else {
        io::backup_then_write(
            "wire",
            cfg,
            "json",
            &WriteBody::Json(&merged),
            if remove {
                "removed DontSpeak shell-hook approvals from"
            } else {
                "wired DontSpeak shell-hook approvals ->"
            },
        )
    }
}

/// Own-the-file JSON hooks (Grok): overwrite on wire, delete on remove (backed up). No merge.
pub(crate) fn grok_json_hooks(
    cfg: &std::path::Path,
    remove: bool,
    print_only: bool,
    paths: &Paths,
) -> i32 {
    if remove {
        // Issue #95: clear AGENTS.md digests even if hooks file never created.
        if !print_only {
            sync_grok_narrate_rules(paths, /*digests_on*/ false);
        }
        if !cfg.exists() {
            return 0;
        }
        if let Err(e) = ds_config::backup_before_write(cfg, "json") {
            eprintln!(
                "wire: WARNING: could not back up {} before removing ({e}); proceeding without a backup",
                cfg.display()
            );
        }
        return match std::fs::remove_file(cfg) {
            Ok(()) => {
                eprintln!("wire: {} {}", hook_action(true), cfg.display());
                0
            }
            Err(e) => {
                eprintln!("wire: could not remove {} ({e})", cfg.display());
                1
            }
        };
    }

    let Some(bin) = io::resolve_dontspeak_bin_at(Some(paths)) else {
        eprintln!("wire: could not resolve the dontspeak binary path");
        return 1;
    };
    let v = ds_config::grok_hooks_value(&bin);

    if print_only {
        println!(
            "// {}\n{}",
            cfg.display(),
            serde_json::to_string_pretty(&v).unwrap_or_default()
        );
        let digests_on =
            ds_config::VoiceConfig::load(paths).narrates(ds_config::NarrateKind::Digests);
        if digests_on {
            let preview = ds_config::apply_grok_narrate_section(
                &std::fs::read_to_string(&paths.grok_agents_md).unwrap_or_default(),
                Some(ds_config::DEFAULT_NARRATION_SPEC),
            );
            println!("// {}\n{preview}", paths.grok_agents_md.display());
        }
        return 0;
    }

    // Issue #95: Grok ignores hook additionalContext — sync AGENTS.md digests.
    let digests_on = ds_config::VoiceConfig::load(paths).narrates(ds_config::NarrateKind::Digests);
    sync_grok_narrate_rules(paths, digests_on);

    // Load-bearing zero-write when identical (every-boot reconcile).
    if std::fs::read_to_string(cfg)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .as_ref()
        == Some(&v)
    {
        return 0;
    }

    io::backup_then_write(
        "wire",
        cfg,
        "json",
        &WriteBody::Json(&v),
        hook_action(false),
    )
}

/// Best-effort AGENTS.md narrate section; errors on stderr only (non-fatal).
fn sync_grok_narrate_rules(paths: &Paths, digests_on: bool) {
    match ds_config::sync_grok_narrate_agents_md(&paths.grok_agents_md, digests_on) {
        Ok(true) if digests_on => {
            eprintln!(
                "wire: synced Grok narrate digests -> {}",
                paths.grok_agents_md.display()
            );
        }
        Ok(true) => {
            eprintln!(
                "wire: cleared Grok narrate digests from {}",
                paths.grok_agents_md.display()
            );
        }
        Ok(false) => {}
        Err(e) => {
            eprintln!(
                "wire: WARNING: could not sync Grok narrate digests in {} ({e})",
                paths.grok_agents_md.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_json(cfg: &std::path::Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(cfg).unwrap()).unwrap()
    }

    // Tests use `Paths::rooted_at` — never real $HOME.

    #[test]
    fn claude_json_hooks_wires_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("settings.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                false,
                false,
                &paths,
                None,
                None,
            ),
            0
        );
        let v = read_json(&cfg);
        assert_eq!(v["hooks"]["MessageDisplay"].as_array().unwrap().len(), 1);

        // Re-run: REPLACE-OURS merge must not duplicate the group.
        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                false,
                false,
                &paths,
                None,
                None,
            ),
            0
        );
        let v2 = read_json(&cfg);
        assert_eq!(v2["hooks"]["MessageDisplay"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn claude_json_hooks_remove_strips_previously_wired_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("settings.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                false,
                false,
                &paths,
                None,
                None,
            ),
            0
        );
        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                true,
                false,
                &paths,
                None,
                None,
            ),
            0
        );

        let v = read_json(&cfg);
        // `strip_hooks` drops an event array once it's emptied of our groups, and drops the
        // whole `hooks` object once every event is gone — undoing the get-or-create scaffold.
        assert!(v.get("hooks").is_none());
    }

    #[test]
    fn claude_json_hooks_malformed_json_is_left_unchanged_non_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("settings.json");
        let paths = Paths::rooted_at(dir.path());
        let raw = b"{ not json";
        std::fs::write(&cfg, raw).unwrap();

        // Caught in `io::read_json_or_bail`, but non-fatal — reported and the file left
        // byte-identical — never reaches `resolve_dontspeak_bin`.
        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                false,
                false,
                &paths,
                None,
                None,
            ),
            0
        );
        assert_eq!(std::fs::read(&cfg).unwrap(), raw);
    }

    #[test]
    fn claude_json_hooks_unmergeable_event_shape_is_left_unchanged_non_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("settings.json");
        let paths = Paths::rooted_at(dir.path());
        // `hooks.MessageDisplay` as an object (not an array) is a hand-edited/foreign shape
        // `merge_hooks` refuses to clobber → `HooksMergeError::UnmergeableShape`, the first
        // canonical event merge tried, so this never reaches the write call.
        let raw = r#"{"hooks":{"MessageDisplay":{"not":"an array"}}}"#;
        std::fs::write(&cfg, raw).unwrap();

        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                false,
                false,
                &paths,
                None,
                None,
            ),
            0
        ); // non-fatal
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), raw); // byte-identical
    }

    #[test]
    fn claude_json_hooks_print_only_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("settings.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                false,
                true,
                &paths,
                None,
                None,
            ),
            0
        );
        assert!(!cfg.exists());
    }

    /// `backup_then_write`'s `Err(e) => 1` arm: force `create_dir_all` on the config's parent
    /// dir to fail by pre-creating a plain FILE at the path that would otherwise be that parent
    /// directory — `create_dir_all` errors because `blocked` already exists and isn't a dir.
    #[test]
    fn claude_json_hooks_write_failure_returns_1() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blocked"), b"not a directory").unwrap();
        let cfg = dir.path().join("blocked").join("settings.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                false,
                false,
                &paths,
                None,
                None,
            ),
            1
        );
    }

    /// The mechanism also supports a non-streaming inline-shell client: `MessageDisplay` is
    /// omitted while commands remain inlined, with the usual merge and strip guarantees.
    #[test]
    fn claude_json_hooks_non_streaming_omits_messagedisplay() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("settings.json");
        let paths = Paths::rooted_at(dir.path());
        let qwen = HookCommandStyle::InlineShell;

        assert_eq!(
            claude_json_hooks(
                &cfg,
                false,
                qwen,
                ClientSource::QwenCode,
                false,
                false,
                &paths,
                None,
                None,
            ),
            0
        );
        let v = read_json(&cfg);
        assert!(
            v["hooks"].get("MessageDisplay").is_none(),
            "non-streaming wire must NOT install MessageDisplay"
        );
        // Stop / UserPromptSubmit / SessionStart / Notification ARE wired — in the inlined
        // shape: no `args` key, verb in the command string.
        for evt in ["Stop", "UserPromptSubmit", "SessionStart", "Notification"] {
            let h = &v["hooks"][evt][0]["hooks"][0];
            assert!(
                h.get("args").is_none(),
                "{evt}: on-disk Qwen shape must be inlined (no `args`), got {h}"
            );
            assert!(
                h["command"].as_str().unwrap().contains(" notify"),
                "{evt}: verb inlined into the command string"
            );
        }
        // Idempotent.
        assert_eq!(
            claude_json_hooks(
                &cfg,
                false,
                qwen,
                ClientSource::QwenCode,
                false,
                false,
                &paths,
                None,
                None,
            ),
            0
        );
        // Strips cleanly (remove removes every DontSpeak group regardless of dialect).
        assert_eq!(
            claude_json_hooks(
                &cfg,
                false,
                qwen,
                ClientSource::QwenCode,
                true,
                false,
                &paths,
                None,
                None,
            ),
            0
        );
        assert!(read_json(&cfg).get("hooks").is_none());
    }

    #[test]
    fn claude_toml_hooks_wires_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, false, false, &paths, None, None),
            0
        );
        let first = std::fs::read_to_string(&cfg).unwrap();

        // SessionStart is wired as a greet-only group: the greeting speaks, but the
        // streaming-witness seed is skipped (Codex is non-streaming — a seed would silence
        // its Stop narration). The command now ENDS with the uniform `--client codex` tail, so
        // the trailing quote pins THAT as the end of the command; toml_edit renders the command
        // as a basic (`"`) or literal (`'`) string depending on what the resolved binary path
        // contains (backslashes on Windows ⇒ literal).
        assert!(first.contains("[[hooks.SessionStart]]"), "got {first}");
        assert!(
            first.contains(" notify --greet-only --client codex\"")
                || first.contains(" notify --greet-only --client codex'"),
            "got {first}"
        );

        // Re-run: an unchanged command is a true byte-for-byte no-op (see `codex_group_matches`).
        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, false, false, &paths, None, None),
            0
        );
        let second = std::fs::read_to_string(&cfg).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn claude_toml_hooks_remove_strips_previously_wired_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, false, false, &paths, None, None),
            0
        );
        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, true, false, &paths, None, None),
            0
        );

        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(!text.contains("dontspeak"));
    }

    #[test]
    fn claude_toml_hooks_remove_on_missing_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        // `existing` is `unwrap_or_default()` on the missing-file read → "", and
        // `strip_codex_hooks("")` short-circuits `Ok("")` before ever calling
        // `resolve_dontspeak_bin` (that call is skipped entirely on the `remove` path anyway).
        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, true, false, &paths, None, None),
            0
        );
        assert!(!cfg.exists());
    }

    #[test]
    fn claude_toml_hooks_malformed_toml_is_left_unchanged_non_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());
        let raw = "hooks = [not valid toml";
        std::fs::write(&cfg, raw).unwrap();

        // `CodexMergeError::Parse` → the final `Err(e)` arm: reported, non-fatal, unchanged —
        // same convention `claude_json_hooks` now matches.
        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, false, false, &paths, None, None),
            0
        );
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), raw);
    }

    #[test]
    fn claude_toml_hooks_print_only_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, false, true, &paths, None, None),
            0
        );
        assert!(!cfg.exists());
    }

    /// Same write-failure technique as `claude_json_hooks_write_failure_returns_1`, for the
    /// TOML writer's `backup_then_write` call.
    #[test]
    fn claude_toml_hooks_write_failure_returns_1() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blocked"), b"not a directory").unwrap();
        let cfg = dir.path().join("blocked").join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, false, false, &paths, None, None),
            1
        );
    }

    // ── KimiTomlHooks: Kimi Code's flat `[[hooks]]` TOML writer ──────────────────────
    // Mirrors the `claude_toml_hooks` tests against Kimi's own shaper (flat entries, the
    // event/command/timeout-only key constraint).

    #[test]
    fn kimi_toml_hooks_wires_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            kimi_toml_hooks(
                &cfg,
                ClientSource::KimiCode,
                false,
                false,
                &paths,
                None,
                None
            ),
            0
        );
        let first = std::fs::read_to_string(&cfg).unwrap();

        // Flat entries (no [[hooks.<Event>]] groups), SessionStart greet-only with the
        // uniform `--client kimi_code` tail. toml_edit renders the command as a basic (`"`)
        // or literal (`'`) string depending on the resolved path (backslashes on Windows).
        assert!(first.contains("[[hooks]]"), "got {first}");
        assert!(!first.contains("[[hooks."), "flat shape: {first}");
        assert!(
            first.contains(" notify --greet-only --client kimi_code\"")
                || first.contains(" notify --greet-only --client kimi_code'"),
            "got {first}"
        );

        // Re-run: an unchanged command set is a true byte-for-byte no-op.
        assert_eq!(
            kimi_toml_hooks(
                &cfg,
                ClientSource::KimiCode,
                false,
                false,
                &paths,
                None,
                None
            ),
            0
        );
        let second = std::fs::read_to_string(&cfg).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn kimi_toml_hooks_remove_strips_previously_wired_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            kimi_toml_hooks(
                &cfg,
                ClientSource::KimiCode,
                false,
                false,
                &paths,
                None,
                None
            ),
            0
        );
        assert_eq!(
            kimi_toml_hooks(
                &cfg,
                ClientSource::KimiCode,
                true,
                false,
                &paths,
                None,
                None
            ),
            0
        );

        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(!text.contains("dontspeak"));
    }

    #[test]
    fn kimi_toml_hooks_remove_on_missing_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            kimi_toml_hooks(
                &cfg,
                ClientSource::KimiCode,
                true,
                false,
                &paths,
                None,
                None
            ),
            0
        );
        assert!(!cfg.exists());
    }

    #[test]
    fn kimi_toml_hooks_malformed_toml_is_left_unchanged_non_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());
        let raw = "hooks = [not valid toml";
        std::fs::write(&cfg, raw).unwrap();

        assert_eq!(
            kimi_toml_hooks(
                &cfg,
                ClientSource::KimiCode,
                false,
                false,
                &paths,
                None,
                None
            ),
            0
        );
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), raw);
    }

    #[test]
    fn kimi_toml_hooks_print_only_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            kimi_toml_hooks(
                &cfg,
                ClientSource::KimiCode,
                false,
                true,
                &paths,
                None,
                None
            ),
            0
        );
        assert!(!cfg.exists());
    }

    /// Same blocked-parent technique as `claude_json_hooks_write_failure_returns_1`, for the
    /// Kimi TOML writer's `backup_then_write` call.
    #[test]
    fn kimi_toml_hooks_write_failure_returns_1() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blocked"), b"not a directory").unwrap();
        let cfg = dir.path().join("blocked").join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            kimi_toml_hooks(
                &cfg,
                ClientSource::KimiCode,
                false,
                false,
                &paths,
                None,
                None
            ),
            1
        );
    }

    #[test]
    fn seed_config_seeds_config_toml_once_then_leaves_it_alone() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());

        seed_config(&paths);
        assert!(paths.config_toml.exists());

        // Append a marker to the seeded file, then prove a second call does NOT overwrite it
        // — the `!exists()` gate only seeds once.
        let mut contents = std::fs::read_to_string(&paths.config_toml).unwrap();
        contents.push_str("\n# marker\n");
        std::fs::write(&paths.config_toml, &contents).unwrap();

        seed_config(&paths);
        let after = std::fs::read_to_string(&paths.config_toml).unwrap();
        assert_eq!(after, contents);
    }

    /// `write_settings`'s `Err` branch inside `seed_config`: pre-create a plain FILE at the
    /// path that would be `paths.config_toml`'s parent directory, so the underlying
    /// `create_dir_all` fails. `seed_config` returns `()`, not a `Result` — reading the
    /// function, its `Err(e)` arm only logs (via `ds_log::log`) and continues (does not
    /// propagate, does not panic), so the only observable effect is that `config_toml` is never
    /// created.
    #[test]
    fn seed_config_write_failure_is_logged_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::write(dir.path().join(".dontspeak"), b"blocking file").unwrap();

        seed_config(&paths); // must not panic
        assert!(!paths.config_toml.exists());
    }

    // ── GrokJsonHooks: the DEDICATED own-the-file JSON writer ────────────────────────
    // Grok's `~/.grok/hooks/dontspeak.json` is exclusively ours: wire OVERWRITES it, unwire
    // DELETES it. These mirror the `claude_json_hooks` tests, adapted to the own-the-file
    // semantics (no merge, no user keys to preserve).

    #[test]
    fn grok_json_hooks_wires_the_five_events() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("dontspeak.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(grok_json_hooks(&cfg, false, false, &paths), 0);
        let v = read_json(&cfg);
        let hooks = v["hooks"].as_object().unwrap();
        let mut keys: Vec<&str> = hooks.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "Notification",
                "SessionEnd",
                "SessionStart",
                "Stop",
                "UserPromptSubmit",
            ]
        );
        // Stop voices the reply — a command carrying our binary, a seconds timeout, no async.
        let stop = &v["hooks"]["Stop"][0]["hooks"][0];
        assert!(
            stop["command"].as_str().unwrap().contains("dontspeak"),
            "Stop command invokes the dontspeak binary, got {stop}"
        );
        assert!(
            stop.get("timeout")
                .and_then(serde_json::Value::as_i64)
                .is_some(),
            "Stop entry carries a numeric (seconds) timeout"
        );
        assert!(
            stop.get("async").is_none(),
            "Grok hooks run synchronously — no async key"
        );
    }

    #[test]
    fn grok_json_hooks_is_idempotent_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("dontspeak.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(grok_json_hooks(&cfg, false, false, &paths), 0);
        let first = std::fs::read(&cfg).unwrap();
        // Own-the-file overwrite: a second wire re-renders the SAME content (same resolved bin),
        // so the file is byte-for-byte identical.
        assert_eq!(grok_json_hooks(&cfg, false, false, &paths), 0);
        let second = std::fs::read(&cfg).unwrap();
        assert_eq!(first, second, "re-wire writes byte-identical contents");
    }

    #[test]
    fn grok_json_hooks_remove_deletes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("dontspeak.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(grok_json_hooks(&cfg, false, false, &paths), 0);
        assert!(cfg.exists());
        assert_eq!(grok_json_hooks(&cfg, true, false, &paths), 0);
        assert!(!cfg.exists(), "unwire deletes the dedicated file");
    }

    #[test]
    fn grok_json_hooks_remove_on_missing_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("dontspeak.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(grok_json_hooks(&cfg, true, false, &paths), 0);
        assert!(!cfg.exists());
    }

    #[test]
    fn grok_json_hooks_print_only_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("dontspeak.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(grok_json_hooks(&cfg, false, true, &paths), 0);
        assert!(!cfg.exists());
    }

    /// Same blocked-parent technique as `claude_json_hooks_write_failure_returns_1`: a plain
    /// FILE at the config's would-be parent dir makes `create_dir_all` fail → exit 1.
    #[test]
    fn grok_json_hooks_write_failure_returns_1() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blocked"), b"not a directory").unwrap();
        let cfg = dir.path().join("blocked").join("dontspeak.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(grok_json_hooks(&cfg, false, false, &paths), 1);
    }
}
