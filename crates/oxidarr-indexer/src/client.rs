//! HTTP request/response types and the [`HttpClient`] trait.
//!
//! Indexer implementations execute requests through an `HttpClient` rather
//! than talking to a transport directly, so the same indexer logic runs
//! against a real client or the in-memory [`crate::testing::FakeClient`].

use std::borrow::Cow;
use std::future::Future;

use crate::error::HttpError;

/// HTTP method used by an indexer request.
///
/// Cardigann definitions only ever issue GET or POST requests; `Put` and
/// `Delete` exist purely for `oxidarr-prowl`'s app-sync engine
/// (`crates/oxidarr-prowl/src/sync.rs`), which speaks Sonarr/Radarr's own
/// `PUT`/`DELETE /api/v3/indexer/{id}` REST routes through this same
/// [`HttpClient`] abstraction rather than introducing a second one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// HTTP GET.
    Get,
    /// HTTP POST.
    Post,
    /// HTTP PUT.
    Put,
    /// HTTP DELETE.
    Delete,
}

/// The body of a request.
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    /// A `application/x-www-form-urlencoded` form body, as ordered
    /// key/value pairs.
    Form(Vec<(String, String)>),
    /// A JSON body, serialized with `Content-Type: application/json`. Used
    /// by `oxidarr-prowl`'s app-sync engine to speak Sonarr/Radarr's own
    /// `POST`/`PUT /api/v3/indexer` JSON API — no Cardigann definition ever
    /// builds one of these.
    Json(serde_json::Value),
}

/// A request an [`HttpClient`] can execute.
///
/// `PartialEq` only (not `Eq`): [`Body::Json`] wraps a `serde_json::Value`,
/// which itself is only `PartialEq` (a JSON number can be a float, so total
/// equality isn't meaningful) — the same reason [`Body`] itself dropped
/// `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequest {
    /// The HTTP method.
    pub method: Method,
    /// The target URL.
    pub url: url::Url,
    /// Request headers, as ordered name/value pairs.
    pub headers: Vec<(String, String)>,
    /// The request body, if any.
    pub body: Option<Body>,
    /// Whether the client should follow HTTP redirects (301/302/303/307/308)
    /// for this request rather than returning the redirect response as-is.
    pub follow_redirects: bool,
}

/// The result of executing an [`HttpRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// The HTTP status code.
    pub status: u16,
    /// Response headers, as ordered name/value pairs.
    pub headers: Vec<(String, String)>,
    /// The raw response body.
    pub body: Vec<u8>,
    /// The URL the response was ultimately served from, after any
    /// redirects the client followed.
    pub final_url: url::Url,
}

impl HttpResponse {
    /// The response body decoded as UTF-8, replacing invalid sequences.
    #[must_use]
    pub fn text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

/// Executes [`HttpRequest`]s and returns [`HttpResponse`]s.
///
/// Implementors are used generically (`C: HttpClient`) throughout the
/// indexer stack — there is no `dyn HttpClient` — so `execute` returns
/// `impl Future` rather than boxing.
pub trait HttpClient: Send + Sync {
    /// Executes `req`, returning the response or a transport-level error.
    fn execute(
        &self,
        req: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{Body, HttpClient, HttpError, HttpRequest, Method};
    use crate::testing::{FakeClient, ok_html};

    #[tokio::test]
    async fn fake_client_matches_in_order_and_records() {
        let client = FakeClient::new().expect(
            |r| r.url.path() == "/login",
            ok_html("https://t.example/login", "<html/>"),
        );
        let req = HttpRequest {
            method: Method::Get,
            url: "https://t.example/login".parse().unwrap(),
            headers: vec![],
            body: None,
            follow_redirects: true,
        };
        let resp = client.execute(req).await.unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(client.requests().len(), 1);
    }

    #[tokio::test]
    async fn fake_client_carries_follow_redirects_through_requests() {
        let client = FakeClient::new().expect(
            |r| !r.follow_redirects,
            ok_html("https://t.example/no-follow", "<html/>"),
        );
        let req = HttpRequest {
            method: Method::Get,
            url: "https://t.example/no-follow".parse().unwrap(),
            headers: vec![],
            body: None,
            follow_redirects: false,
        };
        client.execute(req).await.unwrap();
        assert!(!client.requests()[0].follow_redirects);
    }

    #[tokio::test]
    async fn fake_client_rejects_unexpected_requests() {
        let client = FakeClient::new();
        let req = HttpRequest {
            method: Method::Get,
            url: "https://t.example/surprise".parse().unwrap(),
            headers: vec![],
            body: None,
            follow_redirects: true,
        };
        assert!(matches!(
            client.execute(req).await,
            Err(HttpError::Transport(_))
        ));
    }

    #[tokio::test]
    async fn fake_client_reports_put_and_delete_by_name_in_an_unexpected_request_error() {
        let client = FakeClient::new();
        let put = HttpRequest {
            method: Method::Put,
            url: "https://t.example/api/v3/indexer/7".parse().unwrap(),
            headers: vec![],
            body: Some(Body::Json(serde_json::json!({"id": 7}))),
            follow_redirects: true,
        };
        let delete = HttpRequest {
            method: Method::Delete,
            url: "https://t.example/api/v3/indexer/7".parse().unwrap(),
            headers: vec![],
            body: None,
            follow_redirects: true,
        };

        let put_err = client.execute(put).await.unwrap_err();
        let delete_err = client.execute(delete).await.unwrap_err();

        assert!(
            matches!(put_err, HttpError::Transport(msg) if msg.starts_with("unexpected request: PUT "))
        );
        assert!(
            matches!(delete_err, HttpError::Transport(msg) if msg.starts_with("unexpected request: DELETE "))
        );
    }
}
