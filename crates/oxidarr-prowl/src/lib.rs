//! Prowlarr-compatible indexer manager, in Rust
//!
//! [`torznab`] renders pure Torznab/Newznab XML — capability negotiation
//! (`t=caps`), search results, and error responses — from a Cardigann
//! [`Definition`](oxidarr_cardigann::model::Definition)'s declared
//! capabilities and [`oxidarr_core::Release`] values. It performs no I/O.
//!
//! [`definitions`] caches parsed Cardigann definitions, read-through, behind
//! [`DefinitionStore`].
//!
//! [`server`] wires that rendering, plus [`oxidarr_db`], [`oxidarr_indexer`],
//! and [`DefinitionStore`], into the actual `GET /{indexer_id}/api` Torznab
//! endpoint — see its module docs for authentication and error-code
//! conventions.
//!
//! [`api`] mounts the Prowlarr-compatible `/api/v1` control-plane endpoints
//! behind [`oxidarr_http::auth::require_api_key`]. [`server::app`] merges it
//! with the Torznab router into the one application this crate serves.

pub mod api;
pub mod definitions;
pub mod server;
pub mod torznab;

pub use api::api_router;
pub use definitions::{DefinitionError, DefinitionStore};
pub use server::{AppState, app, router};
pub use torznab::{render_caps, render_error, render_results};
