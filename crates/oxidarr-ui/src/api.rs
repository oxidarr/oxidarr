//! The UI's API client core: [`Http`] is the transport abstraction real
//! browser code (a later shell task — see `crate::fetch`'s own doc comment)
//! and this crate's own tests both implement; [`ApiClient`] is the typed
//! `/api/v1` surface built on top of it. See `crate::dto` for the
//! wire-shape types every endpoint here returns or accepts.
//!
//! This module has no `dioxus` import and no `wasm32`-only code (that lives
//! behind `crate::fetch`'s own `#[cfg(target_arch = "wasm32")]` gate) — it
//! compiles and its tests run on any native target, per this crate's own
//! testing design (`.local/plan-04-ui/design.md`'s "Testing" decision).
//!
//! # Error mapping
//!
//! Every endpoint maps its raw `(status, body)` response the same way:
//!
//! - `401` → [`UiError::Unauthorized`], regardless of body content — the one
//!   status a later key-prompt-flow task reacts to specially, so no
//!   `Problem`/`Decode` distinction is even attempted for it.
//! - Any other status outside `200..300`: the body is parsed as the
//!   server's own `{"message": "..."}` Problem shape
//!   ([`oxidarr_http::Problem`]'s wire contract, re-declared locally as
//!   [`ProblemBody`] rather than depending on that native-only crate).
//!   Success parsing that shape yields [`UiError::Problem`] carrying the
//!   message; a parse failure yields [`UiError::Decode`].
//! - `200..300` for an endpoint whose caller expects to decode a body as
//!   `T`: a JSON parse failure yields [`UiError::Decode`]; success yields
//!   `T`.
//! - `200..300` for an endpoint with nothing this client reads out of the
//!   body (`204 No Content`, or a `200` this client discards —
//!   `POST /indexer/test` and `POST /applications/test` both answer
//!   `200 []` on success): the body is never parsed at all, so an empty
//!   `204` body can never trip [`UiError::Decode`].
//! - [`Http::request`] itself failing is a transport-level failure with no
//!   response to map at all; this module never constructs
//!   [`UiError::Network`] itself, only propagates whatever
//!   [`Http::request`] returned via `?`.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::dto::{
    ApplicationResource, IndexerResource, ReleaseResource, SearchParams, SystemStatus,
};

/// The transport [`ApiClient`] runs every request through. Implemented by
/// this crate's own tests (a scripted fake) and, on `wasm32`, by a real
/// `fetch`-backed type — see `crate::fetch`'s own doc comment for why that
/// implementation does not exist yet.
///
/// `path` is the full URL [`ApiClient`] has already joined with its own
/// `base` before calling this — an [`Http`] implementation is pure
/// transport, with no notion of what "the base" is. `key`, when set, is the
/// configured API key this call should authenticate with; how a concrete
/// implementation attaches it (an `X-Api-Key` header, for the real fetch
/// backend) is that implementation's own concern, not this trait's.
///
/// `async fn` in a public trait (rather than a `-> impl Future` desugaring)
/// is a deliberate choice, not an oversight: it matches this trait's
/// specified shape and the rest of this codebase's native-async style.
/// `#[allow(async_fn_in_trait)]` suppresses the resulting "auto trait
/// bounds cannot be specified" lint — this trait has exactly one
/// implementor per compile target (a scripted fake in tests, a real
/// `fetch` backend on `wasm32`), so the missing `Send` bound the lint
/// warns about never matters in practice.
#[allow(async_fn_in_trait)]
pub trait Http {
    /// Executes one HTTP request, returning the raw `(status, body)` pair on
    /// any completed round trip — including a `4xx`/`5xx` response, which is
    /// not itself an `Err` here — or [`UiError::Network`] if the request
    /// could not be executed at all (a DNS/connection failure, the
    /// browser's `fetch` promise rejecting, ...).
    async fn request(
        &self,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: Option<String>,
    ) -> Result<(u16, String), UiError>;
}

/// Every way an [`ApiClient`] call can fail. See this module's own doc
/// comment for the exact status/body → variant mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiError {
    /// The server answered `401` — the configured key is missing or wrong.
    Unauthorized,
    /// A non-`2xx`, non-`401` response whose body parsed as the server's
    /// `{"message": "..."}` Problem shape; carries that message.
    Problem(String),
    /// [`Http::request`] could not complete the round trip at all; carries
    /// that failure's own description.
    Network(String),
    /// A response body that should have decoded as JSON did not; carries
    /// the decoder's own error message.
    Decode(String),
}

/// The server's own `{"message": "..."}` error body shape
/// ([`oxidarr_http::Problem`]'s wire contract) — declared locally so this
/// wasm-independent, dependency-light crate need not pull in that
/// native-only (`axum`-carrying) crate for one struct shape.
#[derive(Debug, Clone, serde::Deserialize)]
struct ProblemBody {
    message: String,
}

/// A typed `/api/v1` client, generic over its [`Http`] transport. Derives
/// [`Clone`] (whenever `H` itself does — both `NativeHttp` and `WasmHttp`
/// are stateless unit structs, so this is always available on either
/// target) so a caller holding this behind a `Signal` — every screen's own
/// `SharedApi` — can clone one out and drop the `Signal`'s read guard
/// before `.await`ing any of its own methods, rather than holding that
/// guard across an `.await` point.
#[derive(Debug, Clone)]
pub struct ApiClient<H: Http> {
    base: String,
    key: Option<String>,
    http: H,
}

impl<H: Http> ApiClient<H> {
    /// Builds a client against `base` (e.g. `"http://localhost:9696"`, no
    /// trailing slash expected — every endpoint path here already starts
    /// with `/`), with `key` sent as [`Http::request`]'s own `key`
    /// parameter on every call when set.
    pub fn new(base: impl Into<String>, key: Option<String>, http: H) -> Self {
        Self {
            base: base.into(),
            key,
            http,
        }
    }

    /// Replaces the stored API key — called once a later key-prompt-flow
    /// task collects one from the user.
    pub fn set_key(&mut self, key: Option<String>) {
        self.key = key;
    }

    /// `GET /api/v1/system/status`.
    ///
    /// # Errors
    ///
    /// See this module's own "Error mapping" section.
    pub async fn system_status(&self) -> Result<SystemStatus, UiError> {
        self.get("/api/v1/system/status").await
    }

    /// `GET /api/v1/indexer`.
    ///
    /// # Errors
    ///
    /// See this module's own "Error mapping" section.
    pub async fn indexers(&self) -> Result<Vec<IndexerResource>, UiError> {
        self.get("/api/v1/indexer").await
    }

    /// `GET /api/v1/indexer/schema`.
    ///
    /// # Errors
    ///
    /// See this module's own "Error mapping" section.
    pub async fn indexer_schema(&self) -> Result<Vec<IndexerResource>, UiError> {
        self.get("/api/v1/indexer/schema").await
    }

    /// `POST /api/v1/indexer`.
    ///
    /// # Errors
    ///
    /// See this module's own "Error mapping" section.
    pub async fn create_indexer(&self, r: &IndexerResource) -> Result<IndexerResource, UiError> {
        self.post_json("/api/v1/indexer", r).await
    }

    /// `PUT /api/v1/indexer/{id}`.
    ///
    /// # Errors
    ///
    /// See this module's own "Error mapping" section.
    pub async fn update_indexer(&self, r: &IndexerResource) -> Result<IndexerResource, UiError> {
        self.put_json(&format!("/api/v1/indexer/{}", r.id), r).await
    }

    /// `DELETE /api/v1/indexer/{id}`.
    ///
    /// # Errors
    ///
    /// See this module's own "Error mapping" section.
    pub async fn delete_indexer(&self, id: i32) -> Result<(), UiError> {
        self.delete(&format!("/api/v1/indexer/{id}")).await
    }

    /// `POST /api/v1/indexer/test`.
    ///
    /// # Errors
    ///
    /// See this module's own "Error mapping" section.
    pub async fn test_indexer(&self, r: &IndexerResource) -> Result<(), UiError> {
        self.post_unit("/api/v1/indexer/test", r).await
    }

    /// `GET /api/v1/applications`.
    ///
    /// # Errors
    ///
    /// See this module's own "Error mapping" section.
    pub async fn applications(&self) -> Result<Vec<ApplicationResource>, UiError> {
        self.get("/api/v1/applications").await
    }

    /// `POST /api/v1/applications`.
    ///
    /// # Errors
    ///
    /// See this module's own "Error mapping" section.
    pub async fn create_application(
        &self,
        r: &ApplicationResource,
    ) -> Result<ApplicationResource, UiError> {
        self.post_json("/api/v1/applications", r).await
    }

    /// `PUT /api/v1/applications/{id}`.
    ///
    /// # Errors
    ///
    /// See this module's own "Error mapping" section.
    pub async fn update_application(
        &self,
        r: &ApplicationResource,
    ) -> Result<ApplicationResource, UiError> {
        self.put_json(&format!("/api/v1/applications/{}", r.id), r)
            .await
    }

    /// `DELETE /api/v1/applications/{id}`.
    ///
    /// # Errors
    ///
    /// See this module's own "Error mapping" section.
    pub async fn delete_application(&self, id: i32) -> Result<(), UiError> {
        self.delete(&format!("/api/v1/applications/{id}")).await
    }

    /// `POST /api/v1/applications/test`.
    ///
    /// # Errors
    ///
    /// See this module's own "Error mapping" section.
    pub async fn test_application(&self, r: &ApplicationResource) -> Result<(), UiError> {
        self.post_unit("/api/v1/applications/test", r).await
    }

    /// `GET /api/v1/search`, with `q`'s own [`SearchParams::to_query_string`]
    /// appended.
    ///
    /// # Errors
    ///
    /// See this module's own "Error mapping" section.
    pub async fn search(&self, q: &SearchParams) -> Result<Vec<ReleaseResource>, UiError> {
        let path = format!("/api/v1/search{}", q.to_query_string());
        self.get(&path).await
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, UiError> {
        let (status, body) = self.call("GET", path, None).await?;
        decode(status, &body)
    }

    async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, UiError> {
        let payload = encode(body)?;
        let (status, response) = self.call("POST", path, Some(payload)).await?;
        decode(status, &response)
    }

    async fn put_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, UiError> {
        let payload = encode(body)?;
        let (status, response) = self.call("PUT", path, Some(payload)).await?;
        decode(status, &response)
    }

    async fn post_unit(&self, path: &str, body: &impl Serialize) -> Result<(), UiError> {
        let payload = encode(body)?;
        let (status, response) = self.call("POST", path, Some(payload)).await?;
        decode_unit(status, &response)
    }

    async fn delete(&self, path: &str) -> Result<(), UiError> {
        let (status, response) = self.call("DELETE", path, None).await?;
        decode_unit(status, &response)
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
    ) -> Result<(u16, String), UiError> {
        let url = format!("{}{path}", self.base);
        self.http
            .request(method, &url, self.key.as_deref(), body)
            .await
    }
}

/// Serializes `body` to a JSON string, or [`UiError::Decode`] on failure —
/// reusing that variant rather than adding a fifth for what should never
/// happen with this crate's own plain-data DTOs (the only way it can fail is
/// a non-finite `f32` on a [`crate::dto::ReleaseResource`], which this
/// client never sends as a request body in the first place).
fn encode(body: &impl Serialize) -> Result<String, UiError> {
    serde_json::to_string(body).map_err(|err| UiError::Decode(err.to_string()))
}

/// Maps one completed `(status, body)` round trip onto `T`, or the
/// corresponding [`UiError`] — see this module's own doc comment for the
/// exact rules.
fn decode<T: DeserializeOwned>(status: u16, body: &str) -> Result<T, UiError> {
    if status == 401 {
        return Err(UiError::Unauthorized);
    }
    if !(200..300).contains(&status) {
        return Err(problem_error(body));
    }
    serde_json::from_str(body).map_err(|err| UiError::Decode(err.to_string()))
}

/// Like [`decode`], for an endpoint whose success body carries nothing this
/// client reads — see this module's own doc comment.
fn decode_unit(status: u16, body: &str) -> Result<(), UiError> {
    if status == 401 {
        return Err(UiError::Unauthorized);
    }
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(problem_error(body))
    }
}

/// Parses `body` as the server's `{"message": "..."}` Problem shape —
/// [`UiError::Problem`] on success, [`UiError::Decode`] when `body` is not
/// even valid JSON in that shape.
fn problem_error(body: &str) -> UiError {
    match serde_json::from_str::<ProblemBody>(body) {
        Ok(problem) => UiError::Problem(problem.message),
        Err(err) => UiError::Decode(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;
    use crate::dto::{Field, SyncLevel};

    /// One recorded call [`FakeHttp`] received, for tests that assert on
    /// method/path/key/body rather than just the mapped result.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Call {
        method: String,
        path: String,
        key: Option<String>,
        body: Option<String>,
    }

    /// A scripted [`Http`]: returns its `responses` queue's front in order,
    /// one per call, recording every call it received along the way.
    struct FakeHttp {
        calls: RefCell<Vec<Call>>,
        responses: RefCell<VecDeque<Result<(u16, String), UiError>>>,
    }

    impl FakeHttp {
        fn new(responses: Vec<Result<(u16, String), UiError>>) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                responses: RefCell::new(responses.into()),
            }
        }

        fn ok(body: &str) -> Self {
            Self::new(vec![Ok((200, body.to_string()))])
        }
    }

    impl Http for FakeHttp {
        // This fake never actually needs to `.await` anything (it just pops
        // a scripted response off a queue), unlike every real `Http`
        // implementation this trait exists for — `async fn` here still
        // matches the trait signature exactly, so an allow is more honest
        // than restructuring a test double around a lint meant for
        // production code.
        // `unused_async_trait_impl` only exists in clippys newer than the
        // pinned 1.95 toolchain; `unknown_lints` keeps the allow valid on
        // both (same pattern as oxidarr-prowl's definitions.rs).
        #[allow(unknown_lints, clippy::unused_async_trait_impl)]
        async fn request(
            &self,
            method: &str,
            path: &str,
            key: Option<&str>,
            body: Option<String>,
        ) -> Result<(u16, String), UiError> {
            self.calls.borrow_mut().push(Call {
                method: method.to_string(),
                path: path.to_string(),
                key: key.map(str::to_string),
                body,
            });
            self.responses
                .borrow_mut()
                .pop_front()
                .unwrap_or(Ok((500, "no scripted response left".to_string())))
        }
    }

    fn client(http: FakeHttp, key: Option<&str>) -> ApiClient<FakeHttp> {
        ApiClient::new("http://oxidarr.local:9696", key.map(str::to_string), http)
    }

    #[tokio::test]
    async fn system_status_sends_a_get_with_no_body() {
        let fake = FakeHttp::ok(
            r#"{"appName":"Oxidarr","instanceName":"Oxidarr","version":"0.0.0","isProduction":true,"isDocker":false,"startTime":"2024-01-01T00:00:00+00:00","appData":"","authentication":"none","urlBase":""}"#,
        );
        let status = client(fake, None).system_status().await.unwrap();
        assert_eq!(status.app_name, "Oxidarr");
    }

    #[tokio::test]
    async fn every_call_joins_base_and_path_and_uses_get_for_a_list_endpoint() {
        let fake = FakeHttp::ok("[]");
        let api = client(fake, None);
        api.indexers().await.unwrap();
        let calls = api.http.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, "GET");
        assert_eq!(calls[0].path, "http://oxidarr.local:9696/api/v1/indexer");
        assert_eq!(calls[0].body, None);
    }

    #[tokio::test]
    async fn the_configured_key_is_passed_to_http_on_every_call() {
        let fake = FakeHttp::ok("[]");
        let api = client(fake, Some("s3cret"));
        api.indexers().await.unwrap();
        let calls = api.http.calls.borrow();
        assert_eq!(calls[0].key.as_deref(), Some("s3cret"));
    }

    #[tokio::test]
    async fn no_key_configured_passes_none_through_to_http() {
        let fake = FakeHttp::ok("[]");
        let api = client(fake, None);
        api.indexers().await.unwrap();
        let calls = api.http.calls.borrow();
        assert_eq!(calls[0].key, None);
    }

    fn sample_indexer() -> IndexerResource {
        IndexerResource {
            id: 0,
            name: "Example".to_string(),
            implementation: "Cardigann".to_string(),
            definition_name: "example".to_string(),
            description: String::new(),
            language: String::new(),
            enable: true,
            priority: 25,
            capabilities: crate::dto::IndexerCapabilities::default(),
            fields: vec![Field {
                name: "cookie".to_string(),
                label: "cookie".to_string(),
                kind: "textbox".to_string(),
                value: Some(serde_json::Value::String("abc".to_string())),
                select_options: None,
            }],
            sync_error: None,
        }
    }

    #[tokio::test]
    async fn create_indexer_sends_a_post_with_the_resource_as_json() {
        let created = sample_indexer();
        let response_body = serde_json::to_string(&IndexerResource {
            id: 7,
            ..created.clone()
        })
        .unwrap();
        let fake = FakeHttp::new(vec![Ok((201, response_body))]);
        let api = client(fake, None);

        let result = api.create_indexer(&created).await.unwrap();
        assert_eq!(result.id, 7);

        let calls = api.http.calls.borrow();
        assert_eq!(calls[0].method, "POST");
        assert_eq!(calls[0].path, "http://oxidarr.local:9696/api/v1/indexer");
        let sent: IndexerResource = serde_json::from_str(calls[0].body.as_ref().unwrap()).unwrap();
        assert_eq!(sent, created);
    }

    #[tokio::test]
    async fn update_indexer_sends_a_put_to_the_ids_own_path() {
        let resource = IndexerResource {
            id: 9,
            ..sample_indexer()
        };
        let fake = FakeHttp::ok(&serde_json::to_string(&resource).unwrap());
        let api = client(fake, None);

        api.update_indexer(&resource).await.unwrap();

        let calls = api.http.calls.borrow();
        assert_eq!(calls[0].method, "PUT");
        assert_eq!(calls[0].path, "http://oxidarr.local:9696/api/v1/indexer/9");
    }

    #[tokio::test]
    async fn delete_indexer_sends_a_delete_with_no_body_and_maps_204_to_ok() {
        let fake = FakeHttp::new(vec![Ok((204, String::new()))]);
        let api = client(fake, None);

        api.delete_indexer(4).await.unwrap();

        let calls = api.http.calls.borrow();
        assert_eq!(calls[0].method, "DELETE");
        assert_eq!(calls[0].path, "http://oxidarr.local:9696/api/v1/indexer/4");
        assert_eq!(calls[0].body, None);
    }

    #[tokio::test]
    async fn test_indexer_ignores_a_success_body_that_is_not_valid_json() {
        let fake = FakeHttp::new(vec![Ok((200, "not json at all".to_string()))]);
        let api = client(fake, None);

        api.test_indexer(&sample_indexer()).await.unwrap();
    }

    #[tokio::test]
    async fn a_401_maps_to_unauthorized_regardless_of_body() {
        let fake = FakeHttp::new(vec![Ok((401, "anything".to_string()))]);
        let api = client(fake, None);

        let err = api.indexers().await.unwrap_err();
        assert_eq!(err, UiError::Unauthorized);
    }

    #[tokio::test]
    async fn a_400_problem_json_maps_to_problem_with_the_message() {
        let fake = FakeHttp::new(vec![Ok((
            400,
            r#"{"message":"invalid definitionName \"..\""}"#.to_string(),
        ))]);
        let api = client(fake, None);

        let err = api.indexers().await.unwrap_err();
        assert_eq!(
            err,
            UiError::Problem("invalid definitionName \"..\"".to_string())
        );
    }

    #[tokio::test]
    async fn a_500_with_a_problem_body_maps_to_problem_too() {
        let fake = FakeHttp::new(vec![Ok((
            500,
            r#"{"message":"database is corrupt"}"#.to_string(),
        ))]);
        let api = client(fake, None);

        let err = api.indexers().await.unwrap_err();
        assert_eq!(err, UiError::Problem("database is corrupt".to_string()));
    }

    #[tokio::test]
    async fn a_non_problem_error_body_maps_to_decode() {
        let fake = FakeHttp::new(vec![Ok((502, "<html>bad gateway</html>".to_string()))]);
        let api = client(fake, None);

        let err = api.indexers().await.unwrap_err();
        assert!(matches!(err, UiError::Decode(_)), "was: {err:?}");
    }

    #[tokio::test]
    async fn malformed_json_on_a_2xx_maps_to_decode() {
        let fake = FakeHttp::new(vec![Ok((200, "{not valid json".to_string()))]);
        let api = client(fake, None);

        let err = api.indexers().await.unwrap_err();
        assert!(matches!(err, UiError::Decode(_)), "was: {err:?}");
    }

    #[tokio::test]
    async fn a_transport_failure_propagates_as_the_same_network_error() {
        let fake = FakeHttp::new(vec![Err(UiError::Network(
            "connection refused".to_string(),
        ))]);
        let api = client(fake, None);

        let err = api.indexers().await.unwrap_err();
        assert_eq!(err, UiError::Network("connection refused".to_string()));
    }

    fn sample_application() -> ApplicationResource {
        ApplicationResource {
            id: 0,
            name: "My Sonarr".to_string(),
            implementation: "Sonarr".to_string(),
            sync_level: SyncLevel::FullSync,
            fields: vec![
                Field {
                    name: "baseUrl".to_string(),
                    label: "baseUrl".to_string(),
                    kind: "textbox".to_string(),
                    value: Some(serde_json::Value::String(
                        "http://sonarr.example".to_string(),
                    )),
                    select_options: None,
                },
                Field {
                    name: "apiKey".to_string(),
                    label: "apiKey".to_string(),
                    kind: "textbox".to_string(),
                    value: Some(serde_json::Value::String("key".to_string())),
                    select_options: None,
                },
            ],
            sync_error: None,
        }
    }

    #[tokio::test]
    async fn applications_endpoints_use_the_applications_path() {
        let fake = FakeHttp::ok("[]");
        let api = client(fake, None);
        api.applications().await.unwrap();
        let calls = api.http.calls.borrow();
        assert_eq!(
            calls[0].path,
            "http://oxidarr.local:9696/api/v1/applications"
        );
    }

    #[tokio::test]
    async fn delete_application_sends_a_delete_to_the_ids_own_path() {
        let fake = FakeHttp::new(vec![Ok((204, String::new()))]);
        let api = client(fake, None);
        api.delete_application(3).await.unwrap();
        let calls = api.http.calls.borrow();
        assert_eq!(calls[0].method, "DELETE");
        assert_eq!(
            calls[0].path,
            "http://oxidarr.local:9696/api/v1/applications/3"
        );
    }

    #[tokio::test]
    async fn test_application_posts_to_the_test_path_and_ignores_the_body() {
        let fake = FakeHttp::new(vec![Ok((200, "[]".to_string()))]);
        let api = client(fake, None);
        api.test_application(&sample_application()).await.unwrap();
        let calls = api.http.calls.borrow();
        assert_eq!(
            calls[0].path,
            "http://oxidarr.local:9696/api/v1/applications/test"
        );
    }

    #[tokio::test]
    async fn create_application_round_trips_the_sync_level() {
        let resource = sample_application();
        let response_body = serde_json::to_string(&ApplicationResource {
            id: 5,
            ..resource.clone()
        })
        .unwrap();
        let fake = FakeHttp::new(vec![Ok((201, response_body))]);
        let api = client(fake, None);

        let result = api.create_application(&resource).await.unwrap();
        assert_eq!(result.id, 5);
        assert_eq!(result.sync_level, SyncLevel::FullSync);
    }

    #[tokio::test]
    async fn search_appends_the_built_query_string() {
        let fake = FakeHttp::ok("[]");
        let api = client(fake, None);
        let params = SearchParams {
            query: Some("ubuntu".to_string()),
            indexer_ids: vec![1, 2],
            categories: vec![],
        };

        api.search(&params).await.unwrap();

        let calls = api.http.calls.borrow();
        assert_eq!(
            calls[0].path,
            "http://oxidarr.local:9696/api/v1/search?query=ubuntu&indexerIds=1,2"
        );
    }

    #[tokio::test]
    async fn search_with_no_params_hits_the_bare_search_path() {
        let fake = FakeHttp::ok("[]");
        let api = client(fake, None);

        api.search(&SearchParams::default()).await.unwrap();

        let calls = api.http.calls.borrow();
        assert_eq!(calls[0].path, "http://oxidarr.local:9696/api/v1/search");
    }
}
