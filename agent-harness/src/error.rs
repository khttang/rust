/// Errors that abort a harness run.
///
/// Tool failures are *not* represented here: they are reported back to the
/// model as tool results so it can recover.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    /// The model runtime failed (transport, provider, or invalid request).
    /// `anyhow::Error` is not a `std::error::Error`, so the full chain is
    /// rendered into the message instead of exposed through `source()`.
    #[error("model runtime failed: {0:#}")]
    Runtime(anyhow::Error),

    #[error("agent did not produce a final answer within {0} turns")]
    MaxTurnsExceeded(usize),

    #[error("model returned an empty response")]
    EmptyResponse,

    #[error("a tool named `{0}` is already registered")]
    DuplicateTool(String),
}

impl From<anyhow::Error> for HarnessError {
    fn from(error: anyhow::Error) -> Self {
        Self::Runtime(error)
    }
}
