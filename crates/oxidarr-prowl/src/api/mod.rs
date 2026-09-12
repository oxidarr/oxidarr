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
//! `state` (`db`, `defs`, and both clients). Create/update/delete each
//! trigger [`sync_all_and_describe_failures`] afterward.
//!
//! [`applications`] — application CRUD (`GET`/`POST`/`PUT`/`DELETE
//! /applications[/{id}]`) plus `POST /applications/test`, [`indexers`]'s
//! sibling: same shape, `ApplicationRepo` in place of `IndexerRepo`.
//! Create/update trigger [`sync_one_and_describe_failure`] for the one
//! application just written; [`crate::sync`] is the engine both call into.
//!
//! [`search`] — `GET /api/v1/search`, the multi-indexer search endpoint: the
//! first module here that fans a single request out across more than one
//! indexer concurrently, reusing [`crate::server::search_row`]'s
//! row-to-[`oxidarr_indexer::Indexer`] construction rather than
//! reimplementing it a second time.
//!
//! [`dto`] — Prowlarr-shaped response DTOs shared by more than one endpoint
//! here; [`indexer_schema`], [`indexers`], [`applications`], and [`search`]
//! all build on it.
//!
//! # Shared error mapping
//!
//! [`db_error_to_problem`] and [`bad_request`] live here (not in either CRUD
//! module) because both [`indexers`] and [`applications`] need the exact
//! same [`DbError`]/`400`-message-carrying [`Problem`] mapping — extracted
//! once [`applications`] made it a second call site, rather than duplicated.
//!
//! # Shared sync trigger
//!
//! [`sync_all_and_describe_failures`] and [`sync_one_and_describe_failure`]
//! live here for the same reason: both [`indexers`]' and [`applications`]'
//! create/update (and, for [`indexers`], delete) handlers need to run
//! [`crate::sync`] best-effort and fold whatever it returns into a single
//! `Option<String>` for the `syncError` response field — see
//! [`crate::api::dto::IndexerResource::sync_error`]'s own doc comment for
//! that field's contract.

pub mod applications;
pub mod dto;
pub mod indexer_schema;
pub mod indexers;
pub mod search;
pub mod system;

use axum::Router;
use axum::http::StatusCode;
use axum::middleware;
use chrono::{DateTime, Utc};
use oxidarr_db::{ApplicationRow, ConfigRepo, DbError};
use oxidarr_http::Problem;
use oxidarr_http::auth::{ApiKey, require_api_key};
use oxidarr_indexer::HttpClient;

use crate::server::AppState;
use crate::sync;

/// Builds the whole `/api/v1` router tree, wrapped in the API-key auth
/// middleware described in the module docs.
///
/// `start_time` is a plain parameter rather than a clock read internally,
/// so a test can pin it to an exact value; see [`crate::server::app`] for
/// where a real caller supplies `Utc::now()`.
///
/// The `C: HttpClient` bound (absent before [`indexers`] landed) is now
/// required here because [`indexers::router`]'s `POST /indexer/test` route
/// executes a real request through `state.tracker_client`,
/// [`applications::router`]'s `POST /applications/test` route through
/// `state.app_client`, and both CRUD routers' create/update (and, for
/// indexers, delete) handlers trigger [`crate::sync`] through
/// `state.app_client` — see [`AppState`]'s own "Two client fields" section.
pub fn api_router<C>(state: AppState<C>, key: ApiKey, start_time: DateTime<Utc>) -> Router
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let v1 = system::router(start_time)
        .merge(indexer_schema::router(state.defs.clone()))
        .merge(indexers::router(state.clone()))
        .merge(applications::router(state.clone()))
        .merge(search::router(state))
        .layer(middleware::from_fn_with_state(key, require_api_key));
    Router::new().nest("/api/v1", v1)
}

/// Maps a [`DbError`] onto a [`Problem`]: [`DbError::NotFound`] (whose
/// `Display` already names the id, e.g. `"indexer not found: 7"`) becomes a
/// `404`; every other variant (a genuine database/encoding failure) becomes
/// a `500`, still carrying the error's own message.
// Every call site passes this to `.map_err(db_error_to_problem)`, which
// hands `map_err`'s closure the error by value — taking `&DbError` instead
// (clippy's own suggestion) would force every one of those call sites into a
// wrapping closure just to re-borrow, for no actual benefit: nothing here
// needs ownership, but nothing is freed by giving it up either.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn db_error_to_problem(err: DbError) -> Problem {
    let status = match err {
        DbError::NotFound { .. } => StatusCode::NOT_FOUND,
        DbError::Sqlx(_) | DbError::Migrate(_) | DbError::Corrupt { .. } => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    Problem::new(status, err.to_string())
}

/// Builds a `400` [`Problem`] carrying `message`.
pub(crate) fn bad_request(message: impl Into<String>) -> Problem {
    Problem::new(StatusCode::BAD_REQUEST, message)
}

/// Runs [`crate::sync::sync_all`] against every configured application,
/// using `state`'s own `app_client`/`external_url` and the instance's own
/// API key (read fresh via [`ConfigRepo::api_key`] — the same read every
/// sync performs, not cached on [`AppState`]). Used by
/// [`crate::api::indexers`]'s create/update/delete handlers: an indexer
/// change can affect every configured application, not just one.
///
/// Returns `None` when every application's sync succeeded (including when
/// there are no applications configured at all); otherwise a single
/// human-readable string joining every failed application's own error,
/// prefixed with its id — this crate's `syncError` string, not a
/// structured per-application report (real Prowlarr's own background-sync
/// health model this crate does not reproduce; see [`crate::sync`]'s module
/// docs).
pub(crate) async fn sync_all_and_describe_failures<C>(state: &AppState<C>) -> Option<String>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let instance_key = match ConfigRepo::new(&state.db).api_key().await {
        Ok(key) => key,
        Err(err) => return Some(format!("reading instance API key: {err}")),
    };
    let results = sync::sync_all(
        &state.app_client,
        &state.db,
        &state.defs,
        &state.external_url,
        &instance_key,
    )
    .await;
    describe_failures(results)
}

/// Like [`sync_all_and_describe_failures`], but syncs only `app` via
/// [`crate::sync::sync_application`] — used by
/// [`crate::api::applications`]'s create/update handlers, where only the
/// one application just written needs a fresh push, not every configured
/// one.
pub(crate) async fn sync_one_and_describe_failure<C>(
    state: &AppState<C>,
    app: &ApplicationRow,
) -> Option<String>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let instance_key = match ConfigRepo::new(&state.db).api_key().await {
        Ok(key) => key,
        Err(err) => return Some(format!("reading instance API key: {err}")),
    };
    match sync::sync_application(
        &state.app_client,
        &state.db,
        &state.defs,
        app,
        &state.external_url,
        &instance_key,
    )
    .await
    {
        Ok(_) => None,
        Err(err) => Some(err.to_string()),
    }
}

/// Folds [`crate::sync::sync_all`]'s per-application results into
/// [`sync_all_and_describe_failures`]'s single `Option<String>`.
fn describe_failures(
    results: Vec<(
        oxidarr_core::ids::AppId,
        Result<sync::SyncReport, sync::SyncError>,
    )>,
) -> Option<String> {
    let failures: Vec<String> = results
        .into_iter()
        .filter_map(|(app_id, result)| result.err().map(|err| format!("app {app_id}: {err}")))
        .collect();
    if failures.is_empty() {
        None
    } else {
        Some(failures.join("; "))
    }
}
