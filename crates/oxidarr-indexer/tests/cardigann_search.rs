//! End-to-end test for `CardigannIndexer`: login, request
//! building, execution, and extraction composed behind the `Indexer`
//! trait, against the fixture in `tests/fixtures/tracker_search.html` (a
//! copy of `oxidarr-cardigann`'s `simple_tracker.html` extended with a
//! `td.date` column wired through a `dateparse` filter).
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use chrono::{DateTime, TimeZone, Utc};
use oxidarr_cardigann::model::parse_definition;
use oxidarr_indexer::Settings;
use oxidarr_indexer::testing::{FakeClient, ok_html};
use oxidarr_indexer::{CardigannIndexer, Indexer, IndexerError, LoginError, Method, SearchQuery};

const TRACKER_SEARCH_HTML: &str = include_str!("fixtures/tracker_search.html");

fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap()
}

const DEFINITION_WITH_LOGIN: &str = r#"
id: example
name: Example
links:
  - https://example.org/
settings:
  - name: username
    type: text
    label: Username
  - name: password
    type: password
    label: Password
login:
  path: takelogin.php
  method: post
  inputs:
    username: "{{ .Config.username }}"
    password: "{{ .Config.password }}"
  error:
    - selector: div.error
search:
  paths:
    - path: browse
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name a
    details:
      selector: td.name a
      attribute: href
    download:
      selector: td.dl a
      attribute: href
    seeders:
      selector: td.seeds
    leechers:
      selector: td.leech
    size:
      selector: td.size
    date:
      selector: td.date
      filters:
        - name: dateparse
          args: yyyy-MM-dd
  error:
    - selector: div.searcherror
"#;

fn settings() -> Settings {
    let mut values = BTreeMap::new();
    values.insert("username".to_string(), "alice".to_string());
    values.insert("password".to_string(), "hunter2".to_string());
    Settings::new(values)
}

#[tokio::test]
async fn searches_and_extracts_the_fixtures_releases_with_a_valid_session() {
    // A definition with a login block, but a session that's already valid:
    // `search` must go straight to the search GET (no eager login round
    // trip) — the FakeClient here queues only that one expectation.
    let def = parse_definition(DEFINITION_WITH_LOGIN).unwrap();
    let client = FakeClient::new().expect(
        |r| r.method == Method::Get && r.url.path() == "/browse",
        ok_html("https://example.org/browse", TRACKER_SEARCH_HTML),
    );
    let indexer = CardigannIndexer::with_clock(def, settings(), client, fixed_now);

    let releases = indexer.search(&SearchQuery::default()).await.unwrap();

    assert_eq!(releases.len(), 2);

    assert_eq!(releases[0].title, "Big.Buck.Bunny.2008.1080p.BluRay.x264");
    assert_eq!(releases[0].details_url.as_deref(), Some("/details/1"));
    assert_eq!(
        releases[0].download_url.as_deref(),
        Some("/download/1.torrent")
    );
    assert_eq!(releases[0].seeders, Some(120));
    assert_eq!(releases[0].leechers, Some(7));
    assert_eq!(releases[0].size, Some(1_503_238_553));
    assert_eq!(
        releases[0].publish_date,
        Some(Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).unwrap())
    );

    assert_eq!(releases[1].title, "Sintel.2010.720p.WEB-DL.x264");
    assert_eq!(releases[1].details_url.as_deref(), Some("/details/2"));
    assert_eq!(
        releases[1].download_url.as_deref(),
        Some("/download/2.torrent")
    );
    assert_eq!(releases[1].seeders, Some(45));
    assert_eq!(releases[1].leechers, Some(2));
    assert_eq!(releases[1].size, Some(700 * 1024 * 1024));
    assert_eq!(
        releases[1].publish_date,
        Some(Utc.with_ymd_and_hms(2026, 1, 12, 0, 0, 0).unwrap())
    );
}

#[tokio::test]
async fn a_valid_session_records_zero_login_requests() {
    // Dedicated regression for the "no eager login" property:
    // every `search()` call used to perform a full
    // login round-trip first, unconditionally, with no session concept
    // gating it. Real private trackers rate-limit or flag accounts for
    // frequent re-auth, and a server built on this indexer calls `search`
    // constantly, so this must never happen for an already-valid session —
    // only a *rejected* response (checked reactively by
    // `execute_with_reauth`) may ever trigger a login POST. The FakeClient
    // queues a single expectation (the search GET, no login POST at all);
    // an eager login would consume it with a mismatched request and the
    // call would fail with a transport error instead of succeeding.
    let def = parse_definition(DEFINITION_WITH_LOGIN).unwrap();
    let client = FakeClient::new().expect(
        |r| r.method == Method::Get && r.url.path() == "/browse",
        ok_html("https://example.org/browse", TRACKER_SEARCH_HTML),
    );
    let indexer = CardigannIndexer::with_clock(def, settings(), client, fixed_now);

    let releases = indexer.search(&SearchQuery::default()).await.unwrap();

    assert_eq!(releases.len(), 2);
    assert_eq!(releases[0].title, "Big.Buck.Bunny.2008.1080p.BluRay.x264");
}

#[tokio::test]
async fn a_search_response_matching_an_error_rule_reauthenticates_and_retries_once() {
    // The sole login-POST exerciser now that `search` no longer
    // authenticates eagerly: a cold/expired session's first search trips
    // `search.error`, which reactively authenticates once and retries the
    // same request once more. Sequence: search (rejected), login, search
    // (succeeds).
    let def = parse_definition(DEFINITION_WITH_LOGIN).unwrap();
    let client = FakeClient::new()
        .expect(
            |r| r.method == Method::Get && r.url.path() == "/browse",
            ok_html(
                "https://example.org/browse",
                r#"<html><body><div class="searcherror">Session expired</div></body></html>"#,
            ),
        )
        .expect(
            |r| r.method == Method::Post && r.url.path() == "/takelogin.php",
            ok_html("https://example.org/takelogin.php", "<html>ok</html>"),
        )
        .expect(
            |r| r.method == Method::Get && r.url.path() == "/browse",
            ok_html("https://example.org/browse", TRACKER_SEARCH_HTML),
        );
    let indexer = CardigannIndexer::with_clock(def, settings(), client, fixed_now);

    let releases = indexer.search(&SearchQuery::default()).await.unwrap();

    assert_eq!(releases.len(), 2);
    assert_eq!(releases[0].title, "Big.Buck.Bunny.2008.1080p.BluRay.x264");
}

const DEFINITION_NO_LOGIN: &str = r"
id: example
name: Example
links:
  - https://example.org/
search:
  paths:
    - path: browse
  rows:
    selector: tr.result
  fields:
    title:
      selector: td.name a
  error:
    - selector: div.searcherror
";

#[tokio::test]
async fn a_search_error_with_no_login_block_is_a_typed_error_with_no_retry() {
    // No `login` block: `authenticate` is a no-op (zero requests), so the
    // only request the FakeClient ever sees is the single search GET —
    // proving there is no reactive re-auth to fall back on.
    let def = parse_definition(DEFINITION_NO_LOGIN).unwrap();
    let client = FakeClient::new().expect(
        |r| r.method == Method::Get && r.url.path() == "/browse",
        ok_html(
            "https://example.org/browse",
            r#"<html><body><div class="searcherror">Banned</div></body></html>"#,
        ),
    );
    let indexer = CardigannIndexer::with_clock(def, settings(), client, fixed_now);

    let err = indexer.search(&SearchQuery::default()).await.unwrap_err();

    assert!(matches!(
        err,
        IndexerError::Login(LoginError::Rejected(msg)) if msg == "Banned"
    ));
}
