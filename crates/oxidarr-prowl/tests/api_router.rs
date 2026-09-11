//! Integration tests for the `/api/v1` router: the auth layer (missing key,
//! wrong key, `X-Api-Key` header, `apikey` query param) and the two system
//! endpoints it mounts, `GET /api/v1/system/status` and `GET
//! /api/v1/health` — all driven via `tower::ServiceExt::oneshot` against the
//! real [`api_router`], plus a smoke test proving the Torznab router is
//! still reachable once [`app`] merges the two together.
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
use oxidarr_prowl::{AppState, DefinitionStore, api_router, app};
use tower::ServiceExt;

fn definitions_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/defs"))
}

async fn seeded_db() -> Db {
    Db::open_in_memory().await.unwrap()
}

/// Builds an `AppState` for the tests in this file. None of the
/// `/api/v1`-only tests below ever exercise `client`, so a bare
/// [`FakeClient`] with no expectations set is a fine stand-in for the real
/// transport, and the definitions directory need not contain anything the
/// test in question actually reads.
fn state(db: Db) -> AppState<FakeClient> {
    AppState {
        db: Arc::new(db),
        client: FakeClient::new(),
        defs: DefinitionStore::new(definitions_dir()),
    }
}

fn fixed_start_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2024-01-01T00:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc)
}

fn v1_app(db: Db, key: &str) -> Router {
    api_router(state(db), ApiKey::new(key), fixed_start_time())
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn get_with_header(uri: &str, name: &str, value: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(name, value)
        .body(Body::empty())
        .unwrap()
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn a_missing_key_is_rejected_with_a_json_problem() {
    let app = v1_app(seeded_db().await, "s3cret");

    let response = app.oneshot(get("/api/v1/system/status")).await.unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.contains("application/json"),
        "content type was: {content_type}"
    );
    let body = body_text(response).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["message"].is_string(), "body was: {body}");
}

#[tokio::test]
async fn a_wrong_key_is_rejected_with_a_json_problem() {
    let app = v1_app(seeded_db().await, "s3cret");

    let response = app
        .oneshot(get_with_header(
            "/api/v1/system/status",
            "x-api-key",
            "not-the-key",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = body_text(response).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["message"].is_string(), "body was: {body}");
}

#[tokio::test]
async fn the_x_api_key_header_is_accepted() {
    let app = v1_app(seeded_db().await, "s3cret");

    let response = app
        .oneshot(get_with_header(
            "/api/v1/system/status",
            "x-api-key",
            "s3cret",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_apikey_query_param_is_accepted() {
    let app = v1_app(seeded_db().await, "s3cret");

    let response = app
        .oneshot(get("/api/v1/system/status?apikey=s3cret"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn system_status_returns_the_exact_json() {
    let app = v1_app(seeded_db().await, "s3cret");

    let response = app
        .oneshot(get_with_header(
            "/api/v1/system/status",
            "x-api-key",
            "s3cret",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.contains("application/json"),
        "content type was: {content_type}"
    );
    let body = body_text(response).await;
    let expected = format!(
        concat!(
            r#"{{"appName":"Oxidarr","instanceName":"Oxidarr","version":"{}","#,
            r#""isProduction":true,"isDocker":false,"startTime":"2024-01-01T00:00:00+00:00","#,
            r#""appData":"","authentication":"none","urlBase":""}}"#,
        ),
        env!("CARGO_PKG_VERSION"),
    );
    assert_eq!(body, expected);
}

#[tokio::test]
async fn health_returns_an_empty_array() {
    let app = v1_app(seeded_db().await, "s3cret");

    let response = app
        .oneshot(get_with_header("/api/v1/health", "x-api-key", "s3cret"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert_eq!(body, "[]");
}

/// Smoke test: `app()` merges the Torznab router in with the auth-wrapped
/// `/api/v1` router, so the pre-existing Torznab behaviour (see
/// `torznab_router.rs`'s `wrong_apikey_returns_401_with_error_code_100`)
/// must still work end to end once the two are composed.
#[tokio::test]
async fn app_still_serves_torznab_once_composed_with_api_v1() {
    let db = seeded_db().await;
    let indexer_id = IndexerRepo::new(&db)
        .insert(&NewIndexer {
            name: "Example".to_string(),
            definition_id: "example".to_string(),
            kind: IndexerKind::Cardigann,
            enabled: true,
            settings: serde_json::Map::new(),
            priority: 0,
        })
        .await
        .unwrap()
        .id
        .0;
    let router = app(state(db)).await.unwrap();

    let response = router
        .oneshot(get(&format!("/{indexer_id}/api?apikey=not-the-key&t=caps")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert_eq!(content_type, "application/xml; charset=utf-8");
    let body = body_text(response).await;
    assert!(body.contains(r#"code="100""#), "body was: {body}");
}

/// Confirms `app()` also mounts a working `/api/v1` under the real
/// instance API key (`ConfigRepo::api_key`), not just the Torznab side.
#[tokio::test]
async fn app_also_serves_api_v1_under_the_real_instance_key() {
    let db = seeded_db().await;
    let key = ConfigRepo::new(&db).api_key().await.unwrap();
    let router = app(state(db)).await.unwrap();

    let response = router
        .oneshot(get(&format!("/api/v1/health?apikey={key}")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert_eq!(body, "[]");
}
