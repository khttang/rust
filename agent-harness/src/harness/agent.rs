//! The tool-calling agent loop.
//!
//! prompt → model → tool calls → tool results → model … until the model
//! answers without requesting tools, or `max_turns` is exhausted.

use rig_core::{
    completion::Usage,
    message::{Message, ToolCall, UserContent},
    tool::ToolExecutionError,
};

use crate::{
    error::HarnessError,
    harness::{ChatRuntime, Conversation},
    observer::{NoopObserver, Observer},
    policy::{Approval, ApprovalPolicy, AutoApprove},
    tool::ToolRegistry,
};

/// Default cap on model round-trips per [`AgentLoop::run`].
pub const DEFAULT_MAX_TURNS: usize = 8;

/// Result of a successful [`AgentLoop::run`].
#[derive(Debug, Clone, PartialEq)]
pub struct RunOutcome {
    /// Text of the final assistant turn, parts joined with newlines.
    pub output: String,
    /// Number of model round-trips used.
    pub turns: usize,
    /// Number of tool calls requested by the model (approved or not).
    pub tool_calls: usize,
    /// Token usage summed over every turn.
    pub usage: Usage,
}

/// A generic tool-calling agent loop.
///
/// * `R`: the [`ChatRuntime`] (any model, or a scripted runtime in tests).
/// * `P`: the human-in-the-loop [`ApprovalPolicy`] gating tool calls.
/// * `O`: the lifecycle [`Observer`].
///
/// All three are statically dispatched. `AgentLoop` is `Send + Sync` whenever
/// they are, so it can be shared across tasks behind an `Arc`.
pub struct AgentLoop<R, P = AutoApprove, O = NoopObserver> {
    runtime: R,
    tools: ToolRegistry,
    policy: P,
    observer: O,
    max_turns: usize,
}

impl<R: ChatRuntime> AgentLoop<R> {
    /// A loop that auto-approves every tool call and observes nothing.
    pub fn new(runtime: R, tools: ToolRegistry) -> Self {
        Self {
            runtime,
            tools,
            policy: AutoApprove,
            observer: NoopObserver,
            max_turns: DEFAULT_MAX_TURNS,
        }
    }
}

impl<R, P, O> AgentLoop<R, P, O>
where
    R: ChatRuntime,
    P: ApprovalPolicy,
    O: Observer,
{
    pub fn with_policy<P2: ApprovalPolicy>(self, policy: P2) -> AgentLoop<R, P2, O> {
        AgentLoop {
            runtime: self.runtime,
            tools: self.tools,
            policy,
            observer: self.observer,
            max_turns: self.max_turns,
        }
    }

    pub fn with_observer<O2: Observer>(self, observer: O2) -> AgentLoop<R, P, O2> {
        AgentLoop {
            runtime: self.runtime,
            tools: self.tools,
            policy: self.policy,
            observer,
            max_turns: self.max_turns,
        }
    }

    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns;
        self
    }

    pub fn runtime(&self) -> &R {
        &self.runtime
    }

    /// Mutable access, e.g. to switch models. Conversations live outside
    /// the loop, so they continue on the new model.
    pub fn runtime_mut(&mut self) -> &mut R {
        &mut self.runtime
    }

    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    pub fn max_turns(&self) -> usize {
        self.max_turns
    }

    /// Append `prompt` to `conversation` and drive the loop until the model
    /// answers without requesting tools, or `max_turns` is exhausted.
    ///
    /// On success every exchanged message (assistant turns and tool results)
    /// is recorded, so the conversation can continue with another `run`. On
    /// error the conversation is restored to its state before the call, so a
    /// failed run never leaves a half-finished turn in the history.
    pub async fn run(
        &self,
        preamble: &str,
        conversation: &mut Conversation,
        prompt: impl Into<String>,
    ) -> Result<RunOutcome, HarnessError> {
        let start = conversation.len();
        let result = self.drive(preamble, conversation, prompt.into()).await;
        if result.is_err() {
            conversation.truncate(start);
        }
        result
    }

    async fn drive(
        &self,
        preamble: &str,
        conversation: &mut Conversation,
        prompt: String,
    ) -> Result<RunOutcome, HarnessError> {
        conversation.push(Message::user(prompt));
        let tools = self.tools.definitions();
        let mut usage = Usage::default();
        let mut tool_calls = 0;

        for turn in 1..=self.max_turns {
            self.observer.on_turn_start(turn);

            let reply = self
                .runtime
                .chat(preamble, conversation.messages(), &tools)
                .await?;
            self.observer.on_model_response(turn, &reply);
            usage += reply.usage;

            if reply.content.is_empty() {
                return Err(HarnessError::EmptyResponse);
            }
            let calls: Vec<ToolCall> = reply.tool_calls().cloned().collect();
            let text = reply.text();
            conversation.push(reply.into_message());

            if calls.is_empty() {
                self.observer.on_final_answer(turn, &text);
                return Ok(RunOutcome {
                    output: text,
                    turns: turn,
                    tool_calls,
                    usage,
                });
            }

            tool_calls += calls.len();
            let mut results = Vec::with_capacity(calls.len());
            // Sequential on purpose: deterministic ordering of side effects.
            for call in &calls {
                results.push(self.dispatch(call).await);
            }
            conversation.push(Message::User { content: results });
        }

        Err(HarnessError::MaxTurnsExceeded(self.max_turns))
    }

    /// Approve, validate and execute one tool call, producing the tool-result
    /// content for the model. Never fails: every problem becomes feedback.
    async fn dispatch(&self, call: &ToolCall) -> UserContent {
        let approval = self.policy.review(call).await;
        self.observer.on_tool_call(call, &approval);

        let result = match approval {
            Approval::Approved => {
                self.tools
                    .execute(call.function.name.as_str(), call.function.arguments.clone())
                    .await
            }
            Approval::Denied { reason } => Err(ToolExecutionError::refused(format!(
                "tool call denied: {reason}"
            ))),
        };
        self.observer.on_tool_result(call, &result);

        let content = match result {
            Ok(output) => output.into_content(),
            Err(error) => error.model_output().clone().into_content(),
        };
        UserContent::ToolResult(call.result(content))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use rig_core::{
        completion::{AssistantContent, CompletionRequest, ToolDefinition},
        message::{ToolName, ToolResultContent},
        test_utils::{MockCompletionModel, MockTurn},
        tool::ToolOutput,
    };
    use serde_json::json;

    use super::*;
    use crate::{
        harness::{AssistantTurn, HostedProviderRuntime},
        policy::AllowList,
        tools::{Calculator, WordCount},
    };

    type MockRuntime = HostedProviderRuntime<MockCompletionModel>;

    fn registry<const N: usize>(tools: [fn(&mut ToolRegistry); N]) -> ToolRegistry {
        let mut r = ToolRegistry::new();
        for add in tools {
            add(&mut r);
        }
        r
    }

    fn calc(r: &mut ToolRegistry) {
        r.register(Calculator).unwrap();
    }

    fn words(r: &mut ToolRegistry) {
        r.register(WordCount).unwrap();
    }

    fn agent(model: &MockCompletionModel, tools: ToolRegistry) -> AgentLoop<MockRuntime> {
        AgentLoop::new(HostedProviderRuntime::new(model.clone()), tools)
    }

    fn tool_results(message: &Message) -> Vec<(&str, &[ToolResultContent])> {
        match message {
            Message::User { content } => content
                .iter()
                .filter_map(|c| match c {
                    UserContent::ToolResult(r) => Some((r.name.as_str(), r.content.as_slice())),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn last_message(request: &CompletionRequest) -> &Message {
        request.chat_history.last().expect("non-empty history")
    }

    #[test]
    fn agent_loop_is_send_sync_static() {
        fn assert_bounds<T: Send + Sync + 'static>() {}
        assert_bounds::<AgentLoop<MockRuntime>>();
        assert_bounds::<AgentLoop<MockRuntime, AllowList, NoopObserver>>();
        assert_bounds::<AgentLoop<HostedProviderRuntime<crate::ProviderModel>>>();
    }

    #[tokio::test]
    async fn plain_text_answer_finishes_in_one_turn() {
        let model = MockCompletionModel::text("hello there");
        let agent = agent(&model, registry([calc]));
        let mut conversation = Conversation::new();

        let outcome = agent.run("be nice", &mut conversation, "hi").await.unwrap();

        assert_eq!(outcome.output, "hello there");
        assert_eq!((outcome.turns, outcome.tool_calls), (1, 0));
        assert_eq!(conversation.len(), 2);

        let requests = model.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].chat_history,
            vec![Message::system("be nice"), Message::user("hi")]
        );
        assert_eq!(requests[0].tools.len(), 1);
    }

    #[tokio::test]
    async fn tool_call_round_trip() {
        let model = MockCompletionModel::from_turns([
            MockTurn::tool_call("call-1", "calculator", json!({"op": "add", "a": 2, "b": 3})),
            MockTurn::text("The answer is 5."),
        ]);
        let agent = agent(&model, registry([calc, words]));
        let mut conversation = Conversation::new();

        let outcome = agent
            .run("", &mut conversation, "what is 2+3?")
            .await
            .unwrap();

        assert_eq!(outcome.output, "The answer is 5.");
        assert_eq!((outcome.turns, outcome.tool_calls), (2, 1));
        // user, assistant(tool call), user(tool result), assistant(text)
        assert_eq!(conversation.len(), 4);

        let requests = model.requests();
        let results = tool_results(last_message(&requests[1]));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "calculator");
        assert_eq!(results[0].1[0].as_json(), Some(&json!({"result": 5.0})));

        // The tool result must answer the exact call the model made.
        let Message::Assistant { content, .. } = &conversation.messages()[1] else {
            panic!("expected assistant message");
        };
        let AssistantContent::ToolCall(call) = &content[0] else {
            panic!("expected tool call");
        };
        let Message::User { content } = &conversation.messages()[2] else {
            panic!("expected user message");
        };
        let UserContent::ToolResult(result) = &content[0] else {
            panic!("expected tool result");
        };
        assert_eq!(result.call, call.id);
    }

    #[tokio::test]
    async fn tool_errors_are_fed_back_not_fatal() {
        let model = MockCompletionModel::from_turns([
            MockTurn::tool_call("c1", "calculator", json!({"op": "div", "a": 1, "b": 0})),
            MockTurn::tool_call("c2", "no_such_tool", json!({})),
            MockTurn::tool_call("c3", "calculator", json!({"op": "add"})),
            MockTurn::text("gave up"),
        ]);
        let agent = agent(&model, registry([calc]));

        let outcome = agent.run("", &mut Conversation::new(), "go").await.unwrap();
        assert_eq!(outcome.output, "gave up");
        assert_eq!(outcome.tool_calls, 3);

        let requests = model.requests();
        let text = |i: usize| {
            tool_results(last_message(&requests[i]))[0].1[0]
                .as_text()
                .unwrap()
                .to_owned()
        };
        assert_eq!(text(1), "division by zero");
        assert!(text(2).contains("unknown tool `no_such_tool`"));
        assert!(text(3).contains("missing required argument"));
    }

    #[tokio::test]
    async fn denied_calls_are_not_executed() {
        #[derive(Default)]
        struct Counter {
            results: AtomicUsize,
            denied: AtomicUsize,
        }
        impl Observer for &'static Counter {
            fn on_tool_call(&self, _: &ToolCall, approval: &Approval) {
                if !approval.is_approved() {
                    self.denied.fetch_add(1, Ordering::SeqCst);
                }
            }
            fn on_tool_result(&self, _: &ToolCall, r: &Result<ToolOutput, ToolExecutionError>) {
                if r.is_ok() {
                    self.results.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        let counter: &'static Counter = Box::leak(Box::default());

        let model = MockCompletionModel::from_turns([
            MockTurn::tool_call("c1", "calculator", json!({"op": "add", "a": 1, "b": 1})),
            MockTurn::text("ok"),
        ]);
        let agent = agent(&model, registry([calc]))
            .with_policy(AllowList::new(["word_count"]))
            .with_observer(counter);

        agent.run("", &mut Conversation::new(), "go").await.unwrap();

        assert_eq!(counter.denied.load(Ordering::SeqCst), 1);
        assert_eq!(counter.results.load(Ordering::SeqCst), 0);
        let requests = model.requests();
        let results = tool_results(last_message(&requests[1]));
        let text = results[0].1[0].as_text().unwrap();
        assert!(text.contains("denied"), "{text}");
        assert!(text.contains("allow list"), "{text}");
    }

    #[tokio::test]
    async fn max_turns_is_enforced_and_rolled_back() {
        let looping = (0..5)
            .map(|i| MockTurn::tool_call(format!("c{i}"), "word_count", json!({"text": "again"})));
        let model = MockCompletionModel::from_turns(looping);
        let agent = agent(&model, registry([words])).with_max_turns(3);
        let mut conversation = Conversation::new();
        conversation.push(Message::user("earlier"));
        conversation.push(Message::assistant("reply"));
        let before = conversation.clone();

        let err = agent.run("", &mut conversation, "loop").await.unwrap_err();

        assert!(matches!(err, HarnessError::MaxTurnsExceeded(3)));
        assert_eq!(model.request_count(), 3);
        assert_eq!(conversation, before);
    }

    #[tokio::test]
    async fn runtime_errors_propagate_and_roll_back() {
        let model = MockCompletionModel::from_turns([MockTurn::error("boom")]);
        let agent = agent(&model, ToolRegistry::new());
        let mut conversation = Conversation::new();

        let err = agent.run("", &mut conversation, "hi").await.unwrap_err();

        assert!(matches!(err, HarnessError::Runtime(_)));
        assert!(
            err.to_string().contains("completion request failed"),
            "{err}"
        );
        assert!(conversation.is_empty());
    }

    #[tokio::test]
    async fn conversation_carries_across_runs() {
        let model =
            MockCompletionModel::from_turns([MockTurn::text("first"), MockTurn::text("second")]);
        let agent = agent(&model, ToolRegistry::new());
        let mut conversation = Conversation::new();

        agent.run("", &mut conversation, "one").await.unwrap();
        agent.run("", &mut conversation, "two").await.unwrap();

        assert_eq!(
            model.requests()[1].chat_history,
            vec![
                Message::user("one"),
                Message::assistant("first"),
                Message::user("two")
            ]
        );
    }

    #[tokio::test]
    async fn switching_models_keeps_conversation_and_tools() {
        let first = MockCompletionModel::text("from first");
        let second = MockCompletionModel::text("from second");
        let mut agent = agent(&first, registry([calc]));
        let mut conversation = Conversation::new();

        agent.run("", &mut conversation, "one").await.unwrap();
        agent.runtime_mut().set_model(second.clone());
        let outcome = agent.run("", &mut conversation, "two").await.unwrap();

        assert_eq!(outcome.output, "from second");
        assert_eq!(first.request_count(), 1);
        let req = &second.requests()[0];
        assert_eq!(
            req.chat_history,
            vec![
                Message::user("one"),
                Message::assistant("from first"),
                Message::user("two")
            ]
        );
        assert_eq!(req.tools.len(), 1);
    }

    /// A runtime with no rig model behind it: replays scripted turns and
    /// records how much history each call saw.
    struct Scripted {
        turns: Mutex<VecDeque<AssistantTurn>>,
        seen: Mutex<Vec<(usize, usize)>>,
    }

    impl Scripted {
        fn new(turns: impl IntoIterator<Item = AssistantTurn>) -> Self {
            Self {
                turns: Mutex::new(turns.into_iter().collect()),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    impl ChatRuntime for Scripted {
        async fn chat(
            &self,
            _preamble: &str,
            history: &[Message],
            tools: &[ToolDefinition],
        ) -> anyhow::Result<AssistantTurn> {
            self.seen.lock().unwrap().push((history.len(), tools.len()));
            self.turns
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("script exhausted"))
        }
    }

    #[tokio::test]
    async fn generic_over_any_chat_runtime() {
        let mut call = AssistantTurn::text_reply("");
        call.content = vec![AssistantContent::tool_call(
            "t1",
            ToolName::new("word_count").unwrap(),
            json!({"text": "a b c"}),
        )];
        let runtime = Scripted::new([call, AssistantTurn::text_reply("three words")]);
        let agent = AgentLoop::new(runtime, registry([words]));

        let outcome = agent
            .run("", &mut Conversation::new(), "count")
            .await
            .unwrap();

        assert_eq!(outcome.output, "three words");
        // First call sees [user]; second sees [user, assistant, tool result].
        assert_eq!(*agent.runtime().seen.lock().unwrap(), vec![(1, 1), (3, 1)]);
    }

    #[tokio::test]
    async fn empty_response_is_an_error() {
        let mut empty = AssistantTurn::text_reply("");
        empty.content.clear();
        let agent = AgentLoop::new(Scripted::new([empty]), ToolRegistry::new());
        let mut conversation = Conversation::new();

        let err = agent.run("", &mut conversation, "hi").await.unwrap_err();

        assert!(matches!(err, HarnessError::EmptyResponse));
        assert!(conversation.is_empty());
    }
}
