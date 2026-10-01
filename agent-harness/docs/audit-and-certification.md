# Audit trail and certification support

Status: implemented in the core crate (`src/audit.rs`, `src/process.rs`,
`src/task.rs`, `src/workspace.rs`). Every run through `TaskRunner` is audited.

## What this is, and what it is not

An LLM agent harness is not a certified tool. Under DO-178C and DO-330 a
development tool earns certification credit only through tool qualification,
and nothing here claims that. What the harness provides instead is
**evidence a certification process can use**: a complete, tamper-evident,
reproducible record of what an agent did, with which versions and on whose
approval, so every change it proposes can be reviewed and verified
independently, like any other change.

Concretely, the record supports:

- **Review of agent-made changes:** the exact inputs, every model turn, every
  tool call and approval, every program run, and the final file hashes.
- **Configuration identification:** harness version and commit, model and
  provider (requested and reported), program hashes and versions, sandbox
  needs.
- **Independent verification:** acceptance is decided by deterministic
  checks re-run on the final workspace, each traced to the requirement or
  property it verifies, never by the model's own claims.

Whether that evidence is sufficient for a given objective, and whether any
external tool (e.g. a model checker) needs qualification, remains the
certification plan's decision.

## Principles

1. **Model output is never evidence.** Only `Task::accept`'s deterministic
   checks and their recorded results decide acceptance. The model's final
   text is recorded as `final_answer` for information only.
2. **Record everything, including failures.** Model turns, tool calls with
   arguments, approvals with their decider, tool results, program runs,
   timeouts, denials and errors are all recorded, not only successes.
3. **Fail closed.** If a record cannot be written, the log is marked failed:
   every further tool call is denied, no program is started, failing checks
   fail, and the run ends with `TaskError::Audit`. There are no unaudited runs.
4. **Tamper-evident and append-only.** Records form a SHA-256 hash chain;
   editing, inserting, deleting or reordering any record is detected by
   `verify_chain`. Log files are created new and never overwritten.
5. **Configuration identification.** Each run records the harness version
   (and `AGENT_HARNESS_GIT_COMMIT` when set at build time), the runtime's
   provider, model, temperature and token budget, the model id the provider
   reports in each response, the SHA-256 of every program run, input and
   output file hashes, the toolset with each tool's risk, and the task's
   sandbox needs.
6. **Traceability.** Each check names the requirement or property it
   verifies (`Check::verifies`) and points to the audit records holding its
   evidence (`CheckResult::evidence`). The report refers to records rather
   than copying them.
7. **Replayable.** Model responses are recorded in full, so a run can be
   replayed offline through a scripted runtime and its acceptance re-run. The
   model itself is not reproducible; its recorded output is.
8. **No secrets.** Provider keys never enter the sandbox (OpenShell injects
   them), and strings that look like credentials are masked before hashing.
   The record still contains code and prompts: store it with the same access
   controls as the source.
9. **Attributed approvals.** Every approval names its decider: a policy
   (`auto_approve`, `risk_gate:read_only`, `deny_all`, `audit_gate`, …) or a
   human (identified as well as the caller can, e.g. the OS login for the
   console approver). Automatic decisions are never recorded as human ones.
10. **Independence.** Acceptance re-runs verification itself on the final
    workspace, separately from anything the agent ran during the loop.

## Record format

One JSON object per line (JSON Lines):

```json
{"event":{"kind":"tool_call","turn":2,"name":"write_file","risk":"mutating",
  "approval":{"decision":"approved","by":{"kind":"human","id":"alice"}},...},
 "hash":"<sha256>","prev":"<hash of previous record>","seq":5,"ts_ms":1790000000000}
```

- `seq` starts at 0 and increases by one; `ts_ms` is UTC milliseconds.
- `hash` is the SHA-256 of the record serialized **without** its `hash`
  field; `prev` is the previous record's `hash` (64 zeros for the first).
- `event.kind` is one of:

| Kind | Recorded when | Main fields |
|---|---|---|
| `run_started` | before the loop | task, harness version, git commit, runtime, max turns, preamble, prompt, input, input file hashes, tools with risk, sandbox needs |
| `model_turn` | each model response | turn, full content, usage, provider, reported model, response and request ids |
| `tool_call` | each requested call | turn, call id, name, arguments, risk, approval and decider |
| `tool_result` | after each call | turn, call id, name, ok, output or error text |
| `process_run` | each program run | program, program SHA-256, args, cwd, exit code, timed out, elapsed, stdout/stderr SHA-256 and byte counts, truncated |
| `program_identified` | `process::identify` | program, SHA-256, self-reported version |
| `final_answer` | the model stops calling tools | turn, text (informational) |
| `run_ended` | after the loop, success or failure | ok, error, turns, tool calls, usage, output file hashes |
| `check_evaluated` | each acceptance check | name, what it verifies, passed, detail, evidence record numbers |
| `accepted` | the verdict | accepted, number of checks, the task's report |
| `note` | free-form | message |

New kinds may be added; consumers should ignore kinds they do not know.

## Verifying a log

```rust
let summary = agent_harness::verify_chain("run.jsonl")?;
println!("{} records, last hash {}", summary.records, summary.last_hash);
```

`verify_chain` reports the first broken record and why (content changed,
`prev` mismatch, or sequence gap). The last hash can be stored separately
(e.g. in a review ticket or a signed release note) so that truncating the end
of a log is detectable too.

## Using it from a task

- Run programs only through `agent_harness::process::run` (or `identify`
  for versions). It records each run and refuses to start anything once the
  log has failed.
- Register read-only tools with `register_read_only`; everything else is
  mutating and goes to the approval policy (default: `RiskGate<DenyAll>`).
- Build acceptance from small `Check`s run through `evaluate`, give each a
  `verifies()` string naming the requirement or property, and attach the
  audit record numbers of the evidence with `CheckResult::with_evidence`.
- Build the report from the workspace and recorded evidence, never from the
  model's text.

## Known limits

- Timestamps come from the host clock and are not trusted time.
- The chain detects modification but not wholesale replacement of a log by
  someone who recomputes every hash; keep the final hash out of band, or add
  signing (not implemented).
- The console approver identifies people by OS login, which is only as
  strong as the machine's account management.
- Records are flushed per write but not `fsync`ed; a power loss can lose the
  tail of a log (detectable as a missing `accepted` record).
