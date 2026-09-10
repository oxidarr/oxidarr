//! Error types for the indexer HTTP layer.

use crate::login::LoginError;

/// Transport-level failure returned by an [`crate::client::HttpClient`]
/// implementation.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// The request could not be executed at all (DNS, connect, TLS, I/O, …).
    #[error("transport error: {0}")]
    Transport(String),
    /// The request was executed but the server responded with a status the
    /// caller treats as an error.
    #[error("unexpected status {status} from {url}")]
    Status {
        /// The HTTP status code returned by the server.
        status: u16,
        /// The URL that produced the status.
        url: url::Url,
    },
}

/// Errors produced while building requests for, or parsing responses from,
/// an indexer.
#[derive(Debug, thiserror::Error)]
pub enum IndexerError {
    /// Building an [`crate::client::HttpRequest`] for the named indexer
    /// definition failed.
    #[error("building request for {definition}: {reason}")]
    RequestBuild {
        /// The indexer definition the request was being built for.
        definition: String,
        /// Why building the request failed.
        reason: String,
    },
    /// The response body could not be parsed into the expected shape.
    #[error("parsing response: {0}")]
    Parse(String),
    /// The underlying transport failed.
    #[error(transparent)]
    Http(#[from] HttpError),
    /// The Cardigann engine failed to render or evaluate a definition.
    #[error(transparent)]
    Engine(#[from] oxidarr_cardigann::CardigannError),
    /// Logging in, or verifying an existing session, failed.
    #[error(transparent)]
    Login(#[from] LoginError),
}
