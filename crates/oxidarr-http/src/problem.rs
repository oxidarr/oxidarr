//! Uniform error-to-response mapping.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// An HTTP error rendered as a small JSON body: `{"message": "..."}`.
///
/// This is intentionally minimal — it covers what ships today. Extend it
/// with more fields (an error code, a `description`, …) only once a
/// consumer actually needs them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The HTTP status to respond with.
    pub status: StatusCode,
    /// A human-readable description of what went wrong.
    pub message: String,
}

impl Problem {
    /// Build a problem with the given `status` and `message`.
    #[must_use]
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "message": self.message })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use super::Problem;

    #[tokio::test]
    async fn renders_status_and_message_as_json() {
        let problem = Problem::new(StatusCode::NOT_FOUND, "not found");
        let response = problem.into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json, serde_json::json!({ "message": "not found" }));
    }
}
