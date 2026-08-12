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

/// Why a call to [`strip_spans`] stopped consuming input.
enum Stop {
    /// Hit a `{{ else }}` belonging to an enclosing `{{ if }}`.
    Else,
    /// Hit a `{{ end }}` belonging to an enclosing `{{ if }}`.
    End,
    /// Ran out of input.
    Eof,
}

/// Removes every `{{ ... }}` span from `sel`, returning the residual
/// string.
///
/// A bare action (typically a value interpolation, e.g.
/// `{{ .Config.uploader }}`) contributes nothing. An `{{ if COND }}...{{ end }}`
/// or `{{ if COND }}...{{ else }}...{{ end }}` keeps only the `if` branch
/// and drops the `else` branch entirely, rather than concatenating both —
/// concatenating two alternative selector fragments is not valid CSS even
/// though each fragment, alone, would be. Handles multiple and nested
/// spans. A span that is never closed, or an `if` never closed by a
/// matching `end`, drops everything from that point to the end of the
/// string rather than panicking or looping forever.
fn strip_template_spans(sel: &str) -> String {
    let (mut out, stop, remainder) = strip_spans(sel);
    if !matches!(stop, Stop::Eof) {
        // A stray `else`/`end` with no enclosing `if` at this level: keep
        // processing rather than silently dropping the remainder.
        out.push_str(&strip_template_spans(remainder));
    }
    out
}

/// Consumes `s` up to (and past) the first `{{ else }}` or `{{ end }}` that
/// has no enclosing `{{ if }}` opened *within this call*, or to the end of
/// `s` if neither appears. Every `{{ if COND }}` encountered is resolved
/// recursively before the scan continues.
fn strip_spans(s: &str) -> (String, Stop, &str) {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let Some(start) = rest.find("{{") else {
            out.push_str(rest);
            return (out, Stop::Eof, "");
        };
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(close) = after_open.find("}}") else {
            return (out, Stop::Eof, "");
        };
        let action = after_open[..close].trim();
        let after_action = &after_open[close + 2..];

        if action == "else" {
            return (out, Stop::Else, after_action);
        }
        if action == "end" {
            return (out, Stop::End, after_action);
        }
        if action.starts_with("if") {
            let (branch, stop, remainder) = strip_spans(after_action);
            out.push_str(&branch);
            rest = match stop {
                Stop::Else => strip_spans(remainder).2,
                Stop::End | Stop::Eof => remainder,
            };
        } else {
            // Any other bare action contributes nothing.
            rest = after_action;
        }
    }
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
            // Templated selectors cannot be parsed until they are rendered,
            // and no template engine exists until Task 4. Neutralise the
            // `{{ ... }}` spans and require the residual selector to compile:
            // that still exercises the `:contains()` rewriting on them, so
            // none escapes the gate, and the gate stays satisfiable.
            if sel.contains("{{") {
                let stripped = strip_template_spans(&sel);
                if oxidarr_cardigann::selector::compile(&stripped).is_err() {
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
