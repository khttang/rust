//! Human-in-the-loop approval gate for tool calls.
//!
//! Every tool call the model requests passes through an [`ApprovalPolicy`]
//! before it executes. Denied calls are not executed; the denial reason is
//! returned to the model as the tool result.
//!
//! Every [`Approval`] names its [`Decider`] (a policy or a person), so the
//! audit trail can tell automatic approvals from human ones. A policy sees the
//! call and a [`ReviewContext`] (turn, the tool's declared [`ToolRisk`]);
//! the context is `#[non_exhaustive]` so adaptive policies can be given more
//! information later without breaking existing ones.

use std::future::Future;

use rig_core::message::ToolCall;
use serde::Serialize;

/// Who made an approval decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Decider {
    /// An automatic decision by a named policy.
    Policy(String),
    /// A decision by a person, identified as well as the caller can.
    Human(String),
}

impl Decider {
    pub fn policy(name: impl Into<String>) -> Self {
        Self::Policy(name.into())
    }

    pub fn human(id: impl Into<String>) -> Self {
        Self::Human(id.into())
    }
}

/// Decision for a single tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Approval {
    Approved { by: Decider },
    Denied { by: Decider, reason: String },
}

impl Approval {
    pub fn approve(by: Decider) -> Self {
        Self::Approved { by }
    }

    pub fn deny(by: Decider, reason: impl Into<String>) -> Self {
        Self::Denied {
            by,
            reason: reason.into(),
        }
    }

    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approved { .. })
    }

    pub fn decider(&self) -> &Decider {
        match self {
            Self::Approved { by } | Self::Denied { by, .. } => by,
        }
    }
}

/// What a tool can do to the world, declared when it is registered.
///
/// Unknown or unregistered tools are treated as [`ToolRisk::Mutating`], so
/// the safe default always applies. New levels may be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolRisk {
    /// Reads only; no side effects outside the tool's own computation.
    ReadOnly,
    /// Changes state (files, external systems). Needs approval by default.
    Mutating,
}

/// Information available to a policy when it reviews a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReviewContext {
    /// 1-based turn of the agent loop that requested the call.
    pub turn: usize,
    /// The tool's declared risk.
    pub risk: ToolRisk,
}

impl ReviewContext {
    pub fn new(turn: usize, risk: ToolRisk) -> Self {
        Self { turn, risk }
    }
}

/// Decides whether a model-requested tool call may run.
///
/// Implementations may be async (e.g. prompt a human, query a service).
pub trait ApprovalPolicy: Send + Sync {
    fn review(&self, call: &ToolCall, ctx: &ReviewContext)
    -> impl Future<Output = Approval> + Send;
}

impl<P: ApprovalPolicy + ?Sized> ApprovalPolicy for &P {
    fn review(
        &self,
        call: &ToolCall,
        ctx: &ReviewContext,
    ) -> impl Future<Output = Approval> + Send {
        (**self).review(call, ctx)
    }
}

/// Approves every call. Suitable for trusted, side-effect-free tool sets.
#[derive(Debug, Clone, Copy, Default)]
pub struct AutoApprove;

impl ApprovalPolicy for AutoApprove {
    async fn review(&self, _call: &ToolCall, _ctx: &ReviewContext) -> Approval {
        Approval::approve(Decider::policy("auto_approve"))
    }
}

/// Denies every call. The fail-safe default for mutating tools when no
/// human or other policy has been supplied.
#[derive(Debug, Clone, Copy, Default)]
pub struct DenyAll;

impl ApprovalPolicy for DenyAll {
    async fn review(&self, call: &ToolCall, _ctx: &ReviewContext) -> Approval {
        Approval::deny(
            Decider::policy("deny_all"),
            format!(
                "tool `{}` needs approval and no approver is configured",
                call.function.name
            ),
        )
    }
}

/// Approves only tools whose names are on the list.
#[derive(Debug, Clone, Default)]
pub struct AllowList {
    allowed: Vec<String>,
}

impl AllowList {
    pub fn new<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            allowed: names.into_iter().map(Into::into).collect(),
        }
    }
}

impl ApprovalPolicy for AllowList {
    async fn review(&self, call: &ToolCall, _ctx: &ReviewContext) -> Approval {
        let name = &call.function.name;
        let by = Decider::policy("allow_list");
        if self.allowed.iter().any(|a| a == name) {
            Approval::approve(by)
        } else {
            Approval::deny(by, format!("tool `{name}` is not on the allow list"))
        }
    }
}

/// Approves [`ToolRisk::ReadOnly`] calls automatically and sends every other
/// call to `inner` (a human, an allow list, an adaptive policy).
#[derive(Debug, Clone, Copy, Default)]
pub struct RiskGate<P> {
    inner: P,
}

impl<P: ApprovalPolicy> RiskGate<P> {
    pub fn new(inner: P) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &P {
        &self.inner
    }
}

impl<P: ApprovalPolicy> ApprovalPolicy for RiskGate<P> {
    async fn review(&self, call: &ToolCall, ctx: &ReviewContext) -> Approval {
        match ctx.risk {
            ToolRisk::ReadOnly => Approval::approve(Decider::policy("risk_gate:read_only")),
            _ => self.inner.review(call, ctx).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::message::{ToolFunction, ToolName};
    use serde_json::json;

    fn call(name: &str) -> ToolCall {
        ToolCall::from_wire(
            "id-1",
            ToolFunction::new(ToolName::new(name).unwrap(), json!({})),
        )
    }

    fn ctx(risk: ToolRisk) -> ReviewContext {
        ReviewContext::new(1, risk)
    }

    #[tokio::test]
    async fn auto_approve_approves_and_names_itself() {
        let a = AutoApprove
            .review(&call("x"), &ctx(ToolRisk::Mutating))
            .await;
        assert!(a.is_approved());
        assert_eq!(a.decider(), &Decider::policy("auto_approve"));
    }

    #[tokio::test]
    async fn deny_all_denies_with_reason() {
        let a = DenyAll
            .review(&call("patch"), &ctx(ToolRisk::Mutating))
            .await;
        assert!(matches!(a, Approval::Denied { ref reason, .. } if reason.contains("`patch`")));
        assert_eq!(a.decider(), &Decider::policy("deny_all"));
    }

    #[tokio::test]
    async fn allow_list_filters_by_name() {
        let policy = AllowList::new(["calculator"]);
        let m = ctx(ToolRisk::Mutating);
        assert!(policy.review(&call("calculator"), &m).await.is_approved());
        let denied = policy.review(&call("shell"), &m).await;
        assert!(
            matches!(denied, Approval::Denied { ref reason, .. } if reason.contains("`shell`"))
        );
    }

    #[tokio::test]
    async fn risk_gate_auto_approves_read_only_only() {
        let gate = RiskGate::new(DenyAll);
        let ro = gate.review(&call("read"), &ctx(ToolRisk::ReadOnly)).await;
        assert!(ro.is_approved());
        assert_eq!(ro.decider(), &Decider::policy("risk_gate:read_only"));

        let m = gate.review(&call("patch"), &ctx(ToolRisk::Mutating)).await;
        assert!(!m.is_approved());
        assert_eq!(m.decider(), &Decider::policy("deny_all"));
    }

    #[test]
    fn approvals_serialize_with_decider() {
        let a = Approval::deny(Decider::human("operator-1"), "no");
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            json!({"decision": "denied", "by": {"kind": "human", "id": "operator-1"}, "reason": "no"})
        );
    }

    #[test]
    fn policies_are_send_sync_static() {
        fn assert_bounds<T: ApprovalPolicy + 'static>() {}
        assert_bounds::<AutoApprove>();
        assert_bounds::<DenyAll>();
        assert_bounds::<AllowList>();
        assert_bounds::<RiskGate<DenyAll>>();
    }
}
