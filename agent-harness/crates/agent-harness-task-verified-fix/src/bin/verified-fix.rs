//! `verified-fix`: run the verified-fix task from the command line.
//!
//! ```text
//! verified-fix run <case-dir> [--model <provider[:model]>] [--audit <file>] [--discard-workspace]
//! verified-fix self-test <corpus-dir> [--audit <file>]
//! verified-fix sandbox-needs
//! verified-fix --version
//! ```
//!
//! * `run`: fix the case in `<case-dir>` (`case.json` + `src/`). Every patch
//!   is approved on the terminal (`HARNESS_AUTO_APPROVE=1` approves without
//!   asking, recorded as a policy decision). Prints the report as JSON on
//!   stdout; exit code 0 if accepted, 1 if not.
//! * `self-test`: no model. Runs CBMC on every case's original (must fail)
//!   and reference fix (must verify), audited, and verifies the audit chain.
//!   Used to prove CBMC works inside the sandbox. Exit code 0 if all as
//!   expected.
//! * `sandbox-needs`: the programs and egress the task needs, as JSON.
//!
//! Environment: `HARNESS_MODEL` (default `openai`), provider keys read by rig
//! (e.g. `OPENAI_API_KEY`, a placeholder under OpenShell), `CBMC_PATH`
//! (default `/usr/bin/cbmc`). Exit code 2 on usage or setup errors.

use std::{
    path::PathBuf,
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

use agent_harness::{
    AuditLog, BuildInfo, ConsoleApproval, HostedProviderRuntime, ModelSpec, ProviderModel,
    RiskGate, StderrObserver, Task, TaskContext, TaskRunner, Workspace, verify_chain,
};
use agent_harness_task_verified_fix::{VerifiedFix, corpus};
use agent_harness_tools_cbmc::{CbmcConfig, identify, verify};
use serde_json::json;

type Error = Box<dyn std::error::Error>;

const DEFAULT_MODEL: &str = "openai";
const USAGE: &str =
    "usage: verified-fix run <case-dir> [--model <spec>] [--audit <file>] [--discard-workspace]
       verified-fix self-test <corpus-dir> [--audit <file>]
       verified-fix sandbox-needs
       verified-fix --version";

fn cbmc_config() -> CbmcConfig {
    match std::env::var_os("CBMC_PATH") {
        Some(path) => CbmcConfig::default().with_program(PathBuf::from(path)),
        None => CbmcConfig::default(),
    }
}

/// `verified-fix-<label>-<unix ms>.jsonl` in the current directory.
fn default_audit_path(label: &str) -> PathBuf {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    PathBuf::from(format!("verified-fix-{label}-{ms}.jsonl"))
}

struct Options {
    positional: Vec<String>,
    model: Option<String>,
    audit: Option<PathBuf>,
    discard_workspace: bool,
}

fn parse(args: impl Iterator<Item = String>) -> Result<Options, Error> {
    let mut options = Options {
        positional: Vec::new(),
        model: None,
        audit: None,
        discard_workspace: false,
    };
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" | "-m" => options.model = Some(args.next().ok_or("--model needs a value")?),
            "--audit" => options.audit = Some(args.next().ok_or("--audit needs a value")?.into()),
            "--discard-workspace" => options.discard_workspace = true,
            other if other.starts_with('-') => return Err(format!("unknown option {other}").into()),
            _ => options.positional.push(arg),
        }
    }
    Ok(options)
}

async fn run(options: Options) -> Result<bool, Error> {
    let [case_dir] = options.positional.as_slice() else {
        return Err(USAGE.into());
    };
    let case = corpus::load_case(case_dir)?;
    let spec: ModelSpec = options
        .model
        .or_else(|| std::env::var("HARNESS_MODEL").ok())
        .unwrap_or_else(|| DEFAULT_MODEL.to_owned())
        .parse()?;
    let audit_path = options
        .audit
        .unwrap_or_else(|| default_audit_path(&case.name));
    let runtime = HostedProviderRuntime::new(ProviderModel::from_env(spec.clone())?);
    let runner = TaskRunner::new(runtime, AuditLog::create(&audit_path)?)
        .with_policy(RiskGate::new(ConsoleApproval::from_env()))
        .with_observer(StderrObserver)
        .keep_workspace(!options.discard_workspace);

    eprintln!(
        "verified-fix: case {} with {spec}, audit {}",
        case.name,
        audit_path.display()
    );
    let report = runner
        .run(
            &VerifiedFix::new(cbmc_config()),
            case.input.clone(),
            &case.source,
        )
        .await?;
    let chain = verify_chain(&audit_path)?;

    let checks: Vec<_> = report
        .acceptance
        .checks
        .iter()
        .map(|c| json!({"name": c.name, "passed": c.result.passed, "detail": c.result.detail}))
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "case": case.name,
            "model": spec.to_string(),
            "accepted": report.accepted(),
            "checks": checks,
            "report": report.acceptance.report,
            "loop_error": report.outcome.as_ref().err().map(ToString::to_string),
            "workspace": report.workspace,
            "workspace_kept": report.workspace_kept,
            "audit": audit_path,
            "audit_records": chain.records,
            "audit_last_hash": chain.last_hash,
        }))?
    );
    Ok(report.accepted())
}

async fn self_test(options: Options) -> Result<bool, Error> {
    let [corpus_dir] = options.positional.as_slice() else {
        return Err(USAGE.into());
    };
    let cases = corpus::load(corpus_dir)?;
    if cases.is_empty() {
        return Err(format!("no cases in {corpus_dir}").into());
    }
    let audit_path = options
        .audit
        .unwrap_or_else(|| default_audit_path("self-test"));
    let audit = AuditLog::create(&audit_path)?;
    let config = cbmc_config();
    let mut all_ok = true;
    let mut version = None;

    for case in &cases {
        for (label, dir, expect) in [
            ("original", &case.source, false),
            ("reference", &case.reference, true),
        ] {
            let ctx = TaskContext::new(Workspace::create(dir)?, audit.clone());
            if version.is_none() {
                version = Some(identify(&ctx, &config).await?);
            }
            let v = verify(&ctx, &config, case.input.verify_request()).await?;
            let ok = v.verified() == expect;
            all_ok &= ok;
            eprintln!(
                "  {} {:<18} {:<9} {:?} ({} failing)",
                if ok { "ok  " } else { "FAIL" },
                case.name,
                label,
                v.report.outcome,
                v.report.failures().count()
            );
        }
    }
    audit.check()?;
    let chain = verify_chain(&audit_path)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "self_test": if all_ok { "passed" } else { "failed" },
            "cases": cases.len(),
            "cbmc": version,
            "audit": audit_path,
            "audit_records": chain.records,
            "audit_last_hash": chain.last_hash,
        }))?
    );
    Ok(all_ok)
}

async fn dispatch(mut args: impl Iterator<Item = String>) -> Result<bool, Error> {
    match args.next().as_deref() {
        Some("run") => run(parse(args)?).await,
        Some("self-test") => self_test(parse(args)?).await,
        Some("sandbox-needs") => {
            let needs = VerifiedFix::new(cbmc_config()).sandbox();
            println!("{}", serde_json::to_string_pretty(&needs)?);
            Ok(true)
        }
        Some("--version" | "-V") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "build": BuildInfo::current(),
                    "cbmc": cbmc_config().program,
                }))?
            );
            Ok(true)
        }
        _ => Err(USAGE.into()),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match dispatch(std::env::args().skip(1)).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("verified-fix: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn parses_options_and_positionals() {
        let o = parse(args(&[
            "case",
            "--model",
            "openai:gpt-5.6",
            "--audit",
            "a.jsonl",
            "--discard-workspace",
        ]))
        .unwrap();
        assert_eq!(o.positional, ["case"]);
        assert_eq!(o.model.as_deref(), Some("openai:gpt-5.6"));
        assert_eq!(o.audit.as_deref(), Some(Path::new("a.jsonl")));
        assert!(o.discard_workspace);
        assert!(parse(args(&["--bogus"])).is_err());
        assert!(parse(args(&["--model"])).is_err());
    }

    #[tokio::test]
    async fn usage_errors() {
        assert!(dispatch(args(&[])).await.is_err());
        assert!(dispatch(args(&["run"])).await.is_err(), "missing case dir");
        assert!(dispatch(args(&["frobnicate"])).await.is_err());
    }

    #[tokio::test]
    async fn sandbox_needs_and_version_succeed() {
        assert!(dispatch(args(&["sandbox-needs"])).await.unwrap());
        assert!(dispatch(args(&["--version"])).await.unwrap());
    }
}
