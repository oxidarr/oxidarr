//! API-key authentication middleware.
//!
//! Prowlarr accepts its API key two ways: an `X-Api-Key` header, or an
//! `apikey` query parameter. [`require_api_key`] accepts either, checking
//! the header first. A missing or mismatching key produces a 401
//! [`Problem`](crate::problem::Problem) response instead of calling through
//! to the wrapped handler.

use std::fmt;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::problem::Problem;

const HEADER_NAME: &str = "x-api-key";
const QUERY_PARAM_NAME: &str = "apikey";

/// The API key a server instance is configured to require.
///
/// Cheap to clone: the key is stored behind an [`Arc`], so handing a clone
/// to [`axum::middleware::from_fn_with_state`] as its state does not
/// reallocate the key on every request.
#[derive(Clone)]
pub struct ApiKey(Arc<str>);

impl ApiKey {
    /// Wrap `key` for use as [`require_api_key`]'s middleware state.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(Arc::from(key.into()))
    }
}

/// Hand-written so the configured key can never leak through a log or
/// test failure message that formats an [`ApiKey`] — the derived `Debug`
/// would print the raw secret.
impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ApiKey").field(&"<redacted>").finish()
    }
}

/// Reject requests that do not present the configured API key.
///
/// The key may arrive as the `X-Api-Key` header or an `apikey` query
/// parameter. The header is authoritative when present: if it's set, its
/// value is checked and the query parameter is not consulted at all, even
/// when it's also present and would itself have matched. Only when the
/// header is absent does the query parameter get a look. A request with a
/// duplicated `apikey` query parameter uses the first occurrence, matching
/// this crate's `url::form_urlencoded`-based parsing rather than treating
/// the duplication as an error — extracting the candidate key must never
/// itself fail, so every rejection uniformly produces this function's 401
/// [`Problem`] JSON body rather than, say, axum's own 400 plain-text
/// extractor-rejection response.
///
/// Mount via [`axum::middleware::from_fn_with_state`]:
///
/// ```
/// use axum::{middleware, routing::get, Router};
/// use oxidarr_http::auth::{require_api_key, ApiKey};
///
/// let key = ApiKey::new("secret");
/// let _app: Router = Router::new()
///     .route("/ping", get(|| async { "pong" }))
///     .layer(middleware::from_fn_with_state(key, require_api_key));
/// ```
pub async fn require_api_key(State(key): State<ApiKey>, request: Request, next: Next) -> Response {
    let header = request
        .headers()
        .get(HEADER_NAME)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    let provided = header.or_else(|| first_query_value(request.uri().query(), QUERY_PARAM_NAME));

    let authorized =
        provided.is_some_and(|candidate| constant_time_eq(candidate.as_bytes(), key.0.as_bytes()));

    if authorized {
        next.run(request).await
    } else {
        Problem::new(StatusCode::UNAUTHORIZED, "missing or invalid API key").into_response()
    }
}

/// The first value bound to `name` in a raw (already-percent-decoded-by-
/// nothing) query string, or `None` if `name` is absent or `query` itself
/// is `None`.
///
/// Parsed with [`url::form_urlencoded::parse`], which never fails — unlike
/// deserializing into a struct, so a duplicated or malformed query string
/// can never turn into anything other than "no value found" here. A
/// repeated key (`apikey=a&apikey=b`) is legal in a query string; this
/// takes the first occurrence, the same resolution most query-string
/// consumers use.
fn first_query_value(query: Option<&str>, name: &str) -> Option<String> {
    let query = query?;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// Compare `a` and `b` for equality without leaking, via execution time,
/// *where* the two differ.
///
/// Folds an accumulator over every index up to `max(a.len(), b.len())`,
/// reading out-of-range bytes as `0` rather than stopping early, and mixes
/// a length-mismatch bit into the same accumulator up front. The loop
/// therefore always runs the same number of iterations for a given pair
/// of lengths and never returns as soon as a differing byte is found, so
/// the run time depends only on `max(a.len(), b.len())`, not on the
/// content of either slice.
#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff: u8 = u8::from(a.len() != b.len());
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::{Router, middleware};
    use tower::ServiceExt;

    use super::{ApiKey, require_api_key};

    fn app() -> Router {
        Router::new()
            .route("/ping", get(|| async { "pong" }))
            .layer(middleware::from_fn_with_state(
                ApiKey::new("s3cret"),
                require_api_key,
            ))
    }

    #[tokio::test]
    async fn accepts_the_correct_header() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/ping")
                    .header("x-api-key", "s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn accepts_the_correct_query_param() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/ping?apikey=s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rejects_a_missing_key_with_a_json_problem() {
        let response = app()
            .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["message"].is_string());
    }

    #[tokio::test]
    async fn rejects_a_wrong_key() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/ping")
                    .header("x-api-key", "not-the-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_a_wrong_query_param_only() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/ping?apikey=not-the-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_correct_header_wins_over_a_wrong_query_param() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/ping?apikey=not-the-key")
                    .header("x-api-key", "s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_wrong_header_wins_over_a_correct_query_param() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/ping?apikey=s3cret")
                    .header("x-api-key", "not-the-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_duplicate_query_param_uses_the_first_occurrence_when_correct() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/ping?apikey=s3cret&apikey=not-the-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_duplicate_query_param_uses_the_first_occurrence_when_wrong() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/ping?apikey=not-the-key&apikey=s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Must fail closed with the crate's own 401 Problem JSON, never
        // axum's built-in 400 plain-text extractor rejection — extraction
        // of the API key must never itself be fallible.
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        assert!(content_type.contains("application/json"));

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["message"].is_string());
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let key = ApiKey::new("s3cret");
        assert!(!format!("{key:?}").contains("s3cret"));
    }

    #[test]
    fn constant_time_eq_matches_equal_slices() {
        assert!(super::constant_time_eq(b"same-key", b"same-key"));
    }

    #[test]
    fn constant_time_eq_rejects_different_lengths() {
        assert!(!super::constant_time_eq(b"short", b"much-longer"));
    }

    #[test]
    fn constant_time_eq_rejects_different_bytes_of_equal_length() {
        assert!(!super::constant_time_eq(b"aaaa", b"aaab"));
    }
}
