//! Shared axum server scaffolding for Oxidarr apps.
//!
//! Cross-app HTTP concerns that every Oxidarr server needs and none should
//! reimplement on its own: an API-key [`auth`] layer and a uniform
//! [`problem::Problem`] error-response type. Knows nothing about any
//! specific app's routes or domain types.
//!
//! # Mounting the auth layer
//!
//! ```
//! use axum::{middleware, routing::get, Router};
//! use oxidarr_http::auth::{require_api_key, ApiKey};
//!
//! let key = ApiKey::new("secret");
//! let _app: Router = Router::new()
//!     .route("/ping", get(|| async { "pong" }))
//!     .layer(middleware::from_fn_with_state(key, require_api_key));
//! ```
//!
//! # Known limitations
//!
//! - Pagination and sort conventions are not implemented yet; they land
//!   alongside the first paginated endpoint.

pub mod auth;
pub mod problem;

pub use auth::{ApiKey, require_api_key};
pub use problem::Problem;
