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
//! [`definitions_sync`] fetches and refreshes the third-party Cardigann
//! definition corpus [`definitions`] reads, at runtime and in the
//! background: it downloads the archive, extracts it into a staging
//! directory, and swaps it into place with an atomic `rename` — see its own
//! module docs for why the definitions are never committed and how that
//! swap is made safe against a crash mid-update.
//!
//! [`server`] wires that rendering, plus [`oxidarr_db`], [`oxidarr_indexer`],
//! and [`DefinitionStore`], into the actual `GET /{indexer_id}/api` Torznab
//! endpoint — see its module docs for authentication and error-code
//! conventions.
//!
//! [`api`] mounts the Prowlarr-compatible `/api/v1` control-plane endpoints
//! behind [`oxidarr_http::auth::require_api_key`]. [`server::app`] merges it
//! with the Torznab router into the one application this crate serves.
//!
//! [`sync`] is the app-sync engine: it pushes this instance's enabled
//! indexers into a configured Sonarr/Radarr application as Torznab
//! indexers, keyed by [`oxidarr_db::MappingRepo`]'s ownership record rather
//! than by name-matching. [`api::indexers`] and [`api::applications`]'s
//! create/update (and, for indexers, delete) handlers trigger it inline,
//! best-effort, after every write — see [`sync`]'s own module docs for the
//! full contract and its documented divergence from real Prowlarr's
//! asynchronous background sync.
//!
//! [`config`] is the `oxidarr-prowl` binary's own instance configuration —
//! a TOML file plus `OXIDARR_*` environment overrides — consumed by
//! `src/main.rs` to build the [`AppState`] this crate's [`app`] serves.
//!
//! [`ui`] (behind the `ui` cargo feature, off by default) embeds and serves
//! the `oxidarr-ui` web bundle; see that module's own docs for how and why
//! it can never shadow the [`api`]/[`torznab`] routes above.
//!
//! [`health`] is a single unauthenticated `GET /ping`, merged into
//! [`server::app`] beside (not inside) the auth-wrapped `/api/v1` router so
//! the container's own credential-less `HEALTHCHECK` can reach it.

pub mod api;
pub mod config;
pub mod definitions;
pub mod definitions_sync;
pub mod health;
pub mod server;
pub mod sync;
pub mod torznab;
#[cfg(feature = "ui")]
pub mod ui;

pub use api::api_router;
pub use config::{Config, ConfigError, load_config};
pub use definitions::{DefinitionError, DefinitionStore};
pub use server::{AppState, app, router};
pub use sync::{SyncError, SyncReport, sync_all, sync_application};
pub use torznab::{render_caps, render_error, render_results};
