//! Benchmark: the task over a corpus, with several models, several times.
//!
//! Runs are interleaved (run 1 of every case for every model, then run 2,
//! ...), so a temporary provider outage hits models evenly rather than
//! wiping out one model's batch. Every run has its own audit log, verified
//! after the run; its last hash is part of the results, so the benchmark is
//! itself audit evidence. Patches are approved by [`AutoApprove`], recorded
//! as a policy decision: there is no person in a benchmark loop.
//!
//! Each run ends in one of three outcomes:
//! * `accepted`: every acceptance check passed;
//! * `rejected`: the loop finished but a check failed (the model's fix was
//!   wrong or missing);
//! * `error`: the run could not finish (the model API failed after retries,
//!   or the run could not be set up). Not a judgement on the model's skill.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use agent_harness::{
    AuditLog, AutoApprove, BuildInfo, ChatRuntime, RiskGate, TaskReport, TaskRunner, verify_chain,
};
use serde::Serialize;

use crate::{FixReport, VerifiedFix, corpus::Case};

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Accepted,
    Rejected,
    Error,
}

/// One run of one case with one model.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunRecord {
    pub model: String,
    pub case: String,
    /// 1-based run number.
    pub run: usize,
    pub verdict: Verdict,
    /// Acceptance checks that failed (empty when accepted).
    pub failed_checks: Vec<String>,
    /// Why the run did not finish (`error`), or why the loop ended early.
    pub error: Option<String>,
    pub turns: Option<usize>,
    pub total_tokens: Option<u64>,
    /// Model requests retried after a transient failure.
    pub retries: usize,
    pub seconds: f64,
    /// The change the model made (empty if none).
    pub diff: String,
    pub audit: PathBuf,
    /// Last hash of this run's verified audit chain.
    pub audit_last_hash: Option<String>,
}

/// Results for one model across the whole corpus.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelSummary {
    pub model: String,
    pub runs: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub errors: usize,
    /// accepted / runs.
    pub accept_rate: f64,
    /// accepted / (runs that finished), i.e. excluding infrastructure errors.
    pub accept_rate_finished: f64,
    /// Means over accepted runs.
    pub mean_turns_accepted: Option<f64>,
    pub mean_tokens_accepted: Option<f64>,
    pub retries: usize,
    /// case → (accepted, runs).
    pub per_case: BTreeMap<String, (usize, usize)>,
}

/// The whole benchmark.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BenchReport {
    pub build: BuildInfo,
    pub runs_per_case: usize,
    pub cases: Vec<String>,
    pub models: Vec<String>,
    pub summaries: Vec<ModelSummary>,
    pub records: Vec<RunRecord>,
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, n) = values.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / n as f64)
}

/// Per-model summaries, in the order models first appear in `records`.
pub fn summarize(records: &[RunRecord]) -> Vec<ModelSummary> {
    let mut models: Vec<&str> = Vec::new();
    for r in records {
        if !models.contains(&r.model.as_str()) {
            models.push(&r.model);
        }
    }
    models
        .into_iter()
        .map(|model| {
            let mine: Vec<&RunRecord> = records.iter().filter(|r| r.model == model).collect();
            let count = |v: Verdict| mine.iter().filter(|r| r.verdict == v).count();
            let (accepted, rejected, errors) = (
                count(Verdict::Accepted),
                count(Verdict::Rejected),
                count(Verdict::Error),
            );
            let ok: Vec<&&RunRecord> = mine
                .iter()
                .filter(|r| r.verdict == Verdict::Accepted)
                .collect();
            let mut per_case = BTreeMap::<String, (usize, usize)>::new();
            for r in &mine {
                let entry = per_case.entry(r.case.clone()).or_default();
                entry.1 += 1;
                if r.verdict == Verdict::Accepted {
                    entry.0 += 1;
                }
            }
            let ratio = |a: usize, b: usize| if b == 0 { 0.0 } else { a as f64 / b as f64 };
            ModelSummary {
                model: model.to_owned(),
                runs: mine.len(),
                accepted,
                rejected,
                errors,
                accept_rate: ratio(accepted, mine.len()),
                accept_rate_finished: ratio(accepted, accepted + rejected),
                mean_turns_accepted: mean(ok.iter().filter_map(|r| r.turns.map(|t| t as f64))),
                mean_tokens_accepted: mean(
                    ok.iter().filter_map(|r| r.total_tokens.map(|t| t as f64)),
                ),
                retries: mine.iter().map(|r| r.retries).sum(),
                per_case,
            }
        })
        .collect()
}

/// The report as Markdown tables.
pub fn markdown(report: &BenchReport) -> String {
    let mut out = String::new();
    out.push_str("| Model | Accepted | Rejected | Errors | Accept rate | Accept rate (finished runs) | Mean turns | Mean tokens | Retries |\n");
    out.push_str("|---|---|---|---|---|---|---|---|---|\n");
    let fmt = |v: Option<f64>| v.map_or_else(|| "–".to_owned(), |v| format!("{v:.1}"));
    for s in &report.summaries {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {:.0}% | {:.0}% | {} | {} | {} |\n",
            s.model,
            s.accepted,
            s.rejected,
            s.errors,
            s.accept_rate * 100.0,
            s.accept_rate_finished * 100.0,
            fmt(s.mean_turns_accepted),
            fmt(s.mean_tokens_accepted),
            s.retries,
        ));
    }
    out.push_str("\n| Case |");
    for s in &report.summaries {
        out.push_str(&format!(" `{}` |", s.model));
    }
    out.push_str("\n|---|");
    out.push_str(&"---|".repeat(report.summaries.len()));
    out.push('\n');
    for case in &report.cases {
        out.push_str(&format!("| {case} |"));
        for s in &report.summaries {
            let (a, n) = s.per_case.get(case).copied().unwrap_or_default();
            out.push_str(&format!(" {a}/{n} |"));
        }
        out.push('\n');
    }
    out
}

/// A file-name-safe form of a model spec (`openai:gpt-5.6` → `openai_gpt-5.6`).
fn file_safe(text: &str) -> String {
    let safe: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Never a path of its own: "", ".", ".." become "_", "_.", "_..".
    if safe.is_empty() || safe.chars().all(|c| c == '.') {
        format!("_{safe}")
    } else {
        safe
    }
}

fn record_of(
    model: &str,
    case: &Case,
    run: usize,
    result: Result<TaskReport<FixReport>, String>,
    audit: &Path,
    seconds: f64,
) -> RunRecord {
    let chain = verify_chain(audit);
    let retries = fs::read_to_string(audit)
        .map(|text| {
            text.lines()
                .filter(|l| l.contains("\"kind\":\"model_retry\""))
                .count()
        })
        .unwrap_or(0);
    let mut record = RunRecord {
        model: model.to_owned(),
        case: case.name.clone(),
        run,
        verdict: Verdict::Error,
        failed_checks: Vec::new(),
        error: None,
        turns: None,
        total_tokens: None,
        retries,
        seconds,
        diff: String::new(),
        audit: audit.to_path_buf(),
        audit_last_hash: chain.as_ref().ok().map(|c| c.last_hash.clone()),
    };
    match result {
        Ok(report) => {
            let fix = &report.acceptance.report;
            record.turns = fix.turns;
            record.total_tokens = fix.total_tokens;
            record.diff = fix.diff.clone();
            record.failed_checks = report
                .acceptance
                .checks
                .iter()
                .filter(|c| !c.result.passed)
                .map(|c| c.name.clone())
                .collect();
            record.error = report.outcome.as_ref().err().map(ToString::to_string);
            record.verdict = if report.accepted() {
                Verdict::Accepted
            } else if report.outcome.is_err() {
                Verdict::Error
            } else {
                Verdict::Rejected
            };
        }
        Err(error) => record.error = Some(error),
    }
    if let Err(error) = chain {
        record.verdict = Verdict::Error;
        record.error = Some(format!("audit chain did not verify: {error}"));
    }
    record
}

/// Run every case `runs` times with every model, writing one audit log per
/// run under `out_dir/<model>/`. Never stops early: a failed run becomes an
/// `error` record.
pub async fn run_bench<R: ChatRuntime>(
    task: &VerifiedFix,
    cases: &[Case],
    models: &[(String, R)],
    runs: usize,
    out_dir: &Path,
    mut progress: impl FnMut(&RunRecord),
) -> std::io::Result<BenchReport> {
    let mut records = Vec::new();
    for run in 1..=runs {
        for case in cases {
            for (label, runtime) in models {
                let dir = out_dir.join(file_safe(label));
                fs::create_dir_all(&dir)?;
                let audit_path = dir.join(format!("{}-run{run}.jsonl", case.name));
                let start = Instant::now();
                let result = match AuditLog::create(&audit_path) {
                    Ok(audit) => TaskRunner::new(runtime, audit)
                        .with_policy(RiskGate::new(AutoApprove))
                        .keep_workspace(false)
                        .run(task, case.input.clone(), &case.source)
                        .await
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(format!("could not create the audit log: {e}")),
                };
                let record = record_of(
                    label,
                    case,
                    run,
                    result,
                    &audit_path,
                    start.elapsed().as_secs_f64(),
                );
                progress(&record);
                records.push(record);
            }
        }
    }
    Ok(BenchReport {
        build: BuildInfo::current(),
        runs_per_case: runs,
        cases: cases.iter().map(|c| c.name.clone()).collect(),
        models: models.iter().map(|(label, _)| label.clone()).collect(),
        summaries: summarize(&records),
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(model: &str, case: &str, verdict: Verdict, turns: usize, tokens: u64) -> RunRecord {
        RunRecord {
            model: model.into(),
            case: case.into(),
            run: 1,
            verdict,
            failed_checks: Vec::new(),
            error: None,
            turns: Some(turns),
            total_tokens: Some(tokens),
            retries: 1,
            seconds: 1.0,
            diff: String::new(),
            audit: PathBuf::from("a.jsonl"),
            audit_last_hash: None,
        }
    }

    #[test]
    fn summaries_separate_rejections_from_errors() {
        let records = [
            record("a", "x", Verdict::Accepted, 4, 1000),
            record("a", "x", Verdict::Accepted, 6, 3000),
            record("a", "y", Verdict::Rejected, 8, 9000),
            record("a", "y", Verdict::Error, 1, 10),
            record("b", "x", Verdict::Error, 1, 10),
        ];
        let s = summarize(&records);
        assert_eq!(s.len(), 2);
        let a = &s[0];
        assert_eq!((a.runs, a.accepted, a.rejected, a.errors), (4, 2, 1, 1));
        assert_eq!(a.accept_rate, 0.5);
        assert!(
            (a.accept_rate_finished - 2.0 / 3.0).abs() < 1e-9,
            "errors excluded"
        );
        assert_eq!(a.mean_turns_accepted, Some(5.0), "accepted runs only");
        assert_eq!(a.mean_tokens_accepted, Some(2000.0));
        assert_eq!(a.retries, 4);
        assert_eq!(a.per_case["x"], (2, 2));
        assert_eq!(a.per_case["y"], (0, 2));
        let b = &s[1];
        assert_eq!((b.accept_rate, b.accept_rate_finished), (0.0, 0.0));
        assert_eq!(b.mean_turns_accepted, None);
    }

    #[test]
    fn markdown_has_a_row_per_model_and_case() {
        let records = vec![
            record("a", "x", Verdict::Accepted, 4, 1000),
            record("b", "x", Verdict::Rejected, 4, 1000),
        ];
        let report = BenchReport {
            build: BuildInfo::current(),
            runs_per_case: 1,
            cases: vec!["x".into()],
            models: vec!["a".into(), "b".into()],
            summaries: summarize(&records),
            records,
        };
        let md = markdown(&report);
        assert!(
            md.contains("| `a` | 1 | 0 | 0 | 100% | 100% | 4.0 | 1000.0 | 1 |"),
            "{md}"
        );
        assert!(md.contains("| x | 1/1 | 0/1 |"), "{md}");
    }

    #[test]
    fn model_labels_become_safe_file_names() {
        assert_eq!(file_safe("openai:gpt-5.6"), "openai_gpt-5.6");
        assert_eq!(file_safe("../x"), ".._x");
        assert_eq!(file_safe(".."), "_..");
        assert_eq!(file_safe(""), "_");
    }
}
