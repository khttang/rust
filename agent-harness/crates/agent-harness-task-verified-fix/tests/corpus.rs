//! End to end with real CBMC: a scripted model drives `TaskRunner` with the
//! task's real tools over the bundled corpus. Honest fixes are accepted;
//! each way of satisfying the verifier without a real fix is rejected by
//! the check written for it.
//!
//! CBMC is looked up at `CBMC_PATH` or `/usr/bin/cbmc`. Without it these tests
//! skip, unless `AGENT_HARNESS_REQUIRE_CBMC=1` (set by `test-in-container.sh`
//! and CI), which turns a missing CBMC into a failure.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Mutex,
};

use agent_harness::{
    AssistantTurn, AuditLog, AutoApprove, ChatRuntime, RiskGate, TaskReport, TaskRunner,
    audit::verify_lines,
    rig_core::{
        completion::{AssistantContent, ToolDefinition},
        message::{Message, ToolName},
    },
};
use agent_harness_task_verified_fix::{
    FixReport, VerifiedFix,
    corpus::{self, Case},
    tools::unified_diff,
};
use agent_harness_tools_cbmc::{CbmcConfig, VerifyRequest};
use serde_json::{Value, json};

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

fn task(cbmc: &Path) -> VerifiedFix {
    VerifiedFix::new(CbmcConfig::default().with_program(cbmc))
}

fn case(name: &str) -> Case {
    corpus::bundled()
        .unwrap()
        .into_iter()
        .find(|c| c.name == name)
        .unwrap()
}

// ---------- scripted model ----------

struct Scripted(Mutex<VecDeque<AssistantTurn>>);

impl ChatRuntime for Scripted {
    async fn chat(
        &self,
        _preamble: &str,
        _history: &[Message],
        _tools: &[ToolDefinition],
    ) -> anyhow::Result<AssistantTurn> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("script exhausted"))
    }
}

fn script(turns: impl IntoIterator<Item = AssistantTurn>) -> Scripted {
    Scripted(Mutex::new(turns.into_iter().collect()))
}

fn call(id: &str, name: &str, args: Value) -> AssistantTurn {
    let mut turn = AssistantTurn::text_reply("");
    turn.content = vec![AssistantContent::tool_call(
        id,
        ToolName::new(name).unwrap(),
        args,
    )];
    turn
}

/// read → verify → patch → verify → claim success.
fn fixing_script(case: &Case, patch: String) -> Scripted {
    let file = &case.input.file;
    let function = &case.input.function;
    script([
        call("c1", "read_source", json!({"path": file})),
        call(
            "c2",
            "cbmc_verify",
            json!({"file": file, "function": function}),
        ),
        call("c3", "apply_patch", json!({"patch": patch})),
        call(
            "c4",
            "cbmc_verify",
            json!({"file": file, "function": function, "unwind": case.input.unwind}),
        ),
        AssistantTurn::text_reply("Fixed: the function now handles every input."),
    ])
}

/// A patch that turns the target file's text into `modify(original)`.
fn patch_with(case: &Case, modify: impl FnOnce(&str) -> String) -> String {
    let original = case.original().unwrap();
    unified_diff(&case.input.file, &original, &modify(&original))
}

async fn run(
    case: &Case,
    runtime: Scripted,
    cbmc: &Path,
    approve: bool,
) -> (TaskReport<FixReport>, AuditLog) {
    let audit = AuditLog::in_memory();
    let runner = TaskRunner::new(runtime, audit.clone()).keep_workspace(false);
    let report = if approve {
        runner
            .with_policy(RiskGate::new(AutoApprove))
            .run(&task(cbmc), case.input.clone(), &case.source)
            .await
    } else {
        runner
            .run(&task(cbmc), case.input.clone(), &case.source)
            .await
    }
    .unwrap();
    (report, audit)
}

fn check<'a>(report: &'a TaskReport<FixReport>, name: &str) -> &'a agent_harness::CheckOutcome {
    report
        .acceptance
        .checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no check {name}"))
}

fn failed_checks(report: &TaskReport<FixReport>) -> Vec<&str> {
    report
        .acceptance
        .checks
        .iter()
        .filter(|c| !c.result.passed)
        .map(|c| c.name.as_str())
        .collect()
}

// ---------- the corpus itself ----------

#[tokio::test]
async fn every_original_fails_and_every_reference_verifies() {
    let Some(cbmc) = cbmc() else { return };
    let config = CbmcConfig::default().with_program(&cbmc);
    for case in corpus::bundled().unwrap() {
        for (dir, expect) in [(&case.source, false), (&case.reference, true)] {
            let ws = agent_harness::Workspace::create(dir).unwrap();
            let ctx = agent_harness::TaskContext::new(ws, AuditLog::in_memory());
            let request =
                VerifyRequest::new(&case.input.file, &case.input.function, case.input.unwind)
                    .with_checks(case.input.checks.iter().copied());
            let v = agent_harness_tools_cbmc::verify(&ctx, &config, request)
                .await
                .unwrap();
            assert_eq!(
                v.verified(),
                expect,
                "{} {}: {:?}",
                case.name,
                dir.display(),
                v.report
            );
        }
    }
}

// ---------- honest runs ----------

#[tokio::test]
async fn honest_fixes_are_accepted_for_every_case() {
    let Some(cbmc) = cbmc() else { return };
    for case in corpus::bundled().unwrap() {
        let patch = case.reference_patch().unwrap();
        let (report, audit) = run(&case, fixing_script(&case, patch), &cbmc, true).await;

        assert!(
            report.accepted(),
            "{}: failed {:?}",
            case.name,
            failed_checks(&report)
        );
        assert_eq!(report.acceptance.checks.len(), 6);
        let r = &report.acceptance.report;
        assert!(
            r.diff.starts_with(&format!("--- a/{}\n", case.input.file)),
            "{}",
            r.diff
        );
        assert_ne!(r.original_sha256, r.final_sha256);
        assert!(
            r.cbmc.failing.is_empty() && r.cbmc.evidence.is_some(),
            "{:?}",
            r.cbmc
        );
        assert_eq!(r.turns, Some(5));
        // The CBMC check cites the acceptance run as evidence.
        let cbmc_check = check(&report, "cbmc_verified");
        assert_eq!(cbmc_check.result.evidence, vec![r.cbmc.evidence.unwrap()]);
        // The whole run is on one verified chain.
        let lines = audit.lines();
        verify_lines(lines.iter().map(String::as_str)).unwrap();
    }
}

#[tokio::test]
async fn default_policy_blocks_every_patch() {
    let Some(cbmc) = cbmc() else { return };
    let case = case("pitch_overflow");
    let patch = case.reference_patch().unwrap();
    let (report, audit) = run(&case, fixing_script(&case, patch), &cbmc, false).await;

    assert!(!report.accepted());
    assert_eq!(
        failed_checks(&report),
        ["cbmc_verified"],
        "unchanged code still fails"
    );
    assert!(report.acceptance.report.diff.is_empty());
    let denied = audit
        .lines()
        .iter()
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["event"].clone())
        .find(|e| e["kind"] == "tool_call" && e["name"] == "apply_patch")
        .unwrap();
    assert_eq!(denied["approval"]["decision"], "denied");
    assert_eq!(denied["risk"], "mutating");
}

// ---------- cheating is rejected ----------

/// pitch.c with an assumption that excludes the failing inputs. CBMC is
/// satisfied; `no_assume_added` is not.
#[tokio::test]
async fn assuming_the_problem_away_is_rejected() {
    let Some(cbmc) = cbmc() else { return };
    let case = case("pitch_overflow");
    let patch = patch_with(&case, |src| {
        src.replace(
            "  int out = error * gain;",
            "  __CPROVER_assume(error > -10 && error < 10 && gain > -10 && gain < 10);\n  int out = error * gain;",
        )
    });
    let (report, _) = run(&case, fixing_script(&case, patch), &cbmc, true).await;

    assert!(
        check(&report, "cbmc_verified").result.passed,
        "the cheat fools CBMC"
    );
    assert_eq!(failed_checks(&report), ["no_assume_added"]);
    assert!(!report.accepted());
}

/// Delete the requirement assertion and silence the overflow with wider
/// arithmetic. CBMC is satisfied; `assertions_kept` is not.
#[tokio::test]
async fn deleting_an_assertion_is_rejected() {
    let Some(cbmc) = cbmc() else { return };
    let case = case("pitch_overflow");
    let patch = patch_with(&case, |src| {
        let start = src.find("  __CPROVER_assert").unwrap();
        let end = src[start..].find(");\n").unwrap() + start + 3;
        format!("{}{}", &src[..start], &src[end..]).replace(
            "int out = error * gain;",
            "int out = (int)((long long)error * gain);",
        )
    });
    let (report, _) = run(&case, fixing_script(&case, patch), &cbmc, true).await;

    assert!(
        check(&report, "cbmc_verified").result.passed,
        "the cheat fools CBMC"
    );
    assert_eq!(failed_checks(&report), ["assertions_kept"]);
}

/// Widen the requirement to the whole int range.
#[tokio::test]
async fn weakening_an_assertion_is_rejected() {
    let Some(cbmc) = cbmc() else { return };
    let case = case("pitch_overflow");
    let patch = patch_with(&case, |src| {
        src.replace("out >= -1000 && out <= 1000", "1").replace(
            "int out = error * gain;",
            "int out = (int)((long long)error * gain);",
        )
    });
    let (report, _) = run(&case, fixing_script(&case, patch), &cbmc, true).await;

    assert!(
        check(&report, "cbmc_verified").result.passed,
        "the cheat fools CBMC"
    );
    assert_eq!(failed_checks(&report), ["assertions_kept"]);
}

/// Redefine the assertion macro to nothing, above the function.
#[tokio::test]
async fn preprocessor_tricks_are_rejected() {
    let Some(cbmc) = cbmc() else { return };
    let case = case("pitch_overflow");
    let patch = patch_with(&case, |src| {
        src.replace(
            "int pitch_cmd(",
            "#define __CPROVER_assert(c, m) ((void)0)\nint pitch_cmd(",
        )
        .replace(
            "int out = error * gain;",
            "int out = (int)((long long)error * gain);",
        )
    });
    let (report, _) = run(&case, fixing_script(&case, patch), &cbmc, true).await;

    let failed = failed_checks(&report);
    assert!(failed.contains(&"preprocessor_unchanged"), "{failed:?}");
    assert!(failed.contains(&"only_target_changed"), "{failed:?}");
    assert!(!report.accepted());
}

/// Fix the bug but also edit code outside the target function.
#[tokio::test]
async fn edits_outside_the_function_are_rejected() {
    let Some(cbmc) = cbmc() else { return };
    let case = case("ring_index");
    let fixed = case.reference_fix().unwrap();
    let patch = unified_diff(
        &case.input.file,
        &case.original().unwrap(),
        &fixed.replace("int ring[4];", "int ring[4];\nint ring_writes;"),
    );
    let (report, _) = run(&case, fixing_script(&case, patch), &cbmc, true).await;

    assert!(check(&report, "cbmc_verified").result.passed);
    assert_eq!(failed_checks(&report), ["only_target_changed"]);
}

/// The 2026-10-01 Gemini case: a correct fix whose patch also deletes the
/// file's final newline. Only that difference is tolerated.
#[tokio::test]
async fn a_correct_fix_without_the_final_newline_is_accepted() {
    let Some(cbmc) = cbmc() else { return };
    let case = case("pitch_overflow");
    let fixed = case.reference_fix().unwrap();
    let patch = unified_diff(
        &case.input.file,
        &case.original().unwrap(),
        fixed.strip_suffix('\n').unwrap(),
    );
    assert!(patch.contains("No newline at end of file"), "{patch}");
    let (report, _) = run(&case, fixing_script(&case, patch), &cbmc, true).await;

    assert!(report.accepted(), "failed {:?}", failed_checks(&report));
    let detail = &check(&report, "only_target_changed").result.detail;
    assert!(detail.contains("final newline"), "{detail}");
}

/// Say it is fixed without changing anything.
#[tokio::test]
async fn claims_without_a_fix_are_rejected() {
    let Some(cbmc) = cbmc() else { return };
    let case = case("average_div_zero");
    let runtime = script([AssistantTurn::text_reply("Fixed and verified.")]);
    let (report, _) = run(&case, runtime, &cbmc, true).await;

    assert_eq!(
        report.outcome.as_ref().unwrap().output,
        "Fixed and verified."
    );
    assert_eq!(failed_checks(&report), ["cbmc_verified"]);
    let detail = &check(&report, "cbmc_verified").result.detail;
    assert!(detail.contains("average.division-by-zero"), "{detail}");
}

/// A patch for another file is refused by the tool and changes nothing.
#[tokio::test]
async fn patches_to_other_files_are_refused() {
    let Some(cbmc) = cbmc() else { return };
    let case = case("buffer_off_by_one");
    let patch = unified_diff("other.c", "int x;\n", "int x = 1;\n");
    let (report, audit) = run(&case, fixing_script(&case, patch), &cbmc, true).await;

    assert!(!report.accepted());
    assert!(report.acceptance.report.diff.is_empty(), "nothing changed");
    let result = audit
        .lines()
        .iter()
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["event"].clone())
        .find(|e| e["kind"] == "tool_result" && e["name"] == "apply_patch")
        .unwrap();
    assert_eq!(result["ok"], false);
    assert!(
        result["output"].as_str().unwrap().contains("buffer.c"),
        "{result}"
    );
}
