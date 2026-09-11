//! Integration tests for indexer CRUD (`GET`/`POST`/`PUT`/`DELETE
//! /api/v1/indexer[/{id}]`) and `POST /api/v1/indexer/test` — all driven via
//! `tower::ServiceExt::oneshot` against the real [`api_router`], following
//! `tests/api_router.rs`/`tests/indexer_schema.rs`'s own helper style.
//!
//! Every Cardigann-kind test reuses `tests/fixtures/defs/example.yml`, the
//! same fixture `tests/torznab_router.rs` already exercises.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use oxidarr_core::ids::RemoteIndexerId;
use oxidarr_db::{
    AppKind, ApplicationRepo, ApplicationRow, ConfigRepo, Db, IndexerKind, IndexerRepo,
    MappingRepo, MappingRow, NewApplication, NewIndexer, SyncLevel,
};
use oxidarr_http::auth::ApiKey;
use oxidarr_indexer::testing::{FakeClient, ok_html};
use oxidarr_indexer::{Body as HttpBody, HttpResponse, Method};
use oxidarr_prowl::{AppState, DefinitionStore, api_router};
use serde_json::{Value, json};
use tower::ServiceExt;

const SEARCH_HTML: &str = include_str!("fixtures/search.html");
const API_KEY: &str = "s3cret";

fn definitions_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/defs"))
}

async fn seeded_db() -> Db {
    Db::open_in_memory().await.unwrap()
}

fn state(db: Db, client: FakeClient) -> AppState<FakeClient> {
    state_with_app_client(db, client, FakeClient::new())
}

/// Like [`state`], but with an independently-expectation'd `app_client` —
/// used by this file's app-sync CRUD-trigger tests, which need to assert on
/// the Sonarr-bound calls those triggers make without interleaving them
/// with `tracker_client`'s own (usually empty) expectation queue.
fn state_with_app_client(
    db: Db,
    tracker_client: FakeClient,
    app_client: FakeClient,
) -> AppState<FakeClient> {
    AppState {
        db: Arc::new(db),
        tracker_client,
        app_client,
        defs: DefinitionStore::new(definitions_dir()),
        external_url: "http://oxidarr.local:9696".parse().unwrap(),
    }
}

fn fixed_start_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2024-01-01T00:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc)
}

fn v1_app(db: Db, client: FakeClient) -> Router {
    api_router(state(db, client), ApiKey::new(API_KEY), fixed_start_time())
}

/// Like [`v1_app`], but with an independently-expectation'd `app_client` —
/// used by this file's app-sync CRUD-trigger tests below.
fn v1_app_with_app_client(db: Db, tracker_client: FakeClient, app_client: FakeClient) -> Router {
    api_router(
        state_with_app_client(db, tracker_client, app_client),
        ApiKey::new(API_KEY),
        fixed_start_time(),
    )
}

/// Inserts one `fullSync` Sonarr application, so the CRUD-trigger tests
/// below have something for their indexer writes to sync into. Returns the
/// inserted row so a test that needs its id (to build a mapping directly,
/// bypassing an actual sync push) can use it without a separate lookup.
async fn seed_full_sync_application(db: &Db) -> ApplicationRow {
    ApplicationRepo::new(db)
        .insert(&NewApplication {
            name: "Sonarr Main".to_string(),
            kind: AppKind::Sonarr,
            base_url: "http://sonarr.example".to_string(),
            api_key: "sonarr-key".to_string(),
            sync_level: SyncLevel::FullSync,
        })
        .await
        .unwrap()
}

/// A `201 Created`-shaped Sonarr `POST /api/v3/indexer` response carrying
/// `id`.
fn sonarr_created(id: i64) -> HttpResponse {
    HttpResponse {
        status: 201,
        headers: vec![],
        body: json!({"id": id}).to_string().into_bytes(),
        final_url: "http://sonarr.example/api/v3/indexer".parse().unwrap(),
    }
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("x-api-key", API_KEY)
        .body(Body::empty())
        .unwrap()
}

fn get_no_key(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn json_request(method: &str, uri: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("x-api-key", API_KEY)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn delete(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("x-api-key", API_KEY)
        .body(Body::empty())
        .unwrap()
}

async fn body_bytes(response: axum::response::Response) -> Vec<u8> {
    to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = body_bytes(response).await;
    serde_json::from_slice(&bytes).unwrap()
}

/// Extracts `(name, value)` pairs off a `fields` JSON array, sorted by name
/// so two lists built in different orders still compare equal.
fn field_pairs(fields: &Value) -> Vec<(String, Value)> {
    let mut pairs: Vec<(String, Value)> = fields
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["name"].as_str().unwrap().to_string(), f["value"].clone()))
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    pairs
}

fn cardigann_body(name: &str, priority: i64, enable: bool, fields: &Value) -> Value {
    json!({
        "name": name,
        "implementation": "Cardigann",
        "definitionName": "example",
        "enable": enable,
        "priority": priority,
        "fields": fields,
    })
}

#[tokio::test]
async fn full_lifecycle_create_get_list_update_delete_then_404() {
    let app = v1_app(seeded_db().await, FakeClient::new());
    let input = cardigann_body(
        "My Indexer",
        12,
        true,
        &json!([{"name": "apiKey", "value": "secret"}]),
    );

    // create
    let response = app
        .clone()
        .oneshot(json_request("POST", "/api/v1/indexer", &input))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = body_json(response).await;
    let id = created["id"].as_i64().unwrap();
    assert!(id > 0, "created body was: {created}");
    assert_eq!(created["name"], "My Indexer");
    assert_eq!(created["implementation"], "Cardigann");
    assert_eq!(created["definitionName"], "example");
    assert_eq!(created["enable"], true);
    assert_eq!(created["priority"], 12);
    assert_eq!(
        field_pairs(&created["fields"]),
        field_pairs(&input["fields"])
    );

    // get: round-trip law — the shipped fields equal the input exactly.
    let response = app
        .clone()
        .oneshot(get(&format!("/api/v1/indexer/{id}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = body_json(response).await;
    assert_eq!(fetched, created, "get did not return what create returned");
    assert_eq!(fetched["name"], input["name"]);
    assert_eq!(fetched["implementation"], input["implementation"]);
    assert_eq!(fetched["definitionName"], input["definitionName"]);
    assert_eq!(fetched["enable"], input["enable"]);
    assert_eq!(fetched["priority"], input["priority"]);
    assert_eq!(
        field_pairs(&fetched["fields"]),
        field_pairs(&input["fields"])
    );

    // list
    let response = app.clone().oneshot(get("/api/v1/indexer")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let listed = body_json(response).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0], created);

    // update
    let updated_input = cardigann_body("Renamed", 5, false, &json!([]));
    let response = app
        .clone()
        .oneshot(json_request(
            "PUT",
            &format!("/api/v1/indexer/{id}"),
            &updated_input,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let updated = body_json(response).await;
    assert_eq!(updated["id"], id);
    assert_eq!(updated["name"], "Renamed");
    assert_eq!(updated["priority"], 5);
    assert_eq!(updated["enable"], false);
    assert_eq!(updated["fields"].as_array().unwrap().len(), 0);

    // get reflects the update
    let response = app
        .clone()
        .oneshot(get(&format!("/api/v1/indexer/{id}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, updated);

    // delete
    let response = app
        .clone()
        .oneshot(delete(&format!("/api/v1/indexer/{id}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(body_bytes(response).await.is_empty());

    // get after delete: 404 Problem naming the id
    let response = app
        .oneshot(get(&format!("/api/v1/indexer/{id}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string());
    assert!(
        problem["message"]
            .as_str()
            .unwrap()
            .contains(&id.to_string()),
        "message was: {problem}"
    );
}

#[tokio::test]
async fn create_rejects_a_path_traversal_definition_name() {
    let app = v1_app(seeded_db().await, FakeClient::new());
    let mut body = cardigann_body("Bad", 25, true, &json!([]));
    body["definitionName"] = json!("../etc");

    let response = app
        .oneshot(json_request("POST", "/api/v1/indexer", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string(), "body was: {problem}");
}

#[tokio::test]
async fn create_rejects_a_definition_name_that_does_not_exist() {
    let app = v1_app(seeded_db().await, FakeClient::new());
    let mut body = cardigann_body("Bad", 25, true, &json!([]));
    body["definitionName"] = json!("does-not-exist");

    let response = app
        .oneshot(json_request("POST", "/api/v1/indexer", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string(), "body was: {problem}");
}

#[tokio::test]
async fn create_rejects_an_unknown_implementation() {
    let app = v1_app(seeded_db().await, FakeClient::new());
    let mut body = cardigann_body("Bad", 25, true, &json!([]));
    body["implementation"] = json!("Bogus");

    let response = app
        .oneshot(json_request("POST", "/api/v1/indexer", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_and_delete_on_a_missing_id_both_return_404() {
    let app = v1_app(seeded_db().await, FakeClient::new());
    let body = cardigann_body("Ghost", 25, true, &json!([]));

    let response = app
        .clone()
        .oneshot(json_request("PUT", "/api/v1/indexer/999", &body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string());

    let response = app.oneshot(delete("/api/v1/indexer/999")).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string());
}

#[tokio::test]
async fn test_endpoint_returns_an_empty_array_on_a_successful_search() {
    let client = FakeClient::new().expect(
        |r| r.url.as_str() == "https://example.org/browse",
        ok_html("https://example.org/browse", SEARCH_HTML),
    );
    let app = v1_app(seeded_db().await, client);
    let body = cardigann_body("Probe", 25, true, &json!([]));

    let response = app
        .oneshot(json_request("POST", "/api/v1/indexer/test", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json, json!([]));
}

#[tokio::test]
async fn test_endpoint_returns_400_when_the_search_fails() {
    // No expectations set: the very first request the search makes fails
    // with FakeClient's own "unexpected request" transport error.
    let app = v1_app(seeded_db().await, FakeClient::new());
    let body = cardigann_body("Probe", 25, true, &json!([]));

    let response = app
        .oneshot(json_request("POST", "/api/v1/indexer/test", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string(), "body was: {problem}");
}

#[tokio::test]
async fn test_endpoint_returns_400_when_newznab_is_missing_base_url() {
    let app = v1_app(seeded_db().await, FakeClient::new());
    let body = json!({
        "name": "Upstream",
        "implementation": "Newznab",
        "definitionName": "upstream",
        "enable": true,
        "priority": 25,
        "fields": [],
    });

    let response = app
        .oneshot(json_request("POST", "/api/v1/indexer/test", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem = body_json(response).await;
    assert!(
        problem["message"].as_str().unwrap().contains("baseUrl"),
        "body was: {problem}"
    );
}

#[tokio::test]
async fn a_missing_key_is_rejected_with_a_json_problem_on_list() {
    let app = v1_app(seeded_db().await, FakeClient::new());

    let response = app.oneshot(get_no_key("/api/v1/indexer")).await.unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string(), "body was: {problem}");
}

/// `POST /api/v1/indexer` with a `fullSync` application already configured:
/// the create handler's app-sync trigger fires a real `POST
/// /api/v3/indexer` against that application (proven here by `app_client`'s
/// own `FakeClient` expectation actually being consumed — an unconsumed or
/// mismatched expectation would fail this test via `.unwrap()` below), and
/// the create response itself carries no `syncError` since that push
/// succeeded.
#[tokio::test]
async fn create_triggers_a_sync_push_to_a_fullsync_application_and_reports_no_sync_error() {
    let db = seeded_db().await;
    seed_full_sync_application(&db).await;
    let instance_key = ConfigRepo::new(&db).api_key().await.unwrap();
    let app_client = FakeClient::new().expect(
        move |req| {
            req.method == Method::Post
                && req.url.as_str() == "http://sonarr.example/api/v3/indexer"
                && req
                    .headers
                    .contains(&("X-Api-Key".to_string(), "sonarr-key".to_string()))
                && matches!(
                    &req.body,
                    Some(HttpBody::Json(value))
                        if value["implementation"] == "Torznab"
                            && value["fields"]
                                .as_array()
                                .unwrap()
                                .contains(&json!({"name": "apiKey", "value": instance_key}))
                )
        },
        sonarr_created(7),
    );
    let app = v1_app_with_app_client(db, FakeClient::new(), app_client);
    let body = cardigann_body(
        "My Indexer",
        25,
        true,
        &json!([{"name": "apiKey", "value": "secret"}]),
    );

    let response = app
        .oneshot(json_request("POST", "/api/v1/indexer", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let created = body_json(response).await;
    assert!(
        created.get("syncError").is_none() || created["syncError"].is_null(),
        "body was: {created}"
    );
}

/// Same trigger, but the application's own Sonarr is unreachable
/// (`app_client` has no expectations queued, so the very first request it
/// sees fails with `FakeClient`'s own "unexpected request" transport
/// error): the create write itself still succeeds (`201`), with the
/// failure surfaced only in the response's `syncError` field.
#[tokio::test]
async fn create_reports_a_sync_error_when_the_application_is_unreachable_but_still_answers_2xx() {
    let db = seeded_db().await;
    seed_full_sync_application(&db).await;
    let app = v1_app_with_app_client(db, FakeClient::new(), FakeClient::new());
    let body = cardigann_body("My Indexer", 25, true, &json!([]));

    let response = app
        .oneshot(json_request("POST", "/api/v1/indexer", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let created = body_json(response).await;
    assert!(
        created["syncError"].as_str().is_some(),
        "body was: {created}"
    );
}

/// `DELETE /api/v1/indexer/{id}`, with a `fullSync` application already
/// mapping that indexer: the remove handler must issue a real Sonarr
/// `DELETE /api/v3/indexer/{remote_id}` for it — proving the
/// snapshot-before-cascade fix (`crate::api::indexers::remove` reads the
/// mapping via `MappingRepo::for_indexer` *before* `IndexerRepo::delete`
/// destroys it via `ON DELETE CASCADE`) actually reaches the remote side,
/// not just the local one `oxidarr-db`'s own cascade tests already cover.
///
/// Asserted via `probe.requests()` (a clone of `app_client` taken before it
/// moves into `AppState`), not only via `FakeClient::expect`'s own
/// predicate: `remove`'s sync call is best-effort with its outcome
/// discarded, so an unmatched expectation wouldn't fail this test any other
/// way — inspecting the actually-recorded requests is what proves the call
/// really happened.
#[tokio::test]
async fn delete_triggers_a_remote_delete_for_a_fullsync_applications_mapped_indexer() {
    let db = seeded_db().await;
    let sonarr = seed_full_sync_application(&db).await;
    let indexer = IndexerRepo::new(&db)
        .insert(&NewIndexer {
            name: "My Indexer".to_string(),
            definition_id: "example".to_string(),
            kind: IndexerKind::Cardigann,
            enabled: true,
            settings: serde_json::Map::new(),
            priority: 25,
        })
        .await
        .unwrap();
    MappingRepo::new(&db)
        .set(MappingRow {
            app_id: sonarr.id,
            indexer_id: indexer.id,
            remote_indexer_id: RemoteIndexerId(42),
        })
        .await
        .unwrap();
    let app_client = FakeClient::new().expect(
        |req| {
            req.method == Method::Delete
                && req.url.as_str() == "http://sonarr.example/api/v3/indexer/42"
        },
        HttpResponse {
            status: 200,
            headers: vec![],
            body: b"{}".to_vec(),
            final_url: "http://sonarr.example/api/v3/indexer/42".parse().unwrap(),
        },
    );
    let probe = app_client.clone();
    let router = v1_app_with_app_client(db, FakeClient::new(), app_client);

    let response = router
        .oneshot(delete(&format!("/api/v1/indexer/{}", indexer.id.0)))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let requests = probe.requests();
    assert_eq!(
        requests.len(),
        1,
        "expected exactly one Sonarr call, got {requests:?}"
    );
    assert_eq!(requests[0].method, Method::Delete);
    assert_eq!(
        requests[0].url.as_str(),
        "http://sonarr.example/api/v3/indexer/42"
    );
}
