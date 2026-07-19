//! TOML MCP (`[mcp_servers.<name>]`). Pure additive/idempotent; format-preserving.
//! Invalid TOML → Err. `Result<String, String>` matches sole caller.

use toml_edit::{DocumentMut, Item, Table, Value as TomlValue};

/// Merge/update `[mcp_servers.<name>]` stdio entry. Rendered TOML or unmergeable Err.
pub fn merge_mcp_server_toml(
    existing: &str,
    name: &str,
    command: &str,
    args: &[&str],
) -> Result<String, String> {
    let mut doc = if existing.trim().is_empty() {
        DocumentMut::new()
    } else {
        existing
            .parse::<DocumentMut>()
            .map_err(|e| format!("invalid TOML: {e}"))?
    };

    let servers = doc
        .entry("mcp_servers")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| "mcp_servers is not a table".to_string())?;

    let entry = servers
        .entry(name)
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| format!("mcp_servers.{name} is not a table"))?;

    entry.insert("command", Item::Value(TomlValue::from(command)));

    if args.is_empty() {
        entry.remove("args");
    } else {
        let arr: TomlValue = args.iter().map(|s| TomlValue::from(*s)).collect();
        entry.insert("args", Item::Value(arr));
    }

    Ok(doc.to_string())
}

/// Strip `[mcp_servers.<name>]`; drop empty table. Invalid TOML → Err (same as merge —
/// no silent "removed" on unparsed file).
pub fn strip_mcp_server_toml(existing: &str, name: &str) -> Result<String, String> {
    if existing.trim().is_empty() {
        return Ok(existing.to_string());
    }

    let mut doc = existing
        .parse::<DocumentMut>()
        .map_err(|e| format!("invalid TOML: {e}"))?;

    if let Some(servers) = doc.get_mut("mcp_servers").and_then(|i| i.as_table_mut()) {
        servers.remove(name);
        if servers.is_empty() {
            doc.remove("mcp_servers");
        }
    }

    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_creates_entry_in_empty_doc() {
        let out = merge_mcp_server_toml("", "DontSpeak", "/abs/dontspeak", &[]).unwrap();
        assert!(out.contains("[mcp_servers.DontSpeak]"));
        assert!(out.contains("command = \"/abs/dontspeak\""));
        assert!(!out.contains("args"));
    }

    #[test]
    fn merge_preserves_other_tables_and_keys() {
        let existing = r#"
[models]
default = "grok-build"

[mcp_servers.other]
command = "npx"
args = ["foo"]
"#;
        let out = merge_mcp_server_toml(existing, "DontSpeak", "/abs/dontspeak", &[]).unwrap();
        assert!(out.contains("[models]"));
        assert!(out.contains("[mcp_servers.other]"));
        assert!(out.contains("[mcp_servers.DontSpeak]"));
        assert!(out.contains("command = \"/abs/dontspeak\""));
    }

    #[test]
    fn merge_repoints_command_and_preserves_siblings() {
        let existing = r#"
[mcp_servers.DontSpeak]
command = "/old/dontspeak"
env = { FOO = "bar" }
enabled = true
"#;
        let out = merge_mcp_server_toml(existing, "DontSpeak", "/new/dontspeak", &[]).unwrap();
        assert!(out.contains("command = \"/new/dontspeak\""));
        assert!(out.contains("env = { FOO = \"bar\" }"));
        assert!(out.contains("enabled = true"));
    }

    #[test]
    fn strip_removes_entry_and_prunes_empty_table() {
        let existing = r#"
[mcp_servers.DontSpeak]
command = "/abs/dontspeak"

[mcp_servers.other]
command = "npx"
"#;
        let out = strip_mcp_server_toml(existing, "DontSpeak").unwrap();
        assert!(!out.contains("DontSpeak"));
        assert!(out.contains("[mcp_servers.other]"));
    }

    #[test]
    fn strip_prunes_mcp_servers_when_last() {
        let existing = "[mcp_servers.DontSpeak]\ncommand = \"/x\"";
        let out = strip_mcp_server_toml(existing, "DontSpeak").unwrap();
        assert!(!out.contains("mcp_servers"));
    }

    #[test]
    fn bad_toml_is_reported_on_merge() {
        let res = merge_mcp_server_toml("this is not = toml [", "DontSpeak", "/x", &[]);
        assert!(res.is_err());
    }

    #[test]
    fn bad_toml_is_reported_on_strip() {
        // Regression: strip used to swallow a parse failure into `Ok(existing)`, which let
        // the caller report a malformed file as successfully "removed" without ever having
        // parsed it. Must error exactly like `merge_mcp_server_toml` does above.
        let res = strip_mcp_server_toml("this is not = toml [", "DontSpeak");
        assert!(res.is_err());
    }
}
