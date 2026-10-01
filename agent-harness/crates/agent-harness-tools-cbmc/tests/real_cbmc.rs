//! Against a real CBMC binary.
//!
//! CBMC is looked up at `CBMC_PATH` or `/usr/bin/cbmc`. Without it these tests
//! skip (with a message), unless `AGENT_HARNESS_REQUIRE_CBMC=1`, in which case
//! a missing CBMC fails them. `test-in-container.sh` sets that variable and
//! installs CBMC, so the container run can never pass by skipping.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use agent_harness::{
    AuditLog, TaskContext, ToolRegistry, Workspace, audit::sha256_file,
    rig_core::tool::ToolErrorKind,
};
use agent_harness_tools_cbmc::{
    CbmcConfig, CbmcError, CbmcVerify, Outcome, PropertyKind, TraceStep, VerifyRequest, identify,
    verify,
};
use serde_json::{Value, json};

/// The CBMC binary, or `None` to skip.
fn cbmc() -> Option<PathBuf> {
    let path = std::env::var_os("CBMC_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/bin/cbmc"));
    if path.is_file() {
        return Some(path);
    }
    assert!(
        std::env::var("AGENT_HARNESS_REQUIRE_CBMC").as_deref() != Ok("1"),
        "AGENT_HARNESS_REQUIRE_CBMC=1 but CBMC is not at {}",
        path.display()
    );
    eprintln!(
        "skipping: CBMC not found at {} (set CBMC_PATH)",
        path.display()
    );
    None
}

/// A task context over a workspace holding the C fixtures.
fn context() -> (TaskContext, AuditLog) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let source = std::env::temp_dir().join(format!(
        "ah-cbmc-src-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&source).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for entry in std::fs::read_dir(fixtures).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "c") {
            std::fs::copy(&path, source.join(path.file_name().unwrap())).unwrap();
        }
    }
    let workspace = Workspace::create(&source).unwrap();
    std::fs::remove_dir_all(&source).unwrap();
    let audit = AuditLog::in_memory();
    (TaskContext::new(workspace, audit.clone()), audit)
}

fn config(cbmc: &Path) -> CbmcConfig {
    CbmcConfig::default().with_program(cbmc)
}

fn last_event(audit: &AuditLog) -> Value {
    let lines = audit.lines();
    serde_json::from_str::<Value>(lines.last().unwrap()).unwrap()["event"].clone()
}

#[tokio::test]
async fn verifies_correct_code_and_audits_the_run() {
    let Some(cbmc) = cbmc() else { return };
    let (ctx, audit) = context();

    let v = verify(&ctx, &config(&cbmc), VerifyRequest::new("ok.c", "clamp", 4))
        .await
        .unwrap();

    assert!(v.verified(), "{:?}", v.report);
    assert_eq!(v.exit_code, Some(0));
    assert!(v.report.version.as_deref().unwrap().starts_with("CBMC "));
    let event = last_event(&audit);
    assert_eq!(event["kind"], "process_run");
    assert_eq!(event["program_sha256"], sha256_file(&cbmc).unwrap());
    assert_eq!(v.audit_seq, 0);
    assert!(
        event["args"]
            .as_array()
            .unwrap()
            .contains(&json!("--unwinding-assertions"))
    );
}

#[tokio::test]
async fn finds_the_overflow_with_a_counterexample() {
    let Some(cbmc) = cbmc() else { return };
    let (ctx, _) = context();

    let v = verify(
        &ctx,
        &config(&cbmc),
        VerifyRequest::new("overflow.c", "pitch_cmd", 4),
    )
    .await
    .unwrap();

    assert_eq!(v.report.outcome, Outcome::Failed);
    assert_eq!(v.exit_code, Some(10));
    let failure = v.report.failures().next().unwrap();
    assert_eq!(failure.kind, PropertyKind::Overflow);
    let cex = failure.counterexample.as_ref().unwrap();
    assert!(
        cex.steps
            .iter()
            .any(|s| matches!(s, TraceStep::Input { name, .. } if name == "error"))
    );
    assert!(matches!(
        cex.steps.last(),
        Some(TraceStep::Failure { line: Some(2), .. })
    ));
}

#[tokio::test]
async fn loop_bound_decides_unwinding() {
    let Some(cbmc) = cbmc() else { return };
    let (ctx, _) = context();
    let config = config(&cbmc);

    let low = verify(&ctx, &config, VerifyRequest::new("loop5.c", "count5", 3))
        .await
        .unwrap();
    assert!(low.report.bound_too_small(), "{:?}", low.report);

    let enough = verify(&ctx, &config, VerifyRequest::new("loop5.c", "count5", 6))
        .await
        .unwrap();
    assert!(enough.verified(), "{:?}", enough.report);
}

#[tokio::test]
async fn syntax_errors_are_an_error_outcome() {
    let Some(cbmc) = cbmc() else { return };
    let (ctx, _) = context();

    let v = verify(
        &ctx,
        &config(&cbmc),
        VerifyRequest::new("bad.c", "broken", 1),
    )
    .await
    .unwrap();

    assert_eq!(v.report.outcome, Outcome::Error);
    assert!(
        v.report
            .errors
            .iter()
            .any(|e| e.text.contains("syntax error"))
    );
}

#[tokio::test]
async fn timeout_is_a_result() {
    let Some(cbmc) = cbmc() else { return };
    let (ctx, audit) = context();
    let config = config(&cbmc).with_timeout(Duration::from_millis(1));

    let v = verify(&ctx, &config, VerifyRequest::new("ok.c", "clamp", 4))
        .await
        .unwrap();

    assert_eq!(v.report.outcome, Outcome::TimedOut);
    assert!(!v.verified());
    assert_eq!(last_event(&audit)["timed_out"], true);
}

#[tokio::test]
async fn requests_stay_inside_the_workspace() {
    let Some(cbmc) = cbmc() else { return };
    let (ctx, audit) = context();
    let config = config(&cbmc);

    let escape = verify(&ctx, &config, VerifyRequest::new("../etc/passwd", "f", 1)).await;
    assert!(matches!(escape, Err(CbmcError::Workspace(_))));
    let missing = verify(&ctx, &config, VerifyRequest::new("nope.c", "f", 1)).await;
    assert!(matches!(missing, Err(CbmcError::InvalidRequest(_))));
    let option = verify(&ctx, &config, VerifyRequest::new("ok.c", "--trace", 1)).await;
    assert!(matches!(option, Err(CbmcError::InvalidRequest(_))));
    assert!(audit.is_empty(), "invalid requests never start CBMC");
}

#[tokio::test]
async fn tool_reports_failures_to_the_model() {
    let Some(cbmc) = cbmc() else { return };
    let (ctx, _) = context();
    let mut tools = ToolRegistry::new();
    tools
        .register_read_only(CbmcVerify::new(ctx, config(&cbmc)))
        .unwrap();

    let out = tools
        .execute(
            "cbmc_verify",
            json!({"file": "overflow.c", "function": "pitch_cmd"}),
        )
        .await
        .unwrap();
    let out = out.as_json().unwrap();
    assert_eq!(out["outcome"], "failed");
    assert_eq!(out["unwind"], 8, "default bound");
    assert_eq!(out["failures"][0]["kind"], "overflow");
    assert!(out["failures"][0]["counterexample"]["steps"].is_array());
    assert!(out["audit_record"].is_u64());

    let low = tools
        .execute(
            "cbmc_verify",
            json!({"file": "loop5.c", "function": "count5", "unwind": 2}),
        )
        .await
        .unwrap();
    assert!(
        low.as_json().unwrap()["summary"]
            .as_str()
            .unwrap()
            .contains("too small")
    );

    let bad = tools
        .execute(
            "cbmc_verify",
            json!({"file": "ok.c", "function": "--trace"}),
        )
        .await
        .unwrap_err();
    assert_eq!(bad.kind(), ToolErrorKind::InvalidArgs);
    let smuggled = tools
        .execute(
            "cbmc_verify",
            json!({"file": "ok.c", "function": "clamp", "checks": ["outfile"]}),
        )
        .await
        .unwrap_err();
    assert_eq!(smuggled.kind(), ToolErrorKind::InvalidArgs);
}

#[tokio::test]
async fn identify_records_cbmc_version() {
    let Some(cbmc) = cbmc() else { return };
    let (ctx, audit) = context();

    let version = identify(&ctx, &config(&cbmc)).await.unwrap();

    assert!(version.starts_with(char::is_numeric), "{version}");
    let event = last_event(&audit);
    assert_eq!(event["kind"], "program_identified");
    assert_eq!(event["sha256"], sha256_file(&cbmc).unwrap());
}
