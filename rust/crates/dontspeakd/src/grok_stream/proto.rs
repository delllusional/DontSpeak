//! Parse Grok interactive session `updates.jsonl` NDJSON lines.
//!
//! Live shape (verified): `method` is `session/update` (or `_x.ai/session/update`);
//! `params.update.sessionUpdate` selects the event; only `agent_message_chunk` with
//! `content.type == "text"` yields spoken deltas. Thought / tool / user chunks ignored.

use serde::Deserialize;

/// One agent-message text delta extracted from an updates.jsonl line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentTextChunk {
    /// Session id from `params.sessionId` when present.
    pub session_id: Option<String>,
    /// Batch key: `_meta.promptId` if non-empty, else caller falls back to session.
    pub prompt_id: Option<String>,
    pub text: String,
}

#[derive(Debug, Deserialize)]
struct Line {
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Params>,
}

#[derive(Debug, Deserialize)]
struct Params {
    // Live Grok: camelCase `sessionId` (and `_meta.promptId`).
    #[serde(default, rename = "sessionId", alias = "session_id")]
    session_id: Option<String>,
    #[serde(default)]
    update: Option<Update>,
    #[serde(default)]
    _meta: Option<Meta>,
}

#[derive(Debug, Deserialize)]
struct Update {
    #[serde(default, rename = "sessionUpdate", alias = "session_update")]
    session_update: Option<String>,
    #[serde(default)]
    content: Option<Content>,
}

#[derive(Debug, Deserialize)]
struct Content {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Meta {
    #[serde(default, rename = "promptId", alias = "prompt_id")]
    prompt_id: Option<String>,
}

/// Parse one NDJSON line. Returns `None` for non-agent-message, empty text, or malformed.
pub(crate) fn parse_agent_text_chunk(line: &str) -> Option<AgentTextChunk> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let parsed: Line = serde_json::from_str(line).ok()?;
    let method = parsed.method.as_deref()?.trim();
    // Accept stock ACP and Grok-prefixed method names.
    if method != "session/update" && method != "_x.ai/session/update" {
        return None;
    }
    let params = parsed.params?;
    let update = params.update?;
    let session_update = update.session_update.as_deref()?.trim();
    if session_update != "agent_message_chunk" {
        return None;
    }
    let content = update.content?;
    if content
        .kind
        .as_deref()
        .is_some_and(|k| !k.eq_ignore_ascii_case("text"))
    {
        // Non-text content blocks (images, etc.) — ignore.
        // Missing type is treated as text when text is present (defensive).
        if content.kind.is_some() {
            return None;
        }
    }
    let text = content.text.filter(|t| !t.is_empty())?;
    let prompt_id = params
        ._meta
        .and_then(|m| m.prompt_id)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let session_id = params
        .session_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Some(AgentTextChunk {
        session_id,
        prompt_id,
        text,
    })
}

/// Batch key for a chunk: non-empty prompt id, else the registered session id.
pub(crate) fn batch_key(chunk: &AgentTextChunk, session: &str) -> String {
    chunk
        .prompt_id
        .clone()
        .unwrap_or_else(|| session.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_live_agent_message_chunk() {
        let line = r#"{"timestamp":1,"method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"> Digest.\n"}},"_meta":{"promptId":"p1","chunkId":1}}}"#;
        let c = parse_agent_text_chunk(line).expect("agent chunk");
        assert_eq!(c.session_id.as_deref(), Some("s1"));
        assert_eq!(c.prompt_id.as_deref(), Some("p1"));
        assert_eq!(c.text, "> Digest.\n");
        assert_eq!(batch_key(&c, "s1"), "p1");
    }

    #[test]
    fn ignores_thought_tool_user_and_xai_hooks() {
        let lines = [
            r#"{"method":"session/update","params":{"update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking"}}}}"#,
            r#"{"method":"session/update","params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"}}}}"#,
            r#"{"method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"t1"}}}"#,
            r#"{"method":"_x.ai/session/update","params":{"update":{"sessionUpdate":"hook_execution"}}}"#,
            r#"{"method":"other","params":{}}"#,
            r#"{not json"#,
            "",
        ];
        for line in lines {
            assert!(
                parse_agent_text_chunk(line).is_none(),
                "should ignore: {line}"
            );
        }
    }

    #[test]
    fn empty_prompt_id_falls_back_to_session() {
        let line = r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}},"_meta":{"promptId":"  "}}}"#;
        let c = parse_agent_text_chunk(line).unwrap();
        assert!(c.prompt_id.is_none());
        assert_eq!(batch_key(&c, "s"), "s");
    }

    #[test]
    fn empty_text_ignored() {
        let line = r#"{"method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":""}}}}"#;
        assert!(parse_agent_text_chunk(line).is_none());
    }
}
