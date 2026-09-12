//! `GET /api/v1/search` — the multi-indexer search endpoint. Mounted under
//! `/api/v1` by [`crate::api::api_router`], same as every other module in
//! this directory; this module's own [`router`] does not add that prefix
//! itself.
//!
//! # Prowlarr shapes
//!
//! Read `src/Prowlarr.Api.V1/Search/SearchController.cs` and
//! `src/Prowlarr.Api.V1/Search/SearchResource.cs` from `Prowlarr/Prowlarr`
//! at commit `693c7c3b0e8ec6e9dd792c01e5fa1091260b3be1` (the same commit
//! [`crate::api::dto`]'s other citations use). Real Prowlarr's
//! `SearchController.GetAll` binds `[FromQuery] SearchResource`, whose
//! properties give this endpoint's exact query parameter names: `query`
//! (`string`), `type` (`string`, defaults to `"search"`), `indexerIds`
//! (`List<int>`), `categories` (`List<int>`, defaults empty), plus `limit`/
//! `offset` — the last two are not implemented here (no pagination anywhere
//! in this crate yet; see [`oxidarr_http`]'s own "Known limitations" note),
//! an honest omission rather than a silent no-op.
//!
//! `indexerIds`/`categories` bind from a `List<int>` via ASP.NET Core's
//! default query-string collection model binder, which accepts *both* a
//! repeated key (`indexerIds=1&indexerIds=2`) *and* a single
//! comma-separated value (`indexerIds=1,2`) — and, per the framework's own
//! [array binding
//! docs](https://learn.microsoft.com/aspnet/core/mvc/models/model-binding),
//! any mix of the two. [`parse_search_params`] mirrors this exactly: it
//! reads every occurrence of a repeated key and additionally splits each
//! occurrence's value on `,`, rather than relying on `axum::extract::Query`
//! (built on `serde_urlencoded`, which has no such multi-value convention)
//! — hence this handler takes [`axum::extract::RawQuery`] and parses the raw
//! string itself via [`url::form_urlencoded`].
//!
//! `type` is accepted and stored but otherwise unused: real Prowlarr forwards
//! it into a Newznab `t=` search-type parameter that changes which upstream
//! Newznab/Torznab function gets called
//! (`search`/`tvsearch`/`movie-search`/...). This crate's own
//! [`SearchQuery`] has no such per-type behaviour at this endpoint — every
//! enabled indexer is searched the same way regardless of `type` — so the
//! value is read (to keep the parameter from erroring as unknown) and
//! otherwise ignored. A later task can teach [`build_search_query`] to map
//! `type` onto [`SearchQuery::season`]/[`SearchQuery::episode`]/
//! [`SearchQuery::imdb_id`] the way [`crate::server`]'s Torznab dispatch
//! already does for its own `t=tvsearch`/`t=movie`; nothing here forecloses
//! that.
//!
//! # Indexer selection
//!
//! Every indexer with `enabled = true` is searched, unless `indexerIds` is
//! non-empty, in which case only the named ids are searched (an id that
//! does not exist, or that names a disabled indexer, is silently not
//! searched — matching how an absent/mismatched id is simply not present in
//! the result set, rather than a distinct error). Zero indexers to search
//! (an empty instance, every indexer disabled, or an `indexerIds` list that
//! names none of them) is a `200` with an empty JSON array — there is
//! nothing to fail.
//!
//! # Concurrency
//!
//! Every selected indexer's search runs in its own [`tokio::task::JoinSet`]
//! task, bounded by the number of selected indexers (no separate
//! semaphore/limit — acceptable for v1, since an instance's indexer count is
//! small and operator-controlled, not attacker-controlled). This crate has
//! no dependency on the `futures` crate, so this is built on `tokio`'s own
//! [`tokio::task::JoinSet`] (already a dependency via `oxidarr-indexer`'s
//! `HttpClient` machinery) rather than `futures::future::join_all`. Each
//! task calls [`crate::server::search_row`] — the exact same
//! row-to-[`oxidarr_indexer::Indexer`] construction the Torznab router's own
//! `t=search` dispatch uses (see that function's own doc comment for why it
//! is `pub(crate)`), so this endpoint and the Torznab router can never
//! silently disagree on how a `Cardigann`/`Newznab`/`Torznab` row becomes a
//! running search.
//!
//! # Partial-failure semantics
//!
//! A per-indexer failure (a definition load error, a missing/invalid
//! `baseUrl`, a transport error, a non-2xx response, an unparsable feed)
//! does not fail the whole request: this endpoint's success/failure decision
//! is made once, after every selected indexer has finished, based on how
//! many *indexers* (not releases) succeeded:
//!
//! - At least one indexer succeeded (even with zero releases found) → `200`
//!   with every successful indexer's releases, concatenated.
//! - Every selected indexer failed → `400` [`Problem`] naming each failed
//!   indexer and its own failure message.
//! - Zero indexers were selected at all → `200 []` (see "Indexer selection"
//!   above) — never a `400`, since no indexer ran to fail.
//!
//! Real Prowlarr's own `GetSearchReleases` does not make this same choice:
//! it only ever renders a `400` for a thrown `SearchFailedException`
//! (verified against the same commit) — any other exception is logged and
//! swallowed into an empty `200 []`, i.e. real Prowlarr cannot distinguish
//! "found nothing" from "every indexer errored" in its HTTP response at
//! all. This crate's `400`-on-total-failure behaviour is a deliberate
//! improvement (a caller can tell the two apart), not a byte-for-byte port —
//! the task's own ruling, not something read out of Prowlarr's source.
//!
//! # Result shape and ordering
//!
//! Every successful release renders as [`ReleaseResource`] (see that type's
//! own doc comment for the exact field mapping). The concatenated list is
//! sorted by `seeders` descending, releases with no reported `seeders`
//! sorted last, stably (ties, including between two `None`s, keep their
//! original relative order — [`tokio::task::JoinSet`] completion order
//! across indexers, which is not selection order and not deterministic run
//! to run; within one indexer's own results, the order that indexer's own
//! feed reported them in, which is preserved).

use axum::Json;
use axum::Router;
use axum::extract::{RawQuery, State};
use axum::routing::get;
use oxidarr_db::{IndexerRepo, IndexerRow};
use oxidarr_http::Problem;
use oxidarr_indexer::{HttpClient, SearchQuery};
use tokio::task::JoinSet;
use url::form_urlencoded;

use crate::api::dto::ReleaseResource;
use crate::api::{bad_request, db_error_to_problem};
use crate::server::{AppState, search_row};

/// Builds `GET /search` — a relative route meant to be nested under
/// `/api/v1` by [`crate::api::api_router`], which this function does not add
/// that prefix itself.
pub fn router<C>(state: AppState<C>) -> Router
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/search", get(search::<C>))
        .with_state(state)
}

/// This endpoint's parsed query parameters — see the module docs' "Prowlarr
/// shapes" section for where each one comes from and why they are parsed by
/// hand rather than through a `#[derive(Deserialize)]` struct.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SearchParams {
    query: Option<String>,
    /// Read and kept for completeness; unused today — see the module docs'
    /// "Prowlarr shapes" section.
    #[allow(dead_code)]
    search_type: Option<String>,
    indexer_ids: Vec<i32>,
    categories: Vec<u32>,
}

/// Parses `raw` (the request's raw query string, as `axum::extract::RawQuery`
/// hands it over — `None` when the request had no `?...` at all) into
/// [`SearchParams`]. See the module docs' "Prowlarr shapes" section for the
/// exact repeated-key/comma-separated parsing rule this mirrors from
/// ASP.NET Core's own array model binding.
///
/// `query`/`type` take their *first* occurrence when a client repeats them
/// (undefined behaviour on the real Prowlarr side for a `string`-typed
/// property; this crate's own choice, not a cited one). An entry in
/// `indexerIds`/`categories` that does not parse as a plain non-negative
/// integer is dropped rather than failing the whole request, mirroring
/// [`crate::server::parse_categories`]'s own leniency for the Torznab
/// router's `cat` parameter.
fn parse_search_params(raw: Option<&str>) -> SearchParams {
    let mut params = SearchParams::default();
    for (key, value) in form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "query" if params.query.is_none() => params.query = Some(value.into_owned()),
            "type" if params.search_type.is_none() => {
                params.search_type = Some(value.into_owned());
            }
            "indexerIds" => params.indexer_ids.extend(
                value
                    .split(',')
                    .filter_map(|part| part.trim().parse::<i32>().ok()),
            ),
            "categories" => params.categories.extend(
                value
                    .split(',')
                    .filter_map(|part| part.trim().parse::<u32>().ok()),
            ),
            _ => {}
        }
    }
    params
}

/// Builds the [`SearchQuery`] every selected indexer is searched with, off
/// `params`. See the module docs' "Prowlarr shapes" section for why `type`
/// contributes nothing here.
fn build_search_query(params: &SearchParams) -> SearchQuery {
    SearchQuery {
        q: params.query.clone(),
        categories: params.categories.clone(),
        ..SearchQuery::default()
    }
}

/// Keeps `row` only if it should be searched for this request: enabled, and
/// — when `indexer_ids` is non-empty — named in it. See the module docs'
/// "Indexer selection" section.
fn is_selected(row: &IndexerRow, indexer_ids: &[i32]) -> bool {
    row.enabled && (indexer_ids.is_empty() || indexer_ids.contains(&row.id.0))
}

/// One selected indexer's outcome: either its releases, or its own failure
/// message — carried alongside the indexer's id/name so both the success and
/// failure paths in [`search`] can attribute results/messages correctly
/// after every task in the [`JoinSet`] has finished, in completion (not
/// selection) order.
struct IndexerOutcome {
    id: i32,
    name: String,
    result: Result<Vec<oxidarr_core::Release>, String>,
}

/// `GET /api/v1/search` — see the module docs for the full contract.
///
/// # Errors
///
/// Returns a `400` [`Problem`] when at least one indexer was selected and
/// every one of them failed — see the module docs' "Partial-failure
/// semantics" section. Returns [`db_error_to_problem`]'s mapping if listing
/// indexers itself fails.
async fn search<C>(
    State(state): State<AppState<C>>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Vec<ReleaseResource>>, Problem>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let params = parse_search_params(raw.as_deref());
    let query = build_search_query(&params);

    let rows = IndexerRepo::new(&state.db)
        .list()
        .await
        .map_err(db_error_to_problem)?;
    let selected: Vec<IndexerRow> = rows
        .into_iter()
        .filter(|row| is_selected(row, &params.indexer_ids))
        .collect();

    if selected.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let mut tasks = JoinSet::new();
    for row in selected {
        let defs = state.defs.clone();
        let client = state.tracker_client.clone();
        let query = query.clone();
        tasks.spawn(async move {
            let id = row.id.0;
            let name = row.name.clone();
            let result = search_row(&row, &defs, client, &query).await;
            IndexerOutcome { id, name, result }
        });
    }

    let mut succeeded = 0usize;
    let mut releases = Vec::new();
    let mut failures = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(outcome) => match outcome.result {
                Ok(found) => {
                    succeeded += 1;
                    releases.extend(found.into_iter().map(|release| {
                        ReleaseResource::from_release(release, outcome.id, &outcome.name)
                    }));
                }
                Err(message) => {
                    failures.push(format!("{} (id {}): {message}", outcome.name, outcome.id));
                }
            },
            // A task join failure means the search task itself panicked or
            // was cancelled — neither should happen (this crate denies
            // `clippy::panic`/`unwrap_used`/`expect_used` in non-test code),
            // but `JoinSet::join_next` is fallible regardless, so this is
            // handled as that one indexer's own failure rather than left
            // unhandled.
            Err(join_err) => failures.push(format!("indexer task did not complete: {join_err}")),
        }
    }

    if succeeded == 0 {
        return Err(bad_request(failures.join("; ")));
    }

    releases.sort_by_key(|release| std::cmp::Reverse(release.seeders));
    Ok(Json(releases))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_comma_separated_indexer_ids_value() {
        let params = parse_search_params(Some("indexerIds=1,2,3"));
        assert_eq!(params.indexer_ids, vec![1, 2, 3]);
    }

    #[test]
    fn parses_repeated_indexer_ids_keys() {
        let params = parse_search_params(Some("indexerIds=1&indexerIds=2"));
        assert_eq!(params.indexer_ids, vec![1, 2]);
    }

    #[test]
    fn parses_a_mix_of_repeated_and_comma_separated_indexer_ids() {
        let params = parse_search_params(Some("indexerIds=1,2&indexerIds=3"));
        assert_eq!(params.indexer_ids, vec![1, 2, 3]);
    }

    #[test]
    fn parses_categories_the_same_way_as_indexer_ids() {
        let params = parse_search_params(Some("categories=5000,5030"));
        assert_eq!(params.categories, vec![5000, 5030]);
    }

    #[test]
    fn an_absent_query_string_parses_to_all_defaults() {
        let params = parse_search_params(None);
        assert_eq!(params, SearchParams::default());
    }

    #[test]
    fn query_and_type_take_their_first_occurrence() {
        let params = parse_search_params(Some("query=first&query=second&type=search"));
        assert_eq!(params.query.as_deref(), Some("first"));
        assert_eq!(params.search_type.as_deref(), Some("search"));
    }

    #[test]
    fn a_non_numeric_indexer_id_entry_is_dropped_not_fatal() {
        let params = parse_search_params(Some("indexerIds=1,bogus,2"));
        assert_eq!(params.indexer_ids, vec![1, 2]);
    }

    /// Regression pin (fix round 1, minor finding): `categories` gets the
    /// same non-numeric-entry-dropped leniency as `indexerIds`
    /// (`a_non_numeric_indexer_id_entry_is_dropped_not_fatal`, above), but
    /// only `indexerIds` had a test proving it.
    #[test]
    fn a_non_numeric_category_entry_is_dropped_not_fatal() {
        let params = parse_search_params(Some("categories=5000,bogus,5030"));
        assert_eq!(params.categories, vec![5000, 5030]);
    }

    #[test]
    fn is_selected_requires_enabled() {
        let row = disabled_row();
        assert!(!is_selected(&row, &[]));
    }

    #[test]
    fn is_selected_with_empty_ids_accepts_any_enabled_row() {
        let row = enabled_row(1);
        assert!(is_selected(&row, &[]));
    }

    #[test]
    fn is_selected_with_ids_requires_membership() {
        let row = enabled_row(5);
        assert!(is_selected(&row, &[5]));
        assert!(!is_selected(&row, &[6]));
    }

    fn enabled_row(id: i32) -> IndexerRow {
        IndexerRow {
            id: oxidarr_core::ids::IndexerId(id),
            added: chrono::Utc::now(),
            name: "Test".to_string(),
            definition_id: "example".to_string(),
            kind: oxidarr_db::IndexerKind::Newznab,
            enabled: true,
            settings: serde_json::Map::new(),
            priority: 0,
        }
    }

    fn disabled_row() -> IndexerRow {
        IndexerRow {
            enabled: false,
            ..enabled_row(1)
        }
    }
}
