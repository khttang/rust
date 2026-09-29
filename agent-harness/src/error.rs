use rig_core::completion::CompletionError;

/// Errors that abort a harness run.
///
/// Tool failures are *not* represented here: they are reported back to the
/// model as tool results so it can recover.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("completion request failed: {0}")]
    Completion(#[from] CompletionError),

    #[error("agent did not produce a final answer within {0} turns")]
    MaxTurnsExceeded(usize),

    #[error("model returned an empty response")]
    EmptyResponse,

    #[error("a tool named `{0}` is already registered")]
    DuplicateTool(String),
}
