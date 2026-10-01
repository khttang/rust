//! Tamper-evident audit trail for task runs.
//!
//! Every task run writes an append-only record: one JSON object per line
//! (JSON Lines). Each record carries a sequence number, a UTC timestamp, the
//! hash of the previous record and its own SHA-256 hash, so editing,
//! inserting or deleting a record breaks the chain and [`verify_chain`]
//! reports where.
//!
//! ```text
//! {"event":{...},"hash":"<sha256>","prev":"<sha256 of previous record>","seq":0,"ts_ms":...}
//! ```
//!
//! `hash` is the SHA-256 of the record serialized without its `hash` field.
//! The first record's `prev` is 64 zeros.
//!
//! Writing is fail-closed: after any write error the log is *failed*, every
//! later [`AuditLog::record`] and [`AuditLog::check`] returns an error, and
//! [`crate::process::run`] refuses to start programs. Strings that look like
//! credentials are masked before hashing (see [`redact`]).
//!
//! The principles behind this format are in `docs/audit-and-certification.md`.

use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{
    harness::RuntimeInfo,
    policy::{Approval, ToolRisk},
    task::SandboxNeeds,
    workspace::FileDigest,
};

/// `prev` of the first record.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// What built this binary, recorded in every `run_started` record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct BuildInfo {
    pub harness_version: String,
    /// From `AGENT_HARNESS_GIT_COMMIT` at build time; `-dirty` marks
    /// uncommitted changes. `None` if it was not set.
    pub git_commit: Option<String>,
    /// `rustc --version` of the compiler that built the binary.
    pub rustc: String,
    /// Target triple, e.g. `aarch64-unknown-linux-gnu`.
    pub target: String,
    /// Cargo profile: `debug` or `release`.
    pub profile: String,
}

impl BuildInfo {
    /// The identity embedded in this binary by `build.rs`.
    pub fn current() -> Self {
        Self {
            harness_version: env!("CARGO_PKG_VERSION").to_owned(),
            git_commit: option_env!("AGENT_HARNESS_GIT_COMMIT").map(str::to_owned),
            rustc: env!("AGENT_HARNESS_RUSTC_VERSION").to_owned(),
            target: env!("AGENT_HARNESS_BUILD_TARGET").to_owned(),
            profile: env!("AGENT_HARNESS_BUILD_PROFILE").to_owned(),
        }
    }
}

/// A tool offered to the model, as recorded at the start of a run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolRecord {
    pub name: String,
    pub risk: ToolRisk,
    pub description: String,
    pub parameters: Value,
}

/// One audited event. New kinds may be added.
///
/// Variant sizes differ a lot (`RunStarted` is large). That is intended: an
/// event is built, serialized and dropped right away, never stored in bulk,
/// so boxing the large variant would only add an allocation.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
#[allow(clippy::large_enum_variant)]
pub enum AuditEvent {
    /// Everything needed to identify and reproduce the run's configuration.
    RunStarted {
        task: String,
        /// Harness version, git commit, compiler, target and profile.
        build: BuildInfo,
        runtime: RuntimeInfo,
        max_turns: usize,
        preamble: String,
        prompt: String,
        input: Value,
        inputs: Vec<FileDigest>,
        tools: Vec<ToolRecord>,
        sandbox: SandboxNeeds,
    },
    /// One model response, in full.
    ModelTurn {
        turn: usize,
        content: Value,
        usage: Value,
        provider: Option<String>,
        model: Option<String>,
        response_id: Option<String>,
        request_id: Option<String>,
    },
    /// A tool call the model requested, its declared risk and the decision.
    ToolCall {
        turn: usize,
        call_id: Value,
        name: String,
        arguments: Value,
        risk: ToolRisk,
        approval: Approval,
    },
    /// What the tool returned to the model (or why it failed).
    ToolResult {
        turn: usize,
        call_id: Value,
        name: String,
        ok: bool,
        output: String,
    },
    /// An external program run by [`crate::process::run`].
    ProcessRun {
        program: String,
        program_sha256: String,
        args: Vec<String>,
        cwd: String,
        exit_code: Option<i32>,
        timed_out: bool,
        elapsed_ms: u64,
        stdout_sha256: String,
        stderr_sha256: String,
        stdout_bytes: usize,
        stderr_bytes: usize,
        truncated: bool,
    },
    /// An external program's identity: hash and self-reported version.
    ProgramIdentified {
        program: String,
        sha256: String,
        version: String,
    },
    /// The model's final text. Informational only, never evidence.
    FinalAnswer { turn: usize, text: String },
    /// End of the agent loop, successful or not, with the workspace state.
    RunEnded {
        ok: bool,
        error: Option<String>,
        turns: Option<usize>,
        tool_calls: Option<usize>,
        usage: Value,
        outputs: Vec<FileDigest>,
    },
    /// One acceptance check and its result.
    CheckEvaluated {
        name: String,
        verifies: String,
        passed: bool,
        detail: String,
        evidence: Vec<u64>,
    },
    /// The final verdict and the task's report.
    Accepted {
        accepted: bool,
        checks: usize,
        report: Value,
    },
    /// Free-form note from a task or tool.
    Note { message: String },
}

/// Audit failures. Any of these fails the run.
#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("audit log I/O failed: {0}")]
    Io(#[from] io::Error),

    #[error("audit log is failed after an earlier error: {0}")]
    Failed(String),

    #[error("audit event could not be serialized: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("audit chain broken at record {seq}: {reason}")]
    Broken { seq: u64, reason: String },
}

/// Result of a successful [`verify_chain`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainSummary {
    pub records: u64,
    pub last_hash: String,
}

enum Sink {
    File(File),
    Memory(Vec<String>),
}

struct Inner {
    sink: Sink,
    path: Option<PathBuf>,
    seq: u64,
    prev: String,
    failure: Option<String>,
}

/// Handle to an append-only audit log. Clones share the log.
#[derive(Clone)]
pub struct AuditLog {
    inner: Arc<Mutex<Inner>>,
}

impl std::fmt::Debug for AuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.lock();
        f.debug_struct("AuditLog")
            .field("path", &inner.path)
            .field("records", &inner.seq)
            .field("failed", &inner.failure.is_some())
            .finish()
    }
}

impl AuditLog {
    /// Create a new log file. Refuses to overwrite an existing file.
    pub fn create(path: impl AsRef<Path>) -> Result<Self, AuditError> {
        let path = path.as_ref();
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Self::with_sink(Sink::File(file), Some(path.to_path_buf())))
    }

    /// A log kept in memory, for tests and dry runs.
    pub fn in_memory() -> Self {
        Self::with_sink(Sink::Memory(Vec::new()), None)
    }

    fn with_sink(sink: Sink, path: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                sink,
                path,
                seq: 0,
                prev: GENESIS.to_owned(),
                failure: None,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock cannot leave a half-written record:
        // records are written with one `write_all`. Recover the guard.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The log file, if this log is file-backed.
    pub fn path(&self) -> Option<PathBuf> {
        self.lock().path.clone()
    }

    /// Number of records written so far.
    pub fn len(&self) -> u64 {
        self.lock().seq
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The records of an in-memory log (empty for a file-backed log).
    pub fn lines(&self) -> Vec<String> {
        match &self.lock().sink {
            Sink::Memory(lines) => lines.clone(),
            Sink::File(_) => Vec::new(),
        }
    }

    /// `Ok` unless an earlier write failed.
    pub fn check(&self) -> Result<(), AuditError> {
        match &self.lock().failure {
            Some(reason) => Err(AuditError::Failed(reason.clone())),
            None => Ok(()),
        }
    }

    /// Append `event`, returning its sequence number. On any error the log
    /// becomes failed.
    pub fn record(&self, event: AuditEvent) -> Result<u64, AuditError> {
        let mut inner = self.lock();
        if let Some(reason) = &inner.failure {
            return Err(AuditError::Failed(reason.clone()));
        }
        match append(&mut inner, &event) {
            Ok(seq) => Ok(seq),
            Err(error) => {
                inner.failure = Some(error.to_string());
                Err(error)
            }
        }
    }

    /// Mark the log failed, as a write error would. Used to test fail-closed
    /// behaviour.
    #[doc(hidden)]
    pub fn fail_for_test(&self, reason: &str) {
        self.lock().failure = Some(reason.to_owned());
    }
}

fn append(inner: &mut Inner, event: &AuditEvent) -> Result<u64, AuditError> {
    let mut event = serde_json::to_value(event)?;
    redact_value(&mut event);
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let seq = inner.seq;
    let mut record = json!({ "seq": seq, "ts_ms": ts_ms, "prev": inner.prev, "event": event });
    let hash = record_hash(&record)?;
    record
        .as_object_mut()
        .expect("record is an object")
        .insert("hash".to_owned(), Value::String(hash.clone()));
    let line = serde_json::to_string(&record)?;
    match &mut inner.sink {
        Sink::File(file) => {
            let mut bytes = line.into_bytes();
            bytes.push(b'\n');
            file.write_all(&bytes)?;
            file.flush()?;
        }
        Sink::Memory(lines) => lines.push(line),
    }
    inner.seq += 1;
    inner.prev = hash;
    Ok(seq)
}

/// SHA-256 of a record serialized without its `hash` field.
fn record_hash(record: &Value) -> Result<String, AuditError> {
    Ok(sha256_hex(serde_json::to_string(record)?.as_bytes()))
}

/// Verify a log file's hash chain.
pub fn verify_chain(path: impl AsRef<Path>) -> Result<ChainSummary, AuditError> {
    let reader = BufReader::new(File::open(path)?);
    let mut lines = Vec::new();
    for line in reader.lines() {
        lines.push(line?);
    }
    verify_lines(lines.iter().map(String::as_str))
}

/// Verify a sequence of records (one JSON object per item).
pub fn verify_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
) -> Result<ChainSummary, AuditError> {
    let mut expected_seq = 0u64;
    let mut prev = GENESIS.to_owned();
    for line in lines {
        let broken = |reason: &str| AuditError::Broken {
            seq: expected_seq,
            reason: reason.to_owned(),
        };
        let mut record: Map<String, Value> =
            serde_json::from_str(line).map_err(|_| broken("not a JSON object"))?;
        let Some(Value::String(hash)) = record.remove("hash") else {
            return Err(broken("missing hash"));
        };
        if record.get("seq").and_then(Value::as_u64) != Some(expected_seq) {
            return Err(broken(
                "sequence number out of order (record missing or inserted)",
            ));
        }
        if record.get("prev").and_then(Value::as_str) != Some(prev.as_str()) {
            return Err(broken("prev does not match the previous record's hash"));
        }
        if record_hash(&Value::Object(record))? != hash {
            return Err(broken("hash does not match the record's content"));
        }
        prev = hash;
        expected_seq += 1;
    }
    Ok(ChainSummary {
        records: expected_seq,
        last_hash: prev,
    })
}

/// Lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Lower-case hex SHA-256 of a file's contents.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0xf)] as char);
    }
    out
}

/// Prefixes of common credential formats. A match followed by at least
/// [`MIN_SECRET_TAIL`] token characters is masked.
const SECRET_PREFIXES: [&str; 6] = ["sk-", "sk_", "AIza", "ghp_", "github_pat_", "xoxb-"];
const MIN_SECRET_TAIL: usize = 16;

/// Mask substrings that look like API keys: the prefix is kept and the rest
/// replaced with `[REDACTED]`. OpenShell placeholders are not secrets and are
/// left alone.
pub fn redact(text: &str) -> String {
    let is_token = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'scan: while !rest.is_empty() {
        for prefix in SECRET_PREFIXES {
            if let Some(after) = rest.strip_prefix(prefix) {
                let at_boundary = out.chars().last().is_none_or(|c| !is_token(c));
                let tail = after.find(|c: char| !is_token(c)).unwrap_or(after.len());
                if at_boundary && tail >= MIN_SECRET_TAIL {
                    out.push_str(prefix);
                    out.push_str("[REDACTED]");
                    rest = &after[tail..];
                    continue 'scan;
                }
            }
        }
        let mut chars = rest.chars();
        out.push(chars.next().expect("rest is not empty"));
        rest = chars.as_str();
    }
    out
}

fn redact_value(value: &mut Value) {
    match value {
        Value::String(s) => {
            let masked = redact(s);
            if masked != *s {
                *s = masked;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_value),
        Value::Object(map) => map.values_mut().for_each(redact_value),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(message: &str) -> AuditEvent {
        AuditEvent::Note {
            message: message.to_owned(),
        }
    }

    /// A unique temp path; the counter keeps back-to-back calls distinct
    /// even when the clock does not advance.
    fn temp_path(name: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "agent-harness-audit-{}-{name}-{}-{}.jsonl",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    #[test]
    fn records_chain_and_verify() {
        let log = AuditLog::in_memory();
        assert_eq!(log.record(note("a")).unwrap(), 0);
        assert_eq!(log.record(note("b")).unwrap(), 1);
        let lines = log.lines();
        let summary = verify_lines(lines.iter().map(String::as_str)).unwrap();
        assert_eq!(summary.records, 2);

        let first: Value = serde_json::from_str(&lines[0]).unwrap();
        let second: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(first["prev"], GENESIS);
        assert_eq!(second["prev"], first["hash"]);
        assert_eq!(second["event"], json!({"kind": "note", "message": "b"}));
        assert_eq!(summary.last_hash, second["hash"].as_str().unwrap());
    }

    #[test]
    fn detects_edited_record() {
        let log = AuditLog::in_memory();
        for m in ["a", "b", "c"] {
            log.record(note(m)).unwrap();
        }
        let mut lines = log.lines();
        lines[1] = lines[1].replace("\"b\"", "\"B\"");
        let err = verify_lines(lines.iter().map(String::as_str)).unwrap_err();
        assert!(
            matches!(err, AuditError::Broken { seq: 1, ref reason } if reason.contains("hash")),
            "{err}"
        );
    }

    #[test]
    fn detects_deleted_and_reordered_records() {
        let log = AuditLog::in_memory();
        for m in ["a", "b", "c"] {
            log.record(note(m)).unwrap();
        }
        let lines = log.lines();

        let deleted = [lines[0].as_str(), lines[2].as_str()];
        assert!(matches!(
            verify_lines(deleted),
            Err(AuditError::Broken { seq: 1, .. })
        ));

        let reordered = [lines[1].as_str(), lines[0].as_str()];
        assert!(matches!(
            verify_lines(reordered),
            Err(AuditError::Broken { seq: 0, .. })
        ));
    }

    #[test]
    fn detects_record_rehashed_without_chain() {
        // An attacker who edits a record and recomputes its own hash still
        // breaks the next record's `prev`.
        let log = AuditLog::in_memory();
        for m in ["a", "b"] {
            log.record(note(m)).unwrap();
        }
        let lines = log.lines();
        let mut forged: Map<String, Value> = serde_json::from_str(&lines[0]).unwrap();
        forged.remove("hash");
        forged["event"] = json!({"kind": "note", "message": "forged"});
        let hash = record_hash(&Value::Object(forged.clone())).unwrap();
        forged.insert("hash".into(), Value::String(hash));
        let forged = serde_json::to_string(&forged).unwrap();
        let err = verify_lines([forged.as_str(), lines[1].as_str()]).unwrap_err();
        assert!(
            matches!(err, AuditError::Broken { seq: 1, ref reason } if reason.contains("prev")),
            "{err}"
        );
    }

    #[test]
    fn file_log_round_trips_and_refuses_overwrite() {
        let path = temp_path("file");
        let log = AuditLog::create(&path).unwrap();
        log.record(note("one")).unwrap();
        log.record(note("two")).unwrap();
        assert_eq!(verify_chain(&path).unwrap().records, 2);
        assert!(AuditLog::create(&path).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn failed_log_rejects_further_records() {
        let log = AuditLog::in_memory();
        log.record(note("ok")).unwrap();
        log.fail_for_test("disk full");
        assert!(matches!(log.check(), Err(AuditError::Failed(_))));
        assert!(matches!(
            log.record(note("after")),
            Err(AuditError::Failed(_))
        ));
        assert_eq!(log.len(), 1);
    }

    /// A fake credential: `prefix` plus `len` filler characters. Built at
    /// runtime so no key-shaped literal appears in the source.
    fn fake_key(prefix: &str, len: usize) -> String {
        format!("{prefix}{}", "x".repeat(len))
    }

    #[test]
    fn redacts_credentials_but_keeps_text() {
        let tail = MIN_SECRET_TAIL + 4;
        assert_eq!(
            redact(&format!("key {} end", fake_key("sk-", tail))),
            "key sk-[REDACTED] end"
        );
        assert_eq!(
            redact(&format!("x-goog {}", fake_key("AIza", tail))),
            "x-goog AIza[REDACTED]"
        );
        assert_eq!(
            redact(&format!("päth {}.", fake_key("sk-", tail))),
            "päth sk-[REDACTED]."
        );
        // Too short to be a key, inside a word, or a placeholder: unchanged.
        for text in [
            fake_key("sk-", MIN_SECRET_TAIL - 1),
            format!("ta{}", fake_key("sk-", tail)),
            "openshell:resolve:env:OPENAI_API_KEY".to_owned(),
        ] {
            assert_eq!(redact(&text), text);
        }
    }

    #[test]
    fn redaction_applies_before_hashing() {
        let log = AuditLog::in_memory();
        let key = fake_key("ghp_", MIN_SECRET_TAIL + 4);
        log.record(note(&format!("token {key}"))).unwrap();
        let line = &log.lines()[0];
        assert!(!line.contains(&key), "{line}");
        assert!(line.contains("ghp_[REDACTED]"), "{line}");
        assert!(verify_lines([line.as_str()]).is_ok());
    }

    #[test]
    fn sha256_file_matches_known_digest() {
        let path = temp_path("prog");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn build_info_identifies_the_compiler() {
        let build = BuildInfo::current();
        assert_eq!(build.harness_version, env!("CARGO_PKG_VERSION"));
        assert!(build.rustc.starts_with("rustc 1."), "{}", build.rustc);
        assert!(!build.target.is_empty());
        assert!(
            ["debug", "release"].contains(&build.profile.as_str()),
            "{}",
            build.profile
        );
    }

    #[test]
    fn audit_log_is_send_sync_static() {
        fn assert_bounds<T: Send + Sync + 'static>() {}
        assert_bounds::<AuditLog>();
    }
}
