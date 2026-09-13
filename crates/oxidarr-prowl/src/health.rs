//! An unauthenticated liveness endpoint.
//!
//! Real Prowlarr serves `GET /ping` without authentication, so this is a
//! compatibility gain as well as what the container's `HEALTHCHECK` probes —
//! a healthcheck holds no API key and must not need one.
//!
//! This router is merged in [`crate::server::app`] *beside* the `/api/v1`
//! router rather than inside it. That placement is the whole mechanism: the
//! API-key layer wraps only `/api/v1`, so a route merged alongside it never
//! passes through that layer.
//!
//! It needs no entry in `crate::ui`'s own `is_reserved_path`. That guard
//! exists to reject API-shaped paths that reach the UI's *fallback*, and a
//! named route never reaches a fallback — the same mechanism that already
//! protects `/api/v1/*` and `/{indexer_id}/api` protects this.

use axum::Json;
use axum::Router;
use axum::routing::get;
use serde_json::{Value, json};

/// Builds the health router: a single unauthenticated `GET /ping`.
pub fn router() -> Router {
    Router::new().route("/ping", get(ping))
}

/// Answers `{"status":"OK"}`, matching real Prowlarr's own response body.
async fn ping() -> Json<Value> {
    Json(json!({ "status": "OK" }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn ping_answers_ok_without_any_api_key() {
        // The container's HEALTHCHECK has no credentials, so this route
        // must answer before the API-key layer is ever consulted.
        let response = router()
            .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        assert_eq!(&body[..], br#"{"status":"OK"}"#);
    }

    #[tokio::test]
    async fn ping_is_json() {
        let response = router()
            .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap();
        assert_eq!(content_type, "application/json");
    }
}
