//! Synchronous lifecycle hooks for logging, metrics and tracing.

use rig_core::{
    message::ToolCall,
    tool::{ToolExecutionError, ToolOutput},
};

use crate::{
    harness::AssistantTurn,
    policy::{Approval, ReviewContext},
};

/// Receives [`crate::harness::AgentLoop`] lifecycle events. All methods
/// default to no-ops.
pub trait Observer: Send + Sync {
    fn on_turn_start(&self, _turn: usize) {}
    fn on_model_response(&self, _turn: usize, _response: &AssistantTurn) {}
    /// A model request failed transiently and will be retried; `attempt` is
    /// the number of failed attempts so far (1 for the first retry).
    fn on_model_retry(&self, _turn: usize, _attempt: usize, _error: &anyhow::Error) {}
    fn on_tool_call(&self, _call: &ToolCall, _ctx: &ReviewContext, _approval: &Approval) {}
    fn on_tool_result(&self, _call: &ToolCall, _result: &Result<ToolOutput, ToolExecutionError>) {}
    fn on_final_answer(&self, _turn: usize, _text: &str) {}
}

impl<O: Observer + ?Sized> Observer for &O {
    fn on_turn_start(&self, turn: usize) {
        (**self).on_turn_start(turn);
    }
    fn on_model_response(&self, turn: usize, response: &AssistantTurn) {
        (**self).on_model_response(turn, response);
    }
    fn on_model_retry(&self, turn: usize, attempt: usize, error: &anyhow::Error) {
        (**self).on_model_retry(turn, attempt, error);
    }
    fn on_tool_call(&self, call: &ToolCall, ctx: &ReviewContext, approval: &Approval) {
        (**self).on_tool_call(call, ctx, approval);
    }
    fn on_tool_result(&self, call: &ToolCall, result: &Result<ToolOutput, ToolExecutionError>) {
        (**self).on_tool_result(call, result);
    }
    fn on_final_answer(&self, turn: usize, text: &str) {
        (**self).on_final_answer(turn, text);
    }
}

/// Ignores every event.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopObserver;

impl Observer for NoopObserver {}

/// Characters of tool output [`StderrObserver`] shows; the audit trail keeps
/// everything.
pub const STDERR_OUTPUT_PREVIEW: usize = 300;

/// Logs loop events to stderr for an operator watching a run.
#[derive(Debug, Clone, Copy, Default)]
pub struct StderrObserver;

fn preview(text: &str) -> String {
    match text.char_indices().nth(STDERR_OUTPUT_PREVIEW) {
        Some((cut, _)) => format!("{}… ({} chars)", &text[..cut], text.chars().count()),
        None => text.to_owned(),
    }
}

impl Observer for StderrObserver {
    fn on_turn_start(&self, turn: usize) {
        eprintln!("[turn {turn}]");
    }

    fn on_model_response(&self, _turn: usize, response: &AssistantTurn) {
        let calls = response.tool_calls().count();
        if calls > 0 {
            eprintln!("  model requested {calls} tool call(s)");
        }
    }

    fn on_model_retry(&self, turn: usize, attempt: usize, error: &anyhow::Error) {
        eprintln!("  model request failed (turn {turn}, attempt {attempt}), retrying: {error:#}");
    }

    fn on_tool_call(&self, call: &ToolCall, _ctx: &ReviewContext, approval: &Approval) {
        let decision = if approval.is_approved() {
            "approved"
        } else {
            "denied"
        };
        eprintln!(
            "  -> {} {decision} by {:?}",
            call.function.name,
            approval.decider()
        );
    }

    fn on_tool_result(&self, call: &ToolCall, result: &Result<ToolOutput, ToolExecutionError>) {
        match result {
            Ok(out) => eprintln!("  <- {}: {}", call.function.name, preview(&out.render())),
            Err(e) => eprintln!("  <- {} failed: {e}", call.function.name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_are_bounded_and_char_safe() {
        assert_eq!(preview("short"), "short");
        let long = "é".repeat(STDERR_OUTPUT_PREVIEW + 10);
        let p = preview(&long);
        assert!(
            p.ends_with(&format!("({} chars)", STDERR_OUTPUT_PREVIEW + 10)),
            "{p}"
        );
        assert!(p.starts_with(&"é".repeat(STDERR_OUTPUT_PREVIEW)));
    }
}
