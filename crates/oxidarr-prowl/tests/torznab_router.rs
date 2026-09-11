//! Integration tests for the Torznab router (`GET /{indexer_id}/api`):
//! authentication, indexer lookup, `t=caps`, `t=search`/`t=tvsearch`
//! dispatch, and the Newznab/Torznab `ApiIndexer` wiring — all against a
//! real in-memory [`Db`] and axum [`Router`], driven via
//! `tower::ServiceExt::oneshot` rather than a live server.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use oxidarr_db::{ConfigRepo, Db, IndexerKind, IndexerRepo, NewIndexer};
use oxidarr_indexer::testing::{FakeClient, ok_html};
use oxidarr_prowl::{AppState, router};
use tower::ServiceExt;

/// The Cardigann definition every Cardigann-kind test uses:
/// `tests/fixtures/defs/example.yml`, mirroring `oxidarr-indexer`'s own
/// `cardigann_search.rs` fixture shape (a `tr.result` row with a
/// `td.name a`/`td.dl a`/`td.seeds`/`td.leech` field set), plus `q`,
/// `season`, `ep`, and `imdbid` search inputs so a
/// `t=search`/`t=tvsearch`/`t=movie` request's parameters are provably
/// threaded all the way into the tracker request URL.
fn definitions_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/defs"))
}

const SEARCH_HTML: &str = include_str!("fixtures/search.html");

async fn seeded_db() -> Db {
    Db::open_in_memory().await.unwrap()
}

async fn instance_api_key(db: &Db) -> String {
    ConfigRepo::new(db).api_key().await.unwrap()
}

fn settings(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| {
            (
                (*k).to_string(),
                serde_json::Value::String((*v).to_string()),
            )
        })
        .collect()
}

async fn insert_cardigann_indexer(db: &Db, name: &str, enabled: bool) -> i32 {
    let row = IndexerRepo::new(db)
        .insert(&NewIndexer {
            name: name.to_string(),
            definition_id: "example".to_string(),
            kind: IndexerKind::Cardigann,
            enabled,
            settings: settings(&[]),
            priority: 0,
        })
        .await
        .unwrap();
    row.id.0
}

async fn insert_newznab_indexer(db: &Db, base_url: &str, apikey: &str) -> i32 {
    let row = IndexerRepo::new(db)
        .insert(&NewIndexer {
            name: "Upstream Newznab".to_string(),
            definition_id: "upstream".to_string(),
            kind: IndexerKind::Newznab,
            enabled: true,
            settings: settings(&[("baseUrl", base_url), ("apiKey", apikey)]),
            priority: 0,
        })
        .await
        .unwrap();
    row.id.0
}

/// Mirrors [`insert_newznab_indexer`] but for [`IndexerKind::Torznab`] —
/// `ApiIndexer` is shared between the two kinds (see `oxidarr_indexer`'s
/// `indexer` module doc comment), so this proves the router's own
/// `IndexerKind::Torznab` dispatch arm wires through `TorznabIndexer`
/// specifically, not just that `NewznabIndexer` works.
async fn insert_torznab_indexer(db: &Db, base_url: &str, apikey: &str) -> i32 {
    let row = IndexerRepo::new(db)
        .insert(&NewIndexer {
            name: "Upstream Torznab".to_string(),
            definition_id: "upstream".to_string(),
            kind: IndexerKind::Torznab,
            enabled: true,
            settings: settings(&[("baseUrl", base_url), ("apiKey", apikey)]),
            priority: 0,
        })
        .await
        .unwrap();
    row.id.0
}

fn app(db: Db, client: FakeClient) -> (Router, FakeClient) {
    let probe = client.clone();
    let state = AppState {
        db: Arc::new(db),
        client,
        definitions_dir: definitions_dir(),
    };
    (router(state), probe)
}

fn get(uri: String) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn wrong_apikey_returns_401_with_error_code_100() {
    let db = seeded_db().await;
    let indexer_id = insert_cardigann_indexer(&db, "Example", true).await;
    let (app, _probe) = app(db, FakeClient::new());

    let response = app
        .oneshot(get(format!("/{indexer_id}/api?apikey=not-the-key&t=caps")))
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

#[tokio::test]
async fn a_missing_apikey_is_also_rejected() {
    let db = seeded_db().await;
    let indexer_id = insert_cardigann_indexer(&db, "Example", true).await;
    let (app, _probe) = app(db, FakeClient::new());

    let response = app
        .oneshot(get(format!("/{indexer_id}/api?t=caps")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unknown_indexer_id_returns_201() {
    let db = seeded_db().await;
    let key = instance_api_key(&db).await;
    let (app, _probe) = app(db, FakeClient::new());

    let response = app
        .oneshot(get(format!("/999/api?apikey={key}&t=search")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(body.contains(r#"code="201""#), "body was: {body}");
}

#[tokio::test]
async fn disabled_indexer_returns_201() {
    let db = seeded_db().await;
    let key = instance_api_key(&db).await;
    let indexer_id = insert_cardigann_indexer(&db, "Example", false).await;
    let (app, _probe) = app(db, FakeClient::new());

    let response = app
        .oneshot(get(format!("/{indexer_id}/api?apikey={key}&t=search")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(body.contains(r#"code="201""#), "body was: {body}");
}

#[tokio::test]
async fn t_caps_returns_the_exact_caps_xml() {
    let db = seeded_db().await;
    let key = instance_api_key(&db).await;
    let indexer_id = insert_cardigann_indexer(&db, "Example", true).await;
    let (app, _probe) = app(db, FakeClient::new());

    let response = app
        .oneshot(get(format!("/{indexer_id}/api?apikey={key}&t=caps")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert_eq!(content_type, "application/xml; charset=utf-8");
    let body = body_text(response).await;
    assert_eq!(
        body,
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<caps><server title="Example Tracker"/>"#,
            r#"<limits default="100" max="100"/>"#,
            r#"<searching>"#,
            r#"<search available="yes" supportedParams="q"/>"#,
            r#"<tv-search available="yes" supportedParams="q,season,ep"/>"#,
            r#"<movie-search available="no" supportedParams="q"/>"#,
            r#"</searching>"#,
            r#"<categories>"#,
            r#"<category id="2000" name="Movies"><subcat id="2040" name="Movies/HD"/></category>"#,
            r#"<category id="5000" name="TV"><subcat id="5030" name="TV/SD"/></category>"#,
            r#"</categories></caps>"#,
        )
    );
}

#[tokio::test]
async fn t_search_end_to_end_returns_the_exact_results_xml_and_threads_q() {
    // Also covers threading `q` into the tracker request URL: the fixture
    // definition declares a `q` input (`{{ .Query.Q }}`), so `q=ubuntu`
    // lands in the actual tracker request rather than being an unconsumed
    // query parameter this test merely happened to pass.
    let db = seeded_db().await;
    let key = instance_api_key(&db).await;
    let indexer_id = insert_cardigann_indexer(&db, "My Example Indexer", true).await;
    let client = FakeClient::new().expect(
        |r| r.url.as_str() == "https://example.org/browse?q=ubuntu",
        ok_html("https://example.org/browse", SEARCH_HTML),
    );
    let (app, probe) = app(db, client);

    let response = app
        .oneshot(get(format!(
            "/{indexer_id}/api?apikey={key}&t=search&q=ubuntu"
        )))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert_eq!(content_type, "application/xml; charset=utf-8");
    let body = body_text(response).await;
    assert_eq!(
        body,
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed">"#,
            r#"<channel><title>My Example Indexer</title>"#,
            r#"<item>"#,
            r#"<title>Big.Buck.Bunny.2008.1080p.BluRay.x264</title>"#,
            r#"<guid>/details/1</guid>"#,
            r#"<comments>/details/1</comments>"#,
            r#"<link>/download/1.torrent</link>"#,
            r#"<enclosure url="/download/1.torrent" type="application/x-bittorrent"/>"#,
            r#"<torznab:attr name="seeders" value="120"/>"#,
            r#"<torznab:attr name="peers" value="127"/>"#,
            r#"<torznab:attr name="downloadvolumefactor" value="1"/>"#,
            r#"<torznab:attr name="uploadvolumefactor" value="1"/>"#,
            r#"</item></channel></rss>"#,
        )
    );
    let requests = probe.requests();
    assert_eq!(requests.len(), 1, "expected exactly one tracker request");
    assert_eq!(
        requests[0].url.as_str(),
        "https://example.org/browse?q=ubuntu"
    );
}

#[tokio::test]
async fn t_tvsearch_maps_season_and_episode_into_the_tracker_request() {
    let db = seeded_db().await;
    let key = instance_api_key(&db).await;
    let indexer_id = insert_cardigann_indexer(&db, "Example", true).await;
    let client = FakeClient::new().expect(
        |r| r.url.as_str() == "https://example.org/browse?ep=5&q=show&season=2",
        ok_html("https://example.org/browse", SEARCH_HTML),
    );
    let (app, probe) = app(db, client);

    let response = app
        .oneshot(get(format!(
            "/{indexer_id}/api?apikey={key}&t=tvsearch&q=show&season=2&ep=5"
        )))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = probe.requests();
    assert_eq!(requests.len(), 1, "expected exactly one tracker request");
    assert_eq!(
        requests[0].url.as_str(),
        "https://example.org/browse?ep=5&q=show&season=2"
    );
}

#[tokio::test]
async fn bad_t_returns_code_202() {
    // Newznab convention (and `crate::torznab`'s own citations): 202 is "no
    // such function", not the 203 this router used to ship.
    let db = seeded_db().await;
    let key = instance_api_key(&db).await;
    let indexer_id = insert_cardigann_indexer(&db, "Example", true).await;
    let (app, _probe) = app(db, FakeClient::new());

    let response = app
        .oneshot(get(format!("/{indexer_id}/api?apikey={key}&t=bogus")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(body.contains(r#"code="202""#), "body was: {body}");
}

#[tokio::test]
async fn missing_t_returns_code_200() {
    // Newznab convention: 200 is "missing parameter", not the 203 this
    // router used to ship.
    let db = seeded_db().await;
    let key = instance_api_key(&db).await;
    let indexer_id = insert_cardigann_indexer(&db, "Example", true).await;
    let (app, _probe) = app(db, FakeClient::new());

    let response = app
        .oneshot(get(format!("/{indexer_id}/api?apikey={key}")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(body.contains(r#"code="200""#), "body was: {body}");
}

const NEWZNAB_FEED: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8"?>"#,
    r#"<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed">"#,
    r#"<channel><title>Feed</title>"#,
    r#"<item>"#,
    r#"<title>Some.Release.2026</title>"#,
    r#"<guid>https://api.example.com/details/9</guid>"#,
    r#"<link>https://api.example.com/download/9.nzb</link>"#,
    r#"<torznab:attr name="seeders" value="10"/>"#,
    r#"</item></channel></rss>"#,
);

#[tokio::test]
async fn newznab_row_wires_through_the_api_indexer() {
    let db = seeded_db().await;
    let key = instance_api_key(&db).await;
    let indexer_id = insert_newznab_indexer(&db, "https://api.example.com/", "upstream-key").await;
    let client = FakeClient::new().expect(
        |r| r.url.as_str() == "https://api.example.com/api?t=search&apikey=upstream-key",
        ok_html("https://api.example.com/api", NEWZNAB_FEED),
    );
    let (app, probe) = app(db, client);

    let response = app
        .oneshot(get(format!("/{indexer_id}/api?apikey={key}&t=search")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(
        body.contains("<title>Some.Release.2026</title>"),
        "body was: {body}"
    );
    assert!(
        body.contains("https://api.example.com/download/9.nzb"),
        "body was: {body}"
    );
    let requests = probe.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url.as_str(),
        "https://api.example.com/api?t=search&apikey=upstream-key"
    );
}

#[tokio::test]
async fn a_missing_definition_file_renders_a_300_error_for_caps() {
    let db = seeded_db().await;
    let key = instance_api_key(&db).await;
    let row_id = IndexerRepo::new(&db)
        .insert(&NewIndexer {
            name: "Ghost".to_string(),
            definition_id: "does-not-exist".to_string(),
            kind: IndexerKind::Cardigann,
            enabled: true,
            settings: settings(&[]),
            priority: 0,
        })
        .await
        .unwrap()
        .id
        .0;
    let (app, _probe) = app(db, FakeClient::new());

    let response = app
        .oneshot(get(format!("/{row_id}/api?apikey={key}&t=caps")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(body.contains(r#"code="300""#), "body was: {body}");
}

/// Regression pin: `search_row` already treats a missing `baseUrl` setting
/// on a Newznab/Torznab row as a `300` error (see `server.rs`'s
/// `search_row`) — this was previously unexercised by any test in this
/// file.
#[tokio::test]
async fn a_newznab_row_missing_the_base_url_setting_renders_a_300_error() {
    let db = seeded_db().await;
    let key = instance_api_key(&db).await;
    let row_id = IndexerRepo::new(&db)
        .insert(&NewIndexer {
            name: "No Base Url".to_string(),
            definition_id: "upstream".to_string(),
            kind: IndexerKind::Newznab,
            enabled: true,
            settings: settings(&[("apiKey", "upstream-key")]),
            priority: 0,
        })
        .await
        .unwrap()
        .id
        .0;
    let (app, _probe) = app(db, FakeClient::new());

    let response = app
        .oneshot(get(format!("/{row_id}/api?apikey={key}&t=search")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(body.contains(r#"code="300""#), "body was: {body}");
}

/// Regression pin: mirrors [`newznab_row_wires_through_the_api_indexer`],
/// proving `IndexerKind::Torznab` rows dispatch through `TorznabIndexer`
/// end to end, not just `IndexerKind::Newznab` ones.
#[tokio::test]
async fn torznab_row_wires_through_the_api_indexer() {
    let db = seeded_db().await;
    let key = instance_api_key(&db).await;
    let indexer_id = insert_torznab_indexer(&db, "https://api.example.com/", "upstream-key").await;
    let client = FakeClient::new().expect(
        |r| r.url.as_str() == "https://api.example.com/api?t=search&apikey=upstream-key",
        ok_html("https://api.example.com/api", NEWZNAB_FEED),
    );
    let (app, probe) = app(db, client);

    let response = app
        .oneshot(get(format!("/{indexer_id}/api?apikey={key}&t=search")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(
        body.contains("<title>Some.Release.2026</title>"),
        "body was: {body}"
    );
    assert!(
        body.contains("https://api.example.com/download/9.nzb"),
        "body was: {body}"
    );
    let requests = probe.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url.as_str(),
        "https://api.example.com/api?t=search&apikey=upstream-key"
    );
}

/// Regression pin: mirrors
/// [`a_missing_definition_file_renders_a_300_error_for_caps`] but for
/// `t=search` — `search_row`'s `IndexerKind::Cardigann` arm calls the same
/// `load_definition` `t=caps` does, so a missing/unparseable definition file
/// renders the same `300` on a search request too.
#[tokio::test]
async fn a_missing_definition_file_renders_a_300_error_for_search() {
    let db = seeded_db().await;
    let key = instance_api_key(&db).await;
    let row_id = IndexerRepo::new(&db)
        .insert(&NewIndexer {
            name: "Ghost".to_string(),
            definition_id: "does-not-exist".to_string(),
            kind: IndexerKind::Cardigann,
            enabled: true,
            settings: settings(&[]),
            priority: 0,
        })
        .await
        .unwrap()
        .id
        .0;
    let (app, _probe) = app(db, FakeClient::new());

    let response = app
        .oneshot(get(format!("/{row_id}/api?apikey={key}&t=search")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(body.contains(r#"code="300""#), "body was: {body}");
}

#[tokio::test]
async fn t_movie_threads_imdbid_into_the_tracker_request() {
    // The fixture definition declares an `imdbid` input
    // (`{{ .Query.IMDBID }}`), so this proves `t=movie`'s `imdbid` query
    // parameter is threaded all the way into the tracker request URL —
    // mirroring `t_tvsearch_maps_season_and_episode_into_the_tracker_request`
    // for `season`/`ep`.
    let db = seeded_db().await;
    let key = instance_api_key(&db).await;
    let indexer_id = insert_cardigann_indexer(&db, "Example", true).await;
    let client = FakeClient::new().expect(
        |r| r.url.as_str() == "https://example.org/browse?imdbid=tt1234567",
        ok_html("https://example.org/browse", SEARCH_HTML),
    );
    let (app, probe) = app(db, client);

    let response = app
        .oneshot(get(format!(
            "/{indexer_id}/api?apikey={key}&t=movie&imdbid=tt1234567"
        )))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = probe.requests();
    assert_eq!(requests.len(), 1, "expected exactly one tracker request");
    assert_eq!(
        requests[0].url.as_str(),
        "https://example.org/browse?imdbid=tt1234567"
    );
}
