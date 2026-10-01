//! The verified-fix task.

use agent_harness::{
    Acceptance, HarnessError, RunOutcome, SandboxNeeds, Task, TaskContext, ToolRegistry,
    audit::sha256_hex, evaluate,
};
use agent_harness_tools_cbmc::{CbmcConfig, CbmcVerify, CheckFlag, Outcome, VerifyRequest, verify};
use serde::{Deserialize, Serialize};

use crate::{
    checks::{
        AssertionsKept, CbmcVerified, FixContext, NoAssumeAdded, OnlyTargetChanged,
        OtherFilesUnchanged, PreprocessorUnchanged,
    },
    tools::{ApplyPatch, ReadSource, unified_diff},
};

fn default_checks() -> Vec<CheckFlag> {
    CheckFlag::DEFAULT.to_vec()
}

/// One fix request: which function to make verifiable, under which bound
/// and checks. These, not anything the model chooses, decide acceptance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FixInput {
    /// C source file, relative to the source directory.
    pub file: String,
    /// The function to fix.
    pub function: String,
    /// CBMC loop bound used for acceptance.
    pub unwind: u32,
    /// CBMC checks used for acceptance.
    #[serde(default = "default_checks")]
    pub checks: Vec<CheckFlag>,
    /// The bug report, in plain words.
    #[serde(default)]
    pub description: String,
}

impl FixInput {
    pub fn new(file: impl Into<String>, function: impl Into<String>, unwind: u32) -> Self {
        Self {
            file: file.into(),
            function: function.into(),
            unwind,
            checks: default_checks(),
            description: String::new(),
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn with_checks(mut self, checks: impl IntoIterator<Item = CheckFlag>) -> Self {
        self.checks = checks.into_iter().collect();
        self
    }

    /// The CBMC request acceptance uses.
    pub fn verify_request(&self) -> VerifyRequest {
        VerifyRequest::new(&self.file, &self.function, self.unwind)
            .with_checks(self.checks.iter().copied())
    }
}

/// CBMC's verdict on the final file, for the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CbmcSummary {
    pub outcome: Option<Outcome>,
    pub version: Option<String>,
    pub properties: usize,
    pub failing: Vec<String>,
    /// Why CBMC did not produce a verdict, if it did not.
    pub error: Option<String>,
    /// Audit record of the acceptance CBMC run.
    pub evidence: Option<u64>,
}

/// What the run changed and how it was verified. Built from the workspace
/// and an independent CBMC run, never from the model's text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct FixReport {
    pub file: String,
    pub function: String,
    pub unwind: u32,
    pub checks: Vec<CheckFlag>,
    pub original_sha256: Option<String>,
    pub final_sha256: Option<String>,
    /// Unified diff from the original to the final file (empty if unchanged).
    pub diff: String,
    pub cbmc: CbmcSummary,
    pub turns: Option<usize>,
    pub total_tokens: Option<u64>,
}

/// Instructions for the model.
pub const PREAMBLE: &str = "\
You fix bugs in C functions so that CBMC, a bounded model checker, can verify them.

Rules:
1. Change only the target function in the target file, with apply_patch and a unified diff.
2. Never add __CPROVER_assume or __builtin_assume, never remove or weaken an assertion, and never add or change preprocessor lines (#include, #define, #pragma).
3. Keep the function's purpose. Make the smallest change that removes every failure, handling the failing inputs rather than excluding them.
4. Use cbmc_verify to see failures with counterexamples, and to confirm the fix with the task's unwind bound and checks. An `unwinding` failure means the loop bound is too small, not that the code is wrong; the task's bound is fixed.
5. When cbmc_verify reports the target function verified, reply with a short explanation of the fix.

Your result is checked independently: CBMC is re-run on the final file with the task's settings, and every rule above is enforced.";

/// Default turn budget: read, verify, patch, re-verify, with room to retry.
pub const MAX_TURNS: usize = 16;

/// Fix a C function until CBMC verifies it, under anti-cheating checks.
#[derive(Debug, Clone, Default)]
pub struct VerifiedFix {
    cbmc: CbmcConfig,
}

impl VerifiedFix {
    pub fn new(cbmc: CbmcConfig) -> Self {
        Self { cbmc }
    }

    pub fn cbmc(&self) -> &CbmcConfig {
        &self.cbmc
    }
}

impl Task for VerifiedFix {
    type Input = FixInput;
    type Report = FixReport;

    fn name(&self) -> &str {
        "verified-fix"
    }

    fn preamble(&self) -> String {
        PREAMBLE.to_owned()
    }

    fn prompt(&self, input: &FixInput) -> String {
        let checks: Vec<&str> = input.checks.iter().map(|c| c.name()).collect();
        let description = if input.description.is_empty() {
            "(none)"
        } else {
            &input.description
        };
        format!(
            "Fix `{function}` in `{file}`.\n\nBug report: {description}\n\n\
             Acceptance: CBMC must verify `{function}` with unwind {unwind} and checks [{checks}].\n\
             Start by reading `{file}` with read_source and running cbmc_verify on `{function}`.",
            function = input.function,
            file = input.file,
            unwind = input.unwind,
            checks = checks.join(", "),
        )
    }

    fn tools(&self, ctx: &TaskContext, input: &FixInput) -> Result<ToolRegistry, HarnessError> {
        let mut tools = ToolRegistry::new();
        tools.register_read_only(ReadSource::new(ctx.workspace().clone()))?;
        tools.register_read_only(CbmcVerify::new(ctx.clone(), self.cbmc.clone()))?;
        tools.register(ApplyPatch::new(ctx.workspace().clone(), &input.file))?;
        Ok(tools)
    }

    fn max_turns(&self) -> usize {
        MAX_TURNS
    }

    fn sandbox(&self) -> SandboxNeeds {
        self.cbmc.sandbox_needs()
    }

    async fn accept(
        &self,
        ctx: &TaskContext,
        input: &FixInput,
        outcome: Option<&RunOutcome>,
    ) -> Acceptance<FixReport> {
        let original = std::fs::read_to_string(ctx.workspace().source().join(&input.file)).ok();
        let current = ctx.workspace().read_to_string(&input.file).ok();
        // Independent re-verification with the task's bound and checks.
        let verification = verify(ctx, &self.cbmc, input.verify_request()).await;

        let fix = FixContext {
            task: ctx,
            input,
            original: original.clone(),
            current: current.clone(),
            verification: &verification,
        };
        let audit = ctx.audit();
        let checks = vec![
            evaluate(&CbmcVerified, &fix, audit).await,
            evaluate(&NoAssumeAdded, &fix, audit).await,
            evaluate(&AssertionsKept, &fix, audit).await,
            evaluate(&PreprocessorUnchanged, &fix, audit).await,
            evaluate(&OnlyTargetChanged, &fix, audit).await,
            evaluate(&OtherFilesUnchanged, &fix, audit).await,
        ];

        let cbmc = match &verification {
            Ok(v) => CbmcSummary {
                outcome: Some(v.report.outcome),
                version: v.report.version.clone(),
                properties: v.report.properties.len(),
                failing: v.report.failures().map(|p| p.id.clone()).collect(),
                error: None,
                evidence: Some(v.audit_seq),
            },
            Err(e) => CbmcSummary {
                outcome: None,
                version: None,
                properties: 0,
                failing: Vec::new(),
                error: Some(e.to_string()),
                evidence: None,
            },
        };
        let diff = match (&original, &current) {
            (Some(o), Some(c)) if o != c => unified_diff(&input.file, o, c),
            _ => String::new(),
        };
        let report = FixReport {
            file: input.file.clone(),
            function: input.function.clone(),
            unwind: input.unwind,
            checks: input.checks.clone(),
            original_sha256: original.as_deref().map(|t| sha256_hex(t.as_bytes())),
            final_sha256: current.as_deref().map(|t| sha256_hex(t.as_bytes())),
            diff,
            cbmc,
            turns: outcome.map(|o| o.turns),
            total_tokens: outcome.and_then(|o| o.usage.total_tokens),
        };
        Acceptance::new(checks, report)
    }
}
