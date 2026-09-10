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
/// 30 second timeout, and transparent gzip decompression.
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

impl HttpClient for ReqwestClient {
    fn execute(
        &self,
        req: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send {
        let builder = to_reqwest_request(&req, &self.client);
        async move {
            let response = builder
                .send()
                .await
                .map_err(|err| HttpError::Transport(err.to_string()))?;

            let status = response.status().as_u16();
            let final_url = response.url().clone();
            // Header values are latin1-ish byte strings on the wire; lossy
            // UTF-8 decoding is fine here since indexer definitions only
            // ever inspect a handful of well-known, ASCII header values.
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_string(),
                        String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    )
                })
                .collect();
            let body = response
                .bytes()
                .await
                .map_err(|err| HttpError::Transport(err.to_string()))?
                .to_vec();

            Ok(HttpResponse {
                status,
                headers,
                body,
                final_url,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{Body, HttpRequest, Method, ReqwestClient, to_reqwest_request};

    fn login_request(headers: Vec<(String, String)>) -> HttpRequest {
        HttpRequest {
            method: Method::Post,
            url: "https://t.example/login".parse().unwrap(),
            headers,
            body: Some(Body::Form(vec![
                ("user".to_string(), "a".to_string()),
                ("pass".to_string(), "b".to_string()),
            ])),
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

    #[test]
    fn get_request_has_no_body() {
        let client = reqwest::Client::new();
        let req = HttpRequest {
            method: Method::Get,
            url: "https://t.example/search".parse().unwrap(),
            headers: vec![],
            body: None,
        };

        let built = to_reqwest_request(&req, &client).build().unwrap();

        assert_eq!(built.method(), &reqwest::Method::GET);
        assert!(built.body().is_none());
    }
}
