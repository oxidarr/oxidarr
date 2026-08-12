//! Errors produced by the Cardigann engine.

/// Failures arising while compiling or executing a Cardigann definition.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CardigannError {
    /// A selector could not be parsed even after rewriting.
    #[error("selector {selector:?} is not valid CSS after rewriting: {reason}")]
    Selector { selector: String, reason: String },

    /// A `:contains(...)` clause was opened but never closed.
    #[error("selector {selector:?} has an unbalanced :contains(...) clause")]
    UnbalancedContains { selector: String },

    /// A template could not be parsed.
    #[error("template {template:?}: {reason}")]
    Template { template: String, reason: String },
}
