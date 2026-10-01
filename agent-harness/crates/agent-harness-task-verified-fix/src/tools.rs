//! The task's own tools: `read_source` (read-only) and `apply_patch`
//! (mutating; goes to the approval policy).

use agent_harness::{
    Workspace, WorkspaceError,
    rig_core::tool::{PortableTool, ToolExecutionError},
};
use diffy::Patch;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Largest patch accepted, in bytes.
pub const MAX_PATCH_BYTES: usize = 64 * 1024;

/// Reads a file in the workspace.
#[derive(Debug, Clone)]
pub struct ReadSource {
    workspace: Workspace,
}

impl ReadSource {
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadSourceArgs {
    pub path: String,
}

impl PortableTool for ReadSource {
    const NAME: &'static str = "read_source";
    type Args = ReadSourceArgs;
    type Output = String;
    type Error = WorkspaceError;

    fn description(&self) -> String {
        "Read a source file in the workspace, exactly as it is now.".to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to the workspace root" }
            },
            "required": ["path"]
        })
    }

    fn map_error(&self, error: WorkspaceError) -> ToolExecutionError {
        error.into()
    }

    async fn call(&self, args: ReadSourceArgs) -> Result<String, WorkspaceError> {
        self.workspace.read_to_string(args.path)
    }
}

/// Applies a unified diff to the target file, and only to it.
#[derive(Debug, Clone)]
pub struct ApplyPatch {
    workspace: Workspace,
    file: String,
}

impl ApplyPatch {
    /// A tool that may change only `file` (relative to the workspace root).
    pub fn new(workspace: Workspace, file: impl Into<String>) -> Self {
        Self {
            workspace,
            file: file.into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApplyPatchArgs {
    pub patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyPatchOutput {
    pub file: String,
    pub hunks: usize,
    /// The whole file after the patch.
    pub content: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    #[error("patch is larger than {MAX_PATCH_BYTES} bytes")]
    TooLarge,

    #[error("not a unified diff: {0}")]
    Malformed(String),

    #[error("the patch has no hunks")]
    Empty,

    #[error(
        "patches may only change `{allowed}`; this one names {found}. Use \
         `--- a/{allowed}` and `+++ b/{allowed}` headers"
    )]
    WrongFile { allowed: String, found: String },

    #[error(
        "hunk {0} does not apply: its context lines do not match the current \
         file. Re-read it with read_source and make a new patch"
    )]
    DoesNotApply(usize),

    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
}

impl From<PatchError> for ToolExecutionError {
    fn from(error: PatchError) -> Self {
        match error {
            PatchError::Workspace(e) => e.into(),
            PatchError::WrongFile { .. } => Self::refused(error.to_string()),
            _ => Self::invalid_args(error.to_string()),
        }
    }
}

/// `a/pitch.c\t2026-01-01` → `pitch.c`.
fn header_path(header: &str) -> &str {
    let path = header.split('\t').next().unwrap_or_default().trim();
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
}

/// Check `patch` targets `file` and apply it to `current`.
pub fn apply_to(file: &str, current: &str, patch: &str) -> Result<(String, usize), PatchError> {
    if patch.len() > MAX_PATCH_BYTES {
        return Err(PatchError::TooLarge);
    }
    let parsed = Patch::from_str(patch).map_err(|e| PatchError::Malformed(e.to_string()))?;
    // diffy parses any text; text with neither headers nor hunks is not a diff.
    if parsed.original().is_none() && parsed.modified().is_none() && parsed.hunks().is_empty() {
        return Err(PatchError::Malformed(
            "no `---`/`+++` headers or `@@` hunks found".to_owned(),
        ));
    }
    let names = [parsed.original(), parsed.modified()];
    if names
        .iter()
        .any(|n| n.is_none_or(|n| header_path(n) != file))
    {
        let found = names
            .iter()
            .map(|n| n.map_or_else(|| "no file".to_owned(), |n| format!("`{}`", header_path(n))))
            .collect::<Vec<_>>()
            .join(" and ");
        return Err(PatchError::WrongFile {
            allowed: file.to_owned(),
            found,
        });
    }
    if parsed.hunks().is_empty() {
        return Err(PatchError::Empty);
    }
    let patched = diffy::apply(current, &parsed).map_err(|e| {
        let hunk = e
            .to_string()
            .split_whitespace()
            .find_map(|w| w.parse().ok())
            .unwrap_or(1);
        PatchError::DoesNotApply(hunk)
    })?;
    Ok((patched, parsed.hunks().len()))
}

impl PortableTool for ApplyPatch {
    const NAME: &'static str = "apply_patch";
    type Args = ApplyPatchArgs;
    type Output = ApplyPatchOutput;
    type Error = PatchError;

    fn description(&self) -> String {
        format!(
            "Apply a unified diff to `{}` (the only file you may change). Use \
             `--- a/{0}` / `+++ b/{0}` headers and include unchanged context \
             lines exactly as they are. Returns the whole file after the patch.",
            self.file
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "description": "A unified diff" }
            },
            "required": ["patch"]
        })
    }

    fn map_error(&self, error: PatchError) -> ToolExecutionError {
        error.into()
    }

    async fn call(&self, args: ApplyPatchArgs) -> Result<ApplyPatchOutput, PatchError> {
        let current = self.workspace.read_to_string(&self.file)?;
        let (patched, hunks) = apply_to(&self.file, &current, &args.patch)?;
        self.workspace.write(&self.file, &patched)?;
        Ok(ApplyPatchOutput {
            file: self.file.clone(),
            hunks,
            content: patched,
        })
    }
}

/// A unified diff from `original` to `modified` for `file`, with `a/` and
/// `b/` headers (as `git diff` writes them).
pub fn unified_diff(file: &str, original: &str, modified: &str) -> String {
    let mut options = diffy::DiffOptions::new();
    options
        .set_original_filename(format!("a/{file}"))
        .set_modified_filename(format!("b/{file}"));
    options.create_patch(original, modified).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEFORE: &str = "int f(int x) {\n  return x / 0;\n}\n";
    const AFTER: &str = "int f(int x) {\n  return x;\n}\n";

    #[test]
    fn applies_a_diff_for_the_target_file() {
        let patch = unified_diff("f.c", BEFORE, AFTER);
        assert!(patch.starts_with("--- a/f.c\n+++ b/f.c\n"), "{patch}");
        assert_eq!(
            apply_to("f.c", BEFORE, &patch).unwrap(),
            (AFTER.to_owned(), 1)
        );
    }

    #[test]
    fn tolerates_shifted_line_numbers_but_not_wrong_context() {
        let shifted =
            unified_diff("f.c", BEFORE, AFTER).replace("@@ -1,3 +1,3 @@", "@@ -5,3 +5,3 @@");
        assert_eq!(apply_to("f.c", BEFORE, &shifted).unwrap().0, AFTER);

        let wrong =
            unified_diff("f.c", BEFORE, AFTER).replace(" int f(int x) {", " int g(int x) {");
        assert!(matches!(
            apply_to("f.c", BEFORE, &wrong),
            Err(PatchError::DoesNotApply(1))
        ));
    }

    #[test]
    fn only_the_target_file_may_change() {
        for (patch, why) in [
            (unified_diff("other.c", BEFORE, AFTER), "another file"),
            (
                unified_diff("f.c", BEFORE, AFTER).replace("+++ b/f.c", "+++ b/other.c"),
                "rename",
            ),
            (
                unified_diff("f.c", BEFORE, AFTER).replace("--- a/f.c\n+++ b/f.c\n", ""),
                "no headers",
            ),
        ] {
            let err = apply_to("f.c", BEFORE, &patch).unwrap_err();
            assert!(matches!(err, PatchError::WrongFile { .. }), "{why}: {err}");
            assert_eq!(
                ToolExecutionError::from(err).kind(),
                agent_harness::rig_core::tool::ToolErrorKind::PermissionDenied
            );
        }
    }

    #[test]
    fn rejects_malformed_empty_and_huge_patches() {
        assert!(matches!(
            apply_to("f.c", BEFORE, "not a diff\n+++"),
            Err(PatchError::Malformed(_))
        ));
        assert!(matches!(
            apply_to("f.c", BEFORE, "--- a/f.c\n+++ b/f.c\n"),
            Err(PatchError::Empty)
        ));
        let huge = "x".repeat(MAX_PATCH_BYTES + 1);
        assert!(matches!(
            apply_to("f.c", BEFORE, &huge),
            Err(PatchError::TooLarge)
        ));
    }

    #[test]
    fn header_paths_drop_prefixes_and_timestamps() {
        assert_eq!(header_path("a/src/f.c\t2026-01-01 00:00"), "src/f.c");
        assert_eq!(header_path("b/f.c"), "f.c");
        assert_eq!(header_path("f.c"), "f.c");
    }
}
