# verified-fix benchmark, 2026-10-01

The bundled corpus (5 C bugs), 3 runs per case, two models, run inside the
OpenShell sandbox with each provider's key injected by OpenShell (the
sandbox only ever held placeholders). Patches were auto-approved, recorded
as a policy decision.

## Results

| Model | Accepted | Rejected | Errors | Accept rate | Accept rate (finished runs) | Mean turns | Mean tokens | Retries |
|---|---|---|---|---|---|---|---|---|
| `openai:gpt-5.6` | 15 | 0 | 0 | 100% | 100% | 4.2 | 7,228 | 1 |
| `gemini:gemini-3.5-flash` | 2 | 0 | 13 | 13% | 100% | 5.0 | 9,653 | 26 |

| Case | `openai:gpt-5.6` | `gemini:gemini-3.5-flash` |
|---|---|---|
| average_div_zero | 3/3 | 1/3 |
| buffer_off_by_one | 3/3 | 1/3 |
| pitch_overflow | 3/3 | 0/3 (quota) |
| ring_index | 3/3 | 0/3 (quota) |
| shift_scale | 3/3 | 0/3 (quota) |

**Reading it:**

- **`gpt-5.6` fixed every case, every time** (15/15), in 4–5 turns, 9–23 s
  and 5,262–11,043 tokens per run, with near-identical fixes across runs.
  Every fix passed all six acceptance checks, including the independent CBMC
  re-verification. Fix sizes (lines added in run 1): `buffer_off_by_one` 1,
  `ring_index` 2, `average_div_zero` 3, `pitch_overflow` 6, `shift_scale` 16.
- **`gemini-3.5-flash` never produced a wrong fix:** both runs that finished
  were accepted (5 turns each). The 13 errors are all **HTTP 429
  `RESOURCE_EXHAUSTED`**: the API key is on Google's free tier, which allows
  **20 `generateContent` requests per day per model**
  (`GenerateRequestsPerDayPerProjectPerModel-FreeTier`). Each run uses about
  5 requests and earlier tests that day had used some, so the quota ran out
  in round 1. These are counted as errors, not rejections: they say nothing
  about the model's ability. Gemini was also slow while it ran (up to ~100 s
  per response; one run took 202 s).
- **A fair Gemini comparison needs a paid tier**, or spreading runs across
  days (the quota is per model, so another Gemini model has its own 20).

## Evidence

Every run has its own hash-chained audit log under `openai_gpt-5.6/` and
`gemini_gemini-3.5-flash/` (`<case>-run<n>.jsonl`), written inside the
sandbox and copied out. `results.json` holds every run's record (verdict,
failed checks, error, turns, tokens, retries, time, the model's diff) and the
last hash of its audit chain. To check them:

```sh
cargo run -q --bin agent-harness -- verify-audit crates/agent-harness-task-verified-fix/bench/2026-10-01/*/*.jsonl
```

All 30 chains verified after copying, and every last hash matches
`results.json`. The `audit` paths in `results.json` are the sandbox's
(`/tmp/bench/...`). The logs contain the prompts, the code and each model's
turns, and no keys (checked).

## Setup

- Command, inside the sandbox:
  `verified-fix bench /app/corpus --model openai:gpt-5.6 --model gemini:gemini-3.5-flash --runs 3 --out /tmp/bench`,
  with `--provider openai --provider gemini` attached.
- Build (from `results.json`): harness 0.1.0, commit
  `d158cf7e5b5759cc80585cba2e5739f829a21e9d-dirty` (the uncommitted M5 bench
  code, committed afterwards together with this report), `rustc 1.95.0`,
  `aarch64-unknown-linux-gnu`, release. CBMC 6.6.0 (`cbmc=6.6.0-4`).
- OpenShell 0.1.2 on colima (macOS, arm64), policy `openshell-policy.yaml`.
- Runs interleaved (run 1 of every case and model, then run 2, then 3).
- Model requests retried up to 2 times on transient failures (the one
  `gpt-5.6` retry was OpenShell's connection reset about 10 s into the
  sandbox). The request timeout added after this run was not in this build.
- Total: 30 runs, 127,730 tokens across the 17 runs that finished.
