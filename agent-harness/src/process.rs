//! Running external programs safely and audibly.
//!
//! Every tool that wraps a command-line program (a verifier, a compiler, a
//! simulator) runs it through [`run`]:
//!
//! * **No shell.** The program is an absolute path, so it maps one-to-one
//!   onto the sandbox policy's pinned `binaries`; arguments are passed as-is.
//! * **Cleared environment.** Only variables set on the [`ProcessSpec`] reach
//!   the program.
//! * **Bounded.** A timeout kills the program; captured output is capped
//!   (the full streams are still hashed and counted).
//! * **Audited, fail-closed.** Each run is recorded as an
//!   [`AuditEvent::ProcessRun`] with the binary's SHA-256. If the audit log
//!   has failed, nothing is started.

use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use rig_core::tool::ToolExecutionError;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    task::JoinHandle,
};

use crate::audit::{AuditError, AuditEvent, AuditLog, sha256_file};

/// Default wall-clock limit for one program run.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Default cap on captured bytes per stream.
pub const DEFAULT_MAX_OUTPUT: usize = 64 * 1024;
/// How long to wait for output pipes to close after the program exits or is
/// killed (a grandchild may still hold them open).
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// What to run. Build with [`ProcessSpec::new`] and the builder methods.
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    program: PathBuf,
    args: Vec<OsString>,
    cwd: PathBuf,
    env: Vec<(OsString, OsString)>,
    timeout: Duration,
    max_output: usize,
}

impl ProcessSpec {
    /// Run `program` (an absolute path) in `cwd` (an absolute directory).
    pub fn new(program: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            max_output: DEFAULT_MAX_OUTPUT,
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Pass one environment variable (the environment is otherwise empty).
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn max_output(mut self, bytes: usize) -> Self {
        self.max_output = bytes;
        self
    }

    pub fn program(&self) -> &Path {
        &self.program
    }
}

/// What a run produced. Fields may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProcessOutput {
    /// `None` if the program was killed (timeout or signal).
    pub exit_code: Option<i32>,
    /// Captured stdout, at most `max_output` bytes.
    pub stdout: Vec<u8>,
    /// Captured stderr, at most `max_output` bytes.
    pub stderr: Vec<u8>,
    /// Whether either stream was longer than what was captured.
    pub truncated: bool,
    pub timed_out: bool,
    pub elapsed: Duration,
    /// Sequence number of this run's audit record.
    pub audit_seq: u64,
}

impl ProcessOutput {
    /// Exited with status 0 within the timeout.
    pub fn success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out
    }

    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("program must be an absolute path: `{0}`")]
    RelativeProgram(PathBuf),

    #[error("working directory must be an absolute path: `{0}`")]
    RelativeCwd(PathBuf),

    #[error("could not start `{program}`: {source}")]
    Spawn {
        program: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("process I/O failed: {0}")]
    Io(#[from] io::Error),

    #[error(transparent)]
    Audit(#[from] AuditError),
}

impl From<ProcessError> for ToolExecutionError {
    fn from(error: ProcessError) -> Self {
        match error {
            ProcessError::Audit(_) => Self::refused(error.to_string()),
            _ => Self::other(error.to_string()),
        }
    }
}

struct Captured {
    head: Vec<u8>,
    total: usize,
    sha256: String,
}

/// Read a whole stream, keeping the first `cap` bytes and hashing all of it.
async fn capture<R: AsyncRead + Unpin>(mut stream: R, cap: usize) -> io::Result<Captured> {
    let mut head = Vec::new();
    let mut hasher = Sha256::new();
    let mut total = 0;
    let mut buf = [0u8; 8192];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n;
        if head.len() < cap {
            let take = n.min(cap - head.len());
            head.extend_from_slice(&buf[..take]);
        }
    }
    let digest = hasher.finalize();
    Ok(Captured {
        head,
        total,
        sha256: crate::audit::hex(&digest),
    })
}

/// Join a capture task, giving up after [`DRAIN_GRACE`].
async fn finish(task: JoinHandle<io::Result<Captured>>) -> (Captured, bool) {
    let abort = task.abort_handle();
    match tokio::time::timeout(DRAIN_GRACE, task).await {
        Ok(Ok(Ok(captured))) => (captured, false),
        Ok(Ok(Err(_)) | Err(_)) | Err(_) => {
            abort.abort();
            (
                Captured {
                    head: Vec::new(),
                    total: 0,
                    sha256: String::new(),
                },
                true,
            )
        }
    }
}

/// Run `spec`, record it in `audit`, and return what it produced.
///
/// A non-zero exit or a timeout is not an error: it is reported in the
/// [`ProcessOutput`] for the caller (and the model) to interpret. Errors are
/// for runs that could not happen or could not be audited.
pub async fn run(spec: ProcessSpec, audit: &AuditLog) -> Result<ProcessOutput, ProcessError> {
    audit.check()?;
    if !spec.program.is_absolute() {
        return Err(ProcessError::RelativeProgram(spec.program));
    }
    if !spec.cwd.is_absolute() {
        return Err(ProcessError::RelativeCwd(spec.cwd));
    }
    let program_sha256 = sha256_file(&spec.program).map_err(|source| ProcessError::Spawn {
        program: spec.program.clone(),
        source,
    })?;

    let mut child = Command::new(&spec.program)
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| ProcessError::Spawn {
            program: spec.program.clone(),
            source,
        })?;
    let start = Instant::now();
    let stdout = tokio::spawn(capture(
        child.stdout.take().expect("stdout is piped"),
        spec.max_output,
    ));
    let stderr = tokio::spawn(capture(
        child.stderr.take().expect("stderr is piped"),
        spec.max_output,
    ));

    let (exit_code, timed_out) = match tokio::time::timeout(spec.timeout, child.wait()).await {
        Ok(status) => (status?.code(), false),
        Err(_) => {
            child.kill().await?;
            (None, true)
        }
    };
    let elapsed = start.elapsed();
    let ((out, out_lost), (err, err_lost)) = (finish(stdout).await, finish(stderr).await);
    let truncated =
        out_lost || err_lost || out.total > out.head.len() || err.total > err.head.len();

    let audit_seq = audit.record(AuditEvent::ProcessRun {
        program: spec.program.display().to_string(),
        program_sha256,
        args: spec
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        cwd: spec.cwd.display().to_string(),
        exit_code,
        timed_out,
        elapsed_ms: elapsed.as_millis() as u64,
        stdout_sha256: out.sha256,
        stderr_sha256: err.sha256,
        stdout_bytes: out.total,
        stderr_bytes: err.total,
        truncated,
    })?;

    Ok(ProcessOutput {
        exit_code,
        stdout: out.head,
        stderr: err.head,
        truncated,
        timed_out,
        elapsed,
        audit_seq,
    })
}

/// Record a program's identity: its SHA-256 and the output of
/// `program <version_args>` (e.g. `--version`). Returns the version text.
pub async fn identify(
    program: impl Into<PathBuf>,
    version_args: &[&str],
    audit: &AuditLog,
) -> Result<String, ProcessError> {
    let program = program.into();
    let cwd = std::env::temp_dir();
    let output = run(
        ProcessSpec::new(program.clone(), cwd)
            .args(version_args.iter().copied())
            .timeout(Duration::from_secs(10)),
        audit,
    )
    .await?;
    let version = output.stdout_lossy().trim().to_owned();
    audit.record(AuditEvent::ProgramIdentified {
        program: program.display().to_string(),
        sha256: sha256_file(&program)?,
        version: version.clone(),
    })?;
    Ok(version)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::Value;

    fn tmp() -> PathBuf {
        std::env::temp_dir().canonicalize().unwrap()
    }

    fn sh(script: &str) -> ProcessSpec {
        ProcessSpec::new("/bin/sh", tmp()).arg("-c").arg(script)
    }

    fn last_event(audit: &AuditLog) -> Value {
        let lines = audit.lines();
        let record: Value = serde_json::from_str(lines.last().unwrap()).unwrap();
        record["event"].clone()
    }

    #[tokio::test]
    async fn runs_and_records() {
        let audit = AuditLog::in_memory();
        let out = run(sh("echo hello; echo oops >&2; exit 3"), &audit)
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(3));
        assert!(!out.success());
        assert_eq!(out.stdout_lossy(), "hello\n");
        assert_eq!(out.stderr_lossy(), "oops\n");
        assert!(!out.truncated && !out.timed_out);

        let event = last_event(&audit);
        assert_eq!(event["kind"], "process_run");
        assert_eq!(event["program"], "/bin/sh");
        assert_eq!(event["exit_code"], 3);
        assert_eq!(event["stdout_bytes"], 6);
        assert_eq!(
            event["program_sha256"],
            sha256_file(Path::new("/bin/sh")).unwrap()
        );
        assert_eq!(event["stdout_sha256"], crate::audit::sha256_hex(b"hello\n"));
        assert_eq!(out.audit_seq, 0);
    }

    #[tokio::test]
    async fn environment_is_cleared() {
        let audit = AuditLog::in_memory();
        // `HOME` is set in the test's environment; it must not leak through.
        let out = run(sh("echo \"[$HOME][$ONLY]\"").env("ONLY", "yes"), &audit)
            .await
            .unwrap();
        assert_eq!(out.stdout_lossy(), "[][yes]\n");
    }

    #[tokio::test]
    async fn timeout_kills_the_program() {
        let audit = AuditLog::in_memory();
        let out = run(sh("sleep 30").timeout(Duration::from_millis(200)), &audit)
            .await
            .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
        assert!(out.elapsed < Duration::from_secs(10));
        assert_eq!(last_event(&audit)["timed_out"], true);
    }

    #[tokio::test]
    async fn output_is_capped_but_fully_counted() {
        let audit = AuditLog::in_memory();
        let out = run(
            sh("i=0; while [ $i -lt 2000 ]; do echo 0123456789; i=$((i+1)); done").max_output(100),
            &audit,
        )
        .await
        .unwrap();
        assert_eq!(out.stdout.len(), 100);
        assert!(out.truncated);
        assert_eq!(last_event(&audit)["stdout_bytes"], 22000);
    }

    #[tokio::test]
    async fn rejects_relative_paths_and_missing_programs() {
        let audit = AuditLog::in_memory();
        let rel = run(ProcessSpec::new("sh", tmp()), &audit).await;
        assert!(matches!(rel, Err(ProcessError::RelativeProgram(_))));
        let cwd = run(ProcessSpec::new("/bin/sh", "relative"), &audit).await;
        assert!(matches!(cwd, Err(ProcessError::RelativeCwd(_))));
        let missing = run(ProcessSpec::new("/no/such/program", tmp()), &audit).await;
        assert!(matches!(missing, Err(ProcessError::Spawn { .. })));
        assert!(audit.is_empty(), "nothing ran, nothing recorded");
    }

    #[tokio::test]
    async fn failed_audit_log_blocks_programs() {
        let audit = AuditLog::in_memory();
        audit.fail_for_test("disk full");
        let marker = tmp().join(format!("ah-should-not-exist-{}", std::process::id()));
        let result = run(sh(&format!("touch {}", marker.display())), &audit).await;
        assert!(matches!(result, Err(ProcessError::Audit(_))));
        assert!(!marker.exists(), "the program must not have run");
    }

    #[tokio::test]
    async fn identify_records_hash_and_version() {
        let audit = AuditLog::in_memory();
        let version = identify("/bin/sh", &["-c", "echo sh-1.0"], &audit)
            .await
            .unwrap();
        assert_eq!(version, "sh-1.0");
        let event = last_event(&audit);
        assert_eq!(event["kind"], "program_identified");
        assert_eq!(event["version"], "sh-1.0");
    }
}
