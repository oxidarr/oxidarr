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
//! [`system`] — `GET /api/v1/system/status` and `GET /api/v1/health`.
//! Later tasks in this plan add sibling modules here (indexer/application
//! CRUD, sync) that do read `state`; `system` itself needs none of it.

pub mod system;

use axum::Router;
use axum::middleware;
use chrono::{DateTime, Utc};
use oxidarr_http::auth::{ApiKey, require_api_key};

use crate::server::AppState;

/// Builds the whole `/api/v1` router tree, wrapped in the API-key auth
/// middleware described in the module docs.
///
/// `state` is accepted (and threaded through, generic over `C`) for the
/// sibling modules future tasks add here — [`system`]'s own routes read
/// none of it. `start_time` is a plain parameter rather than a clock read
/// internally, so a test can pin it to an exact value; see
/// [`crate::server::app`] for where a real caller supplies `Utc::now()`.
// `state` is unused today — clippy would rather see `&AppState<C>` — but
// every sibling module a later task adds here (`indexer`, `application`,
// `sync`, ...) is going to want to move it into its own `.with_state(...)`
// call, the same way `crate::server::router` already does. Taking it by
// value now, even though this task's own body just drops it, matches the
// interface those modules need rather than one this task would have to
// widen again immediately after.
#[allow(clippy::needless_pass_by_value)]
pub fn api_router<C>(state: AppState<C>, key: ApiKey, start_time: DateTime<Utc>) -> Router {
    let _ = state;
    let v1 = system::router(start_time).layer(middleware::from_fn_with_state(key, require_api_key));
    Router::new().nest("/api/v1", v1)
}
