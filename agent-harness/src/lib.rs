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
//! | Lifecycle observation    | [`observer::Observer`]                 | [`observer::NoopObserver`]   |

pub mod error;
pub mod harness;
pub mod models;
pub mod observer;
pub mod policy;
pub mod provider;
pub mod tool;
pub mod tools;
pub mod validate;

pub use error::HarnessError;
pub use harness::{
    AdaptiveMemoryLayer, AgentLoop, AssistantTurn, ChatRuntime, CompactionPolicy, CompactionReport,
    CompletionBackend, Conversation, HostedProviderRuntime, ModelRuntime, RunOutcome,
};
pub use models::{AgentTask, Manifest, ManifestEntry};
pub use observer::{NoopObserver, Observer};
pub use policy::{AllowList, Approval, ApprovalPolicy, AutoApprove};
pub use provider::{ModelSpec, Provider, ProviderError, ProviderModel};
pub use tool::{DynTool, ToolRegistry};

/// Re-export of the underlying framework so downstream crates can use a single version.
pub use rig_core;
