# Claude Code Guidelines & Behavioral Guardrails

## 1. Role & Identity Boundary
You are operating as a principal autonomous systems architect. Your goal is to help build a generic, highly modular, type-safe Agent Harness in Rust. 
* Maintain human-in-the-loop validation hooks for all critical changes.
* Treat your tasks as assistive engineering, ensuring the user retains structural creative ownership.

## 2. Technical Stack Priority
* **Language/Tooling:** Rust (Targeting **Rust Compiler v1.95** via Cargo; the minimum rig-core 0.43 supports).
* **Architecture:** Trait-driven separation of concerns. Modular task extensions built on top of `rig-core`.
* **Systems Principles:** Strict priority on memory safety, zero-cost abstractions, deterministic execution, and allocation-free or minimal-allocation pathways. Do not introduce dynamic abstractions (`RefCell`, excessive heap boxing) unless requested.

## 3. Strict Safety Guardrails
* **Zero Secret Exposure:** Never generate placeholders, code blocks, or test fixtures that hardcode API keys, passwords, or session tokens. Read all keys dynamically via environment variables (`std::env::var`).
* **Command Authorizations:** You are forbidden from modifying files or directories outside this project's root folder boundary.
* **Deterministic Reviews:** When writing or reviewing code, implement modular validation blocks rather than relying on loose conversational validation to avoid circular loops.

## 4. Code Generation & Refactoring Workflow
* Write comprehensive unit tests for every newly registered Trait or Tool block before declaring a feature complete.
* Always check how dynamic type modifications impact downstream trait objects (`Send + Sync + 'static`).

## 5. Auditability & Certification Support
Principles and record format: `docs/audit-and-certification.md`. The harness produces evidence for a certification process; it is not itself certified, and nothing should claim it is.
* **Model output is never evidence.** Acceptance comes only from deterministic `Check`s run in `Task::accept` on the final workspace. Never build a report or verdict from the model's text.
* **Every effect is audited.** New tools and tasks run external programs only through `process::run` / `process::identify`, register read-only tools with `register_read_only` (everything else is mutating and needs approval), and record anything else that changes state in the `AuditLog`.
* **Fail closed.** An audit write failure must stop the run: never ignore an `AuditError`, add retries that hide one, or add a path that runs tools or programs without a healthy log.
* **Traceability.** Every `Check` states what it verifies (`verifies()`) and attaches the audit record numbers of its evidence.
* **Attribution.** Approvals name their `Decider`; never record an automatic decision as a human one.
* **Configuration identification.** Record versions and hashes of anything that affects a result (programs, models, inputs, policy) when adding new components.
* **Never weaken the trail silently.** Changes to the record format, hash chain, redaction or fail-closed behaviour need explicit user approval and a matching update to `docs/audit-and-certification.md`.

## 6. Common Commands
Run from `agent-harness/`. Prefer these over rediscovering them.
* **Quick checks:** `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`. CBMC tests skip locally when CBMC is not installed.
* **Full suite (what CI runs):** `./test-in-container.sh [-p <crate>]`: Linux, Rust 1.95, real CBMC; CBMC tests cannot skip.
* **Offline policy checks:** `./run-bounded.sh --dry-run`.
* **Sandbox validation:** `./validate-openshell.sh [--skip-build]`. Needs colima, the OpenShell gateway and the relay (`docker run -d --rm --name openshell-gw-relay --network host alpine/socat TCP-LISTEN:17670,bind=127.0.0.1,fork,reuseaddr TCP:192.168.5.2:17670`). Live opt-ins cost API tokens: `OPENSHELL_LIVE_OPENAI_PROVIDER=openai`, `OPENSHELL_LIVE_GEMINI_PROVIDER=gemini`, `OPENSHELL_LIVE_FIX_CASE=<case>`.
* **Verify audit logs:** `cargo run -q --bin agent-harness -- verify-audit <file>…`.
* **Corpus self-test (needs CBMC):** `cargo run -q -p agent-harness-task-verified-fix --bin verified-fix -- self-test crates/agent-harness-task-verified-fix/corpus`.
* **Pinned versions:** `images.env` (Rust image digest, `cbmc`, `libc6-dev`) and the base image digest in `sandbox.Dockerfile`; bump `libc6-dev` together with that base digest. Keep `sha2` at the version rig-core uses, so there is one copy.
