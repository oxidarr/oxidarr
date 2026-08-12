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

/// Why a call to [`expand_spans`] stopped consuming input.
enum Stop {
    /// Hit a `{{ else }}` belonging to an enclosing `{{ if }}`.
    Else,
    /// Hit a `{{ end }}` belonging to an enclosing `{{ if }}`.
    End,
    /// Ran out of input.
    Eof,
}

/// Expands every `{{ ... }}` construct in `sel` into the set of residual
/// strings that could actually be rendered at runtime.
///
/// A bare action (typically a value interpolation, e.g.
/// `{{ .Config.uploader }}`) contributes nothing — it renders to an
/// unknown value, and dropping it entirely is what lets `:contains()`'s
/// argument end up empty, which `compile()` already treats as "matches
/// anything". An `{{ if COND }}A{{ else }}B{{ end }}` produces one residual
/// per distinct branch (both `A` and `B`, not their concatenation, since
/// only one is ever rendered at a time and gluing them together produces a
/// string that was never actually reachable); `{{ if COND }}A{{ end }}`
/// with no `else` produces both `A` and the empty string, since Go's
/// template renders nothing for the block when `COND` is false. Every
/// combination of branch choices across multiple constructs in the same
/// selector is expanded, so nested and sibling constructs are each
/// validated on their own and in combination. A span that is never closed,
/// or an `if` never closed by a matching `end`, drops everything from that
/// point on rather than panicking or looping forever.
fn strip_template_spans(sel: &str) -> Vec<String> {
    let (mut variants, stop, remainder) = expand_spans(sel);
    if !matches!(stop, Stop::Eof) {
        // A stray `else`/`end` with no enclosing `if` at this level: keep
        // processing rather than silently dropping the remainder.
        let mut combined = Vec::new();
        for prefix in &variants {
            for suffix in strip_template_spans(remainder) {
                combined.push(format!("{prefix}{suffix}"));
            }
        }
        variants = combined;
    }
    variants
}

/// Consumes `s` up to (and past) the first `{{ else }}` or `{{ end }}` that
/// has no enclosing `{{ if }}` opened *within this call*, or to the end of
/// `s` if neither appears, returning every residual string reachable up to
/// that point. Every `{{ if COND }}` encountered is resolved recursively
/// before the scan continues, and its branch options are combined
/// (Cartesian-product style) with whatever was already accumulated.
fn expand_spans(s: &str) -> (Vec<String>, Stop, &str) {
    let mut variants = vec![String::new()];
    let mut rest = s;
    loop {
        let Some(start) = rest.find("{{") else {
            for v in &mut variants {
                v.push_str(rest);
            }
            return (variants, Stop::Eof, "");
        };
        let prefix = &rest[..start];
        for v in &mut variants {
            v.push_str(prefix);
        }
        let after_open = &rest[start + 2..];
        let Some(close) = after_open.find("}}") else {
            return (variants, Stop::Eof, "");
        };
        let action = after_open[..close].trim();
        let after_action = &after_open[close + 2..];

        if action == "else" {
            return (variants, Stop::Else, after_action);
        }
        if action == "end" {
            return (variants, Stop::End, after_action);
        }
        if action.starts_with("if") {
            let (then_variants, stop, remainder) = expand_spans(after_action);
            let (branch_options, remainder) = match stop {
                Stop::Else => {
                    let (else_variants, _stop, remainder) = expand_spans(remainder);
                    let mut options = then_variants;
                    for v in else_variants {
                        if !options.contains(&v) {
                            options.push(v);
                        }
                    }
                    (options, remainder)
                }
                Stop::End | Stop::Eof => {
                    // No `{{ else }}`: Go's template renders nothing for
                    // this block when the condition is false, which is
                    // just as reachable as the `if`-branch content, so it
                    // must be included as an option too.
                    let mut options = then_variants;
                    if !options.contains(&String::new()) {
                        options.push(String::new());
                    }
                    (options, remainder)
                }
            };
            let mut combined = Vec::with_capacity(variants.len() * branch_options.len().max(1));
            for prefix in &variants {
                for branch in &branch_options {
                    combined.push(format!("{prefix}{branch}"));
                }
            }
            variants = combined;
            rest = remainder;
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
            // and no template engine exists until Task 4. Expand every
            // `{{ ... }}` construct into the set of residual selectors that
            // could actually be rendered at runtime and require every one
            // of them to compile — not just one branch of an `if`/`else`,
            // since the branch never taken would otherwise never be
            // checked at all.
            if sel.contains("{{") {
                let residuals = strip_template_spans(&sel);
                if residuals
                    .iter()
                    .any(|residual| oxidarr_cardigann::selector::compile(residual).is_err())
                {
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

#[cfg(test)]
mod template_span_tests {
    #![allow(clippy::unwrap_used)]
    use super::strip_template_spans;

    #[test]
    fn plain_text_has_a_single_residual() {
        assert_eq!(strip_template_spans("tr > td"), vec!["tr > td"]);
    }

    #[test]
    fn bare_action_contributes_nothing() {
        assert_eq!(
            strip_template_spans(r"tr:contains({{ .Config.uploader }})"),
            vec!["tr:contains()"]
        );
    }

    #[test]
    fn if_with_no_else_produces_the_branch_and_the_empty_alternative() {
        // `{{ if C }}A{{ end }}` renders to `A` when C is truthy and to
        // nothing when it isn't — both are reachable.
        let mut residuals = strip_template_spans("td{{ if .Config.x }}.a{{ end }}");
        residuals.sort();
        let mut expected = vec!["td".to_string(), "td.a".to_string()];
        expected.sort();
        assert_eq!(residuals, expected);
    }

    #[test]
    fn if_else_produces_both_branches_not_their_concatenation() {
        // This is the regression case for the finding that the original
        // stripper discarded the `else` branch entirely: it must appear as
        // its own residual, and neither residual may contain both `A` and
        // `B` glued together (that string was never actually reachable at
        // render time).
        let residuals = strip_template_spans(
            "{{ if .Result._staff_edit }}td:nth-child(5) > span[title]{{ else }}td:nth-child(4) > span[title]{{ end }}",
        );
        assert!(residuals.contains(&"td:nth-child(5) > span[title]".to_string()));
        assert!(residuals.contains(&"td:nth-child(4) > span[title]".to_string()));
        assert_eq!(residuals.len(), 2);
        for residual in &residuals {
            assert!(!residual.contains("span[title]td"));
        }
    }

    #[test]
    fn if_else_inside_attribute_value_produces_both_branches() {
        let residuals = strip_template_spans(
            r#"td:has(a[{{ if .Result._anchor1 }}href^="comment.php"{{ else }}href$="startcomments"{{ end }}])"#,
        );
        assert!(residuals.contains(&r#"td:has(a[href^="comment.php"])"#.to_string()));
        assert!(residuals.contains(&r#"td:has(a[href$="startcomments"])"#.to_string()));
        assert_eq!(residuals.len(), 2);
    }

    #[test]
    fn multiple_constructs_combine() {
        let residuals = strip_template_spans(
            "{{ if .A }}x{{ else }}y{{ end }}-{{ if .B }}1{{ else }}2{{ end }}",
        );
        let mut residuals = residuals;
        residuals.sort();
        let mut expected = vec!["x-1", "x-2", "y-1", "y-2"];
        expected.sort_unstable();
        assert_eq!(residuals, expected);
    }

    #[test]
    fn unterminated_if_drops_to_end_of_string_without_looping() {
        // No matching `{{ end }}`: nothing panics or hangs. Whatever was
        // accumulated inside the dangling `if` is still one option (`tr.a`);
        // since there's no `else` either, the block-renders-to-nothing
        // alternative (`tr`) is included too.
        let mut residuals = strip_template_spans("tr{{ if .Config.x }}.a");
        residuals.sort();
        let mut expected = vec!["tr".to_string(), "tr.a".to_string()];
        expected.sort();
        assert_eq!(residuals, expected);
    }

    #[test]
    fn unterminated_span_drops_to_end_of_string_without_looping() {
        let residuals = strip_template_spans("tr{{ .Config.x");
        assert_eq!(residuals, vec!["tr"]);
    }
}
