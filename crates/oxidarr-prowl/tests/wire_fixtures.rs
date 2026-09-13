//! Pins the canonical JSON fixtures in `tests/fixtures/wire/` from this
//! crate's own DTOs: `oxidarr_prowl::api::dto`'s public types directly, and
//! a real `GET /system/status` response for `SystemStatus` (private to
//! `api::system`, so this file exercises the endpoint itself rather than
//! constructing that type by hand). `crates/oxidarr-ui/tests/wire_parity.rs`
//! reads the exact same files by relative path and proves the UI's own
//! mirrored DTOs round-trip them byte-for-byte — this file is the
//! "source of truth" half of that two-sided pin: it fails the moment this
//! crate's own wire shape changes without the committed fixture being
//! updated to match.
//!
//! Six fixtures, one assertion each, chosen for variety rather than one
//! happy-path sample per type: a release with `seeders: null`, a
//! magnet-only release (`magnetUrl` set, no `downloadUrl`), an indexer
//! schema entry with a `select` field (carrying `selectOptions`), an
//! indexer resource and an application resource each carrying a
//! `syncError`, and the system status response.
//!
//! # Canonical format
//!
//! Every fixture is `serde_json::to_string_pretty`'s output (that
//! function's own default: two-space indent) plus one trailing `\n`.
//! Chosen over the compact `to_string` form because it reviews far better
//! in a `git diff` for a resource with a dozen fields, and over any
//! hand-rolled formatter because it needs no extra dependency and both
//! sides already depend on `serde_json` for everything else.
//!
//! # Updating a fixture
//!
//! A deliberate shape change (a field added/renamed/removed on the actual
//! DTO) is not, by itself, a reason to hand-edit the committed `.json`
//! file: update the Rust sample in this file to match the new shape, run
//! `cargo test -p oxidarr-prowl --test wire_fixtures`, and let it fail —
//! the assertion failure's own "left"/"right" diff is the new canonical
//! rendering; copy it into the fixture file verbatim (including the
//! trailing newline), rerun this test until it's green, then rerun
//! `cargo test -p oxidarr-ui --test wire_parity` to confirm the UI side's
//! mirrored DTO still parses the updated fixture — a genuine
//! cross-crate incompatibility is exactly what this two-sided pin exists
//! to catch before it reaches a real client.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use chrono::{DateTime, Utc};
use oxidarr_db::SyncLevel;
use oxidarr_prowl::api::dto::{
    ApplicationResource, Field, IndexerCapabilities, IndexerCategory, IndexerResource,
    ReleaseResource, SelectOption,
};
use oxidarr_prowl::api::system::{self, SystemStatus};
use serde_json::Value;
use tower::ServiceExt;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/wire")).join(name)
}

/// Renders `value` in this file's own canonical format (see the module
/// docs) and asserts it matches the committed fixture named `name`
/// byte-for-byte.
fn assert_matches_fixture(name: &str, value: &impl serde::Serialize) {
    let mut rendered = serde_json::to_string_pretty(value).unwrap();
    rendered.push('\n');
    let expected = fs::read_to_string(fixture_path(name)).unwrap();
    assert_eq!(
        rendered, expected,
        "fixture {name} is stale — see this file's own module docs (\"Updating a fixture\") for how to refresh it"
    );
}

fn fixed_start_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2024-01-01T00:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc)
}

/// Constructs the exact `SystemStatus` `GET /system/status` answers with —
/// see [`system_status_matches_its_fixture`]'s second half, which proves the
/// real endpoint renders this same value, not just that a hand-built struct
/// happens to match the fixture.
fn system_status_sample() -> SystemStatus {
    SystemStatus {
        app_name: "Oxidarr",
        instance_name: "Oxidarr",
        version: env!("CARGO_PKG_VERSION"),
        is_production: true,
        is_docker: false,
        start_time: "2024-01-01T00:00:00+00:00".to_string(),
        app_data: "",
        authentication: "none",
        url_base: "",
    }
}

#[tokio::test]
async fn system_status_matches_its_fixture() {
    let sample = system_status_sample();
    assert_matches_fixture("system_status.json", &sample);

    // Not just a struct literal that happens to match: prove the real
    // endpoint (with a start time fixed to line up with the sample above)
    // renders byte-for-byte the same compact JSON `serde_json::to_string`
    // would produce for it.
    let router: Router = system::router(fixed_start_time());
    let response = router
        .oneshot(
            Request::builder()
                .uri("/system/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert_eq!(body, serde_json::to_string(&sample).unwrap());
}

#[test]
fn an_indexer_schema_entry_with_a_select_field_matches_its_fixture() {
    let resource = IndexerResource {
        id: 0,
        name: "Example Tracker".to_string(),
        implementation: "Cardigann".to_string(),
        definition_name: "exampletracker".to_string(),
        description: "An example private tracker definition.".to_string(),
        language: "en-US".to_string(),
        enable: false,
        priority: 25,
        capabilities: IndexerCapabilities {
            categories: vec![
                IndexerCategory {
                    id: 5000,
                    name: "TV".to_string(),
                },
                IndexerCategory {
                    id: 2000,
                    name: "Movies".to_string(),
                },
            ],
        },
        fields: vec![
            Field {
                name: "cookie".to_string(),
                label: "Cookie".to_string(),
                kind: "textbox".to_string(),
                value: None,
                select_options: None,
            },
            Field {
                name: "sort".to_string(),
                label: "Sort by".to_string(),
                kind: "select".to_string(),
                value: Some(Value::String("seeders".to_string())),
                select_options: Some(vec![
                    SelectOption {
                        value: 0,
                        name: "Seeders".to_string(),
                    },
                    SelectOption {
                        value: 1,
                        name: "Size".to_string(),
                    },
                ]),
            },
        ],
        sync_error: None,
    };
    assert_matches_fixture("indexer_schema_select.json", &resource);
}

#[test]
fn an_indexer_resource_carrying_a_sync_error_matches_its_fixture() {
    let resource = IndexerResource {
        id: 7,
        name: "My Tracker".to_string(),
        implementation: "Cardigann".to_string(),
        definition_name: "mytracker".to_string(),
        description: String::new(),
        language: String::new(),
        enable: true,
        priority: 25,
        capabilities: IndexerCapabilities::default(),
        fields: vec![Field {
            name: "cookie".to_string(),
            label: "cookie".to_string(),
            kind: "textbox".to_string(),
            value: Some(Value::String("abc123".to_string())),
            select_options: None,
        }],
        sync_error: Some("app 3: connection refused".to_string()),
    };
    assert_matches_fixture("indexer_resource_sync_error.json", &resource);
}

#[test]
fn an_application_resource_carrying_a_sync_error_matches_its_fixture() {
    let resource = ApplicationResource {
        id: 3,
        name: "My Sonarr".to_string(),
        implementation: "Sonarr".to_string(),
        sync_level: SyncLevel::FullSync,
        fields: vec![
            Field {
                name: "baseUrl".to_string(),
                label: "baseUrl".to_string(),
                kind: "textbox".to_string(),
                value: Some(Value::String("http://sonarr.example".to_string())),
                select_options: None,
            },
            Field {
                name: "apiKey".to_string(),
                label: "apiKey".to_string(),
                kind: "textbox".to_string(),
                value: Some(Value::String("secretkey".to_string())),
                select_options: None,
            },
        ],
        sync_error: Some(
            "unexpected status 500 from http://sonarr.example/api/v3/indexer".to_string(),
        ),
    };
    assert_matches_fixture("application_resource_sync_error.json", &resource);
}

#[test]
fn a_release_with_null_seeders_matches_its_fixture() {
    let resource = ReleaseResource {
        guid: None,
        title: "Some.Show.S01E01.1080p".to_string(),
        size: 1_073_741_824,
        seeders: None,
        leechers: Some(3),
        info_url: Some("https://example.tracker/details/123".to_string()),
        download_url: Some("https://example.tracker/download/123.torrent".to_string()),
        magnet_url: None,
        indexer_id: 1,
        indexer: "Example Tracker".to_string(),
        categories: vec![5000, 5040],
        publish_date: Some("2026-01-15T10:30:00Z".to_string()),
        grabs: Some(42),
        download_volume_factor: 1.0,
        upload_volume_factor: 1.0,
    };
    assert_matches_fixture("release_seeders_null.json", &resource);
}

#[test]
fn a_magnet_only_release_matches_its_fixture() {
    let resource = ReleaseResource {
        guid: None,
        title: "Some.Movie.2026.2160p".to_string(),
        size: 21_474_836_480,
        seeders: Some(120),
        leechers: Some(5),
        info_url: None,
        download_url: None,
        magnet_url: Some(
            "magnet:?xt=urn:btih:abcdef1234567890&dn=Some.Movie.2026.2160p".to_string(),
        ),
        indexer_id: 2,
        indexer: "Private Tracker".to_string(),
        categories: vec![2000, 2040],
        publish_date: None,
        grabs: None,
        download_volume_factor: 0.0,
        upload_volume_factor: 2.0,
    };
    assert_matches_fixture("release_magnet_only.json", &resource);
}
