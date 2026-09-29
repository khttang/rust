//! Central driver loop.
//!
//! Usage: `agent-harness [-m|--model <provider[:model]>] [prompt…]`
//! Runs one prompt, or starts a REPL if none is given.
//!
//! Model selection (first match wins): `--model`, `HARNESS_MODEL`, `anthropic`.
//! Specs look like `anthropic:claude-sonnet-5`, `openai`, `ollama:llama3.2:3b`.
//!
//! Provider credentials are read by rig from `ANTHROPIC_API_KEY`,
//! `OPENAI_API_KEY`, `GEMINI_API_KEY`, `OPENROUTER_API_KEY`;
//! `OLLAMA_API_BASE_URL` is optional.
//!
//! Learned heuristics are appended to the preamble of every prompt. For local
//! (Ollama) models memory is compacted with [`CompactionPolicy::SMALL_MODEL`]
//! before each prompt.
//!
//! REPL commands: `/model [spec]`, `/providers`, `/learn <key> <value>`,
//! `/forget <key>`, `/memory`, `/compact`, `/clear`, `/quit`.

use std::io::{self, BufRead, Write};

use agent_harness::{
    AdaptiveMemoryLayer, AgentTask, CompactionPolicy, CompactionReport, HostedProviderRuntime,
    ModelRuntime, ModelSpec, Provider, ProviderModel,
};

type Runtime = HostedProviderRuntime<ProviderModel>;

const DEFAULT_SPEC: &str = "anthropic";
const PREAMBLE: &str = "You are a helpful assistant. Be concise.";

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

async fn run_prompt(runtime: &Runtime, memory: &AdaptiveMemoryLayer, payload: String) {
    if runtime.model().spec().provider == Provider::Ollama {
        let report = memory.compact(CompactionPolicy::SMALL_MODEL);
        if report.changed() {
            report_compaction(report);
        }
    }
    let task = build_task(memory, payload);
    match runtime.prompt_agent(&task.preamble, &task.payload).await {
        Ok(answer) => {
            println!("{answer}");
            eprintln!("[{}]", runtime.model().spec());
        }
        Err(e) => eprintln!("error: {e:#}"),
    }
}

/// Parse and build a model from a spec string.
fn load_model(spec: &str) -> Result<ProviderModel, Box<dyn std::error::Error>> {
    Ok(ProviderModel::from_env(spec.parse::<ModelSpec>()?)?)
}

/// `/model [spec]`: show the active model, or switch to a new one. On failure
/// the current model stays active.
fn switch_model(runtime: &mut Runtime, arg: &str) {
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
    let (cli_spec, prompt_args) = parse_args(std::env::args().skip(1))?;
    let spec = cli_spec
        .or_else(|| std::env::var("HARNESS_MODEL").ok())
        .unwrap_or_else(|| DEFAULT_SPEC.to_owned());

    let mut runtime = HostedProviderRuntime::new(load_model(&spec)?);
    let memory = AdaptiveMemoryLayer::new();

    if !prompt_args.is_empty() {
        run_prompt(&runtime, &memory, prompt_args.join(" ")).await;
        return Ok(());
    }

    eprintln!(
        "model: {} (/model <provider[:model]> to switch)",
        runtime.model().spec()
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
            "/model" => switch_model(&mut runtime, arg),
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
            _ => run_prompt(&runtime, &memory, line.to_owned()).await,
        }
    }
    Ok(())
}
