//! Shared YAML parse/emit for Hermes `config.yaml` shapers.
//!
//! Hermes hooks + MCP share one document; both re-emit via serde-saphyr
//! (comment loss accepted — no format-preserving YAML editor).

use serde_json::{Map, Value};

/// Empty/whitespace → empty object. Invalid YAML → `Err` with a short label.
pub(crate) fn parse(existing: &str) -> Result<Value, String> {
    if existing.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_saphyr::from_str(existing).map_err(|e| e.to_string())
}

/// Serialize a JSON [`Value`] tree as YAML.
pub(crate) fn emit(root: &Value) -> Result<String, String> {
    serde_saphyr::to_string(root).map_err(|e| e.to_string())
}
