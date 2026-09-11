//! Prowlarr-compatible indexer manager, in Rust
//!
//! [`torznab`] renders pure Torznab/Newznab XML — capability negotiation
//! (`t=caps`), search results, and error responses — from a Cardigann
//! [`Definition`](oxidarr_cardigann::model::Definition)'s declared
//! capabilities and [`oxidarr_core::Release`] values. It performs no I/O.
//!
//! [`server`] wires that rendering, plus [`oxidarr_db`] and
//! [`oxidarr_indexer`], into the actual `GET /{indexer_id}/api` Torznab
//! endpoint — see its module docs for authentication, error-code, and
//! definition-loading conventions.

pub mod server;
pub mod torznab;

pub use server::{AppState, router};
pub use torznab::{render_caps, render_error, render_results};
