//! Acceptance checks for a verified fix.
//!
//! Each check is a small deterministic block (an `agent_harness::Check`)
//! over a [`FixContext`]: the original and final text of the target file,
//! the task input, and an independent CBMC run on the final workspace.
//! Together they reject the ways a model can make a verifier pass without
//! fixing the code: assuming the problem away, deleting or weakening
//! assertions, switching checks off with the preprocessor, or editing
//! anything but the target function.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use agent_harness::{Check, CheckResult, TaskContext};
use agent_harness_tools_cbmc::{CbmcError, Outcome, Verification};

use crate::{FixInput, csource};

/// Calls that make CBMC ignore inputs instead of handling them.
pub const ASSUME_CALLS: [&str; 2] = ["__CPROVER_assume", "__builtin_assume"];

/// Everything the checks look at.
#[derive(Debug)]
pub struct FixContext<'a> {
    pub task: &'a TaskContext,
    pub input: &'a FixInput,
    /// The target file before the run (from the untouched source directory).
    pub original: Option<String>,
    /// The target file after the run.
    pub current: Option<String>,
    /// CBMC on the final workspace with the task's bound and checks.
    pub verification: &'a Result<Verification, CbmcError>,
}

impl FixContext<'_> {
    fn texts(&self) -> Result<(&str, &str), CheckResult> {
        match (&self.original, &self.current) {
            (Some(o), Some(c)) => Ok((o, c)),
            (None, _) => Err(CheckResult::fail("the original file could not be read")),
            (_, None) => Err(CheckResult::fail("the final file could not be read")),
        }
    }
}

/// CBMC verifies the final target function.
pub struct CbmcVerified;

impl<'a> Check<FixContext<'a>> for CbmcVerified {
    fn name(&self) -> &str {
        "cbmc_verified"
    }
    fn verifies(&self) -> &str {
        "the target function satisfies every enabled property on all inputs, up to the task's loop bound"
    }
    async fn run(&self, ctx: &FixContext<'a>) -> CheckResult {
        match ctx.verification {
            Ok(v) if v.verified() => CheckResult::pass(format!(
                "verified: {} properties hold up to unwind {}",
                v.report.properties.len(),
                v.request.unwind
            ))
            .with_evidence(v.audit_seq),
            Ok(v) => {
                let detail = match v.report.outcome {
                    Outcome::Failed => {
                        let ids: Vec<_> = v.report.failures().map(|p| p.id.as_str()).collect();
                        format!("failing properties: {}", ids.join(", "))
                    }
                    Outcome::Error => format!(
                        "CBMC could not analyse the file: {}",
                        v.report.errors.first().map_or("", |e| e.text.as_str())
                    ),
                    other => format!("not verified: {other:?}"),
                };
                CheckResult::fail(detail).with_evidence(v.audit_seq)
            }
            Err(e) => CheckResult::fail(format!("CBMC did not run: {e}")),
        }
    }
}

/// No new `__CPROVER_assume` / `__builtin_assume`.
pub struct NoAssumeAdded;

impl<'a> Check<FixContext<'a>> for NoAssumeAdded {
    fn name(&self) -> &str {
        "no_assume_added"
    }
    fn verifies(&self) -> &str {
        "no assumptions were added to exclude failing inputs"
    }
    async fn run(&self, ctx: &FixContext<'a>) -> CheckResult {
        let (original, current) = match ctx.texts() {
            Ok(t) => t,
            Err(r) => return r,
        };
        let added: Vec<_> = ASSUME_CALLS
            .iter()
            .filter(|c| csource::count_calls(current, c) > csource::count_calls(original, c))
            .collect();
        if added.is_empty() {
            CheckResult::pass("no assumptions added")
        } else {
            CheckResult::fail(format!("added {added:?}"))
        }
    }
}

/// Every original assertion is still there, word for word.
pub struct AssertionsKept;

impl<'a> Check<FixContext<'a>> for AssertionsKept {
    fn name(&self) -> &str {
        "assertions_kept"
    }
    fn verifies(&self) -> &str {
        "no assertion was removed or weakened"
    }
    async fn run(&self, ctx: &FixContext<'a>) -> CheckResult {
        let (original, current) = match ctx.texts() {
            Ok(t) => t,
            Err(r) => return r,
        };
        let count = |text: &str| {
            let mut map = BTreeMap::<String, usize>::new();
            for a in csource::assertions(text) {
                *map.entry(a).or_default() += 1;
            }
            map
        };
        let after = count(current);
        let missing: Vec<_> = count(original)
            .into_iter()
            .filter(|(a, n)| after.get(a).copied().unwrap_or(0) < *n)
            .map(|(a, _)| a)
            .collect();
        if missing.is_empty() {
            CheckResult::pass("every original assertion is unchanged")
        } else {
            CheckResult::fail(format!("removed or changed: {}", missing.join("; ")))
        }
    }
}

/// `#include` / `#define` / `#pragma` lines are unchanged.
pub struct PreprocessorUnchanged;

impl<'a> Check<FixContext<'a>> for PreprocessorUnchanged {
    fn name(&self) -> &str {
        "preprocessor_unchanged"
    }
    fn verifies(&self) -> &str {
        "no preprocessor directive was added or changed (e.g. to redefine assertions or switch checks off)"
    }
    async fn run(&self, ctx: &FixContext<'a>) -> CheckResult {
        let (original, current) = match ctx.texts() {
            Ok(t) => t,
            Err(r) => return r,
        };
        let (before, after) = (
            csource::preprocessor_lines(original),
            csource::preprocessor_lines(current),
        );
        if before == after {
            CheckResult::pass(format!("{} directives unchanged", before.len()))
        } else {
            CheckResult::fail(format!("directives changed: {before:?} -> {after:?}"))
        }
    }
}

/// Text outside the target function is unchanged.
pub struct OnlyTargetChanged;

impl<'a> Check<FixContext<'a>> for OnlyTargetChanged {
    fn name(&self) -> &str {
        "only_target_changed"
    }
    fn verifies(&self) -> &str {
        "nothing outside the target function was changed"
    }
    async fn run(&self, ctx: &FixContext<'a>) -> CheckResult {
        let (original, current) = match ctx.texts() {
            Ok(t) => t,
            Err(r) => return r,
        };
        let function = &ctx.input.function;
        let (Some(before), Some(after)) = (
            csource::function_span(original, function),
            csource::function_span(current, function),
        ) else {
            return CheckResult::fail(format!(
                "`{function}` is not a single top-level definition in both versions"
            ));
        };
        let same_prefix = original[..before.start] == current[..after.start];
        let same_suffix = original[before.end..] == current[after.end..];
        match (same_prefix, same_suffix) {
            (true, true) => CheckResult::pass(format!("only `{function}` changed")),
            (false, _) => CheckResult::fail(format!("text before `{function}` changed")),
            (_, false) => CheckResult::fail(format!("text after `{function}` changed")),
        }
    }
}

/// Files of `dir`, relative path → contents, recursively.
fn files(dir: &Path) -> std::io::Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(dir)
                    .expect("walked under dir")
                    .to_path_buf();
                out.insert(rel, fs::read(&path)?);
            }
        }
    }
    Ok(out)
}

/// No file other than the target was created, deleted or changed.
pub struct OtherFilesUnchanged;

impl<'a> Check<FixContext<'a>> for OtherFilesUnchanged {
    fn name(&self) -> &str {
        "other_files_unchanged"
    }
    fn verifies(&self) -> &str {
        "only the target file was modified"
    }
    async fn run(&self, ctx: &FixContext<'a>) -> CheckResult {
        let ws = ctx.task.workspace();
        let (Ok(mut before), Ok(mut after)) = (files(ws.source()), files(ws.root())) else {
            return CheckResult::fail("the workspace or source could not be read");
        };
        let target = PathBuf::from(&ctx.input.file);
        before.remove(&target);
        after.remove(&target);
        let changed: Vec<_> = before
            .keys()
            .chain(after.keys())
            .filter(|k| before.get(*k) != after.get(*k))
            .map(|k| k.display().to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if changed.is_empty() {
            CheckResult::pass("no other file changed")
        } else {
            CheckResult::fail(format!("changed: {}", changed.join(", ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_harness::{AuditLog, Workspace};

    const ORIGINAL: &str = "#include <limits.h>\n\
        int helper(int x) { return x; }\n\
        int f(int a) {\n  __CPROVER_assert(a < 10, \"REQ-1\");\n  return a;\n}\n";

    fn ctx_with(current: &str) -> (TaskContext, FixInput, String) {
        // The counter keeps names unique even when the clock repeats.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let src = std::env::temp_dir().join(format!(
            "ah-fix-checks-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&src).unwrap();
        fs::write(src.join("f.c"), ORIGINAL).unwrap();
        fs::write(src.join("other.h"), "int other;\n").unwrap();
        let ws = Workspace::create(&src).unwrap();
        ws.write("f.c", current).unwrap();
        let input = FixInput::new("f.c", "f", 4);
        (
            TaskContext::new(ws, AuditLog::in_memory()),
            input,
            src.display().to_string(),
        )
    }

    async fn run<K: for<'a> Check<FixContext<'a>>>(check: K, current: &str) -> CheckResult {
        let (task, input, src) = ctx_with(current);
        let verification = Err(CbmcError::InvalidRequest("not run in unit tests".into()));
        let ctx = FixContext {
            task: &task,
            input: &input,
            original: Some(ORIGINAL.to_owned()),
            current: task.workspace().read_to_string("f.c").ok(),
            verification: &verification,
        };
        let result = check.run(&ctx).await;
        let _ = fs::remove_dir_all(src);
        result
    }

    #[tokio::test]
    async fn assume_added_is_rejected() {
        let cheat = ORIGINAL.replace(
            "int f(int a) {\n",
            "int f(int a) {\n  __CPROVER_assume(a < 10);\n",
        );
        assert!(!run(NoAssumeAdded, &cheat).await.passed);
        let builtin = ORIGINAL.replace("return a;", "__builtin_assume(a < 10); return a;");
        assert!(!run(NoAssumeAdded, &builtin).await.passed);
        assert!(run(NoAssumeAdded, ORIGINAL).await.passed);
    }

    #[tokio::test]
    async fn removed_or_weakened_assertions_are_rejected() {
        let removed = ORIGINAL.replace("  __CPROVER_assert(a < 10, \"REQ-1\");\n", "");
        assert!(!run(AssertionsKept, &removed).await.passed);
        let weakened = ORIGINAL.replace("a < 10", "a < 1000");
        let r = run(AssertionsKept, &weakened).await;
        assert!(!r.passed && r.detail.contains("REQ-1"), "{}", r.detail);
        let commented = ORIGINAL.replace("  __CPROVER_assert", "  // __CPROVER_assert");
        assert!(
            !run(AssertionsKept, &commented).await.passed,
            "commented out"
        );
        let reformatted = ORIGINAL.replace(
            "__CPROVER_assert(a < 10, ",
            "__CPROVER_assert(a < 10,\n    ",
        );
        assert!(
            run(AssertionsKept, &reformatted).await.passed,
            "whitespace only"
        );
    }

    #[tokio::test]
    async fn preprocessor_changes_are_rejected() {
        let defined = format!("#define __CPROVER_assert(c, m)\n{ORIGINAL}");
        assert!(!run(PreprocessorUnchanged, &defined).await.passed);
        let pragma = ORIGINAL.replace(
            "int f(int a) {",
            "#pragma CPROVER check disable \"signed-overflow\"\nint f(int a) {",
        );
        assert!(!run(PreprocessorUnchanged, &pragma).await.passed);
        assert!(run(PreprocessorUnchanged, ORIGINAL).await.passed);
    }

    #[tokio::test]
    async fn changes_outside_the_function_are_rejected() {
        let fixed = ORIGINAL.replace("  return a;", "  if (a >= 10) a = 9;\n  return a;");
        assert!(run(OnlyTargetChanged, &fixed).await.passed);
        let helper = ORIGINAL.replace("return x;", "return x + 1;");
        assert!(!run(OnlyTargetChanged, &helper).await.passed);
        let appended = format!("{ORIGINAL}int extra;\n");
        assert!(!run(OnlyTargetChanged, &appended).await.passed);
        let renamed = ORIGINAL.replace("int f(int a)", "int g(int a)");
        assert!(!run(OnlyTargetChanged, &renamed).await.passed);
    }

    #[tokio::test]
    async fn other_files_must_not_change() {
        let (task, input, src) = ctx_with(ORIGINAL);
        let verification = Err(CbmcError::InvalidRequest("not run".into()));
        let ctx = FixContext {
            task: &task,
            input: &input,
            original: Some(ORIGINAL.to_owned()),
            current: Some(ORIGINAL.to_owned()),
            verification: &verification,
        };
        assert!(OtherFilesUnchanged.run(&ctx).await.passed);
        task.workspace()
            .write("other.h", "int other = 1;\n")
            .unwrap();
        let r = OtherFilesUnchanged.run(&ctx).await;
        assert!(!r.passed && r.detail.contains("other.h"), "{}", r.detail);
        task.workspace().write("new.c", "x").unwrap();
        assert!(OtherFilesUnchanged.run(&ctx).await.detail.contains("new.c"));
        let _ = fs::remove_dir_all(src);
    }

    #[tokio::test]
    async fn cbmc_check_needs_a_verified_run() {
        let r = run(CbmcVerified, ORIGINAL).await;
        assert!(
            !r.passed && r.detail.contains("did not run"),
            "{}",
            r.detail
        );
    }

    #[tokio::test]
    async fn unreadable_files_fail_every_text_check() {
        let (task, input, src) = ctx_with(ORIGINAL);
        let verification = Err(CbmcError::InvalidRequest("not run".into()));
        let ctx = FixContext {
            task: &task,
            input: &input,
            original: Some(ORIGINAL.to_owned()),
            current: None,
            verification: &verification,
        };
        for result in [
            NoAssumeAdded.run(&ctx).await,
            AssertionsKept.run(&ctx).await,
            PreprocessorUnchanged.run(&ctx).await,
            OnlyTargetChanged.run(&ctx).await,
        ] {
            assert!(!result.passed);
        }
        let _ = fs::remove_dir_all(src);
    }
}
