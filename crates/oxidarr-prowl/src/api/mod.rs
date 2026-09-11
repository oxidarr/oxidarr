//! `/api/v1` scaffolding: mounts every Prowlarr-compatible control-plane
//! endpoint behind [`oxidarr_http::auth::require_api_key`].
//!
//! # Mounting
//!
//! [`api_router`] builds the whole `/api/v1` tree and wraps it in the auth
//! middleware in one call. The instance API key is read once by the
//! caller, via [`oxidarr_db::ConfigRepo::api_key`] — see [`crate::server::app`]
//! for the composition that does this at startup. This is a deliberate
//! asymmetry with the sibling Torznab router ([`crate::server::router`]),
//! which re-reads the key from the database on every request (see that
//! router's own module docs): a key rotated after startup takes effect
//! immediately for Torznab clients but requires a restart to take effect
//! here. Acceptable for v1 — real Prowlarr does the same: its API key comes
//! from `config.xml`, read into `IConfigFileProvider` once at process
//! start, not re-read per request.
//!
//! # Modules
//!
//! [`system`] — `GET /api/v1/system/status` and `GET /api/v1/health`; reads
//! no state at all.
//!
//! [`indexer_schema`] — `GET /api/v1/indexer/schema`, the first module here
//! that reads `state`: it needs [`AppState::defs`] to list and load
//! Cardigann definitions.
//!
//! [`indexers`] — indexer CRUD (`GET`/`POST`/`PUT`/`DELETE /indexer[/{id}]`)
//! plus `POST /indexer/test`, the first module here that reads every part of
//! `state` (`db`, `defs`, and `client`). Later tasks in this plan add
//! further sibling modules (application CRUD, sync) alongside it.
//!
//! [`dto`] — Prowlarr-shaped response DTOs shared by more than one endpoint
//! here; [`indexer_schema`] and [`indexers`] both build on it.

pub mod dto;
pub mod indexer_schema;
pub mod indexers;
pub mod system;

use axum::Router;
use axum::middleware;
use chrono::{DateTime, Utc};
use oxidarr_http::auth::{ApiKey, require_api_key};
use oxidarr_indexer::HttpClient;

use crate::server::AppState;

/// Builds the whole `/api/v1` router tree, wrapped in the API-key auth
/// middleware described in the module docs.
///
/// `start_time` is a plain parameter rather than a clock read internally,
/// so a test can pin it to an exact value; see [`crate::server::app`] for
/// where a real caller supplies `Utc::now()`.
///
/// The `C: HttpClient` bound (absent before [`indexers`] landed) is now
/// required here because [`indexers::router`]'s `POST /indexer/test` route
/// executes a real search through `state.client`.
pub fn api_router<C>(state: AppState<C>, key: ApiKey, start_time: DateTime<Utc>) -> Router
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let v1 = system::router(start_time)
        .merge(indexer_schema::router(state.defs.clone()))
        .merge(indexers::router(state))
        .layer(middleware::from_fn_with_state(key, require_api_key));
    Router::new().nest("/api/v1", v1)
}
