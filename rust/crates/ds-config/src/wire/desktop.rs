//! MCP server registration (Claude Desktop's claude_desktop_config.json)
//!
//! Claude Desktop has no hook system, so its DontSpeak integration is purely
//! registering the stdio MCP bridge as a server — Claude then calls speak/listen/…
//! on demand (no auto-narration). The config is the standard MCP shape:
//!   { "mcpServers": { "DontSpeak": { "command": "`<abs path>`", "args": [...] } } }
//! We edit it the same way as settings.json: additive (preserve other servers/keys),
//! our entry OVERWRITTEN so a reinstall re-points `command`, malformed file left to the
//! caller to bail on. PURE — no disk.

use serde_json::{Map, Value, json};

/// Merge an MCP stdio server entry under `mcpServers.<name>`, PRESERVING every other
/// server and top-level key. OUR entry is overwritten (not skipped-if-present) so a
/// reinstall at a new path re-points `command` — idempotent and self-healing. PURE.
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
    let mut entry = Map::new();
    entry.insert("command".to_string(), json!(command));
    if !args.is_empty() {
        entry.insert("args".to_string(), json!(args));
    }
    servers.insert(name.to_string(), Value::Object(entry));
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
        // The real desktop registration is stdio (no args); this just pins that args ARE
        // recorded when a caller supplies them.
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
