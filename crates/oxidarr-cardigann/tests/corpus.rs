//! Whole-corpus gates. Requires `scripts/fetch-definitions.sh` to have run.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

/// Locates `.definitions/v11` by walking up from the crate directory.
///
/// # Panics
///
/// Panics if `.definitions/v11` is not found in any parent directory.
#[must_use]
pub fn corpus_dir() -> PathBuf {
    let mut dir: &Path = Path::new(env!("CARGO_MANIFEST_DIR"));
    loop {
        let candidate = dir.join(".definitions/v11");
        if candidate.is_dir() {
            return candidate;
        }
        dir = dir.parent().expect(
            "`.definitions/v11` not found. Run `scripts/fetch-definitions.sh` from the repo root.",
        );
    }
}

/// Returns all `.yml` file paths in the corpus directory, sorted.
///
/// # Panics
///
/// Panics if the corpus directory is not readable.
#[must_use]
pub fn definition_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(corpus_dir())
        .expect("corpus directory is readable")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "yml"))
        .collect();
    paths.sort();
    paths
}

#[test]
fn corpus_is_present_and_large() {
    let paths = definition_paths();
    assert!(
        paths.len() > 500,
        "expected 500+ definitions, found {}",
        paths.len()
    );
}

#[test]
fn every_definition_is_valid_yaml() {
    let mut failures = Vec::new();
    for path in definition_paths() {
        let raw = std::fs::read_to_string(&path).unwrap();
        // Strip BOM if present (some files have UTF-8 BOM)
        let content = raw.trim_start_matches('\u{FEFF}');
        if let Err(e) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(content) {
            failures.push(format!("{}: {e}", path.display()));
        }
    }
    assert!(
        failures.is_empty(),
        "invalid YAML:\n{}",
        failures.join("\n")
    );
}
