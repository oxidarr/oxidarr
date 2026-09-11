//! Indexer CRUD (`GET`/`POST`/`PUT`/`DELETE /indexer[/{id}]`) plus `POST
//! /indexer/test` — the write side of the resource shape
//! [`crate::api::dto`] and [`crate::api::indexer_schema`] already read from.
//! Mounted under `/api/v1` by [`crate::api::api_router`], same as every
//! other module in this directory; this module's own [`router`] does not
//! add that prefix itself.
//!
//! # Prowlarr shapes
//!
//! Read `src/Prowlarr.Api.V1/Indexers/IndexerController.cs` and
//! `src/Prowlarr.Http/REST/RestControllerBase.cs` from `Prowlarr/Prowlarr`
//! at commit `693c7c3b0e8ec6e9dd792c01e5fa1091260b3be1`: `IndexerController`
//! inherits the standard `RestControllerBase<IndexerResource>` CRUD routes
//! (`GET /`, `GET /{id}`, `POST /`, `PUT /{id}`, `DELETE /{id}`) plus its own
//! `[HttpPost("test")]`. [`IndexerResource`] is reused verbatim for both the
//! request body (`POST`/`PUT`) and the response — see that type's own doc
//! comment for why it now derives `Deserialize` too.
//!
//! # Resource ↔ row mapping
//!
//! `name`, `enable`↔`enabled`, `definitionName`↔`definition_id`, and
//! `priority` map straight across. `implementation` maps to/from
//! [`IndexerKind`] via [`kind_from_implementation`]/[`implementation_from_kind`]:
//! `"Cardigann"`/`"Newznab"`/`"Torznab"`, case-sensitive, matching exactly
//! what [`crate::api::indexer_schema`] itself emits.
//!
//! `fields[]` ↔ the row's `settings` JSON map: [`settings_from_fields`]
//! writes one `settings` entry per `Field` that carries a `value` (a `Field`
//! with no `value` contributes no settings entry at all, rather than an
//! empty-string or `null` one), storing the JSON `value` verbatim — a
//! `select` field's `value` is already the raw default *string*, per
//! [`crate::api::dto`]'s own documented divergence from real Prowlarr's
//! index encoding, so this write path stores that string as-is rather than
//! trying to resolve it back through `selectOptions`. [`fields_from_settings`]
//! is the inverse for `GET`/list: one `Field` per settings entry, in the
//! map's own (sorted) key order. Reconstructing a settings-only `Field` has
//! no access to the original definition's `label`/`selectOptions` metadata,
//! so those two are never reconstructed — `label` is set to the setting's
//! own name and `kind` to `"checkbox"`/`"textbox"` depending on whether the
//! stored value is a JSON bool, an honest-subset simplification analogous to
//! every other one already documented in `dto.rs`. `description`, `language`,
//! and `capabilities` are never persisted per indexer (they describe a
//! *definition*, not a configured indexer instance) and always read back as
//! their zero values, regardless of what a client's request body sent.
//!
//! # `definitionName` validation
//!
//! Every `POST`/`PUT` validates `definitionName` before it ever reaches
//! [`NewIndexer`]/[`IndexerRow`], carried over from Task 2's review: a value
//! containing `/`, `\`, `..`, or that is empty or exactly `.` is rejected
//! with a `400` [`Problem`] — the exact same shape
//! [`crate::definitions::DefinitionStore`]'s own `validate_id` rejects,
//! duplicated here (rather than imported) since that check is private to
//! `DefinitionStore` and this is a defense-in-depth API-boundary gate, not a
//! reuse of the store's own internals. This check runs for every
//! [`IndexerKind`], not only `Cardigann`: a `Newznab`/`Torznab` row's
//! `definition_id` is never used to open a file, but it is still
//! client-controlled data that gets persisted, and nothing downstream should
//! have to re-validate it just because this endpoint didn't.
//!
//! For [`IndexerKind::Cardigann`] specifically, `definitionName` must also
//! *exist*: [`crate::definitions::DefinitionStore::get`] is called and a
//! failure (unknown id, unparsable YAML) is rendered as a `400` `Problem`
//! naming the failure, rather than persisting a row that would 300-error on
//! every future search.
//!
//! # `POST /indexer/test`
//!
//! Builds the same kind of [`oxidarr_indexer::Indexer`]
//! [`crate::server::search_row`]-style dispatch would (`CardigannIndexer`
//! via the definition store, or `NewznabIndexer`/`TorznabIndexer` off a
//! `baseUrl`/`apiKey` pair read from `fields[]`) from the request body — not
//! a persisted row, since this indexer may not be saved yet — and runs one
//! probe [`SearchQuery::default`] search through it. Success renders `200`
//! with an empty JSON array (no results are asserted or returned — this
//! endpoint proves *reachability*, not correctness of the fetched content).
//! Any failure (bad `definitionName`, missing `baseUrl`, a transport error,
//! an engine error) renders as a `400` [`Problem`] carrying that failure's
//! own [`Display`](std::fmt::Display) text.
//!
//! Real Prowlarr's own `TestIndexers`/`[HttpPost("test")]` returns a
//! `List<ValidationFailure>` (empty on success) — this crate's uniform
//! [`Problem`] JSON body is an honest, documented subset of that shape, not
//! a byte-for-byte match; a client that expects Prowlarr's exact
//! `ValidationFailure` array shape on failure will not parse this endpoint's
//! `400` body correctly. Success (`200 []`) is the shape shared with every
//! other endpoint in this crate that returns "no items", so that half is not
//! a divergence.
//!
//! # App-sync trigger
//!
//! [`create`] and [`update`] each trigger
//! [`crate::api::sync_all_and_describe_failures`] (best-effort, against
//! every configured application — an indexer change can affect more than
//! one) after their own database write succeeds, writing whatever it
//! returns into the response's [`IndexerResource::sync_error`] field; the
//! write itself still answers `2xx` even when sync failed. This is a
//! deliberate divergence from real Prowlarr, whose own sync runs
//! asynchronously in the background with a separate indexer-health/status
//! model — this crate settles for one inline best-effort attempt per
//! write, with the outcome folded into the very same response, rather than
//! reproducing that machinery.
//!
//! [`remove`] cannot use that same `sync_all` trigger: the `app_indexer_map`
//! table's `indexer_id` column is `ON DELETE CASCADE`, so
//! `IndexerRepo::delete` erases every mapping naming this indexer
//! *synchronously*, before any sync call afterward could ever see one to
//! react to — running `sync_all` post-delete the way `create`/`update` do
//! would silently never clean up the remote side at all. Instead `remove`
//! snapshots, for every `fullSync` application, the `(application,
//! remote_indexer_id)` pairs currently mapping this indexer — via
//! [`oxidarr_db::MappingRepo::for_indexer`] — *before* calling
//! `IndexerRepo::delete`, then issues each snapshot's own remote `DELETE`
//! directly (via [`crate::sync::delete_indexer`]) once the local delete has
//! succeeded. Every remote delete is best-effort with its outcome discarded,
//! same as before: a `204 No Content` response has no body to report a
//! `syncError` on.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use oxidarr_core::ids::IndexerId;
use oxidarr_db::{
    ApplicationRepo, ApplicationRow, Db, IndexerKind, IndexerRepo, IndexerRow, MappingRepo,
    NewIndexer, SyncLevel,
};
use oxidarr_http::Problem;
use oxidarr_indexer::{
    CardigannIndexer, HttpClient, Indexer, NewznabIndexer, SearchQuery, Settings, TorznabIndexer,
};
use serde_json::Value;
use url::Url;

use crate::api::dto::{Field, IndexerCapabilities, IndexerResource};
use crate::api::{bad_request, db_error_to_problem, sync_all_and_describe_failures};
use crate::definitions::DefinitionStore;
use crate::server::{AppState, settings_from_json};
use crate::sync;

/// Builds `GET`/`POST /indexer`, `GET`/`PUT`/`DELETE /indexer/{id}`, and
/// `POST /indexer/test` — relative routes meant to be nested under
/// `/api/v1` by [`crate::api::api_router`], which this function does not add
/// that prefix itself.
pub fn router<C>(state: AppState<C>) -> Router
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/indexer", get(list::<C>).post(create::<C>))
        .route("/indexer/test", post(test::<C>))
        .route(
            "/indexer/{id}",
            get(get_one::<C>).put(update::<C>).delete(remove::<C>),
        )
        .with_state(state)
}

async fn list<C>(State(state): State<AppState<C>>) -> Result<Json<Vec<IndexerResource>>, Problem>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let rows = IndexerRepo::new(&state.db)
        .list()
        .await
        .map_err(db_error_to_problem)?;
    Ok(Json(rows.iter().map(row_to_resource).collect()))
}

async fn get_one<C>(
    State(state): State<AppState<C>>,
    Path(id): Path<i32>,
) -> Result<Json<IndexerResource>, Problem>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let row = IndexerRepo::new(&state.db)
        .get(IndexerId(id))
        .await
        .map_err(db_error_to_problem)?;
    Ok(Json(row_to_resource(&row)))
}

async fn create<C>(
    State(state): State<AppState<C>>,
    Json(resource): Json<IndexerResource>,
) -> Result<(StatusCode, Json<IndexerResource>), Problem>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let new = build_new_indexer(&resource, &state.defs).await?;
    let row = IndexerRepo::new(&state.db)
        .insert(&new)
        .await
        .map_err(db_error_to_problem)?;
    let mut resource = row_to_resource(&row);
    resource.sync_error = sync_all_and_describe_failures(&state).await;
    Ok((StatusCode::CREATED, Json(resource)))
}

async fn update<C>(
    State(state): State<AppState<C>>,
    Path(id): Path<i32>,
    Json(resource): Json<IndexerResource>,
) -> Result<Json<IndexerResource>, Problem>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let new = build_new_indexer(&resource, &state.defs).await?;
    let row = IndexerRow {
        id: IndexerId(id),
        // Not written by `IndexerRepo::update` (it only ever overwrites
        // name/definition_id/kind/enabled/settings/priority), so any value
        // here is a harmless placeholder.
        added: Utc::now(),
        name: new.name,
        definition_id: new.definition_id,
        kind: new.kind,
        enabled: new.enabled,
        settings: new.settings,
        priority: new.priority,
    };
    IndexerRepo::new(&state.db)
        .update(&row)
        .await
        .map_err(db_error_to_problem)?;
    let mut resource = row_to_resource(&row);
    resource.sync_error = sync_all_and_describe_failures(&state).await;
    Ok(Json(resource))
}

/// `DELETE /indexer/{id}`. See the module docs' "App-sync trigger" section
/// for why this snapshots doomed remote mappings *before* deleting the
/// indexer row, rather than triggering [`sync_all_and_describe_failures`]
/// afterward the way [`create`]/[`update`] do.
async fn remove<C>(
    State(state): State<AppState<C>>,
    Path(id): Path<i32>,
) -> Result<StatusCode, Problem>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let indexer_id = IndexerId(id);
    let doomed = doomed_remote_mappings(&state.db, indexer_id).await;

    IndexerRepo::new(&state.db)
        .delete(indexer_id)
        .await
        .map_err(db_error_to_problem)?;

    for (app, remote_id) in doomed {
        // Best-effort, outcome discarded — same as `create`/`update`'s own
        // sync trigger, and for the same reason: a `204 No Content`
        // response has no body to report a failure on.
        let _ = sync::delete_indexer(&state.app_client, &app, remote_id).await;
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Snapshots which `fullSync` applications currently map `indexer_id`
/// remotely, and under what remote id, so [`remove`] can clean up the
/// remote side *after* deleting the local row. Must run before that
/// delete: `app_indexer_map.indexer_id` is `ON DELETE CASCADE`, so every
/// mapping naming `indexer_id` is gone the instant the row is —
/// [`oxidarr_db::MappingRepo::for_indexer`] would return nothing at all if
/// called afterward.
///
/// `addOnly`/`disabled` applications are excluded: per `crate::sync`'s own
/// contract, only `fullSync` ever deletes a remote entry, so there is
/// nothing for `remove` to clean up on their behalf. A failure listing
/// mappings or applications degrades to "nothing to clean up" (an empty
/// `Vec`) rather than failing the delete itself — the same best-effort
/// posture every other sync trigger in this crate takes.
async fn doomed_remote_mappings(db: &Db, indexer_id: IndexerId) -> Vec<(ApplicationRow, i32)> {
    let mappings = MappingRepo::new(db)
        .for_indexer(indexer_id)
        .await
        .unwrap_or_default();
    if mappings.is_empty() {
        return Vec::new();
    }
    let apps = ApplicationRepo::new(db).list().await.unwrap_or_default();
    mappings
        .into_iter()
        .filter_map(|mapping| {
            apps.iter()
                .find(|app| app.id == mapping.app_id && app.sync_level == SyncLevel::FullSync)
                .cloned()
                .map(|app| (app, mapping.remote_indexer_id.0))
        })
        .collect()
}

async fn test<C>(
    State(state): State<AppState<C>>,
    Json(resource): Json<IndexerResource>,
) -> Result<Json<Vec<Value>>, Problem>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    run_test(&resource, &state.defs, state.tracker_client.clone()).await?;
    Ok(Json(Vec::new()))
}

/// Maps `implementation` (as sent in an `IndexerResource` request body) onto
/// an [`IndexerKind`]. See the module docs' "Resource ↔ row mapping"
/// section for the exact accepted strings.
fn kind_from_implementation(implementation: &str) -> Option<IndexerKind> {
    match implementation {
        "Cardigann" => Some(IndexerKind::Cardigann),
        "Newznab" => Some(IndexerKind::Newznab),
        "Torznab" => Some(IndexerKind::Torznab),
        _ => None,
    }
}

/// The inverse of [`kind_from_implementation`].
const fn implementation_from_kind(kind: IndexerKind) -> &'static str {
    match kind {
        IndexerKind::Cardigann => "Cardigann",
        IndexerKind::Newznab => "Newznab",
        IndexerKind::Torznab => "Torznab",
    }
}

/// Rejects any `id` that is not safe to persist as a definition id: empty,
/// exactly `.`, containing a path separator (`/` or `\`), or containing
/// `..` anywhere. Deliberately mirrors
/// [`crate::definitions::DefinitionStore`]'s own private `validate_id`
/// predicate exactly — see the module docs' "`definitionName` validation"
/// section for why this is a separate, duplicated gate rather than a shared
/// helper.
fn is_plain_stem(id: &str) -> bool {
    !id.is_empty() && id != "." && !id.contains(['/', '\\']) && !id.contains("..")
}

/// Converts `fields` into the JSON `settings` map an [`NewIndexer`]/
/// [`IndexerRow`] stores: one entry per `Field` that carries a `value`,
/// storing that JSON value verbatim (a `Field` with no `value` contributes
/// no entry at all). See the module docs' "Resource ↔ row mapping" section.
fn settings_from_fields(fields: &[Field]) -> serde_json::Map<String, Value> {
    fields
        .iter()
        .filter_map(|field| {
            field
                .value
                .as_ref()
                .map(|value| (field.name.clone(), value.clone()))
        })
        .collect()
}

/// The inverse of [`settings_from_fields`], for `GET`/list responses. See
/// the module docs' "Resource ↔ row mapping" section for why `label`/`kind`
/// are reconstructed placeholders rather than the original definition's
/// metadata.
fn fields_from_settings(settings: &serde_json::Map<String, Value>) -> Vec<Field> {
    settings
        .iter()
        .map(|(name, value)| Field {
            name: name.clone(),
            label: name.clone(),
            kind: if matches!(value, Value::Bool(_)) {
                "checkbox".to_string()
            } else {
                "textbox".to_string()
            },
            value: Some(value.clone()),
            select_options: None,
        })
        .collect()
}

/// Maps a persisted [`IndexerRow`] onto its [`IndexerResource`] wire shape.
/// See the module docs' "Resource ↔ row mapping" section.
fn row_to_resource(row: &IndexerRow) -> IndexerResource {
    IndexerResource {
        id: row.id.0,
        name: row.name.clone(),
        implementation: implementation_from_kind(row.kind).to_string(),
        definition_name: row.definition_id.clone(),
        description: String::new(),
        language: String::new(),
        enable: row.enabled,
        priority: row.priority,
        capabilities: IndexerCapabilities::default(),
        fields: fields_from_settings(&row.settings),
        sync_error: None,
    }
}

/// Validates `resource` (implementation, `definitionName`) and builds the
/// [`NewIndexer`] a `POST`/`PUT` handler persists. See the module docs'
/// "`definitionName` validation" section for the exact rules.
///
/// # Errors
///
/// Returns a `400` [`Problem`] if `resource.implementation` is not one of
/// the three known strings, if `resource.definition_name` is not a plain
/// file stem, or — for [`IndexerKind::Cardigann`] only — if `defs.get`
/// fails to load that definition.
async fn build_new_indexer(
    resource: &IndexerResource,
    defs: &DefinitionStore,
) -> Result<NewIndexer, Problem> {
    let kind = kind_from_implementation(&resource.implementation).ok_or_else(|| {
        bad_request(format!(
            "unknown implementation {:?}",
            resource.implementation
        ))
    })?;

    if !is_plain_stem(&resource.definition_name) {
        return Err(bad_request(format!(
            "invalid definitionName {:?}",
            resource.definition_name
        )));
    }

    if kind == IndexerKind::Cardigann {
        defs.get(&resource.definition_name).await.map_err(|err| {
            bad_request(format!(
                "unknown definitionName {:?}: {err}",
                resource.definition_name
            ))
        })?;
    }

    Ok(NewIndexer {
        name: resource.name.clone(),
        definition_id: resource.definition_name.clone(),
        kind,
        enabled: resource.enable,
        settings: settings_from_fields(&resource.fields),
        priority: resource.priority,
    })
}

/// Runs `POST /indexer/test`'s probe search — see the module docs' "`POST
/// /indexer/test`" section.
///
/// # Errors
///
/// Returns a `400` [`Problem`] for every failure mode described in the
/// module docs.
async fn run_test<C>(
    resource: &IndexerResource,
    defs: &DefinitionStore,
    client: C,
) -> Result<(), Problem>
where
    C: HttpClient,
{
    let kind = kind_from_implementation(&resource.implementation).ok_or_else(|| {
        bad_request(format!(
            "unknown implementation {:?}",
            resource.implementation
        ))
    })?;

    match kind {
        IndexerKind::Cardigann => {
            let def = defs
                .get(&resource.definition_name)
                .await
                .map_err(|err| bad_request(err.to_string()))?;
            let settings_map = settings_from_fields(&resource.fields);
            let settings = Settings::new(settings_from_json(&settings_map));
            CardigannIndexer::new((*def).clone(), settings, client)
                .search(&SearchQuery::default())
                .await
                .map_err(|err| bad_request(err.to_string()))?;
        }
        IndexerKind::Newznab | IndexerKind::Torznab => {
            let settings_map = settings_from_fields(&resource.fields);
            let base = settings_map
                .get("baseUrl")
                .and_then(Value::as_str)
                .ok_or_else(|| bad_request("missing baseUrl setting"))?;
            let base: Url = base.parse().map_err(|err: url::ParseError| {
                bad_request(format!("invalid baseUrl {base:?}: {err}"))
            })?;
            let apikey = settings_map
                .get("apiKey")
                .and_then(Value::as_str)
                .map(str::to_string);
            match kind {
                IndexerKind::Newznab => NewznabIndexer::new(base, apikey, client)
                    .search(&SearchQuery::default())
                    .await
                    .map_err(|err| bad_request(err.to_string()))?,
                _ => TorznabIndexer::new(base, apikey, client)
                    .search(&SearchQuery::default())
                    .await
                    .map_err(|err| bad_request(err.to_string()))?,
            };
        }
    }

    Ok(())
}
