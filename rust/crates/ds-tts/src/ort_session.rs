//! One provider-selection path for every ONNX TTS model.

use std::path::Path;

use ort::session::Session;
use ort::session::builder::SessionBuilder;

pub(crate) struct OrtSessions {
    model: ds_config::TtsModel,
    preference: String,
    realized: Option<ds_config::RealizedProvider>,
}

impl OrtSessions {
    pub(crate) fn from_preference(model: ds_config::TtsModel, preference: &str) -> Self {
        Self {
            model,
            preference: preference.to_string(),
            realized: None,
        }
    }

    pub(crate) fn from_realized(
        model: ds_config::TtsModel,
        provider: ds_config::RealizedProvider,
    ) -> Self {
        Self {
            model,
            preference: provider.as_str().to_string(),
            realized: Some(provider),
        }
    }

    pub(crate) fn builder(
        &mut self,
    ) -> Result<(SessionBuilder, ds_config::RealizedProvider), String> {
        let requested = self.realized.unwrap_or_else(|| self.requested_provider());
        let (builder, provider) = provider_builder(requested)?;
        if let Some(expected) = self.realized {
            if provider != expected {
                return Err(format!(
                    "ORT provider changed from {} to {} while loading {}",
                    expected.as_str(),
                    provider.as_str(),
                    self.model.as_str()
                ));
            }
        } else {
            self.realized = Some(provider);
        }
        Ok((builder, provider))
    }

    pub(crate) fn load_file(&mut self, path: &Path) -> Result<Session, String> {
        self.builder()?
            .0
            .commit_from_file(path)
            .map_err(|error| format!("ort load {}: {error}", path.display()))
    }

    pub(crate) fn provider(&self) -> ds_config::RealizedProvider {
        self.realized.unwrap_or(ds_config::RealizedProvider::Cpu)
    }

    fn requested_provider(&self) -> ds_config::RealizedProvider {
        let descriptor = self.model.descriptor();
        if descriptor.wants_cuda(&self.preference) {
            return ds_config::RealizedProvider::Cuda;
        }
        #[cfg(target_os = "macos")]
        if descriptor.supports_provider(ds_config::Provider::OrtCoreMl)
            && (self
                .preference
                .eq_ignore_ascii_case(ds_config::Provider::OrtCoreMl.as_str())
                || (self.preference.eq_ignore_ascii_case("auto")
                    && std::env::var_os("DONTSPEAK_FULL_DUPLEX").is_some()))
        {
            return ds_config::RealizedProvider::CoreMl;
        }
        ds_config::RealizedProvider::Cpu
    }
}

fn provider_builder(
    requested: ds_config::RealizedProvider,
) -> Result<(SessionBuilder, ds_config::RealizedProvider), String> {
    #[cfg(target_os = "macos")]
    if requested == ds_config::RealizedProvider::CoreMl {
        use ort::execution_providers::CoreMLExecutionProvider;
        match (|| -> ort::Result<_> {
            Ok(Session::builder()?
                .with_execution_providers([CoreMLExecutionProvider::default()
                    .build()
                    .error_on_failure()])?)
        })() {
            Ok(builder) => return Ok((builder, ds_config::RealizedProvider::CoreMl)),
            Err(error) => eprintln!(
                "dontspeak/helper: Core ML EP registration failed — running on CPU: {error}"
            ),
        }
    }
    ds_model::cuda_session_builder(requested == ds_config::RealizedProvider::Cuda)
}

pub(crate) fn load_with_fallback<T>(
    label: &str,
    load: impl Fn(&str) -> Result<T, String>,
) -> Result<T, String> {
    let preference = std::env::var("DONTSPEAK_PROVIDER").unwrap_or_else(|_| "auto".into());
    match load(&preference) {
        Ok(value) => Ok(value),
        Err(error) if !preference.eq_ignore_ascii_case("cpu") => {
            eprintln!(
                "dontspeak/{label}: provider '{preference}' failed ({error}); falling back to CPU"
            );
            load("cpu")
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_session_loading_uses_the_registry_provider_contract() {
        assert_eq!(
            OrtSessions::from_preference(ds_config::TtsModel::Chatterbox, "cuda")
                .requested_provider(),
            ds_config::RealizedProvider::Cuda
        );
        assert_eq!(
            OrtSessions::from_preference(ds_config::TtsModel::OmniVoice, "cuda")
                .requested_provider(),
            ds_config::RealizedProvider::Cpu
        );
    }
}
