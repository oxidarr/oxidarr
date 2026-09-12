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
//! - A search path's `followredirect` is honoured (see
//!   [`client::HttpRequest::follow_redirects`] and [`builder`]'s mapping),
//!   defaulting to following when the key is absent. A definition's
//!   top-level `followredirect` (distinct from each path's own) is still
//!   parsed but not consulted: login requests always follow redirects
//!   regardless of what it declares (see [`login::authenticate`]).
//! - `t=caps` category mapping is modelled and rendered for Cardigann-kind
//!   indexers, straight from each definition's own declared
//!   `caps`/`categorymappings` (see `oxidarr_cardigann::catmap::CategoryMap`
//!   and `oxidarr-prowl`'s `torznab::render_caps`). What is still absent is
//!   fetching `t=caps` *from* an upstream Newznab/Torznab tracker for a
//!   generic (non-Cardigann) feed — a `Newznab`/`Torznab`-kind indexer has
//!   no declared categories of its own to report, and is skipped rather than
//!   guessed at wherever this matters (see `oxidarr-prowl`'s `sync` module).
//! - Search bodies are decoded per the definition's declared `encoding`;
//!   login pages are not yet — see [`login::authenticate`]'s doc comment.

pub mod builder;
pub mod cardigann;
pub mod client;
mod decode;
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
