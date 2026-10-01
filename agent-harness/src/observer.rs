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
