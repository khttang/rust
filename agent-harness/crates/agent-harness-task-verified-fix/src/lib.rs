//! Automated task: fix a C function until CBMC verifies it.
//!
//! The agent gets a C file, a target function and a bug report. It reads
//! the code, runs [CBMC](https://www.cprover.org/cbmc/) (`cbmc_verify`),
//! proposes unified-diff patches (`apply_patch`, the only mutating tool,
//! limited to the target file and subject to approval), and stops when the
//! function verifies.
//!
//! Acceptance never trusts the model. [`VerifiedFix::accept`](agent_harness::Task::accept)
//! re-runs CBMC on the final file with the **task's** loop bound and checks,
//! and rejects the ways a verifier can be satisfied without a real fix
//! ([`checks`]): added assumptions, removed or weakened assertions,
//! preprocessor tricks, and edits outside the target function or file. The
//! [`FixReport`] carries the diff, file hashes, CBMC's verdict and the audit
//! record of the acceptance run.
//!
//! ```no_run
//! # async fn example(runtime: impl agent_harness::ChatRuntime, approver: impl agent_harness::ApprovalPolicy) -> Result<(), Box<dyn std::error::Error>> {
//! use agent_harness::{AuditLog, RiskGate, TaskRunner};
//! use agent_harness_task_verified_fix::{VerifiedFix, corpus};
//!
//! let case = &corpus::bundled()?[0];
//! let runner = TaskRunner::new(runtime, AuditLog::create("fix.jsonl")?)
//!     .with_policy(RiskGate::new(approver));      // a person approves each patch
//! let report = runner.run(&VerifiedFix::default(), case.input.clone(), &case.source).await?;
//! println!("accepted: {}\n{}", report.accepted(), report.acceptance.report.diff);
//! # Ok(()) }
//! ```

pub mod checks;
pub mod corpus;
pub mod csource;
mod task;
pub mod tools;

pub use task::{CbmcSummary, FixInput, FixReport, MAX_TURNS, PREAMBLE, VerifiedFix};
