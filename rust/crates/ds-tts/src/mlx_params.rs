//! MLX `params_json` wire form. Rust and Swift literals pin the hand-maintained
//! registry mirror; update both sides with every registry edit.

/// Serialize only keys declared for `model`. Direct serialization preserves f32's
/// shortest representation instead of widening through `serde_json::Value`.
pub fn mlx_params_json(
    model: ds_config::TtsModel,
    params: &ds_config::ResolvedTtsParams,
) -> String {
    let descriptor = model.descriptor();
    let scoped: std::collections::BTreeMap<&str, &ds_config::TtsParamValue> = params
        .iter()
        .filter(|(key, _)| descriptor.param(key).is_some())
        .collect();
    serde_json::to_string(&scoped).expect("scalar param map serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Literal drift pin shared with Swift's `ttsParamMirror` tests.
    #[test]
    fn params_json_carries_defaults_as_bare_scalars() {
        let defaults = |model: ds_config::TtsModel| {
            let resolved = model.descriptor().resolve_params(&Default::default());
            mlx_params_json(model, &resolved)
        };
        assert_eq!(defaults(ds_config::TtsModel::Kokoro), "{}");
        assert_eq!(
            defaults(ds_config::TtsModel::Chatterbox),
            r#"{"exaggeration":0.5}"#
        );
        assert_eq!(
            defaults(ds_config::TtsModel::Qwen),
            r#"{"repetition_penalty":1.05}"#
        );
        assert_eq!(
            defaults(ds_config::TtsModel::OmniVoice),
            r#"{"seed":-1,"steps":16}"#
        );
    }
}
