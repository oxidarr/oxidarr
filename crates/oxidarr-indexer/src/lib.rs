//! Indexer protocol implementations and request execution
//!
//! The `Indexer` trait plus Newznab, Torznab, and Cardigann-backed implementations.
//! Owns request execution, rate limiting, and proxy support (SOCKS, HTTP, `FlareSolverr`).

pub mod client;
pub mod error;

/// In-memory [`HttpClient`](client::HttpClient) test double, compiled into
/// the library so downstream crates can reuse it in their own tests.
#[doc(hidden)]
pub mod testing;

pub use client::{Body, HttpClient, HttpRequest, HttpResponse, Method};
pub use error::{HttpError, IndexerError};
