//! Integration tests for application CRUD (`GET`/`POST`/`PUT`/`DELETE
//! /api/v1/applications[/{id}]`) and `POST /api/v1/applications/test` —
//! [`crate::api::indexers`]'s sibling, driven the same way via
//! `tower::ServiceExt::oneshot` against the real `api_router`, following
//! `tests/indexers.rs`'s own helper style.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use oxidarr_db::{ConfigRepo, Db, IndexerKind, IndexerRepo, NewIndexer};
use oxidarr_http::auth::ApiKey;
use oxidarr_indexer::testing::FakeClient;
use oxidarr_indexer::{Body as HttpBody, HttpResponse, Method};
use oxidarr_prowl::{AppState, DefinitionStore, api_router};
use serde_json::{Value, json};
use tower::ServiceExt;

fn definitions_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/defs"))
}

const API_KEY: &str = "s3cret";

async fn seeded_db() -> Db {
    Db::open_in_memory().await.unwrap()
}

fn state(db: Db, client: FakeClient) -> AppState<FakeClient> {
    AppState {
        db: Arc::new(db),
        tracker_client: FakeClient::new(),
        app_client: client,
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

fn application_body(
    name: &str,
    implementation: &str,
    sync_level: &str,
    base_url: &str,
    api_key: &str,
) -> Value {
    json!({
        "name": name,
        "implementation": implementation,
        "syncLevel": sync_level,
        "fields": [
            {"name": "baseUrl", "value": base_url},
            {"name": "apiKey", "value": api_key},
        ],
    })
}

fn status_response(status: u16) -> HttpResponse {
    HttpResponse {
        status,
        headers: vec![],
        body: b"{}".to_vec(),
        final_url: "http://sonarr.example/api/v3/system/status"
            .parse()
            .unwrap(),
    }
}

/// A `201 Created`-shaped Sonarr `POST /api/v3/indexer` response carrying
/// `id`.
fn sonarr_created(id: i64) -> HttpResponse {
    HttpResponse {
        status: 201,
        headers: vec![],
        body: json!({"id": id}).to_string().into_bytes(),
        final_url: "http://sonarr.example:8989/api/v3/indexer".parse().unwrap(),
    }
}

/// Inserts one enabled `Cardigann`-kind indexer, so the app-sync
/// CRUD-trigger tests below have something for a newly created application
/// to push.
async fn seed_enabled_indexer(db: &Db) {
    IndexerRepo::new(db)
        .insert(&NewIndexer {
            name: "My Tracker".to_string(),
            definition_id: "example".to_string(),
            kind: IndexerKind::Cardigann,
            enabled: true,
            settings: serde_json::Map::new(),
            priority: 25,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn full_lifecycle_create_get_list_update_delete_then_404() {
    let app = v1_app(seeded_db().await, FakeClient::new());
    let input = application_body(
        "My Sonarr",
        "Sonarr",
        "fullSync",
        "http://sonarr.example:8989",
        "secret",
    );

    // create
    let response = app
        .clone()
        .oneshot(json_request("POST", "/api/v1/applications", &input))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = body_json(response).await;
    let id = created["id"].as_i64().unwrap();
    assert!(id > 0, "created body was: {created}");
    assert_eq!(created["name"], "My Sonarr");
    assert_eq!(created["implementation"], "Sonarr");
    assert_eq!(created["syncLevel"], "fullSync");
    assert_eq!(
        field_pairs(&created["fields"]),
        field_pairs(&input["fields"])
    );

    // get: round-trip law — the shipped fields equal the input exactly.
    let response = app
        .clone()
        .oneshot(get(&format!("/api/v1/applications/{id}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = body_json(response).await;
    assert_eq!(fetched, created, "get did not return what create returned");

    // list
    let response = app
        .clone()
        .oneshot(get("/api/v1/applications"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let listed = body_json(response).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0], created);

    // update
    let updated_input = application_body(
        "Renamed",
        "Sonarr",
        "disabled",
        "http://sonarr.example:9999",
        "new-secret",
    );
    let response = app
        .clone()
        .oneshot(json_request(
            "PUT",
            &format!("/api/v1/applications/{id}"),
            &updated_input,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let updated = body_json(response).await;
    assert_eq!(updated["id"], id);
    assert_eq!(updated["name"], "Renamed");
    assert_eq!(updated["syncLevel"], "disabled");
    assert_eq!(
        field_pairs(&updated["fields"]),
        field_pairs(&updated_input["fields"])
    );

    // get reflects the update
    let response = app
        .clone()
        .oneshot(get(&format!("/api/v1/applications/{id}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, updated);

    // delete
    let response = app
        .clone()
        .oneshot(delete(&format!("/api/v1/applications/{id}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(body_bytes(response).await.is_empty());

    // get after delete: 404 Problem naming the id
    let response = app
        .oneshot(get(&format!("/api/v1/applications/{id}")))
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
async fn every_sync_level_round_trips() {
    let app = v1_app(seeded_db().await, FakeClient::new());

    for (name, level) in [
        ("disabled-app", "disabled"),
        ("add-only-app", "addOnly"),
        ("full-sync-app", "fullSync"),
    ] {
        let input = application_body(
            name,
            "Radarr",
            level,
            "http://radarr.example:7878",
            "secret",
        );
        let response = app
            .clone()
            .oneshot(json_request("POST", "/api/v1/applications", &input))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let created = body_json(response).await;
        assert_eq!(created["syncLevel"], level, "level was: {level}");
        assert_eq!(created["implementation"], "Radarr");
    }
}

#[tokio::test]
async fn create_rejects_an_unknown_implementation() {
    let app = v1_app(seeded_db().await, FakeClient::new());
    let mut body = application_body("Bad", "Sonarr", "fullSync", "http://x:1", "key");
    body["implementation"] = json!("Bogus");

    let response = app
        .oneshot(json_request("POST", "/api/v1/applications", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string(), "body was: {problem}");
}

#[tokio::test]
async fn create_rejects_a_missing_base_url_field() {
    let app = v1_app(seeded_db().await, FakeClient::new());
    let body = json!({
        "name": "No Base Url",
        "implementation": "Sonarr",
        "syncLevel": "fullSync",
        "fields": [{"name": "apiKey", "value": "key"}],
    });

    let response = app
        .oneshot(json_request("POST", "/api/v1/applications", &body))
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
async fn create_rejects_an_empty_base_url_field() {
    let app = v1_app(seeded_db().await, FakeClient::new());
    let body = application_body("Empty Base Url", "Sonarr", "fullSync", "", "key");

    let response = app
        .oneshot(json_request("POST", "/api/v1/applications", &body))
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
async fn create_rejects_a_missing_api_key_field() {
    let app = v1_app(seeded_db().await, FakeClient::new());
    let body = json!({
        "name": "No Api Key",
        "implementation": "Sonarr",
        "syncLevel": "fullSync",
        "fields": [{"name": "baseUrl", "value": "http://x:1"}],
    });

    let response = app
        .oneshot(json_request("POST", "/api/v1/applications", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem = body_json(response).await;
    assert!(
        problem["message"].as_str().unwrap().contains("apiKey"),
        "body was: {problem}"
    );
}

#[tokio::test]
async fn create_rejects_an_invalid_base_url() {
    let app = v1_app(seeded_db().await, FakeClient::new());
    let body = application_body("Bad Url", "Sonarr", "fullSync", "not a url", "key");

    let response = app
        .oneshot(json_request("POST", "/api/v1/applications", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string(), "body was: {problem}");
}

#[tokio::test]
async fn update_rejects_an_invalid_base_url() {
    let app = v1_app(seeded_db().await, FakeClient::new());
    let input = application_body(
        "Good",
        "Sonarr",
        "fullSync",
        "http://sonarr.example:8989",
        "secret",
    );
    let response = app
        .clone()
        .oneshot(json_request("POST", "/api/v1/applications", &input))
        .await
        .unwrap();
    let created = body_json(response).await;
    let id = created["id"].as_i64().unwrap();

    let bad_update = application_body("Good", "Sonarr", "fullSync", "not a url", "secret");
    let response = app
        .oneshot(json_request(
            "PUT",
            &format!("/api/v1/applications/{id}"),
            &bad_update,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string(), "body was: {problem}");
}

#[tokio::test]
async fn update_and_delete_on_a_missing_id_both_return_404() {
    let app = v1_app(seeded_db().await, FakeClient::new());
    let body = application_body("Ghost", "Sonarr", "fullSync", "http://x:1", "key");

    let response = app
        .clone()
        .oneshot(json_request("PUT", "/api/v1/applications/999", &body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string());

    let response = app
        .oneshot(delete("/api/v1/applications/999"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string());
}

#[tokio::test]
async fn test_endpoint_probes_the_exact_status_url_and_header_on_success() {
    let client = FakeClient::new().expect(
        |r| {
            r.method == Method::Get
                && r.url.as_str() == "http://sonarr.example:8989/api/v3/system/status"
                && r.headers
                    .iter()
                    .any(|(k, v)| k == "X-Api-Key" && v == "secret")
        },
        status_response(200),
    );
    let app = v1_app(seeded_db().await, client);
    let body = application_body(
        "Probe",
        "Sonarr",
        "fullSync",
        "http://sonarr.example:8989",
        "secret",
    );

    let response = app
        .oneshot(json_request("POST", "/api/v1/applications/test", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json, json!([]));
}

#[tokio::test]
async fn test_endpoint_probes_radarr_at_the_same_api_v3_path() {
    let client = FakeClient::new().expect(
        |r| r.url.as_str() == "http://radarr.example:7878/api/v3/system/status",
        status_response(200),
    );
    let app = v1_app(seeded_db().await, client);
    let body = application_body(
        "Probe",
        "Radarr",
        "fullSync",
        "http://radarr.example:7878",
        "secret",
    );

    let response = app
        .oneshot(json_request("POST", "/api/v1/applications/test", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_endpoint_returns_400_on_a_non_2xx_status() {
    let client = FakeClient::new().expect(
        |r| r.url.as_str() == "http://sonarr.example:8989/api/v3/system/status",
        status_response(401),
    );
    let app = v1_app(seeded_db().await, client);
    let body = application_body(
        "Probe",
        "Sonarr",
        "fullSync",
        "http://sonarr.example:8989",
        "secret",
    );

    let response = app
        .oneshot(json_request("POST", "/api/v1/applications/test", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string(), "body was: {problem}");
}

#[tokio::test]
async fn test_endpoint_returns_400_on_a_transport_error() {
    // No expectations set: the very first request the probe makes fails
    // with FakeClient's own "unexpected request" transport error.
    let app = v1_app(seeded_db().await, FakeClient::new());
    let body = application_body(
        "Probe",
        "Sonarr",
        "fullSync",
        "http://sonarr.example:8989",
        "secret",
    );

    let response = app
        .oneshot(json_request("POST", "/api/v1/applications/test", &body))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string(), "body was: {problem}");
}

#[tokio::test]
async fn a_missing_key_is_rejected_with_a_json_problem_on_list() {
    let app = v1_app(seeded_db().await, FakeClient::new());

    let response = app
        .oneshot(get_no_key("/api/v1/applications"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string(), "body was: {problem}");
}

/// `POST /api/v1/applications` for a `fullSync` application, with an
/// enabled indexer already configured: the create handler's app-sync
/// trigger pushes that indexer into the new application right away (proven
/// by `client`'s own expectation being consumed), and the create response
/// carries no `syncError`.
#[tokio::test]
async fn create_triggers_a_sync_push_for_the_newly_created_application() {
    let db = seeded_db().await;
    seed_enabled_indexer(&db).await;
    let instance_key = ConfigRepo::new(&db).api_key().await.unwrap();
    let client = FakeClient::new().expect(
        move |req| {
            req.method == Method::Post
                && req.url.as_str() == "http://sonarr.example:8989/api/v3/indexer"
                && matches!(
                    &req.body,
                    Some(HttpBody::Json(value))
                        if value["fields"]
                            .as_array()
                            .unwrap()
                            .contains(&json!({"name": "apiKey", "value": instance_key}))
                )
        },
        sonarr_created(9),
    );
    let app = v1_app(db, client);
    let input = application_body(
        "My Sonarr",
        "Sonarr",
        "fullSync",
        "http://sonarr.example:8989",
        "secret",
    );

    let response = app
        .oneshot(json_request("POST", "/api/v1/applications", &input))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let created = body_json(response).await;
    assert!(
        created.get("syncError").is_none() || created["syncError"].is_null(),
        "body was: {created}"
    );
}

/// Same trigger, but the new application's own Sonarr is unreachable: the
/// create write itself still succeeds (`201`), with the failure surfaced
/// only in `syncError`.
#[tokio::test]
async fn create_reports_a_sync_error_when_the_new_application_is_unreachable() {
    let db = seeded_db().await;
    seed_enabled_indexer(&db).await;
    let app = v1_app(db, FakeClient::new());
    let input = application_body(
        "My Sonarr",
        "Sonarr",
        "fullSync",
        "http://sonarr.example:8989",
        "secret",
    );

    let response = app
        .oneshot(json_request("POST", "/api/v1/applications", &input))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let created = body_json(response).await;
    assert!(
        created["syncError"].as_str().is_some(),
        "body was: {created}"
    );
}
