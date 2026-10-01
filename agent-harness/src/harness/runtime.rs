//! Model-agnostic prompting.
//!
//! Execution domains talk to a runtime trait and never to a provider
//! directly, so swapping models (hosted, local, or a mock in tests) is a
//! change of type parameter, not of calling code.
//!
//! * [`ModelRuntime`]: one prompt in, text out.
//! * [`ChatRuntime`]: a conversation and tool definitions in, one structured
//!   assistant turn (text and/or tool calls) out. Drives
//!   [`crate::harness::AgentLoop`].

use std::future::Future;

use anyhow::{Context, bail};
use rig_core::{
    Model, ProviderError,
    completion::{AssistantContent, CompletionRequest, CompletionResponse, ToolDefinition, Usage},
    driver::Transport,
    message::{Message, ToolCall},
    operation::Completion,
    wire::Wire,
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

/// A multi-turn, tool-aware interface over any model backend.
///
/// Messages use rig's provider-neutral [`Message`] type, so a history built
/// against one model can be replayed to another.
pub trait ChatRuntime: Send + Sync {
    /// Send the whole `history` (oldest first, last message is the newest
    /// user input or tool result) under system instructions `preamble`,
    /// advertising `tools`, and return the model's next turn.
    ///
    /// An empty `preamble` sends no system message.
    fn chat(
        &self,
        preamble: &str,
        history: &[Message],
        tools: &[ToolDefinition],
    ) -> impl Future<Output = Result<AssistantTurn, anyhow::Error>> + Send;
}

/// One assistant turn returned by a [`ChatRuntime`].
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantTurn {
    /// Text, tool calls and reasoning, in the order the model produced them.
    pub content: Vec<AssistantContent>,
    /// Provider id of the assistant message, replayed with the history.
    pub message_id: Option<String>,
    pub usage: Usage,
}

impl AssistantTurn {
    /// A text-only turn, mainly for scripted runtimes in tests.
    pub fn text_reply(text: impl Into<String>) -> Self {
        Self {
            content: vec![AssistantContent::text(text.into())],
            message_id: None,
            usage: Usage::default(),
        }
    }

    /// Text parts joined with newlines; empty if there are none.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for text in self.content.iter().filter_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text()),
            _ => None,
        }) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
        out
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.content.iter().filter_map(|c| match c {
            AssistantContent::ToolCall(call) => Some(call),
            _ => None,
        })
    }

    /// Convert into the history message, keeping every content part
    /// unchanged so provider ids and signatures round-trip.
    pub fn into_message(self) -> Message {
        Message::Assistant {
            id: self.message_id,
            content: self.content,
        }
    }
}

/// A model that answers completion requests.
///
/// Implemented for every rig [`Model`] whose wire performs a [`Completion`]
/// (each provider's `completion(..)` model, and rig's `MockCompletionModel`),
/// and for [`crate::ProviderModel`]. Calls are statically dispatched.
pub trait CompletionBackend: Send + Sync {
    /// Send `request` and return the whole reply.
    fn complete(
        &self,
        request: CompletionRequest,
    ) -> impl Future<Output = Result<CompletionResponse, ProviderError>> + Send;
}

impl<W, T> CompletionBackend for Model<W, T>
where
    W: Wire<Op = Completion>,
    T: Transport<W>,
{
    fn complete(
        &self,
        request: CompletionRequest,
    ) -> impl Future<Output = Result<CompletionResponse, ProviderError>> + Send {
        self.call(request)
    }
}

/// Default per-request output budget. Always sent, so providers that require
/// it (e.g. Anthropic) never need a model-specific default.
pub const DEFAULT_MAX_TOKENS: u64 = 4096;

/// A [`ModelRuntime`] over any [`CompletionBackend`], such as
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

impl<M: CompletionBackend> HostedProviderRuntime<M> {
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

    fn build_request(
        &self,
        preamble: &str,
        history: &[Message],
        tools: &[ToolDefinition],
    ) -> CompletionRequest {
        let system = (!preamble.is_empty()).then(|| Message::system(preamble));
        let mut chat_history = Vec::with_capacity(history.len() + 1);
        chat_history.extend(system);
        chat_history.extend_from_slice(history);

        CompletionRequest {
            model: None,
            chat_history,
            documents: Vec::new(),
            tools: tools.to_vec(),
            temperature: self.temperature,
            max_tokens: Some(self.max_tokens),
            tool_choice: None,
            additional_params: None,
            output_schema: None,
            record_telemetry_content: false,
        }
    }
}

impl<M: CompletionBackend> ChatRuntime for HostedProviderRuntime<M> {
    async fn chat(
        &self,
        preamble: &str,
        history: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<AssistantTurn, anyhow::Error> {
        let request = self.build_request(preamble, history, tools);
        request.validate_message_content()?;
        let response = self
            .model
            .complete(request)
            .await
            .context("completion request failed")?;
        Ok(AssistantTurn {
            content: response.choice,
            message_id: response.message_id,
            usage: response.usage,
        })
    }
}

impl<M: CompletionBackend> ModelRuntime for HostedProviderRuntime<M> {
    async fn prompt_agent(&self, preamble: &str, payload: &str) -> Result<String, anyhow::Error> {
        let turn = self.chat(preamble, &[Message::user(payload)], &[]).await?;
        let text = turn.text();
        if text.is_empty() {
            bail!("model returned no text");
        }
        Ok(text)
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
            HostedProviderRuntime::new(MockCompletionModel::from_turns([MockTurn::error("boom")]));
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

    #[test]
    fn chat_runtimes_are_send_sync_static() {
        fn assert_bounds<T: ChatRuntime + 'static>() {}
        assert_bounds::<HostedProviderRuntime<MockCompletionModel>>();
        assert_bounds::<HostedProviderRuntime<crate::ProviderModel>>();
    }

    #[tokio::test]
    async fn chat_sends_history_and_tools() {
        use crate::{tool::ToolRegistry, tools::Calculator};

        let model = MockCompletionModel::text("ok");
        let runtime = HostedProviderRuntime::new(model.clone());
        let mut registry = ToolRegistry::new();
        registry.register(Calculator).unwrap();
        let history = [
            Message::user("one"),
            Message::assistant("first"),
            Message::user("two"),
        ];

        let turn = runtime
            .chat("sys", &history, &registry.definitions())
            .await
            .unwrap();

        assert_eq!(turn.text(), "ok");
        let request = &model.requests()[0];
        assert_eq!(request.chat_history[0], Message::system("sys"));
        assert_eq!(request.chat_history[1..], history);
        assert_eq!(request.tools, registry.definitions());
    }

    #[tokio::test]
    async fn chat_returns_tool_calls_structured() {
        let model = MockCompletionModel::from_turns([MockTurn::tool_call(
            "c1",
            "calculator",
            serde_json::json!({"op": "add", "a": 1, "b": 2}),
        )]);
        let turn = HostedProviderRuntime::new(model)
            .chat("", &[Message::user("1+2?")], &[])
            .await
            .unwrap();

        let calls: Vec<_> = turn.tool_calls().collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "calculator");
        assert_eq!(turn.text(), "");
    }

    #[tokio::test]
    async fn prompt_agent_rejects_tool_only_reply() {
        let model = MockCompletionModel::from_turns([MockTurn::tool_call(
            "c1",
            "calculator",
            serde_json::json!({}),
        )]);
        let err = HostedProviderRuntime::new(model)
            .prompt_agent("", "hi")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no text"), "{err:#}");
    }

    #[test]
    fn assistant_turn_helpers() {
        let turn = AssistantTurn {
            content: vec![
                AssistantContent::text("a"),
                AssistantContent::tool_call(
                    "c1",
                    rig_core::message::ToolName::new("t").unwrap(),
                    serde_json::json!({}),
                ),
                AssistantContent::text("b"),
            ],
            message_id: Some("msg_1".into()),
            usage: Usage::default(),
        };
        assert_eq!(turn.text(), "a\nb");
        assert_eq!(turn.tool_calls().count(), 1);

        let Message::Assistant { id, content } = turn.clone().into_message() else {
            panic!("expected assistant message");
        };
        assert_eq!(id.as_deref(), Some("msg_1"));
        assert_eq!(content, turn.content);
    }
}
