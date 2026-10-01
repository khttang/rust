//! Runtime-selectable model providers.
//!
//! A [`ModelSpec`] (`provider[:model]`, e.g. `anthropic:claude-sonnet-5` or
//! `ollama:llama3.2:3b`) names a provider and model. [`ProviderModel::from_env`]
//! turns a spec into a [`CompletionBackend`], reading credentials from the
//! provider's standard environment variables via rig.
//!
//! [`ProviderModel`] dispatches over a closed enum of rig provider models, so
//! switching providers at runtime needs no trait objects or boxed futures.

use std::{fmt, str::FromStr};

use rig_core::{
    Model, ProviderError as RigProviderError,
    client::EnvError,
    completion::{CompletionRequest, CompletionResponse},
    providers::{
        anthropic::{self, Anthropic},
        gemini::{self, Gemini},
        ollama::{self, Ollama},
        openai::{self, OpenAI, responses_api},
        openrouter,
    },
};

use crate::harness::{CompletionBackend, runtime::ModelIdentity};

/// A supported LLM provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Anthropic,
    OpenAI,
    Gemini,
    Ollama,
    OpenRouter,
}

impl Provider {
    pub const ALL: [Provider; 5] = [
        Provider::Anthropic,
        Provider::OpenAI,
        Provider::Gemini,
        Provider::Ollama,
        Provider::OpenRouter,
    ];

    /// Identifier used in a [`ModelSpec`].
    pub const fn name(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::OpenAI => "openai",
            Provider::Gemini => "gemini",
            Provider::Ollama => "ollama",
            Provider::OpenRouter => "openrouter",
        }
    }

    /// Environment variable holding the API key, if the provider needs one.
    pub const fn api_key_env(self) -> Option<&'static str> {
        match self {
            Provider::Anthropic => Some("ANTHROPIC_API_KEY"),
            Provider::OpenAI => Some("OPENAI_API_KEY"),
            Provider::Gemini => Some("GEMINI_API_KEY"),
            Provider::Ollama => None,
            Provider::OpenRouter => Some("OPENROUTER_API_KEY"),
        }
    }

    /// Model used when a spec names only the provider. `None` means the
    /// provider has no sensible default and the model must be given.
    pub const fn default_model(self) -> Option<&'static str> {
        match self {
            Provider::Anthropic => Some("claude-sonnet-5"),
            Provider::OpenAI => Some("gpt-5.6"),
            Provider::Gemini => Some("gemini-2.5-flash"),
            Provider::Ollama | Provider::OpenRouter => None,
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Provider {
    type Err = ProviderError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        Provider::ALL
            .into_iter()
            .find(|p| p.name().eq_ignore_ascii_case(s))
            .ok_or_else(|| ProviderError::UnknownProvider(s.to_owned()))
    }
}

/// A provider plus model id, written `provider[:model]`.
///
/// Only the first `:` separates the two, so model ids that contain colons
/// (common with Ollama tags) are preserved.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelSpec {
    pub provider: Provider,
    pub model: String,
}

impl ModelSpec {
    pub fn new(provider: Provider, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }
}

impl fmt::Display for ModelSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.provider, self.model)
    }
}

impl FromStr for ModelSpec {
    type Err = ProviderError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (provider, model) = match s.trim().split_once(':') {
            Some((p, m)) => (p.parse::<Provider>()?, m.trim()),
            None => (s.parse::<Provider>()?, ""),
        };
        let model = if model.is_empty() {
            provider
                .default_model()
                .ok_or(ProviderError::ModelRequired(provider))?
        } else {
            model
        };
        Ok(ModelSpec::new(provider, model))
    }
}

/// Errors from parsing a spec or constructing a provider client.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error(
        "unknown provider `{0}` (expected one of: anthropic, openai, gemini, ollama, openrouter)"
    )]
    UnknownProvider(String),

    #[error("provider `{0}` has no default model; use `{0}:<model>`")]
    ModelRequired(Provider),

    #[error("failed to configure {provider} client: {source}")]
    Client {
        provider: Provider,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

/// One rig completion model per provider. OpenAI uses the Responses API
/// (`POST /v1/responses`, the endpoint `openshell-policy.yaml` allows);
/// OpenRouter uses its OpenAI-compatible Chat Completions endpoint.
#[derive(Clone)]
enum Backend {
    Anthropic(Model<anthropic::wire::Messages>),
    OpenAI(Model<responses_api::wire::Responses>),
    Gemini(Model<gemini::completion::GenerateContent>),
    Ollama(Model<ollama::Chat>),
    OpenRouter(Model<openai::wire::Chat>),
}

/// A completion model whose provider is chosen at runtime.
#[derive(Clone)]
pub struct ProviderModel {
    spec: ModelSpec,
    backend: Backend,
}

impl ProviderModel {
    /// Build the model named by `spec`, reading credentials and base URLs
    /// from the provider's standard environment variables.
    pub fn from_env(spec: ModelSpec) -> Result<Self, ProviderError> {
        let provider = spec.provider;
        let env = |e: EnvError| ProviderError::Client {
            provider,
            source: Box::new(e),
        };

        let id = spec.model.as_str();
        let backend = match provider {
            Provider::Anthropic => {
                Backend::Anthropic(Anthropic::from_env().map_err(env)?.completion(id))
            }
            Provider::OpenAI => Backend::OpenAI(OpenAI::from_env().map_err(env)?.responses(id)),
            Provider::Gemini => Backend::Gemini(Gemini::from_env().map_err(env)?.completion(id)),
            Provider::Ollama => Backend::Ollama(Ollama::from_env().map_err(env)?.completion(id)),
            Provider::OpenRouter => {
                Backend::OpenRouter(openrouter::from_env().map_err(env)?.chat(id))
            }
        };
        Ok(Self { spec, backend })
    }

    pub fn spec(&self) -> &ModelSpec {
        &self.spec
    }
}

impl fmt::Debug for ProviderModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderModel")
            .field("spec", &self.spec)
            .finish()
    }
}

/// Forward a call to whichever provider model is active.
macro_rules! dispatch {
    ($backend:expr, $m:ident => $body:expr) => {
        match $backend {
            Backend::Anthropic($m) => $body,
            Backend::OpenAI($m) => $body,
            Backend::Gemini($m) => $body,
            Backend::Ollama($m) => $body,
            Backend::OpenRouter($m) => $body,
        }
    };
}

impl CompletionBackend for ProviderModel {
    fn identity(&self) -> ModelIdentity {
        ModelIdentity::new(
            Some(self.spec.provider.name().to_owned()),
            Some(self.spec.model.clone()),
        )
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, RigProviderError> {
        dispatch!(&self.backend, m => m.complete(request).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_names_case_insensitively() {
        assert_eq!(
            "Anthropic".parse::<Provider>().unwrap(),
            Provider::Anthropic
        );
        assert_eq!(" openai ".parse::<Provider>().unwrap(), Provider::OpenAI);
        assert!(matches!(
            "bogus".parse::<Provider>(),
            Err(ProviderError::UnknownProvider(p)) if p == "bogus"
        ));
    }

    #[test]
    fn provider_names_round_trip() {
        for p in Provider::ALL {
            assert_eq!(p.name().parse::<Provider>().unwrap(), p);
        }
    }

    #[test]
    fn parses_full_spec() {
        let spec: ModelSpec = "openai:gpt-5.5".parse().unwrap();
        assert_eq!(spec, ModelSpec::new(Provider::OpenAI, "gpt-5.5"));
        assert_eq!(spec.to_string(), "openai:gpt-5.5");
    }

    #[test]
    fn keeps_colons_in_model_id() {
        let spec: ModelSpec = "ollama:llama3.2:3b".parse().unwrap();
        assert_eq!(spec, ModelSpec::new(Provider::Ollama, "llama3.2:3b"));
    }

    #[test]
    fn provider_only_uses_default_model() {
        let spec: ModelSpec = "anthropic".parse().unwrap();
        assert_eq!(spec, ModelSpec::new(Provider::Anthropic, "claude-sonnet-5"));
        let spec: ModelSpec = "gemini:".parse().unwrap();
        assert_eq!(spec.model, "gemini-2.5-flash");
    }

    #[test]
    fn provider_without_default_requires_model() {
        for s in ["ollama", "openrouter:"] {
            assert!(
                matches!(s.parse::<ModelSpec>(), Err(ProviderError::ModelRequired(_))),
                "{s}"
            );
        }
    }

    #[test]
    fn rejects_unknown_provider_in_spec() {
        assert!(matches!(
            "claude-sonnet-5".parse::<ModelSpec>(),
            Err(ProviderError::UnknownProvider(_))
        ));
    }

    #[test]
    fn ollama_builds_without_credentials() {
        let model = ProviderModel::from_env("ollama:llama3.2".parse().unwrap()).unwrap();
        assert_eq!(model.spec().to_string(), "ollama:llama3.2");
    }

    #[test]
    fn provider_model_is_send_sync_static() {
        fn assert_bounds<T: CompletionBackend + Clone + Send + Sync + 'static>() {}
        assert_bounds::<ProviderModel>();
    }
}
