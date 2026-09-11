//! Production [`HttpClient`] implementation backed by [`reqwest`].

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use reqwest::cookie::Jar;

use crate::client::{Body, HttpClient, HttpRequest, HttpResponse, Method};
use crate::error::HttpError;

/// An [`HttpClient`] that executes requests over the network using
/// [`reqwest`].
///
/// Built with a cookie store (Cardigann logins rely on session cookies), a
/// 30 second timeout, transparent gzip decompression, and redirect
/// following disabled at the transport (`Policy::none()`).
/// [`HttpClient::execute`] follows redirects itself, hop by hop, so it can
/// honour each [`HttpRequest::follow_redirects`] individually and apply
/// [`next_hop`]'s method/body rules — `reqwest`'s own policy is
/// all-or-nothing per client and always preserves the original method,
/// neither of which this crate's per-request `follow_redirects` flag can
/// live with. Because every hop still goes out through this same `client`,
/// the cookie store/provider applies exactly as it would to any other
/// request — no separate cookie handling needed for the redirect chain.
///
/// `Clone`: `reqwest::Client` wraps its connection pool, cookie store, and
/// configuration behind an internal `Arc` (see [`HttpClient::execute`]'s own
/// doc comment, which already relies on this for its per-hop loop), so
/// cloning a `ReqwestClient` is cheap and every clone shares the same cookie
/// jar/provider and connection pool rather than getting an independent one —
/// exactly what a Torznab router needs to hand one client to many concurrent
/// request handlers.
#[derive(Clone)]
pub struct ReqwestClient {
    client: reqwest::Client,
}

impl ReqwestClient {
    /// Builds a new `ReqwestClient`.
    ///
    /// # Errors
    ///
    /// Returns [`HttpError::Transport`] if the underlying `reqwest` client
    /// could not be constructed (e.g. TLS backend initialization failure).
    pub fn new() -> Result<Self, HttpError> {
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .gzip(true)
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| HttpError::Transport(err.to_string()))?;
        Ok(Self { client })
    }

    /// Builds a `ReqwestClient` pre-seeded with `cookie` for `base`, for a
    /// definition's `method: cookie` login (e.g. `teamos.yml`:
    /// `login.inputs.cookie: "{{ .Config.cookie }}"`, resolved from a
    /// user-supplied `cookie` setting). `authenticate` itself issues no
    /// request for this login style — the cookie has to be on the
    /// transport *before* any search request goes out, which is why this is
    /// a constructor rather than something `authenticate` calls.
    ///
    /// # Errors
    ///
    /// Returns [`HttpError::Transport`] if the underlying `reqwest` client
    /// could not be constructed.
    pub fn with_cookie(base: &url::Url, cookie: &str) -> Result<Self, HttpError> {
        let jar = Jar::default();
        jar.add_cookie_str(cookie, base);
        let client = reqwest::Client::builder()
            .cookie_provider(Arc::new(jar))
            .gzip(true)
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| HttpError::Transport(err.to_string()))?;
        Ok(Self { client })
    }
}

impl fmt::Debug for ReqwestClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReqwestClient").finish_non_exhaustive()
    }
}

/// The maximum number of redirect hops [`ReqwestClient::execute`] will
/// follow for one request before giving up.
const MAX_REDIRECT_HOPS: u8 = 10;

/// Decides the next hop for a redirect response, or `None` when `status`
/// does not call for one (including when it's a redirect status but
/// carries no usable `Location`).
///
/// Returns `(next_url, next_method, drop_body)`:
/// - `next_url` is `Location` resolved against `current` — absolute as-is,
///   relative joined onto it ([`Url::join`]'s RFC 3986 semantics).
/// - `303 See Other` always switches to GET and drops the body, regardless
///   of the original method (RFC 9110 §15.4.4).
/// - `301`/`302` after `POST` also downgrade to GET and drop the body —
///   not what the RFC technically specifies (both are defined to preserve
///   the method), but the universal real-world behaviour every browser and
///   `reqwest`'s own default policy implement, and the shape Cardigann
///   trackers' login/search redirects actually rely on.
/// - `301`/`302` after `GET` keep GET (a no-op method-wise) and report no
///   body to drop.
/// - `307`/`308` preserve the method and body exactly (RFC 9110 §15.4.8-9).
///
/// This is a pure function precisely so it can be exhaustively unit tested
/// without any network I/O — [`ReqwestClient::execute`]'s loop around it
/// stays thin.
fn next_hop(
    status: u16,
    headers: &[(String, String)],
    current: &url::Url,
    method: Method,
) -> Option<(url::Url, Method, bool)> {
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    let location = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("location"))
        .map(|(_, value)| value.as_str())?;
    let next_url = current.join(location).ok()?;

    let (next_method, drop_body) = match status {
        303 => (Method::Get, true),
        301 | 302 => match method {
            Method::Post => (Method::Get, true),
            Method::Get => (Method::Get, false),
        },
        // 307 | 308
        _ => (method, false),
    };

    Some((next_url, next_method, drop_body))
}

/// Converts an [`HttpRequest`] into a [`reqwest::RequestBuilder`] targeting
/// `client`.
///
/// This is kept separate from [`ReqwestClient::execute`] so the conversion
/// can be exercised in tests by calling `.build()` on the result, without
/// performing any network I/O.
fn to_reqwest_request(req: &HttpRequest, client: &reqwest::Client) -> reqwest::RequestBuilder {
    let method = match req.method {
        Method::Get => reqwest::Method::GET,
        Method::Post => reqwest::Method::POST,
    };
    let mut builder = client.request(method, req.url.clone());
    for (name, value) in &req.headers {
        builder = builder.header(name, value);
    }
    match &req.body {
        Some(Body::Form(pairs)) => builder.form(pairs),
        None => builder,
    }
}

/// Extracts a response's headers as owned name/value string pairs.
///
/// Header values are latin1-ish byte strings on the wire; lossy UTF-8
/// decoding is fine here since indexer definitions only ever inspect a
/// handful of well-known, ASCII header values.
fn response_headers(response: &reqwest::Response) -> Vec<(String, String)> {
    response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect()
}

impl HttpClient for ReqwestClient {
    fn execute(
        &self,
        req: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send {
        // `reqwest::Client` wraps an `Arc` internally, so cloning it here is
        // cheap and lets the loop below send as many hops as it needs
        // without borrowing `self`.
        let client = self.client.clone();
        async move {
            let mut current = req;
            // One initial send plus up to `MAX_REDIRECT_HOPS` redirect hops;
            // a redirect still pending after that many hops is treated as a
            // loop rather than followed further.
            for _ in 0..=MAX_REDIRECT_HOPS {
                let builder = to_reqwest_request(&current, &client);
                let response = builder
                    .send()
                    .await
                    .map_err(|err| HttpError::Transport(err.to_string()))?;

                let status = response.status().as_u16();
                let headers = response_headers(&response);

                let hop = current
                    .follow_redirects
                    .then(|| next_hop(status, &headers, &current.url, current.method))
                    .flatten();

                let Some((url, method, drop_body)) = hop else {
                    // Terminal response for this request: either not a
                    // redirect, or one this request isn't following.
                    let final_url = current.url.clone();
                    let body = response
                        .bytes()
                        .await
                        .map_err(|err| HttpError::Transport(err.to_string()))?
                        .to_vec();
                    return Ok(HttpResponse {
                        status,
                        headers,
                        body,
                        final_url,
                    });
                };

                current = HttpRequest {
                    method,
                    url,
                    headers: current.headers,
                    body: if drop_body { None } else { current.body },
                    follow_redirects: current.follow_redirects,
                };
            }
            Err(HttpError::Transport("too many redirects".to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{
        Body, HttpClient, HttpError, HttpRequest, MAX_REDIRECT_HOPS, Method, ReqwestClient,
        next_hop, to_reqwest_request,
    };

    fn login_request(headers: Vec<(String, String)>) -> HttpRequest {
        HttpRequest {
            method: Method::Post,
            url: "https://t.example/login".parse().unwrap(),
            headers,
            body: Some(Body::Form(vec![
                ("user".to_string(), "a".to_string()),
                ("pass".to_string(), "b".to_string()),
            ])),
            follow_redirects: true,
        }
    }

    #[test]
    fn new_builds_a_client_without_error() {
        ReqwestClient::new().unwrap();
    }

    #[test]
    fn with_cookie_builds_a_client_without_error() {
        let base: url::Url = "https://t.example/".parse().unwrap();
        ReqwestClient::with_cookie(&base, "session=abc123").unwrap();
    }

    #[test]
    fn converts_method_url_and_form_body() {
        let client = reqwest::Client::new();
        let req = login_request(vec![]);

        let built = to_reqwest_request(&req, &client).build().unwrap();

        assert_eq!(built.method(), &reqwest::Method::POST);
        assert_eq!(built.url().as_str(), "https://t.example/login");
        let body_bytes = built.body().and_then(reqwest::Body::as_bytes).unwrap();
        assert_eq!(body_bytes, b"user=a&pass=b");
    }

    #[test]
    fn form_body_sets_urlencoded_content_type() {
        let client = reqwest::Client::new();
        let req = login_request(vec![]);

        let built = to_reqwest_request(&req, &client).build().unwrap();

        let content_type = built.headers().get("content-type").unwrap();
        assert_eq!(content_type, "application/x-www-form-urlencoded");
    }

    #[test]
    fn header_pair_survives_conversion() {
        let client = reqwest::Client::new();
        let req = login_request(vec![("x-test".to_string(), "1".to_string())]);

        let built = to_reqwest_request(&req, &client).build().unwrap();

        let value = built.headers().get("x-test").unwrap();
        assert_eq!(value, "1");
    }

    #[test]
    fn repeated_header_names_all_survive() {
        let client = reqwest::Client::new();
        let req = login_request(vec![
            ("x-test".to_string(), "1".to_string()),
            ("x-test".to_string(), "2".to_string()),
        ]);

        let built = to_reqwest_request(&req, &client).build().unwrap();

        let values: Vec<&str> = built
            .headers()
            .get_all("x-test")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(values, vec!["1", "2"]);
    }

    fn loc(url: &str) -> Vec<(String, String)> {
        vec![("Location".to_string(), url.to_string())]
    }

    fn u(s: &str) -> url::Url {
        s.parse().unwrap()
    }

    #[test]
    fn a_302_with_a_relative_location_resolves_against_the_current_url() {
        let hop = next_hop(302, &loc("/next"), &u("https://t.example/a/b"), Method::Get).unwrap();
        assert_eq!(hop.0.as_str(), "https://t.example/next");
        assert_eq!(hop.1, Method::Get);
        assert!(!hop.2, "GET->GET keeps whatever body it had (none)");
    }

    #[test]
    fn a_302_with_an_absolute_location_uses_it_directly() {
        let hop = next_hop(
            302,
            &loc("https://other.example/target"),
            &u("https://t.example/a"),
            Method::Get,
        )
        .unwrap();
        assert_eq!(hop.0.as_str(), "https://other.example/target");
    }

    #[test]
    fn a_303_always_switches_to_get_and_drops_the_body_even_from_get() {
        let hop = next_hop(303, &loc("/next"), &u("https://t.example/"), Method::Get).unwrap();
        assert_eq!(hop.1, Method::Get);
        assert!(hop.2);
    }

    #[test]
    fn a_303_from_post_switches_to_get_and_drops_the_body() {
        let hop = next_hop(303, &loc("/next"), &u("https://t.example/"), Method::Post).unwrap();
        assert_eq!(hop.1, Method::Get);
        assert!(hop.2);
    }

    #[test]
    fn a_302_after_post_downgrades_to_get_and_drops_the_body() {
        let hop = next_hop(302, &loc("/next"), &u("https://t.example/"), Method::Post).unwrap();
        assert_eq!(hop.1, Method::Get);
        assert!(hop.2);
    }

    #[test]
    fn a_301_after_post_downgrades_to_get_and_drops_the_body() {
        let hop = next_hop(301, &loc("/next"), &u("https://t.example/"), Method::Post).unwrap();
        assert_eq!(hop.1, Method::Get);
        assert!(hop.2);
    }

    #[test]
    fn a_301_after_get_stays_get_and_keeps_the_body_flag_false() {
        let hop = next_hop(301, &loc("/next"), &u("https://t.example/"), Method::Get).unwrap();
        assert_eq!(hop.1, Method::Get);
        assert!(!hop.2);
    }

    #[test]
    fn a_307_preserves_a_post_method_and_does_not_drop_the_body() {
        let hop = next_hop(307, &loc("/next"), &u("https://t.example/"), Method::Post).unwrap();
        assert_eq!(hop.1, Method::Post);
        assert!(!hop.2);
    }

    #[test]
    fn a_308_preserves_a_post_method_and_does_not_drop_the_body() {
        let hop = next_hop(308, &loc("/next"), &u("https://t.example/"), Method::Post).unwrap();
        assert_eq!(hop.1, Method::Post);
        assert!(!hop.2);
    }

    #[test]
    fn a_missing_location_header_is_none() {
        assert!(next_hop(302, &[], &u("https://t.example/"), Method::Get).is_none());
    }

    #[test]
    fn a_non_redirect_status_is_none_even_with_a_location_header() {
        assert!(next_hop(200, &loc("/next"), &u("https://t.example/"), Method::Get).is_none());
        assert!(next_hop(404, &loc("/next"), &u("https://t.example/"), Method::Get).is_none());
        assert!(next_hop(500, &loc("/next"), &u("https://t.example/"), Method::Get).is_none());
    }

    #[test]
    fn an_unresolvable_location_is_none() {
        // Neither an absolute URL nor a valid relative reference against
        // the current URL (a bare, unescaped space is invalid in either
        // position).
        assert!(
            next_hop(
                302,
                &loc("http://\0"),
                &u("https://t.example/"),
                Method::Get
            )
            .is_none()
        );
    }

    #[test]
    fn the_location_header_name_is_matched_case_insensitively() {
        let headers = vec![("location".to_string(), "/next".to_string())];
        let hop = next_hop(302, &headers, &u("https://t.example/a"), Method::Get).unwrap();
        assert_eq!(hop.0.as_str(), "https://t.example/next");
    }

    #[test]
    fn get_request_has_no_body() {
        let client = reqwest::Client::new();
        let req = HttpRequest {
            method: Method::Get,
            url: "https://t.example/search".parse().unwrap(),
            headers: vec![],
            body: None,
            follow_redirects: true,
        };

        let built = to_reqwest_request(&req, &client).build().unwrap();

        assert_eq!(built.method(), &reqwest::Method::GET);
        assert!(built.body().is_none());
    }

    // --- End-to-end redirect handling against a real local server ---
    //
    // `next_hop`'s tests above prove the pure decision function in
    // isolation; these prove the loop actually built around it in
    // `execute` really disables `reqwest`'s own redirect handling and
    // drives the hops itself — the thing `next_hop`'s tests, by
    // construction, cannot demonstrate.

    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Spawns a plain-TCP HTTP/1.1 server on `127.0.0.1` that serves
    /// `responses` in order, one per accepted connection, then stops
    /// accepting. Every response declares `Connection: close` so each
    /// request opens a fresh connection rather than exercising `reqwest`'s
    /// keep-alive pooling, which this test has no need to model.
    ///
    /// Runs on a blocking thread (not a tokio task) since it uses
    /// `std::net::TcpListener` directly — simpler than pulling in
    /// `tokio::net` for what is, per test, at most three tiny fixed
    /// responses.
    fn spawn_responder(responses: Vec<String>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut socket, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0_u8; 1024];
                // Best-effort drain of the request; its content doesn't
                // matter to these fixtures.
                let _ = socket.read(&mut buf);
                let _ = socket.write_all(response.as_bytes());
                let _ = socket.flush();
            }
        });
        port
    }

    fn redirect_response(status: u16, reason: &str, location: &str) -> String {
        format!(
            "HTTP/1.1 {status} {reason}\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    fn ok_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn execute_follows_a_redirect_when_the_request_asks_for_it() {
        let target = redirect_response(302, "Found", "/landed");
        let landed = ok_response("landed here");
        let port = spawn_responder(vec![target, landed]);
        let client = ReqwestClient::new().unwrap();
        let req = HttpRequest {
            method: Method::Get,
            url: format!("http://127.0.0.1:{port}/start").parse().unwrap(),
            headers: vec![],
            body: None,
            follow_redirects: true,
        };

        let resp = client.execute(req).await.unwrap();

        assert_eq!(resp.status, 200);
        assert_eq!(resp.text(), "landed here");
        assert_eq!(resp.final_url.path(), "/landed");
    }

    #[tokio::test]
    async fn execute_does_not_follow_a_redirect_when_the_request_declines_it() {
        let target = redirect_response(302, "Found", "/landed");
        let port = spawn_responder(vec![target]);
        let client = ReqwestClient::new().unwrap();
        let req = HttpRequest {
            method: Method::Get,
            url: format!("http://127.0.0.1:{port}/start").parse().unwrap(),
            headers: vec![],
            body: None,
            follow_redirects: false,
        };

        let resp = client.execute(req).await.unwrap();

        // The redirect response itself comes back untouched — no second
        // request was ever made (`spawn_responder` only had one response
        // queued; a second request would hang trying to accept a
        // connection nothing serves, which `tokio::test`'s default timeout
        // would eventually surface as a failure).
        assert_eq!(resp.status, 302);
        assert_eq!(resp.final_url.path(), "/start");
    }

    #[tokio::test]
    async fn execute_gives_up_after_too_many_redirect_hops() {
        // Every response redirects right back to the same path, forever —
        // proving the loop actually terminates rather than looping forever
        // chasing an endless redirect chain.
        let responses: Vec<String> = (0..(MAX_REDIRECT_HOPS as usize + 2))
            .map(|_| redirect_response(302, "Found", "/loop"))
            .collect();
        let port = spawn_responder(responses);
        let client = ReqwestClient::new().unwrap();
        let req = HttpRequest {
            method: Method::Get,
            url: format!("http://127.0.0.1:{port}/loop").parse().unwrap(),
            headers: vec![],
            body: None,
            follow_redirects: true,
        };

        let err = client.execute(req).await.unwrap_err();

        assert!(matches!(err, HttpError::Transport(msg) if msg == "too many redirects"));
    }
}
