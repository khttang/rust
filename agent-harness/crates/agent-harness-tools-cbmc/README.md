# agent-harness-tools-cbmc

[CBMC](https://www.cprover.org/cbmc/) bounded model checking as an audited
[agent-harness](../../README.md) tool. CBMC checks a C function on **all**
inputs, up to a loop bound, for assertion failures, arithmetic overflow,
out-of-bounds accesses, invalid pointers, division by zero and undefined
shifts, and gives a concrete counterexample for every failure.

## What's in the crate

| Item | Use |
|---|---|
| `CbmcVerify` | The model-facing `cbmc_verify` tool. Read-only: register it with `register_read_only`. |
| `verify(ctx, config, request)` | Run CBMC on a workspace file. Acceptance checks call this directly, so they re-verify independently of anything the agent ran. |
| `report::parse` | CBMC `--json-ui` → `CbmcReport`: outcome, version, properties with kind and location, shortened counterexamples. |
| `identify(ctx, config)` | Record the CBMC binary's SHA-256 and version in the audit log. |
| `CbmcConfig::sandbox_needs()` | Programs CBMC runs: itself, `/bin/sh`, `gcc` (its preprocessor). No network. |

```rust
use agent_harness::ToolRegistry;
use agent_harness_tools_cbmc::{CbmcConfig, CbmcVerify, VerifyRequest, verify};

let config = CbmcConfig::default();                 // /usr/bin/cbmc, 120 s, unwind ≤ 64
let mut tools = ToolRegistry::new();
tools.register_read_only(CbmcVerify::new(ctx.clone(), config.clone()))?;

// In Task::accept: re-verify the final workspace yourself.
let v = verify(&ctx, &config, VerifyRequest::new("pitch.c", "pitch_cmd", 8)).await?;
let evidence = v.audit_seq;                         // cite in CheckResult::with_evidence
assert!(v.verified());
```

## What the model sees

For `tests/fixtures/overflow.c` (the counterexample is the parser's real output on CBMC 6.6.0; `audit_record` varies):

```json
{
  "outcome": "failed",
  "summary": "1 property failing; each failure has a counterexample.",
  "cbmc_version": "CBMC 6.6.0 (cbmc-6.6.0)",
  "unwind": 8,
  "checks": ["bounds", "pointer", "div_by_zero", "signed_overflow", "pointer_overflow", "undefined_shift"],
  "failures": [{
    "id": "pitch_cmd.overflow.1", "kind": "overflow", "status": "failure",
    "description": "arithmetic overflow on signed * in error * gain",
    "location": {"file": "overflow.c", "function": "pitch_cmd", "line": 2},
    "counterexample": {"omitted": 0, "steps": [
      {"step": "input",   "name": "error", "value": "-1073741826", "line": 1},
      {"step": "input",   "name": "gain",  "value": "-2", "line": 1},
      {"step": "call",    "function": "pitch_cmd", "line": 1},
      {"step": "assign",  "name": "error", "value": "-1073741826", "line": 1},
      {"step": "assign",  "name": "gain",  "value": "-2", "line": 1},
      {"step": "failure", "reason": "arithmetic overflow on signed * in error * gain", "line": 2}
    ]}
  }],
  "passed": 0, "errors": [], "audit_record": 12
}
```

- An `unwinding` failure (`<f>.unwind.<n>`) means the loop bound was too
  small, not that the code is wrong; the summary says so.
- A syntax or type error gives `outcome: "error"` with CBMC's messages.
- A timeout gives `outcome: "timed_out"`.

## Safety and auditing

- Every run goes through `agent_harness::process::run`: no shell, cleared
  environment plus an explicit `PATH` (CBMC finds `gcc` on it), timeout,
  capped output, and an audit record with the CBMC binary's SHA-256.
- Requests are validated before CBMC starts: the file must be inside the
  workspace (passed as an absolute path, so it cannot be read as an option),
  the function must be a C identifier, the loop bound within
  `max_unwind`, and checks only from `CheckFlag`.
- Exit codes are cross-checked with CBMC's own verdict (0 verified, 10
  failed, otherwise error); a mismatch is an error, not a result.

## Requirements and tests

Tested with CBMC 6.6.0 from Debian trixie (`cbmc=6.6.0-4`, which pulls in
`gcc`). The parser's unit tests use real CBMC 6.6.0 output in
`tests/fixtures`, so they run anywhere. `tests/real_cbmc.rs` runs the real
binary (`CBMC_PATH` or `/usr/bin/cbmc`); it skips when CBMC is missing unless
`AGENT_HARNESS_REQUIRE_CBMC=1`. `../../test-in-container.sh` installs CBMC and
sets that variable, so nothing is skipped there.
