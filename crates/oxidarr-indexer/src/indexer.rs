//! The [`Indexer`] trait plus its Newznab and Torznab implementations.
//!
//! Both protocols share an identical request/response shape today — a GET to
//! `/api` with the same query parameters, and a Newznab/Torznab RSS response
//! parsed by [`crate::rss::parse_feed`] — so both are thin wrappers around a
//! single private [`ApiIndexer`] core. They are kept as two distinct public
//! types (rather than one struct generic over a marker, or a single type
//! with no Newznab/Torznab distinction at all) because the two protocols are
//! expected to diverge once `caps` (indexer capability negotiation) lands in
//! a later task.

use std::fmt;
use std::future::Future;

use oxidarr_core::Release;
use url::Url;

use crate::client::{HttpClient, HttpRequest, Method};
use crate::error::{HttpError, IndexerError};
use crate::query::SearchQuery;
use crate::rss::parse_feed;

/// Executes a search against an indexer, returning normalised releases.
///
/// Implementors are used generically (`I: Indexer`) rather than through
/// `dyn Indexer`, so `search` returns `impl Future` rather than boxing —
/// mirroring [`HttpClient::execute`]'s own shape.
pub trait Indexer: Send + Sync {
    /// Searches for releases matching `q`.
    ///
    /// # Errors
    ///
    /// Returns [`IndexerError::Http`] if the request could not be executed,
    /// or if the response came back with a non-2xx status. Returns
    /// [`IndexerError::Parse`] if a 2xx response body could not be parsed
    /// as a Newznab/Torznab feed.
    fn search(
        &self,
        q: &SearchQuery,
    ) -> impl Future<Output = Result<Vec<Release>, IndexerError>> + Send;
}

/// Resolves `base` into the Torznab/Newznab API endpoint to request.
///
/// Prowlarr convention: the user pastes the *full* API endpoint, which may
/// already end in `/api` (e.g. `https://host/nzbhydra/api`, the routine
/// shape for an indexer sitting behind a reverse proxy at a subpath) or may
/// still need `/api` appended (e.g. a bare `https://host` or
/// `https://host/nzbhydra/`). Trailing-slash-insensitively: a path already
/// ending in `/api` is used as-is; otherwise an `api` segment is appended,
/// landing as a *sibling* of `base`'s existing path rather than replacing
/// its last segment — the same normalise-then-join convention
/// [`crate::builder::resolve_base_url`] applies to definition links, here
/// done via [`Url::path_segments_mut`] (`pop_if_empty` first removes a
/// trailing-slash's empty segment so `push` doesn't leave a double slash)
/// rather than string surgery, so segment percent-encoding stays exact.
/// Any query on `base` is dropped either way.
fn api_url(base: &Url) -> Url {
    let mut url = base.clone();
    url.set_query(None);

    if url.path().trim_end_matches('/').ends_with("/api") {
        return url;
    }

    // `path_segments_mut` only fails for a cannot-be-a-base URL (e.g.
    // `mailto:`), which an http(s) indexer base never is; a URL that
    // somehow is one is left with whatever path it already had rather than
    // panicking.
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.pop_if_empty().push("api");
    }
    url
}

/// Strips a leading `tt`/`TT`/`Tt`/`tT` from an IMDB id — Torznab's
/// `imdbid` parameter is bare digits, but [`SearchQuery::imdb_id`] may carry
/// either form (some indexers report `tt1234567`, others just `1234567`;
/// see [`crate::rss::parse_feed`]'s own doc comment on the same
/// ambiguity). `imdb_id.get(0..2)` is used rather than slicing directly so
/// a value shorter than two bytes (or otherwise not byte-boundary-aligned)
/// falls through to the unchanged input instead of panicking.
fn strip_tt_prefix(imdb_id: &str) -> &str {
    match imdb_id.get(0..2) {
        Some(prefix) if prefix.eq_ignore_ascii_case("tt") => &imdb_id[2..],
        _ => imdb_id,
    }
}

/// Builds the Torznab/Newznab search request URL from `q` and `apikey`,
/// against [`api_url`]'s resolution of `base`.
///
/// Query parameters are appended in a fixed order — `t`, `q`, `imdbid`,
/// `season`, `ep`, `cat`, `apikey` — omitting any that are absent/empty,
/// rather than relying on incidental insertion order. `t` is `tvsearch`
/// when either `season` or `episode` is set, and `search` otherwise.
/// `imdb_id` has any leading `tt` stripped via [`strip_tt_prefix`];
/// `categories` is comma-joined into `cat`.
fn build_url(base: &Url, q: &SearchQuery, apikey: Option<&str>) -> Url {
    let mut url = api_url(base);

    {
        let mut pairs = url.query_pairs_mut();

        let search_type = if q.season.is_some() || q.episode.is_some() {
            "tvsearch"
        } else {
            "search"
        };
        pairs.append_pair("t", search_type);

        if let Some(term) = &q.q {
            pairs.append_pair("q", term);
        }
        if let Some(imdb_id) = &q.imdb_id {
            pairs.append_pair("imdbid", strip_tt_prefix(imdb_id));
        }
        if let Some(season) = q.season {
            pairs.append_pair("season", &season.to_string());
        }
        if let Some(episode) = q.episode {
            pairs.append_pair("ep", &episode.to_string());
        }
        if !q.categories.is_empty() {
            // `,` is not in `application/x-www-form-urlencoded`'s safe set,
            // so this is percent-encoded to `%2C` by `append_pair` below —
            // standard `url`-crate behaviour, not a bug.
            let cats = q
                .categories
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            pairs.append_pair("cat", &cats);
        }
        if let Some(key) = apikey {
            pairs.append_pair("apikey", key);
        }
    }

    url
}

/// Shared Newznab/Torznab request-building and execution core. See this
/// module's doc comment for why [`NewznabIndexer`] and [`TorznabIndexer`]
/// both wrap this rather than duplicating it.
struct ApiIndexer<C> {
    base: Url,
    apikey: Option<String>,
    client: C,
}

impl<C: HttpClient> ApiIndexer<C> {
    fn new(base: Url, apikey: Option<String>, client: C) -> Self {
        Self {
            base,
            apikey,
            client,
        }
    }

    /// Issues the search GET request and parses a 2xx response as a
    /// Newznab/Torznab feed.
    ///
    /// # Errors
    ///
    /// Returns [`IndexerError::Http`] on a transport failure, or when the
    /// response status is not in the 200-299 range (the error names the
    /// response's final URL, after any redirects). Returns
    /// [`IndexerError::Parse`] if a 2xx body cannot be parsed.
    async fn search(&self, q: &SearchQuery) -> Result<Vec<Release>, IndexerError> {
        let url = build_url(&self.base, q, self.apikey.as_deref());
        let req = HttpRequest {
            method: Method::Get,
            url,
            headers: Vec::new(),
            body: None,
        };
        let resp = self.client.execute(req).await?;

        if !(200..300).contains(&resp.status) {
            return Err(IndexerError::Http(HttpError::Status {
                status: resp.status,
                url: resp.final_url,
            }));
        }

        parse_feed(&resp.body)
    }
}

/// A Newznab-protocol indexer (Usenet/NZB), talking to `base`'s `/api`
/// endpoint.
///
/// Mechanically identical to [`TorznabIndexer`] today — see this module's
/// doc comment.
pub struct NewznabIndexer<C: HttpClient> {
    inner: ApiIndexer<C>,
}

impl<C: HttpClient> NewznabIndexer<C> {
    /// Builds a `NewznabIndexer` targeting `base`, authenticating requests
    /// with `apikey` when given, and executing them through `client`.
    #[must_use]
    pub fn new(base: Url, apikey: Option<String>, client: C) -> Self {
        Self {
            inner: ApiIndexer::new(base, apikey, client),
        }
    }
}

impl<C: HttpClient> Indexer for NewznabIndexer<C> {
    fn search(
        &self,
        q: &SearchQuery,
    ) -> impl Future<Output = Result<Vec<Release>, IndexerError>> + Send {
        self.inner.search(q)
    }
}

impl<C: HttpClient> fmt::Debug for NewznabIndexer<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewznabIndexer").finish_non_exhaustive()
    }
}

/// A Torznab-protocol indexer (`BitTorrent`), talking to `base`'s `/api`
/// endpoint.
///
/// Mechanically identical to [`NewznabIndexer`] today — see this module's
/// doc comment.
pub struct TorznabIndexer<C: HttpClient> {
    inner: ApiIndexer<C>,
}

impl<C: HttpClient> TorznabIndexer<C> {
    /// Builds a `TorznabIndexer` targeting `base`, authenticating requests
    /// with `apikey` when given, and executing them through `client`.
    #[must_use]
    pub fn new(base: Url, apikey: Option<String>, client: C) -> Self {
        Self {
            inner: ApiIndexer::new(base, apikey, client),
        }
    }
}

impl<C: HttpClient> Indexer for TorznabIndexer<C> {
    fn search(
        &self,
        q: &SearchQuery,
    ) -> impl Future<Output = Result<Vec<Release>, IndexerError>> + Send {
        self.inner.search(q)
    }
}

impl<C: HttpClient> fmt::Debug for TorznabIndexer<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TorznabIndexer").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{Indexer, NewznabIndexer, SearchQuery, TorznabIndexer};
    use crate::client::HttpResponse;
    use crate::error::{HttpError, IndexerError};
    use crate::testing::FakeClient;

    const TORZNAB_FEED: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/torznab_feed.xml"
    ));

    fn xml_response(status: u16, final_url: &str, body: &[u8]) -> HttpResponse {
        HttpResponse {
            status,
            headers: vec![],
            body: body.to_vec(),
            final_url: final_url.parse().unwrap(),
        }
    }

    const EMPTY_FEED: &[u8] =
        br#"<?xml version="1.0"?><rss version="2.0"><channel><title>Empty</title></channel></rss>"#;

    #[tokio::test]
    async fn tvsearch_query_maps_to_the_expected_url_in_a_fixed_param_order() {
        let client = FakeClient::new().expect(
            |r| {
                r.url.as_str()
                    == "https://api.example.com/api?t=tvsearch&q=show&season=2&ep=5&apikey=k"
            },
            xml_response(200, "https://api.example.com/api", EMPTY_FEED),
        );
        let base: url::Url = "https://api.example.com/".parse().unwrap();
        let indexer = TorznabIndexer::new(base, Some("k".to_string()), client);
        let q = SearchQuery {
            q: Some("show".to_string()),
            season: Some(2),
            episode: Some(5),
            ..SearchQuery::default()
        };

        let releases = indexer.search(&q).await.unwrap();

        assert_eq!(releases, Vec::new());
    }

    #[tokio::test]
    async fn plain_search_uses_t_search_joins_categories_and_strips_the_tt_imdb_prefix() {
        // Categories are comma-joined before being percent-encoded into the
        // query string, so the literal "," becomes "%2C" here.
        let client = FakeClient::new().expect(
            |r| {
                r.url.as_str()
                    == "https://api.example.com/api?t=search&q=ubuntu&imdbid=1234567&cat=5000%2C5030"
            },
            xml_response(200, "https://api.example.com/api", EMPTY_FEED),
        );
        let base: url::Url = "https://api.example.com/".parse().unwrap();
        let indexer = TorznabIndexer::new(base, None, client);
        let q = SearchQuery {
            q: Some("ubuntu".to_string()),
            imdb_id: Some("tt1234567".to_string()),
            categories: vec![5000, 5030],
            ..SearchQuery::default()
        };

        let releases = indexer.search(&q).await.unwrap();

        assert_eq!(releases, Vec::new());
    }

    #[tokio::test]
    async fn a_base_already_ending_in_api_is_used_as_is() {
        // Prowlarr convention: the user pastes the full API endpoint, and a
        // reverse-proxied NZBHydra2-style base already carries a subpath
        // (e.g. "/nzbhydra") ahead of "/api". `build_url` must not discard
        // it.
        let client = FakeClient::new().expect(
            |r| r.url.as_str() == "https://host/nzbhydra/api?t=search",
            xml_response(200, "https://host/nzbhydra/api", EMPTY_FEED),
        );
        let base: url::Url = "https://host/nzbhydra/api".parse().unwrap();
        let indexer = TorznabIndexer::new(base, None, client);

        indexer.search(&SearchQuery::default()).await.unwrap();
    }

    #[tokio::test]
    async fn a_bare_host_base_with_no_path_still_gets_api_appended() {
        let client = FakeClient::new().expect(
            |r| r.url.as_str() == "https://host/api?t=search",
            xml_response(200, "https://host/api", EMPTY_FEED),
        );
        let base: url::Url = "https://host".parse().unwrap();
        let indexer = TorznabIndexer::new(base, None, client);

        indexer.search(&SearchQuery::default()).await.unwrap();
    }

    #[tokio::test]
    async fn a_base_with_a_subpath_and_trailing_slash_gets_api_appended_as_a_sibling() {
        let client = FakeClient::new().expect(
            |r| r.url.as_str() == "https://host/nzbhydra/api?t=search",
            xml_response(200, "https://host/nzbhydra/api", EMPTY_FEED),
        );
        let base: url::Url = "https://host/nzbhydra/".parse().unwrap();
        let indexer = TorznabIndexer::new(base, None, client);

        indexer.search(&SearchQuery::default()).await.unwrap();
    }

    #[tokio::test]
    async fn an_uppercase_tt_imdb_prefix_is_also_stripped() {
        let client = FakeClient::new().expect(
            |r| r.url.as_str() == "https://host/api?t=search&imdbid=1234567",
            xml_response(200, "https://host/api", EMPTY_FEED),
        );
        let base: url::Url = "https://host".parse().unwrap();
        let indexer = TorznabIndexer::new(base, None, client);
        let q = SearchQuery {
            imdb_id: Some("TT1234567".to_string()),
            ..SearchQuery::default()
        };

        indexer.search(&q).await.unwrap();
    }

    #[tokio::test]
    async fn a_non_2xx_response_is_an_http_status_error() {
        let client = FakeClient::new().expect(
            |r| r.url.path() == "/api",
            xml_response(429, "https://api.example.com/api", b""),
        );
        let base: url::Url = "https://api.example.com/".parse().unwrap();
        let indexer = TorznabIndexer::new(base, None, client);

        let err = indexer.search(&SearchQuery::default()).await.unwrap_err();

        assert!(matches!(
            err,
            IndexerError::Http(HttpError::Status { status: 429, .. })
        ));
    }

    #[tokio::test]
    async fn a_success_response_is_parsed_into_the_fixtures_releases() {
        let client = FakeClient::new().expect(
            |r| r.url.path() == "/api",
            xml_response(200, "https://api.example.com/api", TORZNAB_FEED),
        );
        let base: url::Url = "https://api.example.com/".parse().unwrap();
        let indexer = NewznabIndexer::new(base, None, client);

        let releases = indexer.search(&SearchQuery::default()).await.unwrap();

        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].title, "Some.Show.S01E01.1080p.WEB-DL");
        assert_eq!(releases[0].seeders, Some(42));
        assert_eq!(releases[1].title, "Minimal.Release.2026");
    }
}
