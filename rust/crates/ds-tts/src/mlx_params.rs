//! Wire form of resolved TTS params for the MLX shim ABI (the `params_json` slot of
//! `ds_mlx_tts_synthesize`). Pure — no dlopen — so the cross-language drift test runs
//! on every host. The Swift half is the hand-maintained key-list mirror in
//! `apps/macos/DontSpeakMLX/Sources/DontSpeakMLX/shim.swift` (`ttsParamMirror`),
//! pinned by `TtsAbiTests.swift`; update registry, mirror, and both tests together.

/// Serialize the resolved params the Swift shim decodes. Defensive re-scope: only
/// keys DECLARED for `model` cross the ABI — the shim classifies exactly the declared
/// set (applied vs explicitly-ignored) and logs anything else as unknown.
pub fn mlx_params_json(
    model: ds_config::TtsModel,
    params: &ds_config::ResolvedTtsParams,
) -> String {
    let descriptor = model.descriptor();
    let mut map = serde_json::Map::new();
    for (key, value) in params.iter() {
        if descriptor.param(key).is_none() {
            continue;
        }
        let json = match value {
            ds_config::TtsParamValue::Int(value) => serde_json::json!(value),
            ds_config::TtsParamValue::Float(value) => serde_json::json!(value),
            ds_config::TtsParamValue::Choice(value) => serde_json::json!(value),
        };
        map.insert(key.to_string(), json);
    }
    serde_json::Value::Object(map).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-language drift guard, Rust half: per model, the serialized key set IS the
    /// descriptor's declared key set. The Swift half pins `ttsParamMirror` to the same
    /// sets, so a registry addition that skips either side fails a test.
    #[test]
    fn params_json_key_set_matches_the_declared_descriptors() {
        for model in ds_config::TtsModel::ALL.iter().copied() {
            let descriptor = model.descriptor();
            let resolved = descriptor.resolve_params(&Default::default());
            let json: serde_json::Value =
                serde_json::from_str(&mlx_params_json(model, &resolved)).unwrap();
            let mut keys: Vec<&str> = json
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            let mut declared: Vec<&str> = descriptor.params.iter().map(|p| p.key).collect();
            declared.sort_unstable();
            assert_eq!(keys, declared, "{} params_json drifted", descriptor.id);
        }
    }

    #[test]
    fn params_json_carries_defaults_as_bare_scalars() {
        let chatterbox = ds_config::TtsModel::Chatterbox;
        let resolved = chatterbox.descriptor().resolve_params(&Default::default());
        assert_eq!(
            mlx_params_json(chatterbox, &resolved),
            r#"{"exaggeration":0.5}"#
        );
        let kokoro = ds_config::TtsModel::Kokoro;
        let resolved = kokoro.descriptor().resolve_params(&Default::default());
        assert_eq!(mlx_params_json(kokoro, &resolved), "{}");
    }
}
