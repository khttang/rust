# agent-harness-task-verified-fix

The first automated task for [agent-harness](../../README.md): **fix a C
function until [CBMC](https://www.cprover.org/cbmc/) verifies it**, under
checks that make cheating the verifier fail.

The agent gets a C file, a target function and a bug report. It reads the
code, runs CBMC, proposes unified-diff patches, and stops when the function
verifies. A person approves every patch (the default with no approver is to
deny them).

## Tools

| Tool | Risk | What it does |
|---|---|---|
| `read_source` | read-only | Read a workspace file |
| `cbmc_verify` | read-only | CBMC with counterexamples (from [`agent-harness-tools-cbmc`](../agent-harness-tools-cbmc/README.md)) |
| `apply_patch` | **mutating** | Apply a unified diff to **the target file only**; any other file is refused, and a patch whose context does not match asks the model to re-read and retry |

## Acceptance: verified, not trusted

`accept` re-runs CBMC on the final file with the **task's** loop bound and
checks (never the model's), then runs six checks. Each states what it
verifies and is recorded in the audit trail:

| Check | Rejects |
|---|---|
| `cbmc_verified` | anything CBMC does not verify (cites the CBMC run's audit record) |
| `no_assume_added` | new `__CPROVER_assume` / `__builtin_assume`, which make CBMC ignore failing inputs |
| `assertions_kept` | removing **or rewording** any original assertion (each must survive verbatim, whitespace aside) |
| `preprocessor_unchanged` | added or changed `#define` / `#pragma` / `#include` lines |
| `only_target_changed` | edits outside the target function |
| `other_files_unchanged` | changes to any other workspace file |

The `FixReport` carries the diff, the original and final file hashes, CBMC's
verdict and version, the audit record of the acceptance run, turns and
tokens. It is built from the workspace and that CBMC run, never from the
model's text.

```rust
use agent_harness::{AuditLog, RiskGate, TaskRunner};
use agent_harness_task_verified_fix::{VerifiedFix, corpus};

let case = &corpus::bundled()?[0];
let runner = TaskRunner::new(runtime, AuditLog::create("fix.jsonl")?)
    .with_policy(RiskGate::new(approver));      // a person approves each patch
let report = runner.run(&VerifiedFix::default(), case.input.clone(), &case.source).await?;
println!("accepted: {}\n{}", report.accepted(), report.acceptance.report.diff);
```

## Command line

```sh
verified-fix run corpus/pitch_overflow --model openai      # a person approves each patch
HARNESS_AUTO_APPROVE=1 verified-fix run corpus/pitch_overflow   # approve without asking (recorded as policy)
verified-fix self-test corpus      # no model: CBMC on every case (originals fail, references verify)
verified-fix sandbox-needs         # programs and egress the task needs, as JSON
verified-fix --version             # build identity
```

`run` prints the report as JSON (checks, diff, CBMC verdict and evidence,
audit file and its last hash) and exits 0 if accepted, 1 if not, 2 on
errors. Every run writes a hash-chained audit log (`--audit <file>`, default
`verified-fix-<case>-<ms>.jsonl`), verified before the report is printed.
Environment: `HARNESS_MODEL` (default `openai`), the provider's key (a
placeholder under OpenShell), `CBMC_PATH`.

## In the OpenShell sandbox

The sandbox image ships `/app/verified-fix`, the corpus at `/app/corpus`,
and CBMC with `gcc` (pinned in `images.env`). `validate-openshell.sh` step 6
checks the policy covers `sandbox-needs` and runs `self-test` inside the
sandbox. Verified live on 2026-10-01 with `gpt-5.6`: `average_div_zero` and
`pitch_overflow` were fixed and accepted, the second after one audited retry
(OpenShell resets connections about 10 s in; see the security doc).

## Corpus

`corpus/<case>/` holds `case.json` (the `FixInput`: file, function, unwind,
checks, bug report), `src/` (the buggy file, which becomes the workspace) and
`reference/` (a known-good fix, kept out of `src/` so the model never sees
it). Every original fails CBMC and every reference verifies (tested).

| Case | Bug | Why it is not trivial |
|---|---|---|
| `pitch_overflow` | `error * gain` overflows; command must stay within ±1000 (REQ-PITCH-1) | needs wider arithmetic **and** saturation |
| `buffer_off_by_one` | `i <= 8` writes past an 8-slot frame | loop bound 10 must cover the loop |
| `average_div_zero` | `sum / count` with `count == 0` | also `INT_MIN / -1` overflow: a zero check alone fails |
| `ring_index` | `ring[head]` for any `head` | negative `head` breaks a naive `% 4`; `head + 1` overflows |
| `shift_scale` | `raw << shift` | negative and large shifts, plus overflow of the result |

## Tests

- Unit tests (no CBMC): C text analysis (comment/string blanking, function
  spans, assertions, directives), the patch tool (target-only, shifted line
  numbers, malformed input), each acceptance check against its cheats, and
  corpus well-formedness (each reference is exactly its patch's result).
- `tests/corpus.rs` (real CBMC, through `TaskRunner` with a scripted model
  and the real tools): every original fails and every reference verifies;
  honest fixes are accepted for all five cases with a verified audit chain;
  the default policy blocks every patch; and assuming the problem away,
  deleting or weakening an assertion, redefining the assertion macro,
  editing outside the function, claiming success without a fix, and
  patching another file are each rejected by the check written for them,
  including the cheats that fool CBMC itself.

Run with real CBMC: `../../test-in-container.sh -p agent-harness-task-verified-fix`.

Next (M5): live runs over the whole corpus with several models, reported as a
benchmark.
