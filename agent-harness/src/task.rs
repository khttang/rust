//! Automated tasks: the extension point for task crates.
//!
//! A [`Task`] supplies what defines one kind of job: the instructions, a
//! small fixed toolset (with each tool's [`crate::policy::ToolRisk`]), the
//! sandbox it needs, and a deterministic acceptance check that produces an
//! [`Acceptance`] with a typed report. [`TaskRunner`] does the rest the same
//! way for every task:
//!
//! 1. copies the input into a [`Workspace`] and records the configuration,
//! 2. runs [`AgentLoop`] with every model turn, tool call, approval and
//!    program run written to the [`AuditLog`],
//! 3. **always** runs [`Task::accept`] on the final workspace, whatever the
//!    model claimed, and records each check and the verdict.
//!
//! The model's final answer is kept for information only; only the checks in
//! `accept` count as evidence. Auditing is fail-closed: once the log has
//! failed, every further tool call is denied, programs are not started, and
//! the run returns [`TaskError::Audit`].

use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use rig_core::{
    message::ToolCall,
    tool::{ToolExecutionError, ToolOutput},
};
use serde::Serialize;
use serde_json::Value;

use crate::{
    audit::{AuditError, AuditEvent, AuditLog, ToolRecord, git_commit},
    error::HarnessError,
    harness::{AgentLoop, AssistantTurn, ChatRuntime, Conversation, DEFAULT_MAX_TURNS, RunOutcome},
    observer::{NoopObserver, Observer},
    policy::{Approval, ApprovalPolicy, Decider, DenyAll, ReviewContext, RiskGate},
    tool::ToolRegistry,
    workspace::{Workspace, WorkspaceError},
};

/// One network destination a task needs (feeds the sandbox policy).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Egress {
    pub host: String,
    pub port: u16,
    pub method: String,
    pub path: String,
}

impl Egress {
    pub fn new(
        host: impl Into<String>,
        port: u16,
        method: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            method: method.into(),
            path: path.into(),
        }
    }
}

/// What a task needs from its sandbox: programs it runs (absolute paths, to
/// be pinned as `binaries`) and network destinations beyond the model API.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct SandboxNeeds {
    pub binaries: Vec<PathBuf>,
    pub egress: Vec<Egress>,
}

impl SandboxNeeds {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn binary(mut self, path: impl Into<PathBuf>) -> Self {
        self.binaries.push(path.into());
        self
    }

    pub fn egress(mut self, egress: Egress) -> Self {
        self.egress.push(egress);
        self
    }
}

/// What a task's tools and checks can reach: the workspace and the audit log.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TaskContext {
    workspace: Workspace,
    audit: AuditLog,
}

impl TaskContext {
    pub fn new(workspace: Workspace, audit: AuditLog) -> Self {
        Self { workspace, audit }
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }
}

/// The result of one [`Check`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct CheckResult {
    pub passed: bool,
    pub detail: String,
    /// Audit records holding this check's evidence (e.g. a verifier run).
    pub evidence: Vec<u64>,
}

impl CheckResult {
    pub fn pass(detail: impl Into<String>) -> Self {
        Self {
            passed: true,
            detail: detail.into(),
            evidence: Vec::new(),
        }
    }

    pub fn fail(detail: impl Into<String>) -> Self {
        Self {
            passed: false,
            detail: detail.into(),
            evidence: Vec::new(),
        }
    }

    pub fn with_evidence(mut self, audit_seq: u64) -> Self {
        self.evidence.push(audit_seq);
        self
    }
}

/// A small, deterministic validation block. Acceptance is the conjunction of
/// a task's checks.
pub trait Check<C: ?Sized + Sync>: Send + Sync {
    /// Short identifier, e.g. `no_assume_added`.
    fn name(&self) -> &str;

    /// The requirement or property this check verifies (for traceability).
    fn verifies(&self) -> &str {
        ""
    }

    fn run(&self, ctx: &C) -> impl Future<Output = CheckResult> + Send;
}

/// A check's name, what it verifies, its result and its audit record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct CheckOutcome {
    pub name: String,
    pub verifies: String,
    pub result: CheckResult,
    pub audit_seq: Option<u64>,
}

/// Run `check` and record it. If the record cannot be written, the check
/// fails (fail-closed).
pub async fn evaluate<C, K>(check: &K, ctx: &C, audit: &AuditLog) -> CheckOutcome
where
    C: ?Sized + Sync,
    K: Check<C>,
{
    let mut result = check.run(ctx).await;
    let recorded = audit.record(AuditEvent::CheckEvaluated {
        name: check.name().to_owned(),
        verifies: check.verifies().to_owned(),
        passed: result.passed,
        detail: result.detail.clone(),
        evidence: result.evidence.clone(),
    });
    let audit_seq = match recorded {
        Ok(seq) => Some(seq),
        Err(error) => {
            result.passed = false;
            result.detail = format!("{} (not accepted: {error})", result.detail);
            None
        }
    };
    CheckOutcome {
        name: check.name().to_owned(),
        verifies: check.verifies().to_owned(),
        result,
        audit_seq,
    }
}

/// The verdict on a task run and the task's report.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Acceptance<R> {
    /// True only if there was at least one check and every check passed.
    pub accepted: bool,
    pub checks: Vec<CheckOutcome>,
    /// Built by the task's code from the workspace, never from model text.
    pub report: R,
}

impl<R> Acceptance<R> {
    /// Accepted iff `checks` is non-empty and all passed.
    pub fn new(checks: Vec<CheckOutcome>, report: R) -> Self {
        let accepted = !checks.is_empty() && checks.iter().all(|c| c.result.passed);
        Self {
            accepted,
            checks,
            report,
        }
    }
}

/// One kind of automated job. Implemented by task crates.
pub trait Task: Send + Sync {
    /// What one run works on (serialized into the audit record).
    type Input: Serialize + Send + Sync;
    /// The report accompanying every [`Acceptance`].
    type Report: Serialize + Send;

    /// Stable identifier, e.g. `verified-fix`.
    fn name(&self) -> &str;

    /// System instructions for the model.
    fn preamble(&self) -> String;

    /// The request for one input.
    fn prompt(&self, input: &Self::Input) -> String;

    /// The task's small, fixed toolset. Register read-only tools with
    /// [`ToolRegistry::register_read_only`]; everything else is mutating and
    /// goes to the runner's approval policy.
    fn tools(&self, ctx: &TaskContext) -> Result<ToolRegistry, HarnessError>;

    fn max_turns(&self) -> usize {
        DEFAULT_MAX_TURNS
    }

    /// Programs and network destinations the task needs.
    fn sandbox(&self) -> SandboxNeeds {
        SandboxNeeds::default()
    }

    /// Decide acceptance from the final workspace with deterministic checks
    /// (see [`evaluate`]) and build the report. Runs after every run, even a
    /// failed one; `outcome` is `None` when the agent loop failed.
    fn accept(
        &self,
        ctx: &TaskContext,
        input: &Self::Input,
        outcome: Option<&RunOutcome>,
    ) -> impl Future<Output = Acceptance<Self::Report>> + Send;
}

/// Why a task run could not be completed or trusted.
#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),

    #[error("audit failed; the run is not trusted: {0}")]
    Audit(#[from] AuditError),

    #[error("task could not build its toolset: {0}")]
    Tools(HarnessError),

    #[error("task input could not be serialized: {0}")]
    Input(#[from] serde_json::Error),
}

/// Everything a task run produced.
#[derive(Debug)]
#[non_exhaustive]
pub struct TaskReport<R> {
    /// The agent loop's result. Informational: acceptance does not use it.
    pub outcome: Result<RunOutcome, HarnessError>,
    pub acceptance: Acceptance<R>,
    /// Where the working copy is (kept unless disabled).
    pub workspace: PathBuf,
    pub workspace_kept: bool,
    pub audit_path: Option<PathBuf>,
    pub audit_records: u64,
}

impl<R> TaskReport<R> {
    pub fn accepted(&self) -> bool {
        self.acceptance.accepted
    }
}

/// Runs [`Task`]s with auditing, approval and acceptance.
///
/// The default policy is `RiskGate<DenyAll>`: read-only tools run, mutating
/// tools are denied until you supply an approver with [`Self::with_policy`]
/// (typically `RiskGate::new(your_human_approver)`).
pub struct TaskRunner<R, P = RiskGate<DenyAll>, O = NoopObserver> {
    runtime: R,
    policy: P,
    observer: O,
    audit: AuditLog,
    keep_workspace: bool,
    workspace_parent: Option<PathBuf>,
}

impl<R: ChatRuntime> TaskRunner<R> {
    pub fn new(runtime: R, audit: AuditLog) -> Self {
        Self {
            runtime,
            policy: RiskGate::new(DenyAll),
            observer: NoopObserver,
            audit,
            keep_workspace: true,
            workspace_parent: None,
        }
    }
}

impl<R, P, O> TaskRunner<R, P, O>
where
    R: ChatRuntime,
    P: ApprovalPolicy,
    O: Observer,
{
    pub fn with_policy<P2: ApprovalPolicy>(self, policy: P2) -> TaskRunner<R, P2, O> {
        TaskRunner {
            runtime: self.runtime,
            policy,
            observer: self.observer,
            audit: self.audit,
            keep_workspace: self.keep_workspace,
            workspace_parent: self.workspace_parent,
        }
    }

    pub fn with_observer<O2: Observer>(self, observer: O2) -> TaskRunner<R, P, O2> {
        TaskRunner {
            runtime: self.runtime,
            policy: self.policy,
            observer,
            audit: self.audit,
            keep_workspace: self.keep_workspace,
            workspace_parent: self.workspace_parent,
        }
    }

    /// Keep the working copy after the run (default: true, as evidence).
    pub fn keep_workspace(mut self, keep: bool) -> Self {
        self.keep_workspace = keep;
        self
    }

    /// Create working copies under `parent` instead of the system temp dir.
    pub fn workspace_parent(mut self, parent: impl Into<PathBuf>) -> Self {
        self.workspace_parent = Some(parent.into());
        self
    }

    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    pub fn runtime(&self) -> &R {
        &self.runtime
    }

    /// Run `task` on `input`, working on a copy of `source`.
    pub async fn run<T: Task>(
        &self,
        task: &T,
        input: T::Input,
        source: impl AsRef<Path>,
    ) -> Result<TaskReport<T::Report>, TaskError> {
        self.audit.check()?;
        let workspace = match &self.workspace_parent {
            Some(parent) => Workspace::create_in(source, parent)?,
            None => Workspace::create(source)?,
        };
        if self.keep_workspace {
            workspace.keep();
        }
        let ctx = TaskContext::new(workspace.clone(), self.audit.clone());
        let tools = task.tools(&ctx).map_err(TaskError::Tools)?;
        let preamble = task.preamble();
        let prompt = task.prompt(&input);

        self.audit.record(AuditEvent::RunStarted {
            task: task.name().to_owned(),
            harness_version: env!("CARGO_PKG_VERSION").to_owned(),
            git_commit: git_commit(),
            runtime: self.runtime.describe(),
            max_turns: task.max_turns(),
            preamble: preamble.clone(),
            prompt: prompt.clone(),
            input: serde_json::to_value(&input)?,
            inputs: workspace.digests()?,
            tools: tool_records(&tools),
            sandbox: task.sandbox(),
        })?;

        let agent = AgentLoop::new(&self.runtime, tools)
            .with_policy(AuditGate {
                audit: &self.audit,
                inner: &self.policy,
            })
            .with_observer(AuditObserver::new(&self.audit, &self.observer))
            .with_max_turns(task.max_turns());
        let mut conversation = Conversation::new();
        let outcome = agent.run(&preamble, &mut conversation, prompt).await;

        let outputs = workspace.digests();
        let (usage, turns, tool_calls) = match &outcome {
            Ok(o) => (
                serde_json::to_value(o.usage)?,
                Some(o.turns),
                Some(o.tool_calls),
            ),
            Err(_) => (Value::Null, None, None),
        };
        self.audit.record(AuditEvent::RunEnded {
            ok: outcome.is_ok(),
            error: outcome.as_ref().err().map(ToString::to_string),
            turns,
            tool_calls,
            usage,
            outputs: outputs.as_ref().map_or_else(|_| Vec::new(), Clone::clone),
        })?;
        outputs?;
        self.audit.check()?;

        let acceptance = task.accept(&ctx, &input, outcome.as_ref().ok()).await;
        self.audit.record(AuditEvent::Accepted {
            accepted: acceptance.accepted,
            checks: acceptance.checks.len(),
            report: serde_json::to_value(&acceptance.report)?,
        })?;
        self.audit.check()?;

        Ok(TaskReport {
            outcome,
            acceptance,
            workspace: workspace.root().to_path_buf(),
            workspace_kept: workspace.is_kept(),
            audit_path: self.audit.path(),
            audit_records: self.audit.len(),
        })
    }
}

fn tool_records(tools: &ToolRegistry) -> Vec<ToolRecord> {
    tools
        .definitions()
        .into_iter()
        .map(|d| ToolRecord {
            risk: tools.risk(&d.name),
            name: d.name,
            description: d.description,
            parameters: d.parameters,
        })
        .collect()
}

/// Denies every call once the audit log has failed; otherwise defers to
/// `inner`. Applied by [`TaskRunner`] around any policy.
struct AuditGate<'a, P> {
    audit: &'a AuditLog,
    inner: &'a P,
}

impl<P: ApprovalPolicy> ApprovalPolicy for AuditGate<'_, P> {
    async fn review(&self, call: &ToolCall, ctx: &ReviewContext) -> Approval {
        match self.audit.check() {
            Ok(()) => self.inner.review(call, ctx).await,
            Err(error) => Approval::deny(Decider::policy("audit_gate"), error.to_string()),
        }
    }
}

/// Writes every loop event to the audit log, then forwards it. Write errors
/// mark the log failed (see [`AuditLog::record`]); the runner checks after
/// the loop and [`AuditGate`] stops further tool calls.
struct AuditObserver<'a, O> {
    audit: &'a AuditLog,
    inner: O,
    turn: AtomicUsize,
}

impl<'a, O: Observer> AuditObserver<'a, O> {
    fn new(audit: &'a AuditLog, inner: O) -> Self {
        Self {
            audit,
            inner,
            turn: AtomicUsize::new(0),
        }
    }

    fn record(&self, event: AuditEvent) {
        // Failure is sticky in the log and enforced by AuditGate/TaskRunner.
        let _ = self.audit.record(event);
    }
}

fn to_value_or_null<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

impl<O: Observer> Observer for AuditObserver<'_, O> {
    fn on_turn_start(&self, turn: usize) {
        self.turn.store(turn, Ordering::Relaxed);
        self.inner.on_turn_start(turn);
    }

    fn on_model_response(&self, turn: usize, response: &AssistantTurn) {
        self.record(AuditEvent::ModelTurn {
            turn,
            content: to_value_or_null(&response.content),
            usage: to_value_or_null(&response.usage),
            provider: response.provider.clone(),
            model: response.model.clone(),
            response_id: response.response_id.clone(),
            request_id: response.request_id.clone(),
        });
        self.inner.on_model_response(turn, response);
    }

    fn on_tool_call(&self, call: &ToolCall, ctx: &ReviewContext, approval: &Approval) {
        self.record(AuditEvent::ToolCall {
            turn: ctx.turn,
            call_id: to_value_or_null(&call.id),
            name: call.function.name.to_string(),
            arguments: call.function.arguments.clone(),
            risk: ctx.risk,
            approval: approval.clone(),
        });
        self.inner.on_tool_call(call, ctx, approval);
    }

    fn on_tool_result(&self, call: &ToolCall, result: &Result<ToolOutput, ToolExecutionError>) {
        self.record(AuditEvent::ToolResult {
            turn: self.turn.load(Ordering::Relaxed),
            call_id: to_value_or_null(&call.id),
            name: call.function.name.to_string(),
            ok: result.is_ok(),
            output: match result {
                Ok(output) => output.render(),
                Err(error) => error.to_string(),
            },
        });
        self.inner.on_tool_result(call, result);
    }

    fn on_final_answer(&self, turn: usize, text: &str) {
        self.record(AuditEvent::FinalAnswer {
            turn,
            text: text.to_owned(),
        });
        self.inner.on_final_answer(turn, text);
    }
}
