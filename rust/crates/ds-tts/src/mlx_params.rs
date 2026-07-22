//! Wire form of resolved TTS params for the MLX shim ABI (the `params_json` slot of
//! `ds_mlx_tts_synthesize2`). Pure — no dlopen — so its tests run on every host.
//! The Rust↔Swift link is hand-maintained mirror discipline, not an automatic
//! cross-language check: this module's test pins each model's default serialization
//! LITERALLY, `TtsAbiTests.swift` pins the Swift key-list mirror
//! (`shim.swift` `ttsParamMirror`) literally, and a registry edit must update
//! registry, mirror, and both literal pins together.

/// Serialize the resolved params the Swift shim decodes. Defensive re-scope: only
/// keys DECLARED for `model` cross the ABI — the shim classifies exactly the declared
/// set (applied vs explicitly-ignored) and logs anything else as unknown. Values
/// serialize DIRECTLY (serde's f32 path, shortest form: 1.05 stays "1.05") — routing
/// through `serde_json::Value` would widen f32 to f64 and leak representation noise.
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

    /// Serializer scoping only (resolve_params and mlx_params_json both derive from
    /// the registry, so serialized == declared is true by construction): pins that no
    /// EXTRA key can cross the ABI. The real registry-drift pin is the literal test
    /// below.
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

    /// THE registry pin (hermetic, every host): each model's default wire payload as a
    /// literal. Any registry change — new param, renamed key, changed default — fails
    /// here, which is the prompt to update the Swift `ttsParamMirror` and its
    /// `TtsAbiTests` pins in the same change.
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
