//! Indexer protocol implementations and request execution.
//!
//! The [`HttpClient`] trait ([`client`]) abstracts the transport a search
//! runs over; [`ReqwestClient`] is the production implementation and
//! [`testing::FakeClient`] an in-memory test double sharing the same
//! interface. [`builder`] renders a Cardigann [`Definition`](oxidarr_cardigann::model::Definition)'s
//! `search` block into concrete [`HttpRequest`]s for a given
//! [`SearchQuery`]/[`Settings`] pair, and [`login`] drives the four
//! authentication flows a definition's `login.method` can declare (`post`,
//! `get`, `cookie`, `form`), plus session verification and login-failure
//! detection. [`rss`] parses Newznab/Torznab feed responses into
//! [`Release`](oxidarr_core::Release) values, and [`indexer`] wraps that
//! parsing in [`NewznabIndexer`]/[`TorznabIndexer`], two thin, near-identical
//! implementations of the [`Indexer`] trait. [`cardigann`] composes all of
//! the above — login, request building, execution, and extraction — into
//! [`CardigannIndexer`], the [`Indexer`] implementation for Cardigann-defined
//! trackers.
//!
//! # Known limitations
//!
//! - A definition's `followredirect` field is parsed but not honoured:
//!   requests always follow redirects, regardless of what the definition
//!   declares.
//! - `t=caps` (indexer capability negotiation, including category mappings)
//!   is not fetched or modelled; it lands with the category-mapping
//!   subsystem.
//! - Response bodies are decoded as UTF-8 with invalid sequences replaced.
//!   Definitions that declare a non-UTF-8 page encoding (`windows-1251` and
//!   similar, roughly 8% of the corpus) are decoded lossily rather than
//!   through their declared encoding.

pub mod builder;
pub mod cardigann;
pub mod client;
pub mod error;
pub mod indexer;
pub mod login;
pub mod query;
pub mod reqwest_client;
pub mod rss;

/// In-memory [`HttpClient`](client::HttpClient) test double, compiled into
/// the library so downstream crates can reuse it in their own tests.
#[doc(hidden)]
pub mod testing;

pub use builder::build_search_requests;
pub use cardigann::CardigannIndexer;
pub use client::{Body, HttpClient, HttpRequest, HttpResponse, Method};
pub use error::{HttpError, IndexerError};
pub use indexer::{Indexer, NewznabIndexer, TorznabIndexer};
pub use login::{LoginError, authenticate, verify_login};
pub use query::{SearchQuery, Settings, scope_for};
pub use reqwest_client::ReqwestClient;
pub use rss::parse_feed;
