//! A generic, model-agnostic agent harness built on top of `rig-core`.
//!
//! | Concern                  | Item                                   | Default                      |
//! |--------------------------|----------------------------------------|------------------------------|
//! | Model access             | [`harness::ModelRuntime`]              | [`harness::HostedProviderRuntime`] |
//! | LLM provider             | [`CompletionBackend`] | [`ProviderModel`] (runtime-switchable) |
//! | Learned heuristics       | [`harness::AdaptiveMemoryLayer`]       | —                            |
//! | Data schemas             | [`models`]                             | —                            |
//! | Tools                    | [`rig_core::tool::PortableTool`]       | [`tools::Calculator`], …     |
//! | Human-in-the-loop gate   | [`policy::ApprovalPolicy`]             | [`policy::AutoApprove`]      |
//! | Automated tasks          | [`task::Task`]                         | [`task::TaskRunner`]         |
//! | Audit trail              | [`audit::AuditLog`]                    | fail-closed, hash-chained    |
//! | Lifecycle observation    | [`observer::Observer`]                 | [`observer::NoopObserver`]   |

pub mod audit;
pub mod error;
pub mod harness;
pub mod models;
pub mod observer;
pub mod policy;
pub mod process;
pub mod provider;
pub mod task;
pub mod tool;
pub mod tools;
pub mod validate;
pub mod workspace;

pub use audit::{AuditError, AuditEvent, AuditLog, verify_chain};
pub use error::HarnessError;
pub use harness::{
    AdaptiveMemoryLayer, AgentLoop, AssistantTurn, ChatRuntime, CompactionPolicy, CompactionReport,
    CompletionBackend, Conversation, HostedProviderRuntime, ModelIdentity, ModelRuntime,
    RunOutcome, RuntimeInfo,
};
pub use models::{AgentTask, Manifest, ManifestEntry};
pub use observer::{NoopObserver, Observer};
pub use policy::{
    AllowList, Approval, ApprovalPolicy, AutoApprove, Decider, DenyAll, ReviewContext, RiskGate,
    ToolRisk,
};
pub use process::{ProcessError, ProcessOutput, ProcessSpec};
pub use provider::{ModelSpec, Provider, ProviderError, ProviderModel};
pub use task::{
    Acceptance, Check, CheckOutcome, CheckResult, Egress, SandboxNeeds, Task, TaskContext,
    TaskError, TaskReport, TaskRunner, evaluate,
};
pub use tool::{DynTool, ToolRegistry};
pub use workspace::{FileDigest, Workspace, WorkspaceError};

/// Re-export of the underlying framework so downstream crates can use a single version.
pub use rig_core;
