//! MCP server registration: the `mcpServers.<name>` JSON shaper.
//!
//! [`merge_mcp_server`] / [`strip_mcp_server`] are the generic `mcpServers.<name>` JsonMcp
//! shaper used by any JSON-MCP client — currently Claude Code's `~/.claude.json`. It registers
//! the stdio MCP bridge as a server so the client can call speak/listen/… on demand. The
//! config is the standard MCP shape:
//!   { "mcpServers": { "DontSpeak": { "command": "`<abs path>`", "args": [...] } } }
//! We edit it the same way as settings.json: additive (preserve other servers/keys), our
//! entry's `command`/`args` UPDATED IN PLACE (not the whole object replaced) so a reinstall
//! re-points `command` while any sibling key already on our entry survives, malformed file
//! left to the caller to bail on. PURE — no disk.

use serde_json::{Map, Value, json};

/// Merge an MCP stdio server entry under `mcpServers.<name>`, PRESERVING every other
/// server and top-level key. `command`/`args` are UPDATED (not skipped-if-present) so a
/// reinstall at a new path re-points `command` — idempotent and self-healing. Unlike a
/// whole-object replace, an EXISTING entry's other keys (e.g. a user- or host-added
/// `disabled` flag) are preserved: only `command` and `args` are ours to rewrite. PURE.
pub fn merge_mcp_server(mut root: Value, name: &str, command: &str, args: &[&str]) -> Value {
    if !root.is_object() {
        root = Value::Object(Map::new());
    }
    let obj = root.as_object_mut().expect("coerced to object above");
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));
    if !servers.is_object() {
        *servers = Value::Object(Map::new());
    }
    let servers = servers.as_object_mut().expect("coerced to object above");
    let entry = servers
        .entry(name.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
    }
    let entry = entry.as_object_mut().expect("coerced to object above");
    entry.insert("command".to_string(), json!(command));
    if args.is_empty() {
        entry.remove("args");
    } else {
        entry.insert("args".to_string(), json!(args));
    }
    root
}

/// Remove our MCP server entry `mcpServers.<name>`, dropping an emptied `mcpServers`
/// object. Leaves all other servers and keys untouched. PURE — no disk.
pub fn strip_mcp_server(mut root: Value, name: &str) -> Value {
    let Some(obj) = root.as_object_mut() else {
        return root;
    };
    let mut now_empty = false;
    if let Some(servers) = obj.get_mut("mcpServers").and_then(|s| s.as_object_mut()) {
        servers.remove(name);
        now_empty = servers.is_empty();
    }
    if now_empty {
        obj.remove("mcpServers");
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_mcp_server_into_empty_creates_entry() {
        let out = merge_mcp_server(Value::Null, "dontspeak", "/abs/ds-mcp", &[]);
        assert_eq!(out["mcpServers"]["dontspeak"]["command"], "/abs/ds-mcp");
        // No args → no "args" key (keep the entry minimal).
        assert!(out["mcpServers"]["dontspeak"].get("args").is_none());
    }

    #[test]
    fn merge_mcp_server_preserves_other_servers_and_keys() {
        let existing = json!({
            "globalShortcut": "Cmd+Shift+Space",
            "mcpServers": {
                "other": { "command": "/usr/bin/other", "args": ["--flag"] }
            }
        });
        let out = merge_mcp_server(existing, "dontspeak", "/abs/ds-mcp", &[]);
        // Our entry was added…
        assert_eq!(out["mcpServers"]["dontspeak"]["command"], "/abs/ds-mcp");
        // …and the unrelated server + top-level key are untouched.
        assert_eq!(out["mcpServers"]["other"]["command"], "/usr/bin/other");
        assert_eq!(out["mcpServers"]["other"]["args"][0], "--flag");
        assert_eq!(out["globalShortcut"], "Cmd+Shift+Space");
    }

    #[test]
    fn merge_mcp_server_overwrites_our_entry_to_repoint() {
        // A reinstall at a new path must RE-POINT our command, not duplicate/skip.
        let first = merge_mcp_server(Value::Null, "dontspeak", "/old/ds-mcp", &[]);
        let second = merge_mcp_server(first, "dontspeak", "/new/ds-mcp", &[]);
        assert_eq!(second["mcpServers"]["dontspeak"]["command"], "/new/ds-mcp");
        // Still exactly one entry.
        assert_eq!(second["mcpServers"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn merge_mcp_server_records_args_when_given() {
        // args are recorded when a caller supplies them (the registration is stdio, no args).
        let out = merge_mcp_server(
            Value::Null,
            "dontspeak",
            "/abs/ds-mcp",
            &["--flag", "value"],
        );
        assert_eq!(out["mcpServers"]["dontspeak"]["args"][0], "--flag");
        assert_eq!(out["mcpServers"]["dontspeak"]["args"][1], "value");
    }

    #[test]
    fn merge_mcp_server_preserves_sibling_keys_on_our_own_entry() {
        // Bug: merge_mcp_server used to replace the WHOLE `mcpServers.<name>` object with a
        // freshly built one containing only command/args, silently discarding any other key
        // already on OUR entry — e.g. a `disabled` flag the user (or the host app) added.
        // A re-wire must update ONLY command/args, preserving everything else.
        let existing = json!({ "mcpServers": {
            "dontspeak": {
                "command": "/old/ds-mcp",
                "disabled": true,
                "env": { "FOO": "bar" }
            }
        }});
        let out = merge_mcp_server(existing, "dontspeak", "/new/ds-mcp", &[]);
        assert_eq!(
            out["mcpServers"]["dontspeak"]["command"], "/new/ds-mcp",
            "command re-pointed"
        );
        assert_eq!(
            out["mcpServers"]["dontspeak"]["disabled"],
            json!(true),
            "sibling flag survives re-wire, not silently discarded"
        );
        assert_eq!(
            out["mcpServers"]["dontspeak"]["env"]["FOO"],
            json!("bar"),
            "sibling object survives re-wire"
        );
    }

    #[test]
    fn merge_mcp_server_updates_args_including_clearing_stale_ones() {
        // args are still OURS to control: a re-wire that now supplies no args must clear a
        // stale `args` array a previous wire left behind (only unrelated sibling keys are
        // preserved, not our own stale fields).
        let existing = json!({ "mcpServers": {
            "dontspeak": { "command": "/old/ds-mcp", "args": ["--old-flag"] }
        }});
        let out = merge_mcp_server(existing, "dontspeak", "/new/ds-mcp", &[]);
        assert!(
            out["mcpServers"]["dontspeak"].get("args").is_none(),
            "stale args cleared when the new wire supplies none"
        );
    }

    #[test]
    fn strip_mcp_server_removes_only_ours_and_prunes_empty() {
        // With a sibling server, stripping ours leaves mcpServers intact.
        let cfg = json!({ "mcpServers": {
            "dontspeak": { "command": "/abs/ds-mcp" },
            "other": { "command": "/usr/bin/other" }
        }});
        let out = strip_mcp_server(cfg, "dontspeak");
        assert!(out["mcpServers"].get("dontspeak").is_none());
        assert_eq!(out["mcpServers"]["other"]["command"], "/usr/bin/other");

        // As the ONLY server, stripping ours prunes the now-empty mcpServers object.
        let only = json!({ "mcpServers": { "dontspeak": { "command": "/abs/ds-mcp" } } });
        let out = strip_mcp_server(only, "dontspeak");
        assert!(
            out.get("mcpServers").is_none(),
            "empty mcpServers should be pruned"
        );
    }

    #[test]
    fn strip_mcp_server_is_a_noop_when_absent() {
        let cfg = json!({ "mcpServers": { "other": { "command": "/usr/bin/other" } } });
        let out = strip_mcp_server(cfg.clone(), "dontspeak");
        assert_eq!(out, cfg);
        // Also safe on a doc with no mcpServers at all.
        let bare = json!({ "theme": "dark" });
        assert_eq!(strip_mcp_server(bare.clone(), "dontspeak"), bare);
    }
}
