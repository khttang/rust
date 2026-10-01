//! Central driver loop.
//!
//! Usage: `agent-harness [-m|--model <provider[:model]>] [prompt…]`
//! Runs one prompt, or starts a REPL if none is given.
//! `agent-harness --version` prints the build identity (version, git commit,
//! compiler, target, profile) as JSON, the same `BuildInfo` audit records use.
//!
//! Model selection (first match wins): `--model`, `HARNESS_MODEL`, `anthropic`.
//! Specs look like `anthropic:claude-sonnet-5`, `openai`, `ollama:llama3.2:3b`.
//!
//! Environment:
//! * Provider credentials, read by rig: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`,
//!   `GEMINI_API_KEY`, `OPENROUTER_API_KEY`; `OLLAMA_API_BASE_URL` (optional).
//! * `HARNESS_AUTO_APPROVE=1`: skip the interactive tool-approval prompt.
//!
//! Each prompt runs through the tool-calling [`AgentLoop`] and continues the
//! current conversation. Learned heuristics are appended to the preamble of
//! every prompt. For local (Ollama) models memory is compacted with
//! [`CompactionPolicy::SMALL_MODEL`] before each prompt.
//!
//! REPL commands: `/model [spec]`, `/providers`, `/learn <key> <value>`,
//! `/forget <key>`, `/memory`, `/compact`, `/clear`, `/reset`, `/quit`.

use std::io::{self, BufRead, Write};

use agent_harness::{
    AdaptiveMemoryLayer, AgentLoop, AgentTask, Approval, ApprovalPolicy, AssistantTurn, BuildInfo,
    CompactionPolicy, CompactionReport, Conversation, Decider, HostedProviderRuntime, ModelSpec,
    Observer, Provider, ProviderModel, ReviewContext, ToolRegistry,
    rig_core::{
        message::ToolCall,
        tool::{ToolExecutionError, ToolOutput},
    },
    tools::{Calculator, WordCount},
};

type Runtime = HostedProviderRuntime<ProviderModel>;
type CliAgent = AgentLoop<Runtime, ConsoleApproval, StderrObserver>;

const DEFAULT_SPEC: &str = "anthropic";
const PREAMBLE: &str = "You are a helpful assistant. Use the provided tools when they help \
                        you answer accurately. Be concise.";

/// Asks the operator on the terminal before each tool call.
struct ConsoleApproval {
    auto_approve: bool,
}

/// The operator's OS login, the best identity available to a terminal
/// approver. Recorded as a human decision.
fn operator() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown-operator".to_owned())
}

impl ApprovalPolicy for ConsoleApproval {
    async fn review(&self, call: &ToolCall, _ctx: &ReviewContext) -> Approval {
        if self.auto_approve {
            return Approval::approve(Decider::policy("env:HARNESS_AUTO_APPROVE"));
        }
        let prompt = format!(
            "\n[approve] {}({}) ? [y/N] ",
            call.function.name, call.function.arguments
        );
        match tokio::task::spawn_blocking(move || read_line(&prompt)).await {
            Ok(Ok(Some(answer))) if answer.trim().eq_ignore_ascii_case("y") => {
                Approval::approve(Decider::human(operator()))
            }
            _ => Approval::deny(Decider::human(operator()), "operator rejected the call"),
        }
    }
}

/// Logs lifecycle events to stderr.
struct StderrObserver;

impl Observer for StderrObserver {
    fn on_turn_start(&self, turn: usize) {
        eprintln!("[turn {turn}]");
    }

    fn on_model_response(&self, _turn: usize, response: &AssistantTurn) {
        let calls = response.tool_calls().count();
        if calls > 0 {
            eprintln!("  model requested {calls} tool call(s)");
        }
    }

    fn on_tool_call(&self, call: &ToolCall, _ctx: &ReviewContext, approval: &Approval) {
        let decision = if approval.is_approved() {
            "approved"
        } else {
            "denied"
        };
        eprintln!(
            "  -> {} {decision} by {:?}",
            call.function.name,
            approval.decider()
        );
    }

    fn on_tool_result(&self, call: &ToolCall, result: &Result<ToolOutput, ToolExecutionError>) {
        match result {
            Ok(out) => eprintln!("  <- {}: {}", call.function.name, out.render()),
            Err(e) => eprintln!("  <- {} failed: {e}", call.function.name),
        }
    }
}

/// Prints `prompt` to stderr and reads one line from stdin. `None` on EOF.
fn read_line(prompt: &str) -> io::Result<Option<String>> {
    let mut stderr = io::stderr().lock();
    stderr.write_all(prompt.as_bytes())?;
    stderr.flush()?;
    let mut line = String::new();
    Ok((io::stdin().lock().read_line(&mut line)? > 0).then_some(line))
}

/// Combine the base preamble with everything memory has learned.
fn build_task(memory: &AdaptiveMemoryLayer, payload: String) -> AgentTask {
    let learned = memory.render();
    let preamble = if learned.is_empty() {
        PREAMBLE.to_owned()
    } else {
        format!("{PREAMBLE}\n\nKnown environment heuristics:\n{learned}")
    };
    AgentTask::new(preamble, payload)
}

fn report_compaction(report: CompactionReport) {
    eprintln!(
        "[compacted: {} rewritten, {} empty dropped, {} evicted, {} -> {} bytes]",
        report.rewritten,
        report.dropped_empty,
        report.evicted,
        report.bytes_before,
        report.bytes_after
    );
}

async fn run_prompt(
    agent: &CliAgent,
    memory: &AdaptiveMemoryLayer,
    conversation: &mut Conversation,
    payload: String,
) {
    let spec = agent.runtime().model().spec();
    if spec.provider == Provider::Ollama {
        let report = memory.compact(CompactionPolicy::SMALL_MODEL);
        if report.changed() {
            report_compaction(report);
        }
    }
    let task = build_task(memory, payload);
    match agent.run(&task.preamble, conversation, task.payload).await {
        Ok(outcome) => {
            println!("{}", outcome.output);
            let tokens = outcome.usage.total_tokens.map_or_else(
                || "tokens not reported".to_owned(),
                |n| format!("{n} tokens"),
            );
            eprintln!(
                "[{spec} | {} turn(s), {} tool call(s), {tokens}]",
                outcome.turns, outcome.tool_calls
            );
        }
        Err(e) => eprintln!("error: {e}"),
    }
}

/// Parse and build a model from a spec string.
fn load_model(spec: &str) -> Result<ProviderModel, Box<dyn std::error::Error>> {
    Ok(ProviderModel::from_env(spec.parse::<ModelSpec>()?)?)
}

/// `/model [spec]`: show the active model, or switch to a new one. On failure
/// the current model stays active. The conversation carries over.
fn switch_model(agent: &mut CliAgent, arg: &str) {
    let runtime = agent.runtime_mut();
    if arg.is_empty() {
        eprintln!("active model: {}", runtime.model().spec());
        return;
    }
    match load_model(arg) {
        Ok(model) => {
            let old = runtime.set_model(model);
            eprintln!("switched {} -> {}", old.spec(), runtime.model().spec());
        }
        Err(e) => eprintln!("error: {e} (still using {})", runtime.model().spec()),
    }
}

fn list_providers() {
    for p in Provider::ALL {
        let key = match p.api_key_env() {
            Some(var) if std::env::var_os(var).is_some() => format!("{var} set"),
            Some(var) => format!("{var} missing"),
            None => "no key needed".to_owned(),
        };
        let default = p.default_model().unwrap_or("<model required>");
        eprintln!("  {:<11} default: {:<18} ({key})", p.name(), default);
    }
}

/// Split `-m/--model <spec>` out of the argument list.
fn parse_args(
    args: impl Iterator<Item = String>,
) -> Result<(Option<String>, Vec<String>), Box<dyn std::error::Error>> {
    let mut spec = None;
    let mut rest = Vec::new();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-m" | "--model" => {
                spec = Some(args.next().ok_or("--model requires a value")?);
            }
            _ => match arg.strip_prefix("--model=") {
                Some(value) => spec = Some(value.to_owned()),
                None => rest.push(arg),
            },
        }
    }
    Ok((spec, rest))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if matches!(std::env::args().nth(1).as_deref(), Some("--version" | "-V")) {
        println!("{}", serde_json::to_string_pretty(&BuildInfo::current())?);
        return Ok(());
    }
    let (cli_spec, prompt_args) = parse_args(std::env::args().skip(1))?;
    let spec = cli_spec
        .or_else(|| std::env::var("HARNESS_MODEL").ok())
        .unwrap_or_else(|| DEFAULT_SPEC.to_owned());
    let auto_approve = std::env::var("HARNESS_AUTO_APPROVE").is_ok_and(|v| v == "1");

    let mut tools = ToolRegistry::new();
    tools.register_read_only(Calculator)?;
    tools.register_read_only(WordCount)?;
    let mut agent = AgentLoop::new(HostedProviderRuntime::new(load_model(&spec)?), tools)
        .with_policy(ConsoleApproval { auto_approve })
        .with_observer(StderrObserver);
    let memory = AdaptiveMemoryLayer::new();
    let mut conversation = Conversation::new();

    if !prompt_args.is_empty() {
        run_prompt(&agent, &memory, &mut conversation, prompt_args.join(" ")).await;
        return Ok(());
    }

    eprintln!(
        "model: {} (/model <provider[:model]> to switch)",
        agent.runtime().model().spec()
    );
    loop {
        let line = tokio::task::spawn_blocking(|| read_line("\n> ")).await??;
        let Some(line) = line else { break };
        let line = line.trim();
        let (command, arg) = line.split_once(' ').unwrap_or((line, ""));
        let arg = arg.trim();
        match command {
            "" => continue,
            "/quit" | "/exit" => break,
            "/model" => switch_model(&mut agent, arg),
            "/providers" => list_providers(),
            "/learn" => match arg.split_once(' ') {
                Some((key, value)) => {
                    memory.learn(key, value.trim());
                    eprintln!("learned `{key}`");
                }
                None => eprintln!("usage: /learn <key> <value>"),
            },
            "/forget" => match memory.forget(arg) {
                Some(_) => eprintln!("forgot `{arg}`"),
                None => eprintln!("no entry `{arg}`"),
            },
            "/memory" => eprint!("{}", memory.render()),
            "/compact" => report_compaction(memory.compact(CompactionPolicy::SMALL_MODEL)),
            "/clear" => {
                memory.clear();
                eprintln!("memory cleared");
            }
            "/reset" => {
                conversation.clear();
                eprintln!("conversation reset");
            }
            _ => run_prompt(&agent, &memory, &mut conversation, line.to_owned()).await,
        }
    }
    Ok(())
}
