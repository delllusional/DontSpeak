//! Queue identity shared by the launcher, hooks, and the stdio MCP server.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ds_config::WiredAgent;

pub(crate) const DONTSPEAK_SESSION_ID: &str = "DONTSPEAK_SESSION_ID";

const TERMINAL_IDS: &[&str] = &[
    "WT_SESSION",
    "TERM_SESSION_ID",
    "ITERM_SESSION_ID",
    "WEZTERM_PANE",
    "KITTY_WINDOW_ID",
    "TMUX_PANE",
];

static GENERATED_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Identity inherited by the wrapped client and every hook/MCP child it starts.
pub(crate) fn new_launcher_session(client: WiredAgent) -> String {
    generated("launch", Some(client))
}

/// Stable terminal identity visible to both hooks and the MCP child.
pub(crate) fn ambient() -> Option<String> {
    ambient_with(|name| std::env::var(name).ok())
}

/// MCP operations are always scoped. Clients without an ambient contract get a
/// process-lifetime identity, which preserves isolation between stdio servers.
pub(crate) fn for_mcp(client: Option<WiredAgent>) -> String {
    ambient().unwrap_or_else(|| generated("mcp", client))
}

/// Queue scope for hook-originated operations. Logical payload ids remain the
/// fallback for direct clients that expose no shared ambient identity.
pub(crate) fn for_hook(payload: &str) -> Option<String> {
    for_hook_with(payload, |name| std::env::var(name).ok())
}

pub(crate) fn for_hook_with(payload: &str, get: impl Fn(&str) -> Option<String>) -> Option<String> {
    ambient_with(get).or_else(|| crate::hook_core::session_id_from_payload(payload))
}

fn ambient_with(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    nonempty(get(DONTSPEAK_SESSION_ID))
        .map(|value| tagged("launch", DONTSPEAK_SESSION_ID, &value))
        .or_else(|| {
            TERMINAL_IDS
                .iter()
                .find_map(|name| nonempty(get(name)).map(|value| tagged("terminal", name, &value)))
        })
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn tagged(kind: &str, name: &str, value: &str) -> String {
    format!("dontspeak:{kind}:{name}:{value}")
}

fn generated(kind: &str, client: Option<WiredAgent>) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let sequence = GENERATED_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "dontspeak:{kind}:{}:{}:{nanos}:{sequence}",
        client.map_or("unwired", WiredAgent::as_str),
        std::process::id()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn resolve(values: &[(&str, &str)]) -> Option<String> {
        let values: HashMap<_, _> = values.iter().copied().collect();
        ambient_with(|name| values.get(name).map(|value| (*value).into()))
    }

    #[test]
    fn launcher_identity_wins_over_terminal_ids() {
        assert_eq!(
            resolve(&[
                ("WT_SESSION", "terminal"),
                (DONTSPEAK_SESSION_ID, "launcher"),
            ])
            .as_deref(),
            Some("dontspeak:launch:DONTSPEAK_SESSION_ID:launcher")
        );
    }

    #[test]
    fn terminal_identity_is_shared_by_clients_without_session_env() {
        assert_eq!(
            resolve(&[("TMUX_PANE", "%7")]).as_deref(),
            Some("dontspeak:terminal:TMUX_PANE:%7")
        );
    }

    #[test]
    fn logical_agent_ids_are_not_used_as_queue_ids() {
        assert_eq!(
            resolve(&[
                ("CLAUDE_CODE_SESSION_ID", "changes-on-clear"),
                ("QWEN_CODE_SESSION_ID", "changes-on-new-session"),
                ("CODEX_THREAD_ID", "not-forwarded-to-mcp"),
                ("HERMES_SESSION_ID", "created-after-mcp-startup"),
            ]),
            None
        );
    }

    #[test]
    fn empty_values_do_not_create_shared_empty_scopes() {
        assert_eq!(
            resolve(&[(DONTSPEAK_SESSION_ID, "  "), ("WT_SESSION", "")]),
            None
        );
    }

    #[test]
    fn hook_scope_prefers_ambient_identity_over_payload_session() {
        assert_eq!(
            for_hook_with(r#"{"session_id":"payload"}"#, |name| {
                (name == "TERM_SESSION_ID").then(|| "terminal".into())
            })
            .as_deref(),
            Some("dontspeak:terminal:TERM_SESSION_ID:terminal")
        );
    }

    #[test]
    fn generated_mcp_scopes_are_nonempty_and_unique() {
        let first = generated("mcp", Some(WiredAgent::Grok));
        let second = generated("mcp", Some(WiredAgent::Grok));
        assert!(!first.is_empty());
        assert_ne!(first, second);
    }
}
