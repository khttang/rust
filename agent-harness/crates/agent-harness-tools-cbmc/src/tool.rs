//! The model-facing `cbmc_verify` tool.

use agent_harness::{
    TaskContext,
    rig_core::tool::{PortableTool, ToolExecutionError},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    report::{Message, Outcome, Property},
    verify::{CbmcConfig, CbmcError, CheckFlag, DEFAULT_UNWIND, VerifyRequest, verify},
};

/// Runs CBMC on a workspace file and reports, per property, whether it
/// holds, with a short counterexample for each failure.
///
/// Read-only: it never changes the workspace. Register it with
/// `ToolRegistry::register_read_only`.
#[derive(Debug, Clone)]
pub struct CbmcVerify {
    ctx: TaskContext,
    config: CbmcConfig,
}

impl CbmcVerify {
    pub fn new(ctx: TaskContext, config: CbmcConfig) -> Self {
        Self { ctx, config }
    }

    pub fn config(&self) -> &CbmcConfig {
        &self.config
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CbmcVerifyArgs {
    pub file: String,
    pub function: String,
    #[serde(default)]
    pub unwind: Option<u32>,
    #[serde(default)]
    pub checks: Option<Vec<CheckFlag>>,
}

/// What the model sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CbmcVerifyOutput {
    pub outcome: Outcome,
    /// One sentence for the model.
    pub summary: String,
    pub cbmc_version: Option<String>,
    pub unwind: u32,
    pub checks: Vec<CheckFlag>,
    /// Failing properties first, each with its counterexample.
    pub failures: Vec<Property>,
    /// Number of properties that hold.
    pub passed: usize,
    /// CBMC error messages (e.g. a syntax error).
    pub errors: Vec<Message>,
    /// The audit record of this CBMC run.
    pub audit_record: u64,
}

/// "1 property", "3 properties".
fn properties(n: usize) -> String {
    format!("{n} {}", if n == 1 { "property" } else { "properties" })
}

impl PortableTool for CbmcVerify {
    const NAME: &'static str = "cbmc_verify";
    type Args = CbmcVerifyArgs;
    type Output = CbmcVerifyOutput;
    type Error = CbmcError;

    fn description(&self) -> String {
        format!(
            "Bounded model checking of a C function with CBMC. Checks every \
             property (assertions, overflow, array bounds, pointers, division \
             by zero, shifts) on all inputs, exploring loops up to `unwind` \
             iterations (1..={}, default {DEFAULT_UNWIND}). Returns each failing \
             property with a counterexample: the inputs and assignments that \
             lead to the failure. An `unwinding` failure means the loop bound \
             was too small, not that the code is wrong.",
            self.config.max_unwind
        )
    }

    fn parameters(&self) -> Value {
        let checks: Vec<&str> = CheckFlag::ALL.iter().map(|c| c.name()).collect();
        let defaults: Vec<&str> = CheckFlag::DEFAULT.iter().map(|c| c.name()).collect();
        json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "C source file, relative to the workspace root"
                },
                "function": {
                    "type": "string",
                    "description": "Entry function to verify"
                },
                "unwind": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": self.config.max_unwind,
                    "description": "Loop bound"
                },
                "checks": {
                    "type": "array",
                    "items": { "type": "string", "enum": checks },
                    "description": format!("Checks to enable (default: {})", defaults.join(", "))
                }
            },
            "required": ["file", "function"]
        })
    }

    fn map_error(&self, error: CbmcError) -> ToolExecutionError {
        // Messages are generated here and safe to show; they help the model
        // correct its request.
        error.into()
    }

    async fn call(&self, args: CbmcVerifyArgs) -> Result<CbmcVerifyOutput, CbmcError> {
        let mut request = VerifyRequest::new(
            args.file,
            args.function,
            args.unwind.unwrap_or(DEFAULT_UNWIND),
        );
        if let Some(checks) = args.checks {
            request = request.with_checks(checks);
        }
        let verification = verify(&self.ctx, &self.config, request).await?;
        let report = verification.report;
        let failures: Vec<Property> = report.failures().cloned().collect();
        let passed = report.properties.len() - failures.len();
        let summary = match report.outcome {
            Outcome::Verified => format!(
                "Verified: {} hold up to unwind {}.",
                properties(passed),
                verification.request.unwind
            ),
            Outcome::Failed if report.bound_too_small() => format!(
                "{} failing, including an unwinding assertion: the loop bound {} \
                 is too small. Raise `unwind` (max {}) before changing the code \
                 for that failure.",
                properties(failures.len()),
                verification.request.unwind,
                self.config.max_unwind
            ),
            Outcome::Failed => format!(
                "{} failing; each failure has a counterexample.",
                properties(failures.len())
            ),
            Outcome::Error => "CBMC could not analyse the code; see errors.".to_owned(),
            _ => format!(
                "CBMC did not finish within {} seconds; try a smaller unwind.",
                self.config.timeout.as_secs()
            ),
        };
        Ok(CbmcVerifyOutput {
            outcome: report.outcome,
            summary,
            cbmc_version: report.version,
            unwind: verification.request.unwind,
            checks: verification.request.checks,
            failures,
            passed,
            errors: report.errors,
            audit_record: verification.audit_seq,
        })
    }
}
