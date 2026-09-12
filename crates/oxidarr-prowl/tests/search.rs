//! Integration tests for the multi-indexer search endpoint (`GET
//! /api/v1/search`) — driven via `tower::ServiceExt::oneshot` against the
//! real [`api_router`], following `tests/indexers.rs`/`tests/applications.rs`'s
//! own helper style.
//!
//! Every indexer seeded here is `Newznab`-kind (via `IndexerRepo::insert`
//! directly, mirroring `tests/torznab_router.rs`'s own
//! `insert_newznab_indexer` helper) rather than `Cardigann`: this endpoint's
//! own indexer-selection/aggregation logic is what's under test, not a
//! second exercise of the Cardigann engine (already covered elsewhere), and
//! a bare `baseUrl` row needs no YAML fixture at all.
//!
//! # A note on ordering and `FakeClient`
//!
//! [`oxidarr_indexer::testing::FakeClient`] matches expectations strictly in
//! registration order against a single shared queue (see its own doc
//! comment). This endpoint runs its indexer searches concurrently via a
//! `tokio::task::JoinSet`, so which of two concurrently-running indexers'
//! requests reaches the client *first* is not guaranteed by anything this
//! test file controls. Every test below that exercises more than one
//! concurrent request therefore registers same-shaped expectations (a
//! matcher that accepts any request) and asserts on the *aggregate* outcome
//! (how many releases came back, whether both indexer names appear in a
//! failure message) rather than on which specific indexer produced which
//! specific result — the one assertion style that stays correct regardless
//! of scheduling order.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use oxidarr_db::{Db, IndexerKind, IndexerRepo, NewIndexer};
use oxidarr_http::auth::ApiKey;
use oxidarr_indexer::HttpResponse;
use oxidarr_indexer::testing::FakeClient;
use oxidarr_prowl::{AppState, DefinitionStore, api_router};
use serde_json::Value;
use tower::ServiceExt;

const API_KEY: &str = "s3cret";

fn definitions_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/defs"))
}

async fn seeded_db() -> Db {
    Db::open_in_memory().await.unwrap()
}

fn state(db: Db, client: FakeClient) -> AppState<FakeClient> {
    AppState {
        db: Arc::new(db),
        tracker_client: client,
        app_client: FakeClient::new(),
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

async fn insert_newznab_indexer(db: &Db, name: &str, base_url: &str, enabled: bool) -> i32 {
    let mut settings = serde_json::Map::new();
    settings.insert(
        "baseUrl".to_string(),
        serde_json::Value::String(base_url.to_string()),
    );
    let row = IndexerRepo::new(db)
        .insert(&NewIndexer {
            name: name.to_string(),
            definition_id: "upstream".to_string(),
            kind: IndexerKind::Newznab,
            enabled,
            settings,
            priority: 0,
        })
        .await
        .unwrap();
    row.id.0
}

/// A minimal Newznab/Torznab RSS feed with one `<item>`, carrying `title`
/// and a `torznab:attr name="seeders"` value — enough for this endpoint's
/// sort-by-seeders behaviour to be provable.
fn feed_with_one_item(title: &str, seeders: u32) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <title>Fixture</title>
    <item>
      <title>{title}</title>
      <link>https://example.org/download/{title}</link>
      <pubDate>Sun, 06 Sep 2026 12:00:00 +0000</pubDate>
      <torznab:attr name="seeders" value="{seeders}" />
    </item>
  </channel>
</rss>"#
    )
}

/// The two-item feed [`two_enabled_indexers_one_succeeds_one_fails_returns_only_the_successes`]
/// uses to prove the seeders-descending sort: items deliberately out of
/// seeder order in the feed itself.
fn feed_with_two_items() -> String {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <title>Fixture</title>
    <item>
      <title>Low.Seeders</title>
      <link>https://example.org/low</link>
      <pubDate>Sun, 06 Sep 2026 12:00:00 +0000</pubDate>
      <torznab:attr name="seeders" value="3" />
    </item>
    <item>
      <title>High.Seeders</title>
      <link>https://example.org/high</link>
      <pubDate>Sun, 06 Sep 2026 12:00:00 +0000</pubDate>
      <torznab:attr name="seeders" value="99" />
    </item>
  </channel>
</rss>"#
        .to_string()
}

/// A three-item feed with `Some(50)`, no `seeders` attr at all (→ `None`),
/// and `Some(80)`, in that (deliberately non-sorted) feed order — used by
/// `a_release_with_no_seeders_sorts_last_after_two_with_seeders` to prove the
/// seeders-descending / `None`-last sort actually handles a mix of `Some`
/// and `None`, not just two `Some`s (see [`feed_with_two_items`]).
fn feed_with_three_items_mixed_seeders() -> String {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <title>Fixture</title>
    <item>
      <title>Mid.Seeders</title>
      <link>https://example.org/mid</link>
      <pubDate>Sun, 06 Sep 2026 12:00:00 +0000</pubDate>
      <torznab:attr name="seeders" value="50" />
    </item>
    <item>
      <title>No.Seeders</title>
      <link>https://example.org/none</link>
      <pubDate>Sun, 06 Sep 2026 12:00:00 +0000</pubDate>
    </item>
    <item>
      <title>High.Seeders</title>
      <link>https://example.org/high</link>
      <pubDate>Sun, 06 Sep 2026 12:00:00 +0000</pubDate>
      <torznab:attr name="seeders" value="80" />
    </item>
  </channel>
</rss>"#
        .to_string()
}

fn xml_response(status: u16, final_url: &str, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: vec![("content-type".to_string(), "application/xml".to_string())],
        body: body.as_bytes().to_vec(),
        final_url: final_url.parse().unwrap(),
    }
}

/// Matches any request at all — used by every test below that exercises
/// more than one concurrent indexer, where matching on the request's own
/// content would depend on scheduling order (see this file's module docs).
fn any_request(_: &oxidarr_indexer::HttpRequest) -> bool {
    true
}

#[tokio::test]
async fn zero_configured_indexers_returns_200_empty_array() {
    let app = v1_app(seeded_db().await, FakeClient::new());

    let response = app
        .oneshot(get("/api/v1/search?query=ubuntu"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, serde_json::json!([]));
}

#[tokio::test]
async fn every_indexer_disabled_returns_200_empty_array_and_makes_no_requests() {
    let db = seeded_db().await;
    insert_newznab_indexer(&db, "Disabled", "https://a.example", false).await;
    let client = FakeClient::new();
    let app = v1_app(db, client.clone());

    let response = app.oneshot(get("/api/v1/search")).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, serde_json::json!([]));
    assert!(client.requests().is_empty());
}

#[tokio::test]
async fn a_disabled_indexer_is_excluded_from_an_unfiltered_search() {
    let db = seeded_db().await;
    insert_newznab_indexer(&db, "Enabled", "https://a.example", true).await;
    insert_newznab_indexer(&db, "Disabled", "https://b.example", false).await;
    let client = FakeClient::new().expect(
        any_request,
        xml_response(
            200,
            "https://a.example/api",
            &feed_with_one_item("Only.Result", 5),
        ),
    );
    let app = v1_app(db, client.clone());

    let response = app.oneshot(get("/api/v1/search")).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let releases = json.as_array().unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0]["title"], "Only.Result");
    assert_eq!(client.requests().len(), 1, "disabled indexer must not run");
}

#[tokio::test]
async fn indexer_ids_filter_selects_only_the_named_indexer() {
    let db = seeded_db().await;
    let wanted_id = insert_newznab_indexer(&db, "Wanted", "https://a.example", true).await;
    insert_newznab_indexer(&db, "Other", "https://b.example", true).await;
    let client = FakeClient::new().expect(
        |r| r.url.host_str() == Some("a.example"),
        xml_response(
            200,
            "https://a.example/api",
            &feed_with_one_item("Wanted.Result", 5),
        ),
    );
    let app = v1_app(db, client.clone());

    let response = app
        .oneshot(get(&format!("/api/v1/search?indexerIds={wanted_id}")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let releases = json.as_array().unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0]["title"], "Wanted.Result");
    assert_eq!(releases[0]["indexerId"], wanted_id);
    assert_eq!(client.requests().len(), 1);
}

#[tokio::test]
async fn two_enabled_indexers_one_succeeds_one_fails_returns_only_the_successes() {
    let db = seeded_db().await;
    insert_newznab_indexer(&db, "Good", "https://a.example", true).await;
    insert_newznab_indexer(&db, "Bad", "https://b.example", true).await;
    // Order-independent by design (see module docs): the first request to
    // arrive gets the success response with two differently-seeded
    // releases, the second gets a 500. Whichever indexer that turns out to
    // be, exactly one release set survives — with two releases so the
    // seeders-descending sort is provable within it.
    let client = FakeClient::new()
        .expect(
            any_request,
            xml_response(200, "https://example.org/api", &feed_with_two_items()),
        )
        .expect(
            any_request,
            xml_response(500, "https://example.org/api", ""),
        );
    let app = v1_app(db, client);

    let response = app.oneshot(get("/api/v1/search")).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let releases = json.as_array().unwrap();
    assert_eq!(releases.len(), 2, "body was: {json}");
    assert_eq!(releases[0]["title"], "High.Seeders");
    assert_eq!(releases[0]["seeders"], 99);
    assert_eq!(releases[1]["title"], "Low.Seeders");
    assert_eq!(releases[1]["seeders"], 3);
}

/// Regression pin (fix round 1, finding 1): the seeders-descending sort's
/// `None`-last rule was only ever exercised across two `Some` values
/// (`two_enabled_indexers_one_succeeds_one_fails_returns_only_the_successes`);
/// nothing mixed a `None` (a realistic Newznab response — plenty of
/// indexers just don't report seeders on some items) in with `Some`s to
/// prove it sorts last rather than, say, first (`Option`'s natural `Ord`
/// puts `None` *first* — see `search.rs`'s own `sort_by_key(Reverse(...))`
/// doc comment for why that's inverted here).
#[tokio::test]
async fn a_release_with_no_seeders_sorts_last_after_two_with_seeders() {
    let db = seeded_db().await;
    insert_newznab_indexer(&db, "Mixed", "https://a.example", true).await;
    let client = FakeClient::new().expect(
        any_request,
        xml_response(
            200,
            "https://a.example/api",
            &feed_with_three_items_mixed_seeders(),
        ),
    );
    let app = v1_app(db, client);

    let response = app.oneshot(get("/api/v1/search")).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let releases = json.as_array().unwrap();
    assert_eq!(releases.len(), 3, "body was: {json}");
    assert_eq!(releases[0]["title"], "High.Seeders");
    assert_eq!(releases[0]["seeders"], 80);
    assert_eq!(releases[1]["title"], "Mid.Seeders");
    assert_eq!(releases[1]["seeders"], 50);
    assert_eq!(releases[2]["title"], "No.Seeders");
    assert_eq!(releases[2]["seeders"], Value::Null);
}

/// Regression pin (fix round 1, finding 2): `indexerIds` naming an id that
/// does not exist alongside one that does was documented
/// (`search.rs`'s "Indexer selection" section: "an id that does not exist
/// ... is silently not searched") but never actually tested — only a
/// same-length, all-real `indexerIds` list was exercised
/// (`indexer_ids_filter_selects_only_the_named_indexer`).
#[tokio::test]
async fn indexer_ids_naming_one_real_and_one_nonexistent_id_returns_only_the_real_results() {
    let db = seeded_db().await;
    let real_id = insert_newznab_indexer(&db, "Real", "https://a.example", true).await;
    let nonexistent_id = real_id + 999;
    let client = FakeClient::new().expect(
        any_request,
        xml_response(
            200,
            "https://a.example/api",
            &feed_with_one_item("Real.Result", 5),
        ),
    );
    let app = v1_app(db, client.clone());

    let response = app
        .oneshot(get(&format!(
            "/api/v1/search?indexerIds={real_id},{nonexistent_id}"
        )))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let releases = json.as_array().unwrap();
    assert_eq!(releases.len(), 1, "body was: {json}");
    assert_eq!(releases[0]["title"], "Real.Result");
    assert_eq!(
        client.requests().len(),
        1,
        "the nonexistent id must not run"
    );
}

#[tokio::test]
async fn all_enabled_indexers_failing_returns_400_naming_both() {
    let db = seeded_db().await;
    insert_newznab_indexer(&db, "First Indexer", "https://a.example", true).await;
    insert_newznab_indexer(&db, "Second Indexer", "https://b.example", true).await;
    let client = FakeClient::new()
        .expect(
            any_request,
            xml_response(500, "https://example.org/api", ""),
        )
        .expect(
            any_request,
            xml_response(500, "https://example.org/api", ""),
        );
    let app = v1_app(db, client);

    let response = app.oneshot(get("/api/v1/search")).await.unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let problem = body_json(response).await;
    let message = problem["message"].as_str().unwrap();
    assert!(message.contains("First Indexer"), "message was: {message}");
    assert!(message.contains("Second Indexer"), "message was: {message}");
}

#[tokio::test]
async fn categories_and_query_are_forwarded_to_the_upstream_request() {
    let db = seeded_db().await;
    insert_newznab_indexer(&db, "Only", "https://a.example", true).await;
    let client = FakeClient::new().expect(
        any_request,
        xml_response(200, "https://a.example/api", &feed_with_one_item("Hit", 1)),
    );
    let app = v1_app(db, client.clone());

    let response = app
        .oneshot(get("/api/v1/search?query=ubuntu&categories=5000,5030"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = client.requests();
    assert_eq!(requests.len(), 1);
    let sent = requests[0].url.as_str();
    assert!(sent.contains("q=ubuntu"), "sent url was: {sent}");
    assert!(sent.contains("cat=5000%2C5030"), "sent url was: {sent}");
}

#[tokio::test]
async fn a_missing_key_is_rejected_with_a_json_problem() {
    let app = v1_app(seeded_db().await, FakeClient::new());

    let response = app.oneshot(get_no_key("/api/v1/search")).await.unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let problem = body_json(response).await;
    assert!(problem["message"].is_string(), "body was: {problem}");
}
