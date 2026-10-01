//! The test corpus: small C functions with real bugs.
//!
//! Each case is a directory holding `case.json` (a [`FixInput`]), `src/`
//! (the buggy sources, which become the workspace) and `reference/` (a
//! known-good fix of the target file, kept out of `src/` so the model never
//! sees it). Every original fails CBMC and every reference verifies; the
//! crate's tests check both.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{FixInput, tools::unified_diff};

/// One corpus case.
#[derive(Debug, Clone)]
pub struct Case {
    pub name: String,
    pub input: FixInput,
    /// Directory to copy into the workspace.
    pub source: PathBuf,
    /// Directory holding the reference fix of `input.file`.
    pub reference: PathBuf,
}

impl Case {
    pub fn original(&self) -> io::Result<String> {
        fs::read_to_string(self.source.join(&self.input.file))
    }

    pub fn reference_fix(&self) -> io::Result<String> {
        fs::read_to_string(self.reference.join(&self.input.file))
    }

    /// The reference fix as a unified diff, as `apply_patch` expects it.
    pub fn reference_patch(&self) -> io::Result<String> {
        Ok(unified_diff(
            &self.input.file,
            &self.original()?,
            &self.reference_fix()?,
        ))
    }
}

/// The corpus shipped with this crate.
pub fn bundled_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus")
}

/// Load the case in `dir` (a directory with `case.json`, `src/` and
/// `reference/`).
pub fn load_case(dir: impl AsRef<Path>) -> io::Result<Case> {
    let path = dir.as_ref().to_path_buf();
    let manifest = path.join("case.json");
    let input: FixInput = serde_json::from_str(&fs::read_to_string(&manifest)?).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {e}", manifest.display()),
        )
    })?;
    Ok(Case {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        input,
        source: path.join("src"),
        reference: path.join("reference"),
    })
}

/// Load every case under `dir` (subdirectories with a `case.json`), sorted
/// by name.
pub fn load(dir: impl AsRef<Path>) -> io::Result<Vec<Case>> {
    let mut cases = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.join("case.json").is_file() {
            cases.push(load_case(&path)?);
        }
    }
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(cases)
}

/// The bundled corpus.
pub fn bundled() -> io::Result<Vec<Case>> {
    load(bundled_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{csource, tools::apply_to};

    #[test]
    fn bundled_corpus_is_well_formed() {
        let cases = bundled().unwrap();
        let names: Vec<_> = cases.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "average_div_zero",
                "buffer_off_by_one",
                "pitch_overflow",
                "ring_index",
                "sensor_delta",
                "shift_scale"
            ]
        );
        for case in &cases {
            let original = case.original().unwrap();
            let fixed = case.reference_fix().unwrap();
            assert_ne!(original, fixed, "{}", case.name);
            assert!(!case.input.description.is_empty(), "{}", case.name);
            assert!(
                csource::function_span(&original, &case.input.function).is_some(),
                "{}: target function not found",
                case.name
            );
            // The reference fix is exactly what its patch produces.
            let patch = case.reference_patch().unwrap();
            let (patched, _) = apply_to(&case.input.file, &original, &patch).unwrap();
            assert_eq!(patched, fixed, "{}", case.name);
            // Only the target file is in src/.
            let files: Vec<_> = fs::read_dir(&case.source).unwrap().collect();
            assert_eq!(files.len(), 1, "{}: src/ holds one file", case.name);
        }
    }
}
