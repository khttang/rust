//! A disposable working copy that tools are confined to.
//!
//! [`Workspace::create`] copies a source directory into a fresh private
//! directory under the system temp dir (`$TMPDIR`, which is `/tmp` inside the
//! OpenShell sandbox). Tools read and write only through [`Workspace::resolve`],
//! which rejects absolute paths, `..` and symlinks that lead outside, so the
//! original files are never touched. The copy is deleted when the last handle
//! is dropped, unless [`Workspace::keep`] was called (e.g. to retain evidence).

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

use crate::audit::sha256_file;

/// A file's identity: path relative to the workspace root (`/`-separated),
/// SHA-256 and size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileDigest {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("workspace I/O failed: {0}")]
    Io(#[from] io::Error),

    #[error("source `{0}` is not a directory")]
    NotADirectory(PathBuf),

    #[error("symlinks are not allowed in a workspace: `{0}`")]
    Symlink(PathBuf),

    #[error("path `{0}` is outside the workspace")]
    Escape(String),
}

impl From<WorkspaceError> for rig_core::tool::ToolExecutionError {
    fn from(error: WorkspaceError) -> Self {
        match error {
            WorkspaceError::Escape(_) | WorkspaceError::Symlink(_) => {
                Self::refused(error.to_string())
            }
            _ => Self::other(error.to_string()),
        }
    }
}

struct Inner {
    root: PathBuf,
    source: PathBuf,
    keep: AtomicBool,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if !self.keep.load(Ordering::Acquire) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

/// Handle to a working copy. Clones share it; the directory is removed when
/// the last clone is dropped unless kept.
#[derive(Clone)]
pub struct Workspace {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace")
            .field("root", &self.inner.root)
            .field("source", &self.inner.source)
            .finish()
    }
}

impl Workspace {
    /// Copy `source` (a directory without symlinks) into a new workspace.
    pub fn create(source: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        Self::create_in(source, std::env::temp_dir())
    }

    /// [`Self::create`] under an explicit parent directory.
    pub fn create_in(
        source: impl AsRef<Path>,
        parent: impl AsRef<Path>,
    ) -> Result<Self, WorkspaceError> {
        let source = source.as_ref().canonicalize()?;
        if !source.is_dir() {
            return Err(WorkspaceError::NotADirectory(source));
        }
        let parent = parent.as_ref().canonicalize()?;
        let root = parent.join(unique_name());
        create_private_dir(&root)?;
        // Clean up a half-made copy if copying fails.
        let workspace = Self {
            inner: Arc::new(Inner {
                root,
                source,
                keep: AtomicBool::new(false),
            }),
        };
        copy_tree(&workspace.inner.source, &workspace.inner.root)?;
        Ok(workspace)
    }

    /// Canonical path of the working copy.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Canonical path of the original source directory (never modified).
    pub fn source(&self) -> &Path {
        &self.inner.source
    }

    /// Keep the directory after the last handle is dropped.
    pub fn keep(&self) {
        self.inner.keep.store(true, Ordering::Release);
    }

    pub fn is_kept(&self) -> bool {
        self.inner.keep.load(Ordering::Acquire)
    }

    /// Absolute path for `rel`, which must be a relative path inside the
    /// workspace. Rejects absolute paths, `..`, and symlinks leading outside.
    pub fn resolve(&self, rel: impl AsRef<Path>) -> Result<PathBuf, WorkspaceError> {
        let rel = rel.as_ref();
        let display = || rel.display().to_string();
        if rel.as_os_str().is_empty() {
            return Err(WorkspaceError::Escape(display()));
        }
        for component in rel.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                _ => return Err(WorkspaceError::Escape(display())),
            }
        }
        let path = self.inner.root.join(rel);
        // The deepest existing ancestor (or the path itself) must resolve,
        // symlinks included, to somewhere inside the root.
        let mut existing = path.as_path();
        while !existing.exists() {
            existing = existing
                .parent()
                .ok_or_else(|| WorkspaceError::Escape(display()))?;
        }
        if !existing.canonicalize()?.starts_with(&self.inner.root) {
            return Err(WorkspaceError::Escape(display()));
        }
        Ok(path)
    }

    pub fn read(&self, rel: impl AsRef<Path>) -> Result<Vec<u8>, WorkspaceError> {
        Ok(fs::read(self.resolve(rel)?)?)
    }

    pub fn read_to_string(&self, rel: impl AsRef<Path>) -> Result<String, WorkspaceError> {
        Ok(fs::read_to_string(self.resolve(rel)?)?)
    }

    /// Write `contents` to `rel`, creating parent directories inside the
    /// workspace as needed.
    pub fn write(
        &self,
        rel: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> Result<(), WorkspaceError> {
        let path = self.resolve(rel)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(fs::write(path, contents)?)
    }

    /// SHA-256 of every file in the workspace, sorted by path. Fails on
    /// symlinks, which tools must not create.
    pub fn digests(&self) -> Result<Vec<FileDigest>, WorkspaceError> {
        let mut out = Vec::new();
        digest_tree(&self.inner.root, &self.inner.root, &mut out)?;
        Ok(out)
    }
}

fn unique_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "agent-harness-ws-{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// Entries of `dir`, sorted by name, so copies and digests are deterministic.
fn sorted_entries(dir: &Path) -> io::Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), WorkspaceError> {
    for entry in sorted_entries(from)? {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(WorkspaceError::Symlink(src));
        } else if kind.is_dir() {
            fs::create_dir(&dst)?;
            copy_tree(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

fn digest_tree(root: &Path, dir: &Path, out: &mut Vec<FileDigest>) -> Result<(), WorkspaceError> {
    for entry in sorted_entries(dir)? {
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(WorkspaceError::Symlink(path));
        } else if kind.is_dir() {
            digest_tree(root, &path, out)?;
        } else {
            let rel = path.strip_prefix(root).expect("walked under root");
            out.push(FileDigest {
                path: rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
                sha256: sha256_file(&path)?,
                bytes: entry.metadata()?.len(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source tree: `a.c`, `sub/b.h`.
    fn source() -> Workspace {
        let src = Workspace::create_in(empty_dir(), std::env::temp_dir()).unwrap();
        src.write("a.c", "int a;").unwrap();
        src.write("sub/b.h", "int b;").unwrap();
        src
    }

    fn empty_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(unique_name());
        fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn copies_source_and_leaves_it_untouched() {
        let src = source();
        let ws = Workspace::create(src.root()).unwrap();
        assert_ne!(ws.root(), src.root());
        assert_eq!(ws.read_to_string("sub/b.h").unwrap(), "int b;");

        ws.write("a.c", "int a = 1;").unwrap();
        assert_eq!(src.read_to_string("a.c").unwrap(), "int a;");
        assert_eq!(ws.read_to_string("a.c").unwrap(), "int a = 1;");
    }

    #[test]
    fn rejects_paths_outside() {
        let ws = Workspace::create(source().root()).unwrap();
        for bad in ["../x", "/etc/passwd", "sub/../../x", ""] {
            assert!(
                matches!(ws.resolve(bad), Err(WorkspaceError::Escape(_))),
                "{bad}"
            );
        }
        assert!(ws.resolve("./sub/b.h").is_ok());
        assert!(ws.resolve("new/dir/file.c").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escapes_and_symlinked_sources() {
        let ws = Workspace::create(source().root()).unwrap();
        std::os::unix::fs::symlink("/etc", ws.root().join("out")).unwrap();
        assert!(matches!(
            ws.resolve("out/passwd"),
            Err(WorkspaceError::Escape(_))
        ));
        assert!(matches!(ws.digests(), Err(WorkspaceError::Symlink(_))));

        let src = source();
        std::os::unix::fs::symlink("/etc", src.root().join("link")).unwrap();
        assert!(matches!(
            Workspace::create(src.root()),
            Err(WorkspaceError::Symlink(_))
        ));
    }

    #[test]
    fn digests_are_sorted_and_track_changes() {
        let ws = Workspace::create(source().root()).unwrap();
        let before = ws.digests().unwrap();
        let paths: Vec<_> = before.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["a.c", "sub/b.h"]);
        assert_eq!(before[0].bytes, 6);

        ws.write("a.c", "int a = 2;").unwrap();
        let after = ws.digests().unwrap();
        assert_ne!(before[0].sha256, after[0].sha256);
        assert_eq!(before[1], after[1]);
    }

    #[test]
    fn removed_on_drop_unless_kept() {
        let src = source();
        let ws = Workspace::create(src.root()).unwrap();
        let root = ws.root().to_path_buf();
        let clone = ws.clone();
        drop(ws);
        assert!(root.exists(), "a clone is still alive");
        drop(clone);
        assert!(!root.exists());

        let kept = Workspace::create(src.root()).unwrap();
        kept.keep();
        let root = kept.root().to_path_buf();
        drop(kept);
        assert!(root.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_non_directory_source() {
        let src = source();
        let file = src.resolve("a.c").unwrap();
        assert!(matches!(
            Workspace::create(file),
            Err(WorkspaceError::NotADirectory(_))
        ));
    }

    #[test]
    fn workspace_is_send_sync_static() {
        fn assert_bounds<T: Send + Sync + 'static>() {}
        assert_bounds::<Workspace>();
    }
}
