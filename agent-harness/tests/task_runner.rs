//! The task layer (`Task`, `Check`, `evaluate`, `Acceptance`, `TaskRunner`)
//! end to end, through the public API only, as a task crate would use it.
//!
//! The toy task "fix-greeting" asks the model to correct `greeting.txt`
//! from "helo" to "hello". It has one read-only tool and one mutating tool,
//! and accepts only if the file really says "hello" afterwards.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use agent_harness::{
    Acceptance, AssistantTurn, AuditError, AuditLog, AutoApprove, ChatRuntime, Check, CheckResult,
    HarnessError, Observer, RiskGate, RunOutcome, SandboxNeeds, Task, TaskContext, TaskError,
    TaskRunner, ToolRegistry, Workspace, WorkspaceError,
    audit::verify_lines,
    evaluate,
    rig_core::{
        completion::{AssistantContent, ToolDefinition},
        message::{Message, ToolName},
        tool::PortableTool,
    },
    verify_chain,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ---------- the toy task ----------

struct ReadFile(Workspace);
struct WriteFile(Workspace);

#[derive(Deserialize)]
struct PathArgs {
    path: String,
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    contents: String,
}

impl PortableTool for ReadFile {
    const NAME: &'static str = "read_file";
    type Args = PathArgs;
    type Output = String;
    type Error = WorkspaceError;

    fn description(&self) -> String {
        "Read a file in the workspace.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})
    }
    async fn call(&self, args: PathArgs) -> Result<String, WorkspaceError> {
        self.0.read_to_string(args.path)
    }
}

impl PortableTool for WriteFile {
    const NAME: &'static str = "write_file";
    type Args = WriteArgs;
    type Output = String;
    type Error = WorkspaceError;

    fn description(&self) -> String {
        "Overwrite a file in the workspace.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"path": {"type": "string"}, "contents": {"type": "string"}},
            "required": ["path", "contents"]
        })
    }
    async fn call(&self, args: WriteArgs) -> Result<String, WorkspaceError> {
        self.0.write(&args.path, &args.contents)?;
        Ok(format!("wrote {}", args.path))
    }
}

/// Accepts when `greeting.txt` reads exactly "hello".
struct SaysHello;

impl Check<TaskContext> for SaysHello {
    fn name(&self) -> &str {
        "says_hello"
    }
    fn verifies(&self) -> &str {
        "REQ-1: greeting.txt is spelled correctly"
    }
    async fn run(&self, ctx: &TaskContext) -> CheckResult {
        match ctx.workspace().read_to_string("greeting.txt") {
            Ok(text) if text == "hello" => CheckResult::pass("greeting.txt is \"hello\""),
            Ok(text) => CheckResult::fail(format!("greeting.txt is {text:?}")),
            Err(e) => CheckResult::fail(e.to_string()),
        }
    }
}

#[derive(Debug, PartialEq, Serialize)]
struct GreetingReport {
    final_text: Option<String>,
    turns: Option<usize>,
}

#[derive(Serialize)]
struct GreetingInput {
    file: String,
}

struct FixGreeting;

impl Task for FixGreeting {
    type Input = GreetingInput;
    type Report = GreetingReport;

    fn name(&self) -> &str {
        "fix-greeting"
    }
    fn preamble(&self) -> String {
        "Fix spelling mistakes.".into()
    }
    fn prompt(&self, input: &GreetingInput) -> String {
        format!("Fix the spelling in {}.", input.file)
    }
    fn tools(&self, ctx: &TaskContext) -> Result<ToolRegistry, HarnessError> {
        let mut tools = ToolRegistry::new();
        tools.register_read_only(ReadFile(ctx.workspace().clone()))?;
        tools.register(WriteFile(ctx.workspace().clone()))?;
        Ok(tools)
    }
    fn max_turns(&self) -> usize {
        4
    }
    fn sandbox(&self) -> SandboxNeeds {
        SandboxNeeds::new()
    }
    async fn accept(
        &self,
        ctx: &TaskContext,
        _input: &GreetingInput,
        outcome: Option<&RunOutcome>,
    ) -> Acceptance<GreetingReport> {
        let checks = vec![evaluate(&SaysHello, ctx, ctx.audit()).await];
        let report = GreetingReport {
            final_text: ctx.workspace().read_to_string("greeting.txt").ok(),
            turns: outcome.map(|o| o.turns),
        };
        Acceptance::new(checks, report)
    }
}

// ---------- scripted model ----------

struct Scripted(Mutex<VecDeque<AssistantTurn>>);

impl Scripted {
    fn new(turns: impl IntoIterator<Item = AssistantTurn>) -> Self {
        Self(Mutex::new(turns.into_iter().collect()))
    }
}

impl ChatRuntime for Scripted {
    async fn chat(
        &self,
        _preamble: &str,
        _history: &[Message],
        _tools: &[ToolDefinition],
    ) -> anyhow::Result<AssistantTurn> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("script exhausted"))
    }
}

fn call(id: &str, name: &str, args: Value) -> AssistantTurn {
    let mut turn = AssistantTurn::text_reply("");
    turn.content = vec![AssistantContent::tool_call(
        id,
        ToolName::new(name).unwrap(),
        args,
    )];
    turn
}

/// read the file, write the fix, then claim success.
fn fixing_script() -> Scripted {
    Scripted::new([
        call("c1", "read_file", json!({"path": "greeting.txt"})),
        call(
            "c2",
            "write_file",
            json!({"path": "greeting.txt", "contents": "hello"}),
        ),
        AssistantTurn::text_reply("Fixed it."),
    ])
}

// ---------- helpers ----------

/// A unique temp path. The counter matters: the clock alone can repeat
/// between back-to-back calls.
fn unique_temp(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn source_dir() -> PathBuf {
    let dir = unique_temp("ah-task-src");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("greeting.txt"), "helo").unwrap();
    dir
}

fn input() -> GreetingInput {
    GreetingInput {
        file: "greeting.txt".into(),
    }
}

fn events(audit: &AuditLog) -> Vec<Value> {
    audit
        .lines()
        .iter()
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["event"].clone())
        .collect()
}

fn kinds(audit: &AuditLog) -> Vec<String> {
    events(audit)
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_owned())
        .collect()
}

fn cleanup(source: &Path, workspace: &Path) {
    let _ = std::fs::remove_dir_all(source);
    let _ = std::fs::remove_dir_all(workspace);
}

// ---------- tests ----------

#[tokio::test]
async fn approved_fix_is_accepted_and_fully_audited() {
    let source = source_dir();
    let audit = AuditLog::in_memory();
    let runner =
        TaskRunner::new(fixing_script(), audit.clone()).with_policy(RiskGate::new(AutoApprove));

    let report = runner.run(&FixGreeting, input(), &source).await.unwrap();

    assert!(report.accepted());
    assert_eq!(report.acceptance.checks.len(), 1);
    assert_eq!(
        report.acceptance.checks[0].verifies,
        "REQ-1: greeting.txt is spelled correctly"
    );
    assert_eq!(
        report.acceptance.report,
        GreetingReport {
            final_text: Some("hello".into()),
            turns: Some(3)
        }
    );
    assert_eq!(
        std::fs::read_to_string(source.join("greeting.txt")).unwrap(),
        "helo",
        "source untouched"
    );
    assert!(report.workspace_kept, "kept as evidence by default");

    assert_eq!(
        kinds(&audit),
        [
            "run_started",
            "model_turn",
            "tool_call",
            "tool_result",
            "model_turn",
            "tool_call",
            "tool_result",
            "model_turn",
            "final_answer",
            "run_ended",
            "check_evaluated",
            "accepted",
        ]
    );
    let ev = events(&audit);
    assert_eq!(ev[0]["task"], "fix-greeting");
    assert!(
        ev[0]["build"]["rustc"]
            .as_str()
            .unwrap()
            .starts_with("rustc 1.")
    );
    assert!(!ev[0]["build"]["target"].as_str().unwrap().is_empty());
    assert_eq!(ev[0]["inputs"][0]["path"], "greeting.txt");
    assert_eq!(ev[0]["tools"][0]["risk"], "read_only");
    assert_eq!(ev[0]["tools"][1]["risk"], "mutating");
    // The read-only call was approved by the gate, the write by the inner policy.
    assert_eq!(
        ev[2]["approval"]["by"],
        json!({"kind": "policy", "id": "risk_gate:read_only"})
    );
    assert_eq!(
        ev[5]["approval"]["by"],
        json!({"kind": "policy", "id": "auto_approve"})
    );
    assert_ne!(
        ev[0]["inputs"][0]["sha256"], ev[9]["outputs"][0]["sha256"],
        "change is recorded"
    );
    assert_eq!(ev[11]["accepted"], true);
    assert_eq!(ev[11]["report"]["final_text"], "hello");

    let lines = audit.lines();
    assert_eq!(
        verify_lines(lines.iter().map(String::as_str))
            .unwrap()
            .records,
        12
    );
    cleanup(&source, &report.workspace);
}

#[tokio::test]
async fn default_policy_denies_mutating_tools() {
    let source = source_dir();
    let audit = AuditLog::in_memory();
    let report = TaskRunner::new(fixing_script(), audit.clone())
        .run(&FixGreeting, input(), &source)
        .await
        .unwrap();

    assert!(
        !report.accepted(),
        "the write was denied, so the file is unchanged"
    );
    let ev = events(&audit);
    assert_eq!(ev[5]["approval"]["decision"], "denied");
    assert_eq!(ev[5]["approval"]["by"]["id"], "deny_all");
    assert_eq!(ev[6]["ok"], false);
    cleanup(&source, &report.workspace);
}

#[tokio::test]
async fn model_claims_are_not_evidence() {
    let source = source_dir();
    let runner = TaskRunner::new(
        Scripted::new([AssistantTurn::text_reply("All fixed, trust me.")]),
        AuditLog::in_memory(),
    )
    .with_policy(RiskGate::new(AutoApprove));

    let report = runner.run(&FixGreeting, input(), &source).await.unwrap();

    assert_eq!(
        report.outcome.as_ref().unwrap().output,
        "All fixed, trust me."
    );
    assert!(!report.accepted());
    assert!(report.acceptance.checks[0].result.detail.contains("helo"));
    cleanup(&source, &report.workspace);
}

#[tokio::test]
async fn acceptance_runs_even_when_the_loop_fails() {
    let source = source_dir();
    let audit = AuditLog::in_memory();
    let runner = TaskRunner::new(Scripted::new([]), audit.clone());

    let report = runner.run(&FixGreeting, input(), &source).await.unwrap();

    assert!(matches!(report.outcome, Err(HarnessError::Runtime(_))));
    assert!(!report.accepted());
    assert_eq!(report.acceptance.report.turns, None);
    let ev = events(&audit);
    let ended = ev.iter().find(|e| e["kind"] == "run_ended").unwrap();
    assert_eq!(ended["ok"], false);
    assert!(
        ended["error"]
            .as_str()
            .unwrap()
            .contains("script exhausted")
    );
    assert_eq!(ev.last().unwrap()["kind"], "accepted");
    cleanup(&source, &report.workspace);
}

/// Fails the audit log at the start of turn 2.
struct FailAuditAtTurn2(AuditLog);

impl Observer for FailAuditAtTurn2 {
    fn on_turn_start(&self, turn: usize) {
        if turn == 2 {
            self.0.fail_for_test("disk full");
        }
    }
}

#[tokio::test]
async fn audit_failure_stops_tool_calls_and_fails_the_run() {
    let source = source_dir();
    let audit = AuditLog::in_memory();
    let runner = TaskRunner::new(fixing_script(), audit.clone())
        .with_policy(RiskGate::new(AutoApprove))
        .with_observer(FailAuditAtTurn2(audit.clone()))
        .keep_workspace(false);

    let result = runner.run(&FixGreeting, input(), &source).await;

    assert!(matches!(
        result,
        Err(TaskError::Audit(AuditError::Failed(_)))
    ));
    // Turn 1 was recorded; nothing after the failure was.
    assert_eq!(
        kinds(&audit),
        ["run_started", "model_turn", "tool_call", "tool_result"]
    );
    assert_eq!(
        std::fs::read_to_string(source.join("greeting.txt")).unwrap(),
        "helo"
    );
    let _ = std::fs::remove_dir_all(&source);
}

#[tokio::test]
async fn failed_log_refuses_to_start() {
    let source = source_dir();
    let audit = AuditLog::in_memory();
    audit.fail_for_test("disk full");
    let result = TaskRunner::new(fixing_script(), audit)
        .run(&FixGreeting, input(), &source)
        .await;
    assert!(matches!(result, Err(TaskError::Audit(_))));
    let _ = std::fs::remove_dir_all(&source);
}

#[tokio::test]
async fn one_runner_serves_several_inputs_on_one_chain() {
    let first = source_dir();
    let second = source_dir();
    let path = unique_temp("ah-task-audit").with_extension("jsonl");
    let script = Scripted::new(
        [
            call(
                "c1",
                "write_file",
                json!({"path": "greeting.txt", "contents": "hello"}),
            ),
            AssistantTurn::text_reply("done"),
        ]
        .into_iter()
        .cycle()
        .take(4),
    );
    let runner = TaskRunner::new(script, AuditLog::create(&path).unwrap())
        .with_policy(RiskGate::new(AutoApprove))
        .keep_workspace(false);

    let a = runner.run(&FixGreeting, input(), &first).await.unwrap();
    let b = runner.run(&FixGreeting, input(), &second).await.unwrap();

    assert!(a.accepted() && b.accepted());
    assert_eq!(b.audit_path.as_deref(), Some(path.as_path()));
    let summary = verify_chain(&path).unwrap();
    assert_eq!(summary.records, b.audit_records);
    for dir in [&first, &second] {
        let _ = std::fs::remove_dir_all(dir);
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn acceptance_needs_at_least_one_passing_check() {
    let none: Acceptance<()> = Acceptance::new(Vec::new(), ());
    assert!(!none.accepted, "no checks means not accepted");
}

#[tokio::test]
async fn evaluate_fails_closed_when_the_check_cannot_be_recorded() {
    struct AlwaysPasses;
    impl Check<()> for AlwaysPasses {
        fn name(&self) -> &str {
            "always_passes"
        }
        async fn run(&self, _: &()) -> CheckResult {
            CheckResult::pass("ok")
        }
    }
    let audit = AuditLog::in_memory();
    let ok = evaluate(&AlwaysPasses, &(), &audit).await;
    assert!(ok.result.passed);
    assert_eq!(ok.audit_seq, Some(0));

    audit.fail_for_test("disk full");
    let failed = evaluate(&AlwaysPasses, &(), &audit).await;
    assert!(!failed.result.passed);
    assert_eq!(failed.audit_seq, None);
    assert!(!Acceptance::new(vec![failed], ()).accepted);
}

#[test]
fn task_types_are_send_sync_static() {
    fn assert_bounds<T: Send + Sync + 'static>() {}
    assert_bounds::<FixGreeting>();
    assert_bounds::<TaskRunner<Scripted>>();
    assert_bounds::<TaskContext>();
}
