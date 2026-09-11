//! Prowlarr-compatible indexer manager, in Rust
//!
//! [`torznab`] renders pure Torznab/Newznab XML — capability negotiation
//! (`t=caps`), search results, and error responses — from a Cardigann
//! [`Definition`](oxidarr_cardigann::model::Definition)'s declared
//! capabilities and [`oxidarr_core::Release`] values. It performs no I/O and
//! wires up no `axum` router; the HTTP server behind `/api/v1` lands in a
//! later task.

pub mod torznab;

pub use torznab::{render_caps, render_error, render_results};
