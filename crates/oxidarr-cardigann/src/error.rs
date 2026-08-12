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

    /// A definition referenced a filter this engine does not implement.
    #[error("unknown filter {name:?}")]
    UnknownFilter { name: String },

    /// A filter was called with arguments it cannot use.
    #[error("filter {name:?}: {reason}")]
    Filter { name: String, reason: String },
}
