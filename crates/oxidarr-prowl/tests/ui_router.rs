//! Confirms the `ui` feature's SPA fallback, once merged into [`app`],
//! never shadows the pre-existing `/api/v1` or Torznab routes — the
//! precedence [`crate::server::app`]'s docs (and this crate's `ui` feature)
//! promise. `oxidarr_prowl::ui`'s own inline tests (in `src/ui.rs`, run
//! against a fixture bundle) cover the UI's own serving behavior — index
//! html, assets, the SPA fallback itself — so this file only proves the
//! composition, not that serving logic again.
//!
//! Only compiled with `--features ui`: without it, `oxidarr_prowl::app`
//! never touches the UI at all, so there is nothing here to test.
#![cfg(feature = "ui")]
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use oxidarr_db::{Db, IndexerKind, IndexerRepo, NewIndexer};
use oxidarr_indexer::testing::FakeClient;
use oxidarr_prowl::{AppState, DefinitionStore, app};
use tower::ServiceExt;

fn definitions_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/defs"))
}

async fn seeded_db() -> Db {
    Db::open_in_memory().await.unwrap()
}

fn state(db: Db) -> AppState<FakeClient> {
    AppState {
        db: Arc::new(db),
        tracker_client: FakeClient::new(),
        app_client: FakeClient::new(),
        defs: DefinitionStore::new(definitions_dir()),
        external_url: "http://oxidarr.local:9696".parse().unwrap(),
    }
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn api_v1_still_requires_a_key_with_the_ui_feature_on() {
    let router = app(state(seeded_db().await)).await.unwrap();

    let response = router.oneshot(get("/api/v1/system/status")).await.unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_torznab_route_is_still_reachable_with_the_ui_feature_on() {
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
}

/// A path that matches neither `/api/v1/*` nor `/{indexer_id}/api` used to
/// 404 (there was no fallback at all); with `ui` on, it must fall through
/// to the UI's SPA fallback instead — proving the fallback is actually
/// wired in, not just present-but-unreachable.
#[tokio::test]
async fn an_unrelated_get_path_falls_through_to_the_ui_spa_fallback() {
    let router = app(state(seeded_db().await)).await.unwrap();

    let response = router.oneshot(get("/some/spa/route")).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

/// A typo'd or not-yet-implemented `/api/v1/*` endpoint must 404 through
/// the *composed* `app()`, exactly like it did before this feature
/// existed — not silently 200 as the SPA shell. Unlike the two tests
/// above (which exercise paths axum's `path_router` matches directly),
/// this path matches nothing in `path_router` at all, so without
/// `crate::ui::is_reserved_path`'s guard it falls straight through to the
/// UI's now-global fallback (`Router::merge` promotes a fallback
/// app-wide — see `crate::ui`'s module docs for the exact axum-source
/// citation this relies on).
#[tokio::test]
async fn an_unimplemented_api_v1_path_is_not_found_not_the_spa_shell() {
    let router = app(state(seeded_db().await)).await.unwrap();

    let response = router.oneshot(get("/api/v1/does-not-exist")).await.unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_text(response).await;
    assert!(
        !body.contains("oxidarr-app"),
        "the SPA shell answered for an /api/v1 path: {body}"
    );
}

/// Proves the *production* embed (`crate::ui::router`, backed by the real
/// `DIST`) actually serves the real app shell — not the placeholder stub
/// committed at `crates/oxidarr-ui/dist/index.html` for a plain
/// `cargo build --features ui` to compile without a prior `dx build`.
/// Ignored by default: locally, without a real `dx build`, the stub is
/// deliberately marker-less (see that file's own comment) and this fails
/// on purpose. CI's `ui` job runs `dx build --release` first and then
/// runs with `--include-ignored`, so it's the one place this actually
/// exercises the real bundle.
#[ignore = "run in the ui CI job after `dx build` — the local dist/ is a marker-less stub"]
#[tokio::test]
async fn the_real_embedded_bundle_serves_the_app_shell() {
    let router = oxidarr_prowl::ui::router();

    let response = router.oneshot(get("/")).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(body.contains(r#"id="oxidarr-app""#), "body was: {body}");
}
