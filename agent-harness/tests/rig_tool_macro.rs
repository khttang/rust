//! Tools written with rig's `#[rig_tool]` macro work with the harness's
//! `ToolRegistry` and `AgentLoop`, using only the public API a downstream
//! tool author would use.

use std::{collections::VecDeque, sync::Mutex};

use agent_harness::{
    AgentLoop, AssistantTurn, ChatRuntime, Conversation, DynTool, ToolRegistry,
    rig_core::{
        completion::{AssistantContent, ToolDefinition},
        message::{Message, ToolName, ToolResultContent, UserContent},
        rig_tool,
        tool::ToolExecutionError,
    },
};
use serde_json::{Value, json};

#[rig_tool(
    description = "Multiply two integers",
    params(a = "Left factor", b = "Right factor")
)]
fn multiply(a: i64, b: i64) -> Result<i64, ToolExecutionError> {
    a.checked_mul(b)
        .ok_or_else(|| ToolExecutionError::other("overflow"))
}

#[rig_tool(description = "Greet someone, optionally with a title")]
async fn greet(name: String, title: Option<String>) -> Result<String, ToolExecutionError> {
    Ok(match title {
        Some(t) => format!("Hello, {t} {name}"),
        None => format!("Hello, {name}"),
    })
}

#[rig_tool(name = "search-docs", description = "Search the docs")]
fn search_docs_impl(query: String) -> Result<String, ToolExecutionError> {
    Ok(format!("results for {query}"))
}

fn registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Multiply).unwrap();
    r.register(Greet).unwrap();
    r.register(SearchDocsImpl).unwrap();
    r
}

fn definition(r: &ToolRegistry, name: &str) -> ToolDefinition {
    r.definitions()
        .into_iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no definition for {name}"))
}

fn required(schema: &Value) -> Vec<&str> {
    let mut v: Vec<&str> = schema["required"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    v.sort_unstable();
    v
}

#[test]
fn macro_tools_meet_registry_bounds() {
    fn assert_dyn_tool<T: DynTool + Send + Sync + 'static>() {}
    assert_dyn_tool::<Multiply>();
    assert_dyn_tool::<Greet>();
    assert_dyn_tool::<SearchDocsImpl>();
}

#[test]
fn definitions_carry_name_description_and_schema() {
    let r = registry();

    let m = definition(&r, "multiply");
    assert_eq!(m.description, "Multiply two integers");
    assert_eq!(m.parameters["type"], "object");
    assert_eq!(required(&m.parameters), ["a", "b"]);
    assert_eq!(
        m.parameters["properties"]["a"]["description"],
        "Left factor"
    );

    // Option<T> parameters are advertised as optional.
    let g = definition(&r, "greet");
    assert_eq!(required(&g.parameters), ["name"]);
    assert!(g.parameters["properties"].get("title").is_some());

    // An explicit name overrides the function name.
    assert_eq!(definition(&r, "search-docs").description, "Search the docs");
}

#[test]
fn duplicate_macro_tool_is_rejected() {
    let mut r = registry();
    assert!(r.register(Multiply).is_err());
}

#[tokio::test]
async fn executes_sync_and_async_macro_tools() {
    let r = registry();

    let out = r
        .execute("multiply", json!({"a": 6, "b": 7}))
        .await
        .unwrap();
    assert_eq!(out.as_json(), Some(&json!(42)));

    // `String` outputs become literal text; other `Serialize` outputs JSON.
    let out = r.execute("greet", json!({"name": "Ada"})).await.unwrap();
    assert_eq!(out.as_text(), Some("Hello, Ada"));

    let out = r
        .execute("greet", json!({"name": "Ada", "title": "Dr."}))
        .await
        .unwrap();
    assert_eq!(out.as_text(), Some("Hello, Dr. Ada"));
}

#[tokio::test]
async fn registry_validation_applies_to_macro_tools() {
    let r = registry();

    let missing = r.execute("multiply", json!({"a": 6})).await.unwrap_err();
    assert!(missing.to_string().contains('b'), "{missing}");

    let wrong_type = r
        .execute("multiply", json!({"a": "six", "b": 7}))
        .await
        .unwrap_err();
    assert!(!wrong_type.to_string().is_empty());

    let not_object = r.execute("multiply", json!([6, 7])).await.unwrap_err();
    assert!(not_object.to_string().contains("object"), "{not_object}");
}

#[tokio::test]
async fn tool_errors_surface_as_tool_execution_errors() {
    let r = registry();
    let err = r
        .execute("multiply", json!({"a": i64::MAX, "b": 2}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("overflow"), "{err}");
}

/// Replays scripted assistant turns; records the tool names offered.
struct Scripted {
    turns: Mutex<VecDeque<AssistantTurn>>,
    offered: Mutex<Vec<Vec<String>>>,
}

impl ChatRuntime for Scripted {
    async fn chat(
        &self,
        _preamble: &str,
        _history: &[Message],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<AssistantTurn> {
        self.offered
            .lock()
            .unwrap()
            .push(tools.iter().map(|t| t.name.clone()).collect());
        self.turns
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("script exhausted"))
    }
}

fn tool(name: &str) -> ToolName {
    ToolName::new(name).unwrap()
}

#[tokio::test]
async fn agent_loop_runs_macro_tools() {
    let mut call = AssistantTurn::text_reply("");
    call.content = vec![
        AssistantContent::tool_call("c1", tool("multiply"), json!({"a": 3, "b": 4})),
        AssistantContent::tool_call("c2", tool("search-docs"), json!({"query": "loop"})),
    ];
    let runtime = Scripted {
        turns: Mutex::new([call, AssistantTurn::text_reply("done")].into()),
        offered: Mutex::new(Vec::new()),
    };
    let agent = AgentLoop::new(runtime, registry());
    let mut conversation = Conversation::new();

    let outcome = agent.run("", &mut conversation, "go").await.unwrap();
    assert_eq!(outcome.output, "done");
    assert_eq!((outcome.turns, outcome.tool_calls), (2, 2));

    // The static toolset is offered on every turn, in registration order.
    let offered = agent.runtime().offered.lock().unwrap().clone();
    let expected = vec!["multiply", "greet", "search-docs"];
    assert_eq!(offered, vec![expected.clone(), expected]);

    // Both results went back in one user message: JSON for the i64 result,
    // literal text for the String result.
    let Message::User { content } = &conversation.messages()[2] else {
        panic!("expected a user message with tool results");
    };
    let results: Vec<(&str, &ToolResultContent)> = content
        .iter()
        .filter_map(|c| match c {
            UserContent::ToolResult(r) => Some((r.name.as_str(), r.content.first()?)),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, "multiply");
    assert_eq!(results[0].1.as_json(), Some(&json!(12)));
    assert_eq!(results[1].0, "search-docs");
    assert_eq!(results[1].1.as_text(), Some("results for loop"));
}
