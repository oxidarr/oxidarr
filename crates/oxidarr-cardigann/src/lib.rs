//! Cardigann v11 indexer definition engine
//!
//! Parses and executes Prowlarr-compatible Cardigann YAML definitions (schema v11).
//! The network client is injected, so the engine is testable against fixtures.

pub mod engine;
pub mod error;
pub mod filters;
pub mod model;
pub mod selector;
pub mod template;

pub use error::CardigannError;

use crate::model::Definition;

/// Statically checks a definition: every selector compiles as CSS, every
/// filter is implemented, and every template parses. Performs no I/O.
///
/// # Errors
/// Returns the first problem found.
pub fn validate(def: &Definition) -> Result<(), CardigannError> {
    check_selector(&def.search.rows.selector)?;

    // Prowlarr picks its HTML or JSON extraction path from the DECLARED
    // response type, not from the shape of a selector — a JSON definition's
    // row selector is a bare field name like `data`, which is
    // indistinguishable from a CSS tag selector. 101 of the 543 definitions
    // declare `response: {type: json}`; in those, `case:` keys are compared
    // to the extracted value for equality (CardigannBase.cs:
    // `value.Equals(case.Key)`) and are literals such as `0%` or `True`,
    // so validating them as CSS is wrong.
    let html_mode = !def
        .search
        .paths
        .iter()
        .any(|p| p.response.as_ref().is_some_and(|r| r.kind == "json"));

    for spec in &def.search.rows.filters {
        filters::parse_filter(&spec.name, &spec.args.to_vec())?;
    }

    for path in &def.search.paths {
        if path.path.contains("{{") {
            template::render(&path.path, &template::Scope::new())?;
        }
    }

    for (_name, field) in &def.search.fields {
        if let Some(sel) = &field.selector {
            check_selector(sel)?;
        }
        if let Some(remove) = &field.remove {
            check_selector(remove)?;
        }
        // `case:` keys are only CSS selectors for HTML definitions. When the
        // search response is JSON, Prowlarr compares the key to the extracted
        // value for equality instead (CardigannBase.cs: `value.Equals(case.Key)`),
        // so keys there are literals like `0%` or `True` and must not be
        // validated as CSS.
        if let Some(case) = &field.case
            && html_mode
        {
            for (key, _) in &case.0 {
                // `'*'` is the catch-all, not a selector.
                if key != "*" {
                    check_selector(key)?;
                }
            }
        }
        if let Some(text) = &field.text {
            template::render(text, &template::Scope::new())?;
        }
        for spec in &field.filters {
            filters::parse_filter(&spec.name, &spec.args.to_vec())?;
        }
    }

    Ok(())
}

/// Checks one selector compiles, and reaches selectors no earlier gate did.
///
/// `case:` keys and `remove:` selectors are real selectors that nothing before
/// this validated. Whether a selector is CSS or a JSON field path is decided by
/// [`selector::compile`] and guarded by its own regression tests: a bare
/// `tag.class` was once misclassified as a JSON path and silently matched
/// nothing, across 622 selectors in 225 of the 543 definitions. JSON field
/// paths themselves are legitimate — definitions whose search response is JSON
/// use them — so they are accepted here rather than rejected.
fn check_selector(raw: &str) -> Result<(), CardigannError> {
    // Templated selectors are resolved at request time, not here.
    if raw.contains("{{") {
        return Ok(());
    }
    selector::compile(raw)?;
    Ok(())
}
