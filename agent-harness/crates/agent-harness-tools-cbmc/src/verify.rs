//! Running CBMC on a workspace file.
//!
//! [`verify`] is the one entry point. The model-facing tool
//! ([`crate::CbmcVerify`]) and acceptance checks both call it, so acceptance
//! can re-verify independently of anything the agent ran.

use std::{collections::BTreeSet, path::PathBuf, time::Duration};

use agent_harness::{
    SandboxNeeds, TaskContext, WorkspaceError,
    process::{self, ProcessError, ProcessSpec},
    rig_core::tool::ToolExecutionError,
};
use serde::{Deserialize, Serialize};

use crate::report::{CbmcReport, Outcome, ParseError, parse};

/// Where Debian installs CBMC.
pub const DEFAULT_CBMC: &str = "/usr/bin/cbmc";
/// CBMC finds `gcc` (its C preprocessor) on `PATH`, and [`process::run`]
/// clears the environment, so `PATH` is set explicitly.
pub const DEFAULT_PATH: &str = "/usr/bin:/bin";
/// Loop bound used when the caller does not choose one.
pub const DEFAULT_UNWIND: u32 = 8;

/// How to run CBMC.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CbmcConfig {
    /// Absolute path to the `cbmc` binary.
    pub program: PathBuf,
    /// `PATH` given to CBMC so it can find `gcc`.
    pub path_env: String,
    pub timeout: Duration,
    /// Cap on captured output. CBMC's JSON must fit entirely to be parsed.
    pub max_output: usize,
    /// Largest `--unwind` a request may ask for.
    pub max_unwind: u32,
}

impl Default for CbmcConfig {
    fn default() -> Self {
        Self {
            program: PathBuf::from(DEFAULT_CBMC),
            path_env: DEFAULT_PATH.to_owned(),
            timeout: Duration::from_secs(120),
            max_output: 16 * 1024 * 1024,
            max_unwind: 64,
        }
    }
}

impl CbmcConfig {
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_unwind(mut self, max_unwind: u32) -> Self {
        self.max_unwind = max_unwind;
        self
    }

    /// Programs CBMC runs: itself, `/bin/sh` and `gcc` (the preprocessor,
    /// which in turn runs its `cc1` from `/usr/libexec/gcc`). No network.
    /// Observed with `strace` on Debian trixie's CBMC 6.6.0.
    pub fn sandbox_needs(&self) -> SandboxNeeds {
        SandboxNeeds::new()
            .binary(self.program.clone())
            .binary("/bin/sh")
            .binary("/usr/bin/gcc")
    }
}

/// A CBMC check the caller may enable. Only these flags can be passed, so
/// a request cannot smuggle in other options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckFlag {
    Bounds,
    Pointer,
    DivByZero,
    SignedOverflow,
    UnsignedOverflow,
    PointerOverflow,
    Conversion,
    UndefinedShift,
    FloatOverflow,
    Nan,
    MemoryLeak,
}

impl CheckFlag {
    pub const ALL: [CheckFlag; 11] = [
        Self::Bounds,
        Self::Pointer,
        Self::DivByZero,
        Self::SignedOverflow,
        Self::UnsignedOverflow,
        Self::PointerOverflow,
        Self::Conversion,
        Self::UndefinedShift,
        Self::FloatOverflow,
        Self::Nan,
        Self::MemoryLeak,
    ];

    /// The checks used when a request names none: the usual safety checks
    /// for integer C code.
    pub const DEFAULT: [CheckFlag; 6] = [
        Self::Bounds,
        Self::Pointer,
        Self::DivByZero,
        Self::SignedOverflow,
        Self::UndefinedShift,
        Self::PointerOverflow,
    ];

    /// The CBMC command-line flag.
    pub const fn flag(self) -> &'static str {
        match self {
            Self::Bounds => "--bounds-check",
            Self::Pointer => "--pointer-check",
            Self::DivByZero => "--div-by-zero-check",
            Self::SignedOverflow => "--signed-overflow-check",
            Self::UnsignedOverflow => "--unsigned-overflow-check",
            Self::PointerOverflow => "--pointer-overflow-check",
            Self::Conversion => "--conversion-check",
            Self::UndefinedShift => "--undefined-shift-check",
            Self::FloatOverflow => "--float-overflow-check",
            Self::Nan => "--nan-check",
            Self::MemoryLeak => "--memory-leak-check",
        }
    }

    /// The name used in requests and JSON (`signed_overflow`, ...).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bounds => "bounds",
            Self::Pointer => "pointer",
            Self::DivByZero => "div_by_zero",
            Self::SignedOverflow => "signed_overflow",
            Self::UnsignedOverflow => "unsigned_overflow",
            Self::PointerOverflow => "pointer_overflow",
            Self::Conversion => "conversion",
            Self::UndefinedShift => "undefined_shift",
            Self::FloatOverflow => "float_overflow",
            Self::Nan => "nan",
            Self::MemoryLeak => "memory_leak",
        }
    }
}

/// What to verify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct VerifyRequest {
    /// C source file, relative to the workspace root.
    pub file: String,
    /// Entry function (a C identifier).
    pub function: String,
    /// Loop bound (`--unwind`); unwinding assertions are always on.
    pub unwind: u32,
    /// Checks to enable, sorted and without duplicates.
    pub checks: Vec<CheckFlag>,
}

impl VerifyRequest {
    /// Verify `function` in `file` with the [`CheckFlag::DEFAULT`] checks.
    pub fn new(file: impl Into<String>, function: impl Into<String>, unwind: u32) -> Self {
        Self {
            file: file.into(),
            function: function.into(),
            unwind,
            checks: CheckFlag::DEFAULT.to_vec(),
        }
    }

    pub fn with_checks(mut self, checks: impl IntoIterator<Item = CheckFlag>) -> Self {
        self.checks = checks.into_iter().collect();
        self
    }
}

/// A completed CBMC run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Verification {
    pub request: VerifyRequest,
    pub report: CbmcReport,
    pub exit_code: Option<i32>,
    /// Audit record of the CBMC run (`process_run`), for evidence.
    pub audit_seq: u64,
}

impl Verification {
    /// Every property holds up to the loop bound.
    pub fn verified(&self) -> bool {
        self.report.outcome == Outcome::Verified
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CbmcError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error(transparent)]
    Workspace(#[from] WorkspaceError),

    #[error(transparent)]
    Process(#[from] ProcessError),

    #[error("CBMC output exceeded {limit} bytes; verify a smaller function or fewer checks")]
    OutputTooLarge { limit: usize },

    #[error("CBMC output could not be read (exit code {exit_code:?}): {source}")]
    Parse {
        exit_code: Option<i32>,
        #[source]
        source: ParseError,
    },

    #[error("CBMC exit code {exit_code:?} does not match its verdict {outcome:?}")]
    Inconsistent {
        exit_code: Option<i32>,
        outcome: Outcome,
    },
}

impl From<CbmcError> for ToolExecutionError {
    fn from(error: CbmcError) -> Self {
        match error {
            CbmcError::InvalidRequest(_) => Self::invalid_args(error.to_string()),
            CbmcError::Workspace(e) => e.into(),
            CbmcError::Process(e) => e.into(),
            _ => Self::other(error.to_string()),
        }
    }
}

fn is_c_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 128
}

/// Validate `request` and normalize its checks (sorted, deduplicated).
fn validate(config: &CbmcConfig, mut request: VerifyRequest) -> Result<VerifyRequest, CbmcError> {
    if !is_c_identifier(&request.function) {
        return Err(CbmcError::InvalidRequest(format!(
            "`{}` is not a C function name",
            request.function
        )));
    }
    if request.unwind == 0 || request.unwind > config.max_unwind {
        return Err(CbmcError::InvalidRequest(format!(
            "unwind must be between 1 and {}, got {}",
            config.max_unwind, request.unwind
        )));
    }
    request.checks = request
        .checks
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(request)
}

/// Exit codes: 0 verified, 10 a property failed, anything else an error.
fn consistent(exit_code: Option<i32>, outcome: Outcome) -> bool {
    match outcome {
        Outcome::Verified => exit_code == Some(0),
        Outcome::Failed => exit_code == Some(10),
        Outcome::Error => !matches!(exit_code, Some(0 | 10)),
        _ => false,
    }
}

/// Run CBMC on `request.file` in the task's workspace.
///
/// The run goes through [`process::run`], so it is audited (with the CBMC
/// binary's SHA-256) and refused if the audit log has failed. A failing
/// property, a parse error and a timeout are results, not errors.
pub async fn verify(
    ctx: &TaskContext,
    config: &CbmcConfig,
    request: VerifyRequest,
) -> Result<Verification, CbmcError> {
    let request = validate(config, request)?;
    // An absolute path inside the workspace: never read as an option.
    let file = ctx.workspace().resolve(&request.file)?;
    if !file.is_file() {
        return Err(CbmcError::InvalidRequest(format!(
            "`{}` is not a file in the workspace",
            request.file
        )));
    }

    let spec = ProcessSpec::new(&config.program, ctx.workspace().root())
        .arg(file)
        .args(["--function", &request.function])
        .args(["--unwind", &request.unwind.to_string()])
        .args(["--unwinding-assertions", "--trace", "--json-ui"])
        .args(request.checks.iter().map(|c| c.flag()))
        .env("PATH", &config.path_env)
        .timeout(config.timeout)
        .max_output(config.max_output);
    let output = process::run(spec, ctx.audit()).await?;

    let report = if output.timed_out {
        CbmcReport::timed_out()
    } else if output.truncated {
        return Err(CbmcError::OutputTooLarge {
            limit: config.max_output,
        });
    } else {
        let report = parse(&output.stdout_lossy()).map_err(|source| CbmcError::Parse {
            exit_code: output.exit_code,
            source,
        })?;
        if !consistent(output.exit_code, report.outcome) {
            return Err(CbmcError::Inconsistent {
                exit_code: output.exit_code,
                outcome: report.outcome,
            });
        }
        report
    };

    Ok(Verification {
        request,
        report,
        exit_code: output.exit_code,
        audit_seq: output.audit_seq,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CbmcConfig {
        CbmcConfig::default()
    }

    #[test]
    fn accepts_c_identifiers_only() {
        for good in ["main", "_f", "pitch_cmd2"] {
            assert!(is_c_identifier(good), "{good}");
        }
        for bad in [
            "",
            "2f",
            "--function",
            "f g",
            "f;rm",
            "ünicode",
            &"a".repeat(129),
        ] {
            assert!(!is_c_identifier(bad), "{bad}");
        }
    }

    #[test]
    fn rejects_bad_requests() {
        let bad_name = validate(&config(), VerifyRequest::new("a.c", "--trace", 4));
        assert!(matches!(bad_name, Err(CbmcError::InvalidRequest(_))));
        for unwind in [0, 65] {
            let r = validate(&config(), VerifyRequest::new("a.c", "f", unwind));
            assert!(matches!(r, Err(CbmcError::InvalidRequest(ref m)) if m.contains("unwind")));
        }
        assert!(
            validate(
                &config().with_max_unwind(100),
                VerifyRequest::new("a.c", "f", 65)
            )
            .is_ok()
        );
    }

    #[test]
    fn checks_are_sorted_and_deduplicated() {
        let r = validate(
            &config(),
            VerifyRequest::new("a.c", "f", 1).with_checks([
                CheckFlag::Pointer,
                CheckFlag::Bounds,
                CheckFlag::Pointer,
            ]),
        )
        .unwrap();
        assert_eq!(r.checks, [CheckFlag::Bounds, CheckFlag::Pointer]);
    }

    #[test]
    fn check_flags_and_names_round_trip() {
        for check in CheckFlag::ALL {
            assert!(check.flag().starts_with("--") && check.flag().ends_with("-check"));
            let json = serde_json::to_value(check).unwrap();
            assert_eq!(json, check.name());
            assert_eq!(serde_json::from_value::<CheckFlag>(json).unwrap(), check);
        }
        assert!(serde_json::from_value::<CheckFlag>("outfile".into()).is_err());
    }

    #[test]
    fn exit_codes_must_match_the_verdict() {
        assert!(consistent(Some(0), Outcome::Verified));
        assert!(consistent(Some(10), Outcome::Failed));
        assert!(consistent(Some(6), Outcome::Error));
        assert!(!consistent(Some(10), Outcome::Verified));
        assert!(!consistent(Some(0), Outcome::Failed));
        assert!(!consistent(Some(0), Outcome::Error));
    }

    #[test]
    fn sandbox_needs_list_cbmc_and_its_preprocessor() {
        let needs = config().sandbox_needs();
        let paths: Vec<_> = needs.binaries.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(paths, ["/usr/bin/cbmc", "/bin/sh", "/usr/bin/gcc"]);
        assert!(needs.egress.is_empty());
    }
}
