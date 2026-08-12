//! Cardigann v11 indexer definition engine
//!
//! Parses and executes Prowlarr-compatible Cardigann YAML definitions (schema v11).
//! The network client is injected, so the engine is testable against fixtures.

pub mod error;
pub mod selector;

pub use error::CardigannError;
