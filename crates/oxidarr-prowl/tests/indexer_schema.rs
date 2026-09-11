//! Integration tests for `GET /api/v1/indexer/schema`: the exact JSON shape
//! for a two-definition fixture directory (categories resolved per
//! definition, settings mapped to Prowlarr `Field`s including a `select`
//! with `selectOptions`), a broken definition file being skipped rather than
//! failing the whole listing, and auth enforcement — all driven via
//! `tower::ServiceExt::oneshot` against the real [`api_router`], following
//! `tests/api_router.rs`'s own helper style.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use oxidarr_db::Db;
use oxidarr_http::auth::ApiKey;
use oxidarr_indexer::testing::FakeClient;
use oxidarr_prowl::{AppState, DefinitionStore, api_router};
use tower::ServiceExt;

fn schema_defs_dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/indexer_schema"
    ))
}

fn broken_defs_dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/indexer_schema_broken"
    ))
}

async fn seeded_db() -> Db {
    Db::open_in_memory().await.unwrap()
}

/// Builds an `AppState` reading definitions out of `defs_dir`, otherwise
/// matching `tests/api_router.rs`'s own `state` helper.
fn state(db: Db, defs_dir: PathBuf) -> AppState<FakeClient> {
    AppState {
        db: Arc::new(db),
        tracker_client: FakeClient::new(),
        app_client: FakeClient::new(),
        defs: DefinitionStore::new(defs_dir),
        external_url: "http://oxidarr.local:9696".parse().unwrap(),
    }
}

fn fixed_start_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2024-01-01T00:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc)
}

fn v1_app(db: Db, key: &str, defs_dir: PathBuf) -> Router {
    api_router(state(db, defs_dir), ApiKey::new(key), fixed_start_time())
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
async fn schema_returns_the_exact_json_for_every_definition() {
    let app = v1_app(seeded_db().await, "s3cret", schema_defs_dir());

    let response = app
        .oneshot(get_with_header(
            "/api/v1/indexer/schema",
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
    let expected = concat!(
        "[",
        r#"{"name":"Alpha Tracker","implementation":"Cardigann","definitionName":"alpha","#,
        r#""description":"An alpha description","language":"en-US","enable":false,"priority":25,"#,
        r#""capabilities":{"categories":[{"id":2000,"name":"Movies"}]},"#,
        r#""fields":[{"name":"cookie","label":"Cookie","type":"textbox"},"#,
        r#"{"name":"sort","label":"Sort By","type":"select","value":"seeders","#,
        r#""selectOptions":[{"value":0,"name":"Seeders"},{"value":1,"name":"Size"}]}]},"#,
        r#"{"name":"Beta Tracker","implementation":"Cardigann","definitionName":"beta","#,
        r#""description":"A beta description","language":"en","enable":false,"priority":25,"#,
        r#""capabilities":{"categories":[{"id":5000,"name":"TV"}]},"#,
        r#""fields":[{"name":"apikey","label":"API Key","type":"password"},"#,
        r#"{"name":"freeleech","label":"Freeleech Only","type":"checkbox","value":true}]}"#,
        "]",
    );
    assert_eq!(body, expected);
}

#[tokio::test]
async fn a_broken_definition_file_is_skipped_not_fatal() {
    let app = v1_app(seeded_db().await, "s3cret", broken_defs_dir());

    let response = app
        .oneshot(get_with_header(
            "/api/v1/indexer/schema",
            "x-api-key",
            "s3cret",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let entries = json.as_array().unwrap();
    assert_eq!(entries.len(), 1, "body was: {body}");
    assert_eq!(entries[0]["definitionName"], "good");
}

#[tokio::test]
async fn a_missing_key_is_rejected_with_a_json_problem() {
    let app = v1_app(seeded_db().await, "s3cret", schema_defs_dir());

    let response = app.oneshot(get("/api/v1/indexer/schema")).await.unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = body_text(response).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["message"].is_string(), "body was: {body}");
}
