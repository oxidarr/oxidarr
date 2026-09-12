//! In-memory [`HttpClient`] test double.
//!
//! This module is compiled into the library unconditionally (not gated on
//! `#[cfg(test)]`) because it is used from integration tests in this crate
//! as well as from unit tests in downstream crates that depend on
//! `oxidarr-indexer`. It is re-exported hidden from docs; treat it as test
//! infrastructure, not part of the public API surface.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex};

use crate::client::{HttpClient, HttpRequest, HttpResponse, Method};
use crate::error::HttpError;

type Matcher = Box<dyn Fn(&HttpRequest) -> bool + Send + Sync>;

struct Expectation {
    matcher: Matcher,
    response: HttpResponse,
}

struct State {
    expectations: VecDeque<Expectation>,
    requests: Vec<HttpRequest>,
}

/// An [`HttpClient`] backed by a fixed, ordered list of expected requests.
///
/// Each call to [`FakeClient::execute`] consumes the next expectation in
/// order. A request that does not match the expected matcher, or that
/// arrives with no expectations left, fails loudly with
/// [`HttpError::Transport`] rather than silently returning a default
/// response — tests should never mistake an unexpected call for a 404.
///
/// `Clone`: the shared state lives behind an `Arc`, so every clone
/// observes the same expectations/requests as the original — needed by any
/// consumer (e.g. `oxidarr-prowl`'s Torznab router) whose `axum` `State`
/// requires its client type to be `Clone`, where a naive per-clone-fresh
/// state would silently desync the clone actually handling a request from
/// the handle a test asserts against afterwards.
#[derive(Clone)]
pub struct FakeClient {
    state: Arc<Mutex<State>>,
}

impl FakeClient {
    /// Creates a `FakeClient` with no expectations set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                expectations: VecDeque::new(),
                requests: Vec::new(),
            })),
        }
    }

    /// Registers the next expected request and the response to return for
    /// it. Expectations are matched strictly in the order they were added.
    #[must_use]
    pub fn expect(
        self,
        matcher: impl Fn(&HttpRequest) -> bool + Send + Sync + 'static,
        response: HttpResponse,
    ) -> Self {
        self.state
            .lock()
            .expect("FakeClient mutex poisoned")
            .expectations
            .push_back(Expectation {
                matcher: Box::new(matcher),
                response,
            });
        self
    }

    /// Returns clones of every request executed so far, in execution order.
    #[must_use]
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.state
            .lock()
            .expect("FakeClient mutex poisoned")
            .requests
            .clone()
    }
}

impl Default for FakeClient {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for FakeClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock().expect("FakeClient mutex poisoned");
        f.debug_struct("FakeClient")
            .field("pending_expectations", &state.expectations.len())
            .field("recorded_requests", &state.requests.len())
            .finish()
    }
}

impl HttpClient for FakeClient {
    fn execute(
        &self,
        req: HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send {
        let mut state = self.state.lock().expect("FakeClient mutex poisoned");
        state.requests.push(req.clone());
        let result = match state.expectations.pop_front() {
            Some(expectation) if (expectation.matcher)(&req) => Ok(expectation.response),
            _ => Err(HttpError::Transport(format!(
                "unexpected request: {} {}",
                method_str(req.method),
                req.url
            ))),
        };
        std::future::ready(result)
    }
}

const fn method_str(method: Method) -> &'static str {
    match method {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Delete => "DELETE",
    }
}

/// Builds a `200 OK` `text/html` response for use with [`FakeClient`].
///
/// # Panics
///
/// Panics if `final_url` is not a valid URL. This is a test helper only —
/// callers are expected to pass literal, well-formed URLs.
#[must_use]
pub fn ok_html(final_url: &str, body: &str) -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: vec![("content-type".to_string(), "text/html".to_string())],
        body: body.as_bytes().to_vec(),
        final_url: final_url.parse().expect("ok_html: invalid URL"),
    }
}
