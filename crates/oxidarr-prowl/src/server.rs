//! The Torznab HTTP router: `GET /{indexer_id}/api`.
//!
//! Wires everything the rest of this workspace already built into one
//! endpoint: [`oxidarr_db`] for the indexer row and the instance API key,
//! [`oxidarr_indexer`] to actually run a search (a Cardigann definition
//! through [`CardigannIndexer`], or a bare Newznab/Torznab base URL through
//! [`NewznabIndexer`]/[`TorznabIndexer`]), and [`crate::torznab`] to render
//! the result as Torznab XML.
//!
//! # Authentication
//!
//! Every request (including `t=caps`) must present the instance's API key as
//! an `apikey` query parameter, checked with
//! [`oxidarr_http::auth::constant_time_eq`] against
//! [`oxidarr_db::ConfigRepo::api_key`] — looked up fresh on every request
//! rather than cached on [`AppState`], since memoizing it is a later
//! milestone's concern, not this task's.
//!
//! Real Prowlarr's own `NewznabController` never checks the API key itself:
//! authentication happens one layer up, in ASP.NET's
//! `ApiKeyAuthenticationHandler`, whose `HandleChallengeAsync` sets the
//! response status to a bare `401` with no body at all (verified by reading
//! `src/Prowlarr.Http/Authentication/ApiKeyAuthenticationHandler.cs` and
//! `src/Prowlarr.Api.V1/Indexers/NewznabController.cs` at commit
//! `693c7c3b0e8ec6e9dd792c01e5fa1091260b3be1`) — not the `200` the original,
//! never-authoritatively-XSD'd Newznab/Torznab spec text implies every error
//! (including auth) rides. This router follows verified real Prowlarr for
//! this one case: a bad/missing `apikey` gets a `401`, not a `200`. It still
//! carries a `render_error(100, ...)` XML body (real Prowlarr's is empty) —
//! there is no reason to withhold a description a client might find useful,
//! and Sonarr's own Torznab client does not depend on the body being empty
//! specifically on a `401`.
//!
//! Every other error path in this router — unknown/disabled indexer
//! (`201`), a missing `t` (`200`), an unsupported `t` (`202`), a
//! search/definition failure (`300`) — was not singled out for this same
//! verification, so each keeps the traditional Newznab/Torznab convention
//! (200 missing parameter, 202 no such function, per the original spec
//! text — not the `203` an earlier version of this router shipped) of
//! riding a bare HTTP `200`: the error is self-describing in the XML body,
//! which is what the original spec text (and every Cardigann-era Torznab
//! client that predates Prowlarr's own ASP.NET-specific status-code
//! choices) assumes a client checks first, rather than the HTTP status.
//!
//! # Definition loading
//!
//! [`AppState::defs`] is a [`DefinitionStore`]: every `t=caps` and every
//! Cardigann-kind search fetches its definition through
//! [`DefinitionStore::get`], which parses `<definition_id>.yml` only once
//! per id and caches the result — see that type's own docs for exact cache
//! semantics (including why `t=caps`'s definition-listing sibling,
//! `list_ids`, deliberately does not share that cache).
//!
//! # Newznab/Torznab row settings
//!
//! An [`IndexerKind::Newznab`]/[`IndexerKind::Torznab`] row's `settings` JSON
//! map is read for two keys, by convention: `"baseUrl"` (required — a
//! missing or unparseable value is a `300` error) and `"apiKey"` (optional,
//! forwarded as the indexer's own upstream API key, unrelated to this
//! instance's own `apikey` query parameter checked above).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as PathParam, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::Utc;
use oxidarr_cardigann::catmap::CategoryMap;
use oxidarr_core::Release;
use oxidarr_core::ids::IndexerId;
use oxidarr_db::{ConfigRepo, Db, DbError, IndexerKind, IndexerRepo, IndexerRow};
use oxidarr_http::auth::{ApiKey, constant_time_eq};
use oxidarr_indexer::{
    CardigannIndexer, HttpClient, Indexer, NewznabIndexer, SearchQuery, Settings, TorznabIndexer,
};
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::api::api_router;
use crate::definitions::DefinitionStore;
use crate::torznab::{render_caps, render_error, render_results};

/// Shared state behind the Torznab router.
///
/// Generic over `C` (the HTTP client every indexer search executes through)
/// so tests can inject [`oxidarr_indexer::testing::FakeClient`] in place of
/// the real [`oxidarr_indexer::ReqwestClient`]. `db` is `Arc`-wrapped since
/// [`Db`] itself is not `Clone` and this state is cloned once per request by
/// axum's [`State`] extractor.
pub struct AppState<C> {
    /// The database every request looks up its indexer row and the
    /// instance API key in.
    pub db: Arc<Db>,
    /// The HTTP client indexer searches execute through.
    pub client: C,
    /// The cached Cardigann definition store every `t=caps` and
    /// Cardigann-kind search reads its definition through. See the module
    /// docs' "Definition loading" section.
    pub defs: DefinitionStore,
}

impl<C: Clone> Clone for AppState<C> {
    fn clone(&self) -> Self {
        Self {
            db: Arc::clone(&self.db),
            client: self.client.clone(),
            defs: self.defs.clone(),
        }
    }
}

/// Hand-written rather than derived so `AppState<C>` does not require
/// `C: Debug` — the client type (a real transport, or a test double) has no
/// business needing one just to satisfy this workspace's
/// `missing_debug_implementations` lint on the struct that holds it.
impl<C> fmt::Debug for AppState<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppState")
            .field("defs", &self.defs)
            .finish_non_exhaustive()
    }
}

/// Raw query parameters `GET /{indexer_id}/api` accepts, before any
/// per-`t` interpretation. Every field is a bare `Option<String>` (never a
/// typed `Option<u32>`) so a malformed value (e.g. `season=abc`) degrades to
/// "absent" rather than making axum's `Query` extractor itself reject the
/// request outside this router's own error-rendering path.
#[derive(Debug, Deserialize)]
struct ApiParams {
    #[serde(default)]
    apikey: Option<String>,
    #[serde(default)]
    t: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    imdbid: Option<String>,
    #[serde(default)]
    season: Option<String>,
    #[serde(default)]
    ep: Option<String>,
    #[serde(default)]
    cat: Option<String>,
}

/// Builds the Torznab router: a single `GET /{indexer_id}/api` route bound
/// to `state`.
pub fn router<C>(state: AppState<C>) -> Router
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/{indexer_id}/api", get(handle_api::<C>))
        .with_state(state)
}

/// Builds the whole application: this crate's Torznab router (`GET
/// /{indexer_id}/api`, above) merged with the auth-wrapped `/api/v1` router
/// ([`crate::api::api_router`]) — see that module's own docs for the
/// deliberate asymmetry in how each side reads the instance API key.
///
/// Reads [`ConfigRepo::api_key`] once, at call time, to build the
/// `/api/v1` auth layer's [`ApiKey`] state, and captures `Utc::now()` here
/// as the `/api/v1/system/status` `startTime` every response after this
/// call reports (see `crate::api::system::router`'s own `start_time`
/// parameter for why it's captured once at router-build time rather than
/// read fresh per request).
///
/// # Errors
///
/// Returns whatever [`ConfigRepo::api_key`] itself can fail with — a
/// database unavailable at startup.
pub async fn app<C>(state: AppState<C>) -> Result<Router, DbError>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let key = ConfigRepo::new(&state.db).api_key().await?;
    let v1 = api_router(state.clone(), ApiKey::new(key), Utc::now());
    Ok(router(state).merge(v1))
}

async fn handle_api<C>(
    State(state): State<AppState<C>>,
    PathParam(indexer_id): PathParam<i32>,
    Query(params): Query<ApiParams>,
) -> Response
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let configured_key = match ConfigRepo::new(&state.db).api_key().await {
        Ok(key) => key,
        Err(err) => return internal_error_response(&err),
    };

    let provided = params.apikey.as_deref().unwrap_or_default();
    if !constant_time_eq(provided.as_bytes(), configured_key.as_bytes()) {
        // See the module docs' "Authentication" section for why this is a
        // `401`, not the `200` the rest of this router's errors use.
        return xml_response(
            StatusCode::UNAUTHORIZED,
            render_error(100, "Incorrect user credentials"),
        );
    }

    let row = match IndexerRepo::new(&state.db).get(IndexerId(indexer_id)).await {
        Ok(row) => row,
        Err(DbError::NotFound { .. }) => {
            return xml_response(
                StatusCode::OK,
                render_error(201, "Incorrect parameter (indexer id)"),
            );
        }
        Err(err) => return internal_error_response(&err),
    };
    if !row.enabled {
        return xml_response(
            StatusCode::OK,
            render_error(201, "Incorrect parameter (indexer disabled)"),
        );
    }

    // `cat` (`SearchQuery::categories`) is read the same way for every mode
    // below, even though real Torznab clients only ever pair it with a
    // `t=search` request in practice — a genuine category-filtered
    // `tvsearch`/`movie` request is legal Torznab, so there is no reason to
    // special-case it away here.
    match params.t.as_deref() {
        Some("caps") => caps_response(&state.defs, &row.definition_id).await,
        Some("search") => {
            let q = SearchQuery {
                q: params.q.clone(),
                categories: parse_categories(params.cat.as_deref()),
                ..SearchQuery::default()
            };
            search_response(&row, &state.defs, state.client.clone(), &q).await
        }
        Some("tvsearch") => {
            let q = SearchQuery {
                q: params.q.clone(),
                season: params.season.as_deref().and_then(|s| s.parse().ok()),
                episode: params.ep.as_deref().and_then(|s| s.parse().ok()),
                categories: parse_categories(params.cat.as_deref()),
                ..SearchQuery::default()
            };
            search_response(&row, &state.defs, state.client.clone(), &q).await
        }
        Some("movie") => {
            let q = SearchQuery {
                q: params.q.clone(),
                imdb_id: params.imdbid.clone(),
                categories: parse_categories(params.cat.as_deref()),
                ..SearchQuery::default()
            };
            search_response(&row, &state.defs, state.client.clone(), &q).await
        }
        Some(other) => xml_response(StatusCode::OK, render_error(202, &no_such_function(other))),
        None => xml_response(StatusCode::OK, render_error(200, "Missing parameter (t)")),
    }
}

fn no_such_function(t: &str) -> String {
    format!("No such function ({t})")
}

/// Renders `err` as a bare `500` — reachable only if the database itself is
/// unavailable (a lookup returning [`DbError::NotFound`] is handled by its
/// own, specific call site instead), which no test in this router exercises
/// on purpose; kept for the same reason [`crate::torznab::render`] keeps its
/// own unreachable-in-practice fallback rather than unwrapping.
fn internal_error_response(err: &DbError) -> Response {
    xml_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        render_error(900, &err.to_string()),
    )
}

/// Renders `body` with the fixed `application/xml; charset=utf-8`
/// content type every response from this router carries.
fn xml_response(status: StatusCode, body: String) -> Response {
    (
        status,
        [("content-type", "application/xml; charset=utf-8")],
        body,
    )
        .into_response()
}

/// Parses a Torznab `cat` query value (comma-separated Newznab category
/// ids) into a `Vec<u32>`. Any entry that does not parse as a plain
/// non-negative integer is dropped rather than failing the whole request —
/// mirroring how a missing/absent `cat` becomes an empty list rather than an
/// error.
fn parse_categories(raw: Option<&str>) -> Vec<u32> {
    raw.unwrap_or_default()
        .split(',')
        .filter_map(|part| part.trim().parse().ok())
        .collect()
}

/// Converts an indexer row's JSON `settings` map into the string-valued map
/// [`Settings::new`] expects. A [`Value::String`] is used verbatim; any
/// other JSON type falls back to its compact JSON text — every real
/// Cardigann `settings:` value is declared/stored as a string, so this
/// fallback only matters for a value some caller stored non-canonically.
fn settings_from_json(settings: &serde_json::Map<String, Value>) -> BTreeMap<String, String> {
    settings
        .iter()
        .map(|(key, value)| {
            let value = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (key.clone(), value)
        })
        .collect()
}

/// Renders `t=caps`: loads `row.definition_id`'s definition (through
/// [`DefinitionStore::get`]) and its [`CategoryMap`], then [`render_caps`]s
/// them. A definition load failure renders as a `300` error with the
/// failure's own [`Display`](fmt::Display) text.
async fn caps_response(defs: &DefinitionStore, definition_id: &str) -> Response {
    match defs.get(definition_id).await {
        Ok(def) => {
            let map = CategoryMap::from_definition(&def);
            xml_response(StatusCode::OK, render_caps(&def, &map))
        }
        Err(err) => xml_response(StatusCode::OK, render_error(300, &err.to_string())),
    }
}

/// Runs `q` against `row`, dispatching on [`IndexerRow::kind`], and renders
/// the outcome: [`render_results`] on success, or a `300` error carrying the
/// failure's own [`Display`](fmt::Display) text on any failure — a
/// definition load failure, a missing/unparseable `baseUrl` setting, or the
/// search itself failing.
async fn search_response<C>(
    row: &IndexerRow,
    defs: &DefinitionStore,
    client: C,
    q: &SearchQuery,
) -> Response
where
    C: HttpClient,
{
    match search_row(row, defs, client, q).await {
        Ok(releases) => xml_response(StatusCode::OK, render_results(&releases, &row.name)),
        Err(message) => xml_response(StatusCode::OK, render_error(300, &message)),
    }
}

/// Executes `q` against `row`, building the concrete [`Indexer`] its
/// [`IndexerKind`] calls for. Every failure — definition load, a missing or
/// unparseable `baseUrl` row setting, or the search call itself — is
/// collapsed to its `Display` text as a plain `String`, since every one of
/// them renders through the same `300` error path in [`search_response`].
async fn search_row<C>(
    row: &IndexerRow,
    defs: &DefinitionStore,
    client: C,
    q: &SearchQuery,
) -> Result<Vec<Release>, String>
where
    C: HttpClient,
{
    match row.kind {
        IndexerKind::Cardigann => {
            let def = defs
                .get(&row.definition_id)
                .await
                .map_err(|err| err.to_string())?;
            let settings = Settings::new(settings_from_json(&row.settings));
            // `CardigannIndexer::new` takes an owned `Definition`; `def` is
            // the store's cached `Arc`, so this clones out of it rather
            // than re-reading/re-parsing the file.
            CardigannIndexer::new((*def).clone(), settings, client)
                .search(q)
                .await
                .map_err(|err| err.to_string())
        }
        IndexerKind::Newznab | IndexerKind::Torznab => {
            let base = row
                .settings
                .get("baseUrl")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing baseUrl setting".to_string())?;
            let base: Url = base
                .parse()
                .map_err(|err: url::ParseError| format!("invalid baseUrl {base:?}: {err}"))?;
            let apikey = row
                .settings
                .get("apiKey")
                .and_then(Value::as_str)
                .map(str::to_string);
            match row.kind {
                IndexerKind::Newznab => NewznabIndexer::new(base, apikey, client)
                    .search(q)
                    .await
                    .map_err(|err| err.to_string()),
                _ => TorznabIndexer::new(base, apikey, client)
                    .search(q)
                    .await
                    .map_err(|err| err.to_string()),
            }
        }
    }
}
