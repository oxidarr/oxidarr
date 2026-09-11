//! Application CRUD (`GET`/`POST`/`PUT`/`DELETE /applications[/{id}]`) plus
//! `POST /applications/test` — [`crate::api::indexers`]'s sibling: same
//! shape, [`ApplicationRepo`] in place of `IndexerRepo`. Mounted under
//! `/api/v1` by [`crate::api::api_router`], same as every other module in
//! this directory; this module's own [`router`] does not add that prefix
//! itself.
//!
//! # Prowlarr shapes
//!
//! See [`crate::api::dto::ApplicationResource`]'s own doc comment for the
//! exact Prowlarr source citations (`ApplicationResource.cs`,
//! `SonarrSettings.cs`) this module's resource shape and validation rules
//! are read from.
//!
//! # Resource ↔ row mapping
//!
//! `name` and `syncLevel` map straight across (`syncLevel`'s wire strings —
//! `"disabled"`/`"addOnly"`/`"fullSync"` — are exactly [`SyncLevel`]'s own
//! `serde` renames, so no separate string mapping is needed for it, unlike
//! [`AppKind`] below). `implementation` maps to/from [`AppKind`] via
//! [`kind_from_implementation`]/[`implementation_from_kind`]:
//! `"Sonarr"`/`"Radarr"`, case-sensitive.
//!
//! `fields[]` carries exactly two entries, `baseUrl` and `apiKey`, in that
//! fixed order — [`row_to_fields`] builds them; [`required_field`] reads
//! them back out of a request body's `fields[]` by name (not position), so a
//! client that sends them in the opposite order still round-trips
//! correctly. Both values are always plain strings on this resource (unlike
//! [`crate::api::indexers`]'s settings, which can be a Cardigann `checkbox`
//! bool), so — mirroring that module's `fields_from_settings`/
//! `settings_from_fields` reconstruction approach of deriving `label`/`kind`
//! placeholders rather than persisting Prowlarr's real field metadata —
//! every field here reconstructs as `label` = its own name and `kind` =
//! the fixed `"textbox"`, with no `select_options`.
//!
//! # Validation
//!
//! Every `POST`/`PUT` validates `implementation` first: a value other than
//! `"Sonarr"`/`"Radarr"` is rejected with a `400` [`Problem`] naming the
//! unknown value. `baseUrl` and `apiKey` are then read out of `fields[]`
//! ([`required_field`]): a field that is missing entirely, present with no
//! `value`, or present with an empty string `value` all reject the same way
//! — a `400` `Problem` naming the missing field — mirroring real Prowlarr's
//! own `SonarrSettingsValidator.ApiKey.NotEmpty()` rule. `baseUrl` is then
//! parsed as a [`Url`] (`SonarrSettingsValidator.BaseUrl.IsValidUrl()`); a
//! parse failure is a `400` naming the invalid value, and — deliberately —
//! the *original* string is what gets persisted, not the parsed `Url`'s own
//! (possibly normalized, e.g. trailing-slash-appended) rendering: the
//! parse is a validation gate only, not a canonicalization step.
//!
//! # `POST /applications/test`
//!
//! Builds a raw `GET {baseUrl}/api/v3/system/status` [`HttpRequest`]
//! carrying an `X-Api-Key: {apiKey}` header, straight off the request
//! body's `fields[]` (not a persisted row, since this application may not
//! be saved yet), and executes it through `state.app_client` — this endpoint
//! probes a configured *application*, not a tracker, so it shares
//! [`crate::server::AppState`]'s app-bound client with [`crate::sync`]
//! rather than the tracker-bound one [`crate::api::indexers`]'s own test
//! endpoint uses; see that type's own "Two client fields" section. Radarr's own
//! `System/Status` endpoint lives at the identical `/api/v3/system/status`
//! path (`src/Radarr.Api.V3/System/SystemController.cs` in `Radarr/Radarr`
//! declares the same `[Route("api/v3/system")]` + `status` action Sonarr
//! does), so this one probe path serves both [`AppKind`] variants — `kind`
//! is validated (an unknown `implementation` still rejects) but otherwise
//! unused in this handler, unlike [`crate::api::indexers`]'s test endpoint,
//! whose kind changes which concrete [`oxidarr_indexer::Indexer`] runs.
//!
//! A `2xx` response renders `200` with an empty JSON array — the same
//! "no items" shape [`crate::api::indexers`]'s own test endpoint and every
//! other endpoint in this crate use for success-with-nothing-to-report. Any
//! non-`2xx` status, or a transport-level failure executing the request at
//! all, renders as a `400` [`Problem`] carrying a message describing the
//! failure — this crate's honest-subset [`Problem`] shape, not real
//! Prowlarr's `List<ValidationFailure>` (see
//! [`crate::api::indexers`]'s own module docs for the same documented
//! divergence on its sibling test endpoint).
//!
//! # App-sync trigger
//!
//! [`create`] and [`update`] each trigger
//! [`crate::api::sync_one_and_describe_failure`] (best-effort, against just
//! the one application being written) after their own database write
//! succeeds, folding whatever it returns into the response's
//! [`ApplicationResource::sync_error`] field — see
//! [`crate::api::indexers`]'s own module docs for the full rationale, shared
//! verbatim here. `remove` does **not** trigger a sync: deleting an
//! application already cascades every one of its mapping rows away locally
//! (see [`oxidarr_db::MappingRepo`]'s own cascade tests), and there is no
//! honest use in calling back out to an application this instance is in the
//! middle of forgetting about.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use oxidarr_core::ids::AppId;
use oxidarr_db::{AppKind, ApplicationRepo, ApplicationRow, NewApplication};
use oxidarr_http::Problem;
use oxidarr_indexer::{HttpClient, HttpRequest, Method};
use serde_json::Value;
use url::Url;

use crate::api::dto::{ApplicationResource, Field};
use crate::api::{bad_request, db_error_to_problem, sync_one_and_describe_failure};
use crate::server::AppState;

/// Builds `GET`/`POST /applications`, `GET`/`PUT`/`DELETE
/// /applications/{id}`, and `POST /applications/test` — relative routes
/// meant to be nested under `/api/v1` by [`crate::api::api_router`], which
/// this function does not add that prefix itself.
pub fn router<C>(state: AppState<C>) -> Router
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/applications", get(list::<C>).post(create::<C>))
        .route("/applications/test", post(test::<C>))
        .route(
            "/applications/{id}",
            get(get_one::<C>).put(update::<C>).delete(remove::<C>),
        )
        .with_state(state)
}

async fn list<C>(
    State(state): State<AppState<C>>,
) -> Result<Json<Vec<ApplicationResource>>, Problem>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let rows = ApplicationRepo::new(&state.db)
        .list()
        .await
        .map_err(db_error_to_problem)?;
    Ok(Json(rows.iter().map(row_to_resource).collect()))
}

async fn get_one<C>(
    State(state): State<AppState<C>>,
    Path(id): Path<i32>,
) -> Result<Json<ApplicationResource>, Problem>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let row = ApplicationRepo::new(&state.db)
        .get(AppId(id))
        .await
        .map_err(db_error_to_problem)?;
    Ok(Json(row_to_resource(&row)))
}

async fn create<C>(
    State(state): State<AppState<C>>,
    Json(resource): Json<ApplicationResource>,
) -> Result<(StatusCode, Json<ApplicationResource>), Problem>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let new = build_new_application(&resource)?;
    let row = ApplicationRepo::new(&state.db)
        .insert(&new)
        .await
        .map_err(db_error_to_problem)?;
    let mut resource = row_to_resource(&row);
    resource.sync_error = sync_one_and_describe_failure(&state, &row).await;
    Ok((StatusCode::CREATED, Json(resource)))
}

async fn update<C>(
    State(state): State<AppState<C>>,
    Path(id): Path<i32>,
    Json(resource): Json<ApplicationResource>,
) -> Result<Json<ApplicationResource>, Problem>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    let new = build_new_application(&resource)?;
    let row = ApplicationRow {
        id: AppId(id),
        // Not written by `ApplicationRepo::update` (it only ever overwrites
        // name/kind/base_url/api_key/sync_level), so any value here is a
        // harmless placeholder.
        added: Utc::now(),
        name: new.name,
        kind: new.kind,
        base_url: new.base_url,
        api_key: new.api_key,
        sync_level: new.sync_level,
    };
    ApplicationRepo::new(&state.db)
        .update(&row)
        .await
        .map_err(db_error_to_problem)?;
    let mut resource = row_to_resource(&row);
    resource.sync_error = sync_one_and_describe_failure(&state, &row).await;
    Ok(Json(resource))
}

async fn remove<C>(
    State(state): State<AppState<C>>,
    Path(id): Path<i32>,
) -> Result<StatusCode, Problem>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    ApplicationRepo::new(&state.db)
        .delete(AppId(id))
        .await
        .map_err(db_error_to_problem)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn test<C>(
    State(state): State<AppState<C>>,
    Json(resource): Json<ApplicationResource>,
) -> Result<Json<Vec<Value>>, Problem>
where
    C: HttpClient + Clone + Send + Sync + 'static,
{
    run_test(&resource, state.app_client.clone()).await?;
    Ok(Json(Vec::new()))
}

/// Maps `implementation` (as sent in an `ApplicationResource` request body)
/// onto an [`AppKind`]. See the module docs' "Resource ↔ row mapping"
/// section for the exact accepted strings.
fn kind_from_implementation(implementation: &str) -> Option<AppKind> {
    match implementation {
        "Sonarr" => Some(AppKind::Sonarr),
        "Radarr" => Some(AppKind::Radarr),
        _ => None,
    }
}

/// The inverse of [`kind_from_implementation`].
const fn implementation_from_kind(kind: AppKind) -> &'static str {
    match kind {
        AppKind::Sonarr => "Sonarr",
        AppKind::Radarr => "Radarr",
    }
}

/// Reads the `value` of the `fields[]` entry named `name`, as a non-empty
/// string. See the module docs' "Validation" section: a field that is
/// missing entirely, present with no `value`, or present with an empty
/// string `value` all reject the same way.
fn required_field(fields: &[Field], name: &str) -> Result<String, Problem> {
    let value = fields
        .iter()
        .find(|field| field.name == name)
        .and_then(|field| field.value.as_ref())
        .and_then(Value::as_str)
        .unwrap_or_default();
    if value.is_empty() {
        return Err(bad_request(format!("missing {name} field")));
    }
    Ok(value.to_string())
}

/// Builds the two-entry `fields[]` a [`ApplicationRow`] renders as — see the
/// module docs' "Resource ↔ row mapping" section for why `baseUrl` and
/// `apiKey` are always in this fixed order with placeholder `label`/`kind`.
fn row_to_fields(row: &ApplicationRow) -> Vec<Field> {
    vec![
        text_field("baseUrl", &row.base_url),
        text_field("apiKey", &row.api_key),
    ]
}

/// Builds one string-valued `Field`, `label`/`kind` set to placeholders —
/// see the module docs' "Resource ↔ row mapping" section.
fn text_field(name: &str, value: &str) -> Field {
    Field {
        name: name.to_string(),
        label: name.to_string(),
        kind: "textbox".to_string(),
        value: Some(Value::String(value.to_string())),
        select_options: None,
    }
}

/// Maps a persisted [`ApplicationRow`] onto its [`ApplicationResource`] wire
/// shape. See the module docs' "Resource ↔ row mapping" section.
fn row_to_resource(row: &ApplicationRow) -> ApplicationResource {
    ApplicationResource {
        id: row.id.0,
        name: row.name.clone(),
        implementation: implementation_from_kind(row.kind).to_string(),
        sync_level: row.sync_level,
        fields: row_to_fields(row),
        sync_error: None,
    }
}

/// Validates `resource` (implementation, `baseUrl`, `apiKey`) and builds the
/// [`NewApplication`] a `POST`/`PUT` handler persists. See the module docs'
/// "Validation" section for the exact rules.
///
/// # Errors
///
/// Returns a `400` [`Problem`] if `resource.implementation` is not one of
/// the two known strings, if `fields[]` is missing a non-empty `baseUrl` or
/// `apiKey`, or if `baseUrl` does not parse as a [`Url`].
fn build_new_application(resource: &ApplicationResource) -> Result<NewApplication, Problem> {
    let kind = kind_from_implementation(&resource.implementation).ok_or_else(|| {
        bad_request(format!(
            "unknown implementation {:?}",
            resource.implementation
        ))
    })?;

    let base_url = required_field(&resource.fields, "baseUrl")?;
    validate_url(&base_url)?;
    let api_key = required_field(&resource.fields, "apiKey")?;

    Ok(NewApplication {
        name: resource.name.clone(),
        kind,
        base_url,
        api_key,
        sync_level: resource.sync_level,
    })
}

/// Parses `raw` as a [`Url`], discarding the parsed value — see the module
/// docs' "Validation" section for why the original string, not this parsed
/// form, is what gets persisted.
fn validate_url(raw: &str) -> Result<(), Problem> {
    raw.parse::<Url>()
        .map(|_| ())
        .map_err(|err| bad_request(format!("invalid baseUrl {raw:?}: {err}")))
}

/// Runs `POST /applications/test`'s probe request — see the module docs'
/// "`POST /applications/test`" section.
///
/// # Errors
///
/// Returns a `400` [`Problem`] for every failure mode described in the
/// module docs.
async fn run_test<C>(resource: &ApplicationResource, client: C) -> Result<(), Problem>
where
    C: HttpClient,
{
    kind_from_implementation(&resource.implementation).ok_or_else(|| {
        bad_request(format!(
            "unknown implementation {:?}",
            resource.implementation
        ))
    })?;

    let base_url = required_field(&resource.fields, "baseUrl")?;
    validate_url(&base_url)?;
    let api_key = required_field(&resource.fields, "apiKey")?;

    let url: Url = format!("{}/api/v3/system/status", base_url.trim_end_matches('/'))
        .parse()
        .map_err(|err: url::ParseError| bad_request(format!("building status probe URL: {err}")))?;

    let response = client
        .execute(HttpRequest {
            method: Method::Get,
            url: url.clone(),
            headers: vec![("X-Api-Key".to_string(), api_key)],
            body: None,
            follow_redirects: true,
        })
        .await
        .map_err(|err| bad_request(err.to_string()))?;

    if (200..300).contains(&response.status) {
        Ok(())
    } else {
        Err(bad_request(format!(
            "unexpected status {} from {url}",
            response.status
        )))
    }
}
