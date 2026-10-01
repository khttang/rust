//! `run_bench` end to end with real CBMC and two scripted models: an oracle
//! that applies each case's reference fix, and a lazy one that only claims
//! success. Skips without CBMC unless `AGENT_HARNESS_REQUIRE_CBMC=1`.

use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use agent_harness::{
    AssistantTurn, ChatRuntime,
    rig_core::{
        completion::{AssistantContent, ToolDefinition},
        message::{Message, ToolName},
    },
    verify_chain,
};
use agent_harness_task_verified_fix::{
    VerifiedFix,
    bench::{Verdict, markdown, run_bench},
    corpus::{self, Case},
};
use agent_harness_tools_cbmc::CbmcConfig;
use serde_json::json;

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
    eprintln!("skipping: CBMC not found at {}", path.display());
    None
}

/// Stateless scripted model. It finds the case from the prompt and its step
/// from the number of assistant turns so far: patch, then claim success.
struct Oracle {
    cases: Vec<Case>,
    lazy: bool,
}

impl ChatRuntime for Oracle {
    async fn chat(
        &self,
        _preamble: &str,
        history: &[Message],
        _tools: &[ToolDefinition],
    ) -> anyhow::Result<AssistantTurn> {
        let prompt = serde_json::to_string(&history[0])?;
        let case = self
            .cases
            .iter()
            .find(|c| prompt.contains(&format!("`{}`", c.input.function)))
            .ok_or_else(|| anyhow::anyhow!("unknown case"))?;
        let turns = history
            .iter()
            .filter(|m| matches!(m, Message::Assistant { .. }))
            .count();
        if self.lazy || turns > 0 {
            return Ok(AssistantTurn::text_reply("Fixed and verified."));
        }
        let mut turn = AssistantTurn::text_reply("");
        turn.content = vec![AssistantContent::tool_call(
            "c1",
            ToolName::new("apply_patch").unwrap(),
            json!({"patch": case.reference_patch()?}),
        )];
        Ok(turn)
    }
}

fn out_dir() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "ah-bench-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

#[tokio::test]
async fn bench_scores_models_and_keeps_audited_evidence() {
    let Some(cbmc) = cbmc() else { return };
    let cases: Vec<Case> = corpus::bundled().unwrap().into_iter().take(2).collect();
    let models = vec![
        (
            "oracle".to_owned(),
            Oracle {
                cases: cases.clone(),
                lazy: false,
            },
        ),
        (
            "lazy".to_owned(),
            Oracle {
                cases: cases.clone(),
                lazy: true,
            },
        ),
    ];
    let out = out_dir();
    let task = VerifiedFix::new(CbmcConfig::default().with_program(cbmc));
    let mut seen = Vec::new();

    let report = run_bench(&task, &cases, &models, 2, &out, |r| {
        seen.push((r.run, r.case.clone(), r.model.clone()))
    })
    .await
    .unwrap();

    // Interleaved: run 1 of every case and model before run 2.
    assert_eq!(seen.len(), 8);
    assert_eq!(seen[0], (1, cases[0].name.clone(), "oracle".to_owned()));
    assert_eq!(seen[1], (1, cases[0].name.clone(), "lazy".to_owned()));
    assert!(seen[..4].iter().all(|(run, ..)| *run == 1));

    let oracle = &report.summaries[0];
    assert_eq!(
        (oracle.model.as_str(), oracle.accepted, oracle.runs),
        ("oracle", 4, 4)
    );
    assert_eq!(oracle.accept_rate, 1.0);
    assert_eq!(oracle.mean_turns_accepted, Some(2.0));
    let lazy = &report.summaries[1];
    assert_eq!((lazy.rejected, lazy.errors, lazy.accept_rate), (4, 0, 0.0));

    for record in &report.records {
        // Each run's own audit log, verified, with its hash in the results.
        let chain = verify_chain(&record.audit).unwrap();
        assert_eq!(
            record.audit_last_hash.as_deref(),
            Some(chain.last_hash.as_str())
        );
        match record.verdict {
            Verdict::Accepted => {
                assert!(record.diff.starts_with("--- a/"), "{}", record.diff);
                assert!(record.failed_checks.is_empty());
            }
            Verdict::Rejected => assert_eq!(record.failed_checks, ["cbmc_verified"]),
            Verdict::Error => panic!("unexpected error: {:?}", record.error),
        }
    }
    let md = markdown(&report);
    assert!(md.contains("| `oracle` | 4 | 0 | 0 | 100% |"), "{md}");
    let _ = std::fs::remove_dir_all(out);
}
