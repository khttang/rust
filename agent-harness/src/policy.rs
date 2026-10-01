//! Human-in-the-loop approval gate for tool calls.
//!
//! Every tool call the model requests passes through an [`ApprovalPolicy`]
//! before it executes. Denied calls are not executed; the denial reason is
//! returned to the model as the tool result.

use std::future::Future;

use rig_core::message::ToolCall;

/// Decision for a single tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    Approved,
    Denied { reason: String },
}

impl Approval {
    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Denied {
            reason: reason.into(),
        }
    }

    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approved)
    }
}

/// Decides whether a model-requested tool call may run.
///
/// Implementations may be async (e.g. prompt a human, query a service).
pub trait ApprovalPolicy: Send + Sync {
    fn review(&self, call: &ToolCall) -> impl Future<Output = Approval> + Send;
}

/// Approves every call. Suitable for trusted, side-effect-free tool sets.
#[derive(Debug, Clone, Copy, Default)]
pub struct AutoApprove;

impl ApprovalPolicy for AutoApprove {
    async fn review(&self, _call: &ToolCall) -> Approval {
        Approval::Approved
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
    async fn review(&self, call: &ToolCall) -> Approval {
        let name = &call.function.name;
        if self.allowed.iter().any(|a| a == name) {
            Approval::Approved
        } else {
            Approval::deny(format!("tool `{name}` is not on the allow list"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::message::ToolFunction;
    use serde_json::json;

    fn call(name: &str) -> ToolCall {
        ToolCall::from_wire("id-1", ToolFunction::new(name.into(), json!({})))
    }

    #[tokio::test]
    async fn auto_approve_approves() {
        assert!(AutoApprove.review(&call("anything")).await.is_approved());
    }

    #[tokio::test]
    async fn allow_list_filters_by_name() {
        let policy = AllowList::new(["calculator"]);
        assert_eq!(policy.review(&call("calculator")).await, Approval::Approved);
        let denied = policy.review(&call("shell")).await;
        assert!(matches!(denied, Approval::Denied { ref reason } if reason.contains("`shell`")));
    }

    #[test]
    fn policies_are_send_sync_static() {
        fn assert_bounds<T: ApprovalPolicy + 'static>() {}
        assert_bounds::<AutoApprove>();
        assert_bounds::<AllowList>();
    }
}
