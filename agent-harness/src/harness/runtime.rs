//! Model-agnostic prompting.
//!
//! Execution domains talk to a [`ModelRuntime`] and never to a provider
//! directly, so swapping models (hosted, local, or a mock in tests) is a
//! change of type parameter, not of calling code.

use std::future::Future;

use anyhow::{Context, bail};
use rig_core::{
    completion::{AssistantContent, CompletionModel, CompletionRequest},
    message::Message,
};

/// A single-shot prompt interface over any model backend.
///
/// The returned future is `Send`, so runtimes can be driven from any tokio
/// task. Implementations may use `async fn prompt_agent(...)` directly.
pub trait ModelRuntime: Send + Sync {
    /// Send `payload` under system instructions `preamble` and return the
    /// model's text answer.
    fn prompt_agent(
        &self,
        preamble: &str,
        payload: &str,
    ) -> impl Future<Output = Result<String, anyhow::Error>> + Send;
}

/// Default per-request output budget. Always sent, so providers that require
/// it (e.g. Anthropic) never need a model-specific default.
pub const DEFAULT_MAX_TOKENS: u64 = 4096;

/// A [`ModelRuntime`] over any rig [`CompletionModel`], such as
/// [`crate::ProviderModel`] for runtime-selected hosted providers.
///
/// Statically dispatched: `HostedProviderRuntime<M>` is `Send + Sync + 'static`
/// whenever `M` is.
#[derive(Debug, Clone)]
pub struct HostedProviderRuntime<M> {
    model: M,
    max_tokens: u64,
    temperature: Option<f64>,
}

impl<M: CompletionModel> HostedProviderRuntime<M> {
    pub fn new(model: M) -> Self {
        Self {
            model,
            max_tokens: DEFAULT_MAX_TOKENS,
            temperature: None,
        }
    }

    pub fn with_max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_temperature(mut self, temperature: f64) -> Self {
        self.temperature = Some(temperature);
        self
    }

    pub fn model(&self) -> &M {
        &self.model
    }

    /// Replace the model, returning the previous one.
    pub fn set_model(&mut self, model: M) -> M {
        std::mem::replace(&mut self.model, model)
    }

    fn build_request(&self, preamble: &str, payload: &str) -> CompletionRequest {
        let mut chat_history = Vec::with_capacity(2);
        if !preamble.is_empty() {
            chat_history.push(Message::system(preamble));
        }
        chat_history.push(Message::user(payload));

        CompletionRequest {
            model: None,
            preamble: None,
            chat_history,
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: self.temperature,
            max_tokens: Some(self.max_tokens),
            tool_choice: None,
            additional_params: None,
            output_schema: None,
            record_telemetry_content: false,
        }
    }
}

impl<M: CompletionModel> ModelRuntime for HostedProviderRuntime<M> {
    async fn prompt_agent(&self, preamble: &str, payload: &str) -> Result<String, anyhow::Error> {
        let request = self.build_request(preamble, payload);
        request.validate_message_content()?;
        let response = self
            .model
            .completion(request)
            .await
            .context("completion request failed")?;

        let mut out = String::new();
        for text in response.choice.iter().filter_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text()),
            _ => None,
        }) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
        if out.is_empty() {
            bail!("model returned no text");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::test_utils::{MockCompletionModel, MockTurn};

    /// Scripted runtime with no model behind it.
    struct Echo;

    impl ModelRuntime for Echo {
        async fn prompt_agent(&self, preamble: &str, payload: &str) -> anyhow::Result<String> {
            Ok(format!("{preamble}|{payload}"))
        }
    }

    /// Generic caller: proves execution code compiles against the trait alone.
    async fn run_on<R: ModelRuntime>(runtime: &R) -> anyhow::Result<String> {
        runtime.prompt_agent("sys", "hi").await
    }

    #[test]
    fn runtimes_are_send_sync_static() {
        fn assert_bounds<T: ModelRuntime + 'static>() {}
        assert_bounds::<Echo>();
        assert_bounds::<HostedProviderRuntime<MockCompletionModel>>();
        assert_bounds::<HostedProviderRuntime<crate::ProviderModel>>();
    }

    #[tokio::test]
    async fn prompt_future_can_be_spawned() {
        let runtime = std::sync::Arc::new(Echo);
        let handle = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.prompt_agent("sys", "task").await }
        });
        assert_eq!(handle.await.unwrap().unwrap(), "sys|task");
    }

    #[tokio::test]
    async fn dummy_runtime_through_generic_caller() {
        assert_eq!(run_on(&Echo).await.unwrap(), "sys|hi");
    }

    #[tokio::test]
    async fn hosted_runtime_sends_preamble_and_payload() {
        let model = MockCompletionModel::text("answer");
        let runtime = HostedProviderRuntime::new(model.clone()).with_max_tokens(256);

        assert_eq!(run_on(&runtime).await.unwrap(), "answer");

        let requests = model.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].chat_history,
            vec![Message::system("sys"), Message::user("hi")]
        );
        assert_eq!(requests[0].max_tokens, Some(256));
        assert!(requests[0].tools.is_empty());
    }

    #[tokio::test]
    async fn empty_preamble_is_omitted() {
        let model = MockCompletionModel::text("ok");
        HostedProviderRuntime::new(model.clone())
            .prompt_agent("", "hi")
            .await
            .unwrap();
        assert_eq!(model.requests()[0].chat_history, vec![Message::user("hi")]);
    }

    #[tokio::test]
    async fn provider_errors_propagate() {
        let runtime =
            HostedProviderRuntime::new(MockCompletionModel::new([MockTurn::error("boom")]));
        let err = runtime.prompt_agent("sys", "hi").await.unwrap_err();
        assert!(
            err.to_string().contains("completion request failed"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn switching_models_routes_to_new_model() {
        let first = MockCompletionModel::text("from first");
        let second = MockCompletionModel::text("from second");
        let mut runtime = HostedProviderRuntime::new(first.clone());

        runtime.set_model(second.clone());
        assert_eq!(runtime.prompt_agent("", "x").await.unwrap(), "from second");
        assert_eq!(first.request_count(), 0);
        assert_eq!(second.request_count(), 1);
    }
}
