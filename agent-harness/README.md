# agent-harness

A modular, type-safe, model-agnostic agent harness in Rust, built on [`rig-core`](https://crates.io/crates/rig-core) 0.42.

Execution code talks to one small trait, `ModelRuntime`, and never to a provider directly. Swapping between Anthropic, OpenAI, Gemini, Ollama and OpenRouter (or a mock in tests) is a change of type parameter or a runtime `/model` command, not a change of calling code. Learned environment facts live in a shared, schema-less `AdaptiveMemoryLayer` that can be compacted for small local models. The crate ships a library (`agent_harness`) and an interactive CLI (`agent-harness`).

- [Architecture](#architecture)
  - [Design principles](#design-principles)
  - [Module map](#module-map)
  - [ModelRuntime](#modelruntime)
  - [AdaptiveMemoryLayer](#adaptivememorylayer)
  - [Data schemas](#data-schemas)
  - [Provider layer](#provider-layer)
  - [Tool components](#tool-components)
- [Security model](#security-model)
- [User guide](#user-guide)
  - [Requirements](#requirements)
  - [Build and test](#build-and-test)
  - [Using the CLI](#using-the-cli)
  - [Bounded launch](#bounded-launch)
  - [Using the library](#using-the-library)
  - [Writing a runtime](#writing-a-runtime)
  - [Writing a tool](#writing-a-tool)
  - [Adding a provider](#adding-a-provider)
- [Testing](#testing)
- [Limitations](#limitations)

---

## Architecture

### Design principles

| Principle | How it shows up |
|---|---|
| **Trait-driven separation** | Model access is the `ModelRuntime` trait; tools, approval and observation are each their own trait. |
| **Static dispatch by default** | `HostedProviderRuntime<M>` is generic over the rig model. Runtime provider switching uses a closed `enum` (`ProviderModel`), not `dyn`. |
| **`Send` futures** | `ModelRuntime::prompt_agent` returns `impl Future + Send`, so any runtime can be driven from `tokio::spawn`. Implementors still write plain `async fn`. |
| **Deterministic execution** | Memory renders sorted by key; compaction depends only on contents and policy, never on `HashMap` order. |
| **No secrets in code** | Credentials are read only from environment variables, via rig's `ProviderClient::from_env`. |
| **Thread safety** | Runtimes, `AdaptiveMemoryLayer`, `ProviderModel`, schemas and built-in policies are `Send + Sync + 'static`, and tests assert it. |
| **Defense in depth** | The process is designed to run inside an NVIDIA OpenShell sandbox with an independent BlueField-4 Sentry monitor on the egress path. See [Security model](#security-model). |

### Module map

```
src/
├── main.rs              CLI driver loop: REPL, model switching, memory commands
├── lib.rs               Public API and re-exports (including `rig_core`)
├── models.rs            AgentTask, ManifestEntry, Manifest: provider-agnostic schemas
├── harness/
│   ├── mod.rs           Harness exports
│   ├── runtime.rs       ModelRuntime trait, HostedProviderRuntime<M>
│   └── memory.rs        AdaptiveMemoryLayer, CompactionPolicy, CompactionReport
├── provider.rs          Provider, ModelSpec, ProviderModel: runtime provider/model selection
├── tool.rs              DynTool (object-safe tool) and ToolRegistry
├── validate.rs          Deterministic JSON-Schema-subset argument validation
├── policy.rs            ApprovalPolicy, Approval, AutoApprove, AllowList
├── observer.rs          Observer lifecycle hooks, NoopObserver
├── error.rs             HarnessError
└── tools/               Example tools: Calculator, WordCount
docs/
└── security-architecture.md   Policy review, egress data path, Sentry mapping
openshell-policy.yaml    OpenShell sandbox policy (v0.0.116 schema)
run-bounded.sh           Checks the policy, mocks its layout locally, then launches the REPL
```

```mermaid
flowchart LR
    CLI["main.rs (driver loop)"] -->|"prompt_agent(preamble, payload)"| RT["ModelRuntime"]
validate-openshell.sh    Runs the harness in a live OpenShell sandbox and checks each policy rule
sandbox.Dockerfile       Sandbox image used by validate-openshell.sh
    RT -.impl.- HPR["HostedProviderRuntime&lt;M&gt;"]
    HPR -->|"M: CompletionModel"| PM["ProviderModel"]
    PM --> A[anthropic]
    PM --> OA[openai]
    PM --> G[gemini]
    PM --> OL[ollama]
    PM --> OR[openrouter]
    CLI -->|"learn / render / compact"| MEM["AdaptiveMemoryLayer<br/>Arc&lt;RwLock&lt;HashMap&gt;&gt;"]
    MEM -->|"learn_manifest"| MAN["Manifest / ManifestEntry"]
    CLI --> TASK["AgentTask"]
```

### ModelRuntime

```rust
pub trait ModelRuntime: Send + Sync {
    fn prompt_agent(
        &self,
        preamble: &str,
        payload: &str,
    ) -> impl Future<Output = Result<String, anyhow::Error>> + Send;
}
```

The only contract between execution code and a model. One call sends system instructions (`preamble`) and a request (`payload`) and returns the model's text.

**`HostedProviderRuntime<M: CompletionModel>`** is the built-in implementation for any rig model:

| Method | Effect |
|---|---|
| `new(model)` | Wrap a model. `max_tokens` defaults to `DEFAULT_MAX_TOKENS` (4096), always sent so Anthropic never needs a model-specific default. |
| `with_max_tokens(n)`, `with_temperature(t)` | Builder-style request settings. |
| `model()`, `set_model(new)` | Inspect or replace the model; `set_model` returns the old one. |

Per call it builds a `CompletionRequest` with a leading `Message::system(preamble)` (omitted when empty) and `Message::user(payload)`, no tools, runs `validate_message_content()`, and joins the text parts of the response with newlines. A response with no text is an error; provider failures are wrapped with `"completion request failed"` context.

### AdaptiveMemoryLayer

A cheaply cloneable handle over `Arc<RwLock<HashMap<String, String>>>`. Clones share one map, so every execution domain sees what any other has learned. Locks are `std::sync::RwLock`, held for one map operation and never across an `.await`. A poisoned lock is recovered rather than propagated: every mutation is a single map call on plain strings, so a panicking writer cannot leave the map inconsistent.

| Method | Effect |
|---|---|
| `learn(key, value)` | Insert; returns the replaced value. |
| `learn_manifest(&entries)` | Insert every `ManifestEntry` (non-string JSON values stored as compact JSON). |
| `recall`, `forget`, `clear`, `len`, `is_empty`, `total_bytes` | The usual accessors; sizes count key + value bytes. |
| `render()` | `key: value` lines sorted by key, for inclusion in a prompt. |
| `compact(policy)` | Strip context-window bloat in place; returns a `CompactionReport`. |

`compact` runs four deterministic steps:

1. collapse whitespace runs and trim each value;
2. drop entries left empty;
3. truncate values to `max_value_chars` (on a character boundary, trailing space trimmed);
4. evict the largest entries (ties broken by key) until the store fits `max_total_bytes`.

`CompactionPolicy::SMALL_MODEL` is 256 characters per value and 2048 bytes total. The CLI applies it automatically before each prompt when the active provider is Ollama. `CompactionReport` records `dropped_empty`, `rewritten`, `evicted`, `bytes_before` and `bytes_after`.

Memory is **process-local and not persisted**. It is lost when the process exits.

### Data schemas

`src/models.rs`, all `Serialize + Deserialize`:

| Type | Shape |
|---|---|
| `AgentTask` | `{ preamble, payload }`; `preamble` defaults to `""` when absent. |
| `ManifestEntry` | `{ key, value }` where `value` is any JSON. `value_text()` gives strings unquoted and everything else as compact JSON. |
| `Manifest` | `{ entries: [ManifestEntry] }`. `get(key)` returns the **last** matching entry, so later entries override earlier ones. |

### Provider layer

`src/provider.rs` makes the model choosable at runtime without trait objects:

- **`Provider`** is an enum of supported backends, with `name()`, `api_key_env()` and `default_model()`.
- **`ModelSpec`** is `provider[:model]`, parsed with `FromStr`. Only the first `:` separates the two, so `ollama:llama3.2:3b` keeps the model id `llama3.2:3b`. A spec with no model uses the provider's default; providers without a default reject it.
- **`ProviderModel`** holds a `ModelSpec` and a private `Backend` enum of each rig provider's concrete model type. Its `CompletionModel` impl forwards through a `match`: no boxing, no vtables.

| Provider | Spec name | Default model | Credentials / config (read by rig) |
|---|---|---|---|
| Anthropic | `anthropic` | `claude-sonnet-5` | `ANTHROPIC_API_KEY`, optional `ANTHROPIC_BASE_URL` |
| OpenAI | `openai` | `gpt-5.6` | `OPENAI_API_KEY`, optional `OPENAI_BASE_URL` |
| Google Gemini | `gemini` | `gemini-2.5-flash` | `GEMINI_API_KEY` |
| Ollama | `ollama` | *(model required)* | optional `OLLAMA_API_BASE_URL` (default `http://localhost:11434`), optional `OLLAMA_API_KEY` |
| OpenRouter | `openrouter` | *(model required)* | `OPENROUTER_API_KEY` |

`ProviderError` covers `UnknownProvider`, `ModelRequired` and `Client { provider, source }` (for example a missing API key).

### Tool components

These library components are kept and tested, but **no driver loop currently uses them**: `ModelRuntime` sends no tool definitions.

| Component | Purpose |
|---|---|
| `ToolRegistry` | `register` (duplicate names → `HarnessError::DuplicateTool`), `definitions`, and `execute(name, args)` which looks up, validates and runs a tool. |
| `DynTool` | Object-safe wrapper; every rig `PortableTool` gets it via a blanket impl. |
| `validate_args` | Deterministic subset of JSON Schema: `type: object`, `required`, and primitive property types. Serde is the final gate. |
| `ApprovalPolicy` | Async human-in-the-loop gate; `AutoApprove`, `AllowList`. |
| `Observer` | Synchronous lifecycle hooks; `NoopObserver`. |
| `tools::Calculator`, `tools::WordCount` | Example tools. |

---

## Security model

Defense in depth across three independent layers. Full detail, findings and sources are in [`docs/security-architecture.md`](docs/security-architecture.md).

```mermaid
flowchart LR
    subgraph HOST["Host CPU"]
        subgraph SB["OpenShell sandbox"]
            R["agent-harness<br/>(plaintext in memory)"]
        end
        P["OpenShell egress proxy<br/>policy check (OPA/Rego)<br/>TLS terminate + inspect<br/>credential injection"]
    end
    subgraph DPU["BlueField-4 DPU"]
        S["Sentry<br/>out-of-band monitor"]
    end
    API["provider API :443"]
    R -- "TLS #1" --> P
    P -- "TLS #2 (re-encrypted)" --> S
    S --> API
```

| Layer | Enforcer | Sees plaintext? | Role |
|---|---|---|---|
| 1. In process | the code (`ModelRuntime` boundary, env-only credentials) | yes | The cheapest place for audit logging and redaction, e.g. a `ModelRuntime` decorator. |
| 2. Sandbox runtime | NVIDIA OpenShell: filesystem (Landlock), process (seccomp), network policy evaluated at its egress proxy | yes, for inspected HTTPS (the proxy terminates TLS) | Primary policy enforcement and content inspection. Attached providers replace keys with placeholders the proxy resolves in header values, so real keys never enter the sandbox. This covers rig's `x-api-key` and `Authorization: Bearer` with no code change. |
| 3. Hardware, out of band | NVIDIA Sentry on BlueField-4 | not from the re-encrypted stream on its own (*unverified* what Sentry can inspect) | Independent check on the egress path that keeps working if the host or proxy is compromised. |

Key properties:

- **Encryption happens in-process.** rig/reqwest encrypts before traffic leaves the harness. There is no unencrypted token stream on the wire to port 443.
- **Ordering.** Outbound traffic reaches the DPU *after* OpenShell's policy has allowed it. Sentry is a second enforcement point, not a pre-filter.
- **Local Ollama** is plain HTTP on port 11434, reached from the sandbox at `host.openshell.internal` on the same machine, so it should not cross the NIC or DPU.

Current status:

- `openshell-policy.yaml` follows the OpenShell v0.0.116 schema. It allows only `POST /v1/messages` (Anthropic), `POST /v1/responses` (OpenAI), `POST /v1beta/models/*:generateContent` (Gemini) and `POST /api/chat` (Ollama via `host.openshell.internal`), runs as the non-root `sandbox` user with `landlock: hard_requirement`, and makes the binary read-only. OpenShell 0.1.2 loads it unchanged and enforces it; see *Live validation* in the security doc.
- Gemini is allowlisted (`POST /v1beta/models/*:generateContent`) so a `GEMINI_API_KEY` can be used for testing. OpenShell has no built-in Gemini provider type, so keeping the key out of the sandbox needs a custom provider; see the security doc. OpenRouter is deliberately not allowlisted.
- OpenShell enforcement was validated on 2026-09-30 with `validate-openshell.sh` (process, filesystem, binary pinning, host allowlist, Gemini egress). L7 method/path rules, credential injection and the other providers are not yet exercised. Sentry has not been tested. `run-bounded.sh` remains an offline mock.

---

## User guide

### Requirements

- Rust toolchain **1.90+** (edition 2024; `rust-version = "1.90"` in `Cargo.toml`)
- An API key for at least one hosted provider, or a running [Ollama](https://ollama.com) server

### Build and test

```sh
cargo build --release
cargo test
cargo clippy --all-targets
```

### Using the CLI

```text
agent-harness [-m|--model <provider[:model]>] [prompt…]
```

With a prompt it runs once and exits. Without one it starts an interactive REPL. Each prompt is a **single, independent request**: the base preamble plus everything in memory, then your text. Earlier prompts and answers are not resent.

**Choosing the model** (first match wins):

1. `-m` / `--model <spec>` (also `--model=<spec>`)
2. `HARNESS_MODEL` environment variable
3. `anthropic` (that is, `anthropic:claude-sonnet-5`)

**Environment variables:**

| Variable | Purpose |
|---|---|
| `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `OPENROUTER_API_KEY` | Provider credentials; only the selected provider's key is needed |
| `OLLAMA_API_BASE_URL` | Ollama server URL (optional) |
| `HARNESS_MODEL` | Default model spec |

Keep keys in your shell environment or a git-ignored `.env` that you `source`. Never commit them.

**Examples:**

```sh
export ANTHROPIC_API_KEY=...            # set in your shell, never in code

cargo run -- "Summarise the borrow checker in one sentence."
cargo run -- -m openai:gpt-5.5 "hello"
cargo run -- --model=ollama:llama3.2:3b "hello"
```

**REPL commands:**

| Command | Effect |
|---|---|
| `/model` | Show the active model |
| `/model <spec>` | Switch provider/model. On error the current model stays. |
| `/providers` | List providers, default models, and whether each API key is set |
| `/learn <key> <value>` | Store a heuristic; it is added to every later preamble |
| `/forget <key>` | Remove a heuristic |
| `/memory` | Print memory, sorted by key |
| `/compact` | Compact memory with `CompactionPolicy::SMALL_MODEL` and print the report |
| `/clear` | Clear memory |
| `/quit`, `/exit`, Ctrl-D | Exit |

Example session (stderr diagnostics are prefixed with `[`):

```text
model: ollama:llama3.2 (/model <provider[:model]> to switch)

> /learn shell zsh on macOS
learned `shell`

> how do I list hidden files?
ls -a
[ollama:llama3.2]
```

### Bounded launch

```sh
./run-bounded.sh [--dry-run] [agent-harness args…]
```

A local **mock** of the layout in `openshell-policy.yaml`. It enforces nothing (no Landlock, seccomp or egress proxy); real enforcement comes from OpenShell. It maps the policy's sandbox paths to local stand-ins:

| Policy path | Local stand-in |
|---|---|
| `/app/agent-harness` | `target/release/agent-harness` |
| `/tmp` (read-write) | `.sandbox/tmp`, mode 700, exported as `TMPDIR` |

Steps:

1. **Structure check (grep):** `version: 1`; `filesystem_policy`, `landlock`, `process` and `network_policies` present; `/app/agent-harness` listed as read-only and as an allowed binary.
2. **Schema check (Ruby's bundled YAML parser, skipped if Ruby is absent):** no unknown top-level keys; absolute paths without `..`; valid `landlock.compatibility`; no root in `process`; every network entry has endpoints and binaries; integer ports; `rest` endpoints have exactly one of `access` / `rules`; `rules` non-empty with `allow.method` and `allow.path`; valid `access` values.
3. **Non-root:** refuses to run as root, mirroring `process.run_as_user`.
4. **Mock `/tmp`:** creates `.sandbox/tmp`, refusing symlinks and paths that resolve outside the project root.
5. **Credentials:** reports whether `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` and `GEMINI_API_KEY` are set, never their values. For Ollama it notes that inside the sandbox `OLLAMA_API_BASE_URL` should be `http://host.openshell.internal:11434`.
6. **Launch:** builds the release binary and `exec`s it with the remaining arguments.

`--dry-run` runs every check but creates and launches nothing. `.sandbox/` is git-ignored.

### Using the library

```toml
[dependencies]
agent-harness = { path = "../agent-harness" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use agent_harness::{
    AdaptiveMemoryLayer, CompactionPolicy, HostedProviderRuntime, ManifestEntry, ModelRuntime,
    ProviderModel,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = ProviderModel::from_env("anthropic:claude-sonnet-5".parse()?)?;
    let mut runtime = HostedProviderRuntime::new(model).with_max_tokens(1024);

    let memory = AdaptiveMemoryLayer::new();
    memory.learn_manifest(&[
        ManifestEntry::new("os", "linux"),
        ManifestEntry::new("cores", 8),
    ]);

    let preamble = format!("Be concise.\n\nEnvironment:\n{}", memory.render());
    println!("{}", runtime.prompt_agent(&preamble, "Suggest a -j value for make.").await?);

    // Switch to a small local model; compact memory to fit its context first.
    runtime.set_model(ProviderModel::from_env("ollama:llama3.2:3b".parse()?)?);
    memory.compact(CompactionPolicy::SMALL_MODEL);
    let preamble = format!("Be concise.\n\nEnvironment:\n{}", memory.render());
    println!("{}", runtime.prompt_agent(&preamble, "Same question.").await?);
    Ok(())
}
```

Any rig `CompletionModel` works in place of `ProviderModel`, including a concrete provider model for fully static dispatch, or `rig_core::test_utils::MockCompletionModel` in tests (dev feature `test-utils`).

### Writing a runtime

Code generic over `R: ModelRuntime` works with every backend. Implement it directly for anything that isn't a rig model, such as a scripted stub:

```rust
use agent_harness::ModelRuntime;

struct Echo;

impl ModelRuntime for Echo {
    async fn prompt_agent(&self, preamble: &str, payload: &str) -> anyhow::Result<String> {
        Ok(format!("{preamble}|{payload}"))
    }
}

async fn run_on<R: ModelRuntime>(runtime: &R) -> anyhow::Result<String> {
    runtime.prompt_agent("sys", "hi").await
}
```

The future your `async fn` produces must be `Send`: don't hold a `std::sync` guard, `Rc` or other `!Send` value across an `.await`.

### Writing a tool

Implement rig's `PortableTool`. It automatically becomes a `DynTool` and can be registered with `ToolRegistry::register`.

```rust
use agent_harness::rig_core::tool::{PortableTool, ToolExecutionError};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub struct Reverse;

#[derive(Deserialize)]
pub struct ReverseArgs { text: String }

#[derive(Serialize)]
pub struct ReverseOutput { reversed: String }

#[derive(Debug, thiserror::Error)]
pub enum ReverseError {
    #[error("text is empty")]
    Empty,
}

impl PortableTool for Reverse {
    const NAME: &'static str = "reverse";           // unique, model-visible
    type Args = ReverseArgs;
    type Output = ReverseOutput;                    // any Serialize → JSON; String → text
    type Error = ReverseError;

    fn description(&self) -> String { "Reverse a string.".into() }
### OpenShell validation

```sh
./validate-openshell.sh [--skip-build]
```

Needs a running OpenShell gateway with the `docker` compute driver. It builds a Linux binary in `rust:1.90`, builds `sandbox.Dockerfile`, checks that the gateway reports this policy as effective, then runs probes in one sandbox: the non-root user, seccomp, writes outside `/tmp`, reads outside the allowlist, `curl` (an unpinned binary) to listed and unlisted hosts, and the harness reaching Gemini with a dummy key. Exits non-zero on any mismatch. On colima the gateway also needs a relay into the VM; the script detects this and prints the command. See the security doc for setup notes.


    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"]
        })
    }

    // Optional: show the error text to the model (default sends only safe kind-level feedback).
    fn map_error(&self, e: Self::Error) -> ToolExecutionError {
        ToolExecutionError::invalid_args(e.to_string())
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if args.text.is_empty() { return Err(ReverseError::Empty); }
        Ok(ReverseOutput { reversed: args.text.chars().rev().collect() })
    }
}
```

Keep `parameters()` in sync with `Args`, keep tools `Send + Sync + 'static`, and read any credentials from `std::env::var`, never hard-coded.

### Adding a provider

In `src/provider.rs`:

1. Add a variant to `Provider` and to `Provider::ALL`, and fill in `name`, `api_key_env` and `default_model`.
2. Add a `type XModel = <x::Client as CompletionClient>::CompletionModel;` alias and a `Backend::X(XModel)` variant.
3. Add the arm to `ProviderModel::from_env` and to the `dispatch!` macro.
4. Add the name to the `UnknownProvider` error message and add parsing tests.

The compiler's exhaustiveness checks point out any place you missed. Then add the provider's API host to the sandbox policy's `network_policies`.

---

## Testing

All tests are unit tests next to each module; none calls a live provider API.

- **`harness::runtime`**: a dummy `Echo` runtime through a generic caller and through `tokio::spawn`; `HostedProviderRuntime` against rig's `MockCompletionModel` (request shape, empty preamble, error propagation, model switching).
- **`harness::memory`**: learn/recall/forget, shared clones, concurrent writers from tasks, recovery from a poisoned lock, sorted rendering, manifest ingestion, and compaction (normalisation, multi-byte truncation, deterministic eviction, no-op within budget).
- **`models`**: JSON round trips, override order, `value_text`, defaults.
- **Retained modules**: validation, registry, tools, policies, provider/spec parsing.
- **Compile-time bound checks** assert `Send + Sync + 'static` on runtimes, memory, schemas, `ProviderModel` and policies.

## Limitations

- **No conversation history.** Each `prompt_agent` call is independent; only memory carries context between prompts.
- **No tool calling.** `ModelRuntime` sends no tools; the tool, policy and observer modules are not wired into any loop.
- **Memory is not persisted** and is lost on exit. Compaction eviction is permanent within the process.
- **Non-streaming.** `ProviderModel` forwards `stream()`, but `ModelRuntime` has no streaming method.
- **Vestigial error variants.** `HarnessError::{Completion, MaxTurnsExceeded, EmptyResponse}` are no longer produced; only `DuplicateTool` is (from `ToolRegistry::register`).
- **Sandbox policy is a draft.** See [Security model](#security-model).
- **Model defaults may go stale.** Override them with an explicit `provider:model` spec.
