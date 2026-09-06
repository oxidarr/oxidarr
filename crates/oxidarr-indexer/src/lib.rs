//! Indexer protocol implementations and request execution
//!
//! The `Indexer` trait plus Newznab, Torznab, and Cardigann-backed implementations.
//! Owns request execution, rate limiting, and proxy support (SOCKS, HTTP, `FlareSolverr`).

pub mod builder;
pub mod client;
pub mod error;
pub mod login;
pub mod query;
pub mod reqwest_client;
pub mod rss;

/// In-memory [`HttpClient`](client::HttpClient) test double, compiled into
/// the library so downstream crates can reuse it in their own tests.
#[doc(hidden)]
pub mod testing;

pub use builder::build_search_requests;
pub use client::{Body, HttpClient, HttpRequest, HttpResponse, Method};
pub use error::{HttpError, IndexerError};
pub use login::{LoginError, authenticate, verify_login};
pub use query::{SearchQuery, Settings, scope_for};
pub use reqwest_client::ReqwestClient;
pub use rss::parse_feed;
