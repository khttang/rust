//! Synchronous lifecycle hooks for logging, metrics and tracing.

use rig_core::{
    completion::CompletionResponse,
    message::ToolCall,
    tool::{ToolExecutionError, ToolOutput},
};

use crate::policy::Approval;

/// Receives harness lifecycle events. All methods default to no-ops.
pub trait Observer: Send + Sync {
    fn on_turn_start(&self, _turn: usize) {}
    fn on_model_response(&self, _turn: usize, _response: &CompletionResponse) {}
    fn on_tool_call(&self, _call: &ToolCall, _approval: &Approval) {}
    fn on_tool_result(&self, _call: &ToolCall, _result: &Result<ToolOutput, ToolExecutionError>) {}
    fn on_final_answer(&self, _turn: usize, _text: &str) {}
}

/// Ignores every event.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopObserver;

impl Observer for NoopObserver {}
