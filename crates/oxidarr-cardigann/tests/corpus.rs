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

/// Loads and parses a YAML definition file, handling UTF-8 BOM if present.
fn load_definition(path: &Path) -> Result<serde_yaml_ng::Value, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(path)?;
    // Strip BOM if present (some files have UTF-8 BOM)
    let content = raw.trim_start_matches('\u{FEFF}');
    Ok(serde_yaml_ng::from_str(content)?)
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
        if let Err(e) = load_definition(&path) {
            failures.push(format!("{}: {e}", path.display()));
        }
    }
    assert!(
        failures.is_empty(),
        "invalid YAML:\n{}",
        failures.join("\n")
    );
}

/// Recursively collects every `selector:` string in a definition.
fn collect_selectors(node: &serde_yaml_ng::Value, out: &mut Vec<String>) {
    match node {
        serde_yaml_ng::Value::Mapping(map) => {
            for (k, v) in map {
                if k.as_str() == Some("selector")
                    && let Some(s) = v.as_str()
                {
                    out.push(s.to_string());
                }
                collect_selectors(v, out);
            }
        }
        serde_yaml_ng::Value::Sequence(seq) => {
            for v in seq {
                collect_selectors(v, out);
            }
        }
        _ => {}
    }
}

#[test]
fn every_selector_parses_as_standard_css() {
    let mut total = 0usize;
    let mut failed = Vec::new();

    for path in definition_paths() {
        let doc = load_definition(&path).unwrap();
        let mut selectors = Vec::new();
        collect_selectors(&doc, &mut selectors);

        for sel in selectors {
            total += 1;
            // Templated selectors cannot be parsed until they are rendered.
            // A literal `:contains(` in one is still broken once it is, so
            // count those rather than letting them escape the gate.
            if sel.contains("{{") {
                if sel.contains(":contains(") {
                    failed.push(sel);
                }
                continue;
            }
            if oxidarr_cardigann::selector::compile(&sel).is_err() {
                failed.push(sel);
            }
        }
    }

    assert!(
        failed.is_empty(),
        "{} of {total} selectors do not parse as standard CSS. Examples:\n{}",
        failed.len(),
        failed
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
