//! Prowlarr-compatible indexer manager, in Rust
//!
//! Application layer: Prowlarr-shaped `/api/v1` routes, indexer configuration,
//! app-sync to downstream *arr instances, search proxying, and scheduling.
//! Intentionally thin — the work lives in `oxidarr-cardigann`, `oxidarr-indexer`,
//! `oxidarr-http`, and `oxidarr-db`.

/// Placeholder export. Scaffolding only — no business logic yet.
pub const CRATE_NAME: &str = "oxidarr-prowl";
