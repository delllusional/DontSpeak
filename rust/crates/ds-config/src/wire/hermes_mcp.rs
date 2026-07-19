//! Hermes Agent MCP (`~/.hermes/config.yaml` `mcp_servers.DontSpeak`).
//!
//! Same YAML document as shell hooks; snake_case `mcp_servers` (not Claude
//! `mcpServers`). Comment loss on re-emit accepted.

use serde_json::{Map, Value, json};

fn parse_yaml(existing: &str) -> Result<Value, String> {
    if existing.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_saphyr::from_str(existing).map_err(|e| format!("invalid YAML: {e}"))
}

fn emit_yaml(root: &Value) -> Result<String, String> {
    serde_saphyr::to_string(root).map_err(|e| format!("YAML serialize failed: {e}"))
}

/// Merge/update `mcp_servers.<name>` stdio entry. Rendered YAML or unmergeable Err.
pub fn merge_hermes_mcp(
    existing: &str,
    name: &str,
    command: &str,
    args: &[&str],
) -> Result<String, String> {
    let mut root = parse_yaml(existing)?;
    if !root.is_object() {
        return Err("config.yaml root is not a mapping".into());
    }
    let obj = root.as_object_mut().expect("object checked above");
    let servers = obj
        .entry("mcp_servers".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !servers.is_object() {
        return Err("mcp_servers is not a mapping".into());
    }
    let servers = servers.as_object_mut().expect("object checked above");
    let entry = servers
        .entry(name.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        return Err(format!("mcp_servers.{name} is not a mapping"));
    }
    let entry = entry.as_object_mut().expect("object checked above");
    entry.insert("command".to_string(), json!(command));
    if args.is_empty() {
        entry.remove("args");
    } else {
        entry.insert("args".to_string(), json!(args));
    }
    emit_yaml(&root)
}

/// Strip `mcp_servers.<name>`; drop empty table.
pub fn strip_hermes_mcp(existing: &str, name: &str) -> Result<String, String> {
    if existing.trim().is_empty() {
        return Ok(existing.to_string());
    }
    let mut root = parse_yaml(existing)?;
    let Some(obj) = root.as_object_mut() else {
        return Ok(existing.to_string());
    };
    if let Some(servers) = obj.get_mut("mcp_servers").and_then(|s| s.as_object_mut()) {
        servers.remove(name);
        if servers.is_empty() {
            obj.remove("mcp_servers");
        }
    }
    emit_yaml(&root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_creates_entry_in_empty_doc() {
        let out = merge_hermes_mcp("", "DontSpeak", "/abs/dontspeak", &[]).unwrap();
        assert!(out.contains("mcp_servers"), "{out}");
        assert!(out.contains("DontSpeak"), "{out}");
        assert!(out.contains("/abs/dontspeak"), "{out}");
        assert!(!out.contains("args"), "{out}");
    }

    #[test]
    fn merge_preserves_other_tables_and_keys() {
        let existing = r#"
model: gpt
mcp_servers:
  other:
    command: npx
    args: [foo]
"#;
        let out = merge_hermes_mcp(existing, "DontSpeak", "/abs/dontspeak", &[]).unwrap();
        assert!(out.contains("model"), "{out}");
        assert!(out.contains("other"), "{out}");
        assert!(out.contains("DontSpeak"), "{out}");
        assert!(out.contains("/abs/dontspeak"), "{out}");
    }

    #[test]
    fn merge_repoints_command_and_preserves_siblings() {
        let existing = r#"
mcp_servers:
  DontSpeak:
    command: /old/dontspeak
    enabled: true
"#;
        let out = merge_hermes_mcp(existing, "DontSpeak", "/new/dontspeak", &[]).unwrap();
        assert!(out.contains("/new/dontspeak"), "{out}");
        assert!(out.contains("enabled"), "{out}");
    }

    #[test]
    fn strip_removes_entry_and_prunes_empty_table() {
        let existing = r#"
mcp_servers:
  DontSpeak:
    command: /abs/dontspeak
  other:
    command: npx
"#;
        let out = strip_hermes_mcp(existing, "DontSpeak").unwrap();
        assert!(!out.contains("DontSpeak"), "{out}");
        assert!(out.contains("other"), "{out}");
    }

    #[test]
    fn strip_prunes_mcp_servers_when_last() {
        let existing = "mcp_servers:\n  DontSpeak:\n    command: /x\n";
        let out = strip_hermes_mcp(existing, "DontSpeak").unwrap();
        assert!(!out.contains("mcp_servers"), "{out}");
    }

    #[test]
    fn bad_yaml_is_reported_on_merge() {
        let res = merge_hermes_mcp("hooks: [\n  - :\n", "DontSpeak", "/x", &[]);
        assert!(res.is_err());
    }

    #[test]
    fn bad_yaml_is_reported_on_strip() {
        let res = strip_hermes_mcp("hooks: [\n  - :\n", "DontSpeak");
        assert!(res.is_err());
    }
}
