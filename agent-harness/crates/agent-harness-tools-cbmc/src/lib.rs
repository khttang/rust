//! CBMC bounded model checking as an audited agent-harness tool.
//!
//! [CBMC](https://www.cprover.org/cbmc/) checks a C function on all inputs,
//! up to a loop bound, for assertion failures, overflow, out-of-bounds
//! accesses, invalid pointers, division by zero and undefined shifts, and
//! returns a concrete counterexample for every failure.
//!
//! * [`verify`]: run CBMC on a workspace file; used by tools and by
//!   acceptance checks alike, so acceptance can re-verify independently.
//! * [`CbmcVerify`]: the model-facing `cbmc_verify` tool (read-only).
//! * [`report`]: CBMC `--json-ui` parsing, with shortened counterexamples.
//!
//! Every run goes through `agent_harness::process::run`: no shell, cleared
//! environment (plus an explicit `PATH` so CBMC finds `gcc`), timeout, and an
//! audit record with the CBMC binary's SHA-256. Requests are validated: the
//! file must be inside the workspace, the function a C identifier, the loop
//! bound within [`CbmcConfig::max_unwind`], and checks from [`CheckFlag`].
//!
//! Requires CBMC (tested with 6.6.0 from Debian trixie, which pulls in `gcc`).
//!
//! ```no_run
//! # async fn example(ctx: agent_harness::TaskContext) -> Result<(), Box<dyn std::error::Error>> {
//! use agent_harness::ToolRegistry;
//! use agent_harness_tools_cbmc::{CbmcConfig, CbmcVerify, VerifyRequest, verify};
//!
//! let config = CbmcConfig::default();
//! let mut tools = ToolRegistry::new();
//! tools.register_read_only(CbmcVerify::new(ctx.clone(), config.clone()))?;
//!
//! // In an acceptance check: re-verify the final workspace yourself.
//! let result = verify(&ctx, &config, VerifyRequest::new("pitch.c", "pitch_cmd", 8)).await?;
//! assert!(result.verified());
//! # Ok(()) }
//! ```

pub mod report;
mod tool;
mod verify;

pub use report::{
    CbmcReport, Counterexample, Outcome, Property, PropertyKind, PropertyStatus, TraceStep,
};
pub use tool::{CbmcVerify, CbmcVerifyArgs, CbmcVerifyOutput};
pub use verify::{
    CbmcConfig, CbmcError, CheckFlag, DEFAULT_CBMC, DEFAULT_PATH, DEFAULT_UNWIND, Verification,
    VerifyRequest, verify,
};

/// Record the CBMC binary's SHA-256 and version in the audit log.
pub async fn identify(
    ctx: &agent_harness::TaskContext,
    config: &CbmcConfig,
) -> Result<String, agent_harness::ProcessError> {
    agent_harness::process::identify(&config.program, &["--version"], ctx.audit()).await
}
