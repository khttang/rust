# crates/

Extension crates for agent-harness. Every directory here with a `Cargo.toml`
is a workspace member (`members = ["crates/*"]` in the root `Cargo.toml`).
The core crate, `agent-harness`, stays at the workspace root.

| Prefix | Holds | Example |
|---|---|---|
| `agent-harness-tools-*` | A tool family: `PortableTool`s around one library or program | `agent-harness-tools-cbmc` |
| `agent-harness-task-*` | One automated task: instructions, a fixed toolset, an acceptance check, its corpus and CLI | `agent-harness-task-verified-fix` |

A new crate inherits shared settings from the workspace:

```toml
[package]
name = "agent-harness-tools-example"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
agent-harness.workspace = true
rig-core.workspace = true   # required, and must be the workspace version, for #[rig_tool]
```

Take every shared dependency with `workspace = true`, so all crates use one
rig-core version.
