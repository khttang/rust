# verified-fix benchmark, 2026-10-01: Gemini on a paid tier

A re-run of `gemini-3.5-flash` after its API key moved from the free tier
(20 requests per day per model, which stopped 13 of its 15 runs in the
[first benchmark](../2026-10-01/README.md)) to pay-as-you-go. Same corpus,
3 runs per case, inside the OpenShell sandbox with the key injected by
OpenShell, patches auto-approved (recorded as a policy decision).

## Results

| Model | Accepted | Rejected | Errors | Accept rate | Mean turns | Mean tokens (accepted) | Retries |
|---|---|---|---|---|---|---|---|
| `gemini:gemini-3.5-flash` | 13 | 2 | 0 | 87% | 5.5 | 17,369 | 1 |

| Case | Run 1 | Run 2 | Run 3 |
|---|---|---|---|
| average_div_zero | ✅ | ✅ | ✅ |
| buffer_off_by_one | ✅ | ✅ | ✅ |
| pitch_overflow | ❌ `only_target_changed` | ✅ | ✅ |
| ring_index | ✅ | ✅ | ✅ |
| shift_scale | ❌ `cbmc_verified` | ✅ | ✅ |

Runs took 6–47 s; accepted runs used 4–9 turns and 8,666–62,519 tokens
(275,492 tokens over all 15).

## The two rejections

- **`pitch_overflow`, run 1: a correct fix that also deleted the file's final
  newline.** The function itself was fixed properly (64-bit multiply,
  saturation to ±1000, the requirement assertion untouched), but the patch
  removed the trailing newline after the closing brace (`\ No newline at end
  of file`). That text is outside the target function, so
  `only_target_changed` rejected the run. The check worked as specified; the
  specification is strict about whitespace. Changing that is a policy
  decision, and it would apply only to future runs: this run stays rejected.
- **`shift_scale`, run 1: an explanation instead of a fix.** After reading
  the file and running CBMC, the model answered with a long description of a
  `long long`-based fix but never called `apply_patch`. A text-only reply
  ends the agent loop, the file was unchanged, and `cbmc_verified` rejected
  it. The other two runs of the same case were accepted.

In neither case did the model try to cheat the verifier; both were caught
by the check written for that failure.

## Comparison with `gpt-5.6`

| Model | Accepted | Mean turns | Mean tokens (accepted) | Run time |
|---|---|---|---|---|
| `openai:gpt-5.6` ([first run](../2026-10-01/README.md)) | 15/15 | 4.2 | 7,228 | 9–23 s |
| `gemini:gemini-3.5-flash` (this run) | 13/15 | 5.5 | 17,369 | 6–47 s |

The two models ran about an hour apart, not interleaved, so provider-side
conditions differed; the build, corpus, CBMC version, policy and checks were
the same apart from the build difference below. `gpt-5.6` was more reliable
and used fewer tokens on this corpus; `gemini-3.5-flash` solved every case
at least twice out of three.

## Evidence

Each run's hash-chained audit log is in `gemini_gemini-3.5-flash/`, with
every run's record and the last hash of its chain in `results.json`. All 15
chains verified after being copied out of the sandbox, and the logs contain
no keys (checked):

```sh
cargo run -q --bin agent-harness -- verify-audit crates/agent-harness-task-verified-fix/bench/2026-10-01-gemini-paid/*/*.jsonl
```

## Setup

- `verified-fix bench /app/corpus --model gemini:gemini-3.5-flash --runs 3 --out /tmp/bench`,
  with `--provider gemini` attached.
- Build (from `results.json`): harness 0.1.0, commit
  `4a6ead066bda25e8bee024eae7f53d1f1dac7871` (clean; the M5 commit), `rustc
  1.95.0`, `aarch64-unknown-linux-gnu`, release. Unlike the first run, this
  build includes the 300 s request timeout. CBMC 6.6.0 (`cbmc=6.6.0-4`).
- OpenShell 0.1.2 on colima (macOS, arm64), policy `openshell-policy.yaml`.
