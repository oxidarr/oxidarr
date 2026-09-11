//! Composes login, request building, execution, and extraction into a
//! single [`Indexer`] implementation for Cardigann-defined trackers.
//!
//! [`CardigannIndexer::search`] is the one place every other building
//! block meets: [`build_search_requests`] renders the
//! definition's `search` block into concrete requests, each is executed
//! through the injected [`HttpClient`], [`check_error_rules`] inspects the
//! response body against `search.error`, and a clean body is handed to
//! [`oxidarr_cardigann::engine::extract`].
//!
//! `search` never authenticates eagerly — no session concept exists here to
//! tell an already-valid session apart from an expired one, and real
//! private trackers rate-limit or flag accounts for frequent re-auth, so
//! paying for a login round trip on every call would be actively harmful
//! once a caller (e.g. an HTTP search endpoint) calls `search` constantly.
//! Instead, [`authenticate`] runs reactively, only when a response actually
//! trips `search.error` (see [`CardigannIndexer::execute_with_reauth`]) — a
//! cold session's first search costs one wasted GET before that kicks in,
//! which is what Jackett's own `CardigannIndexer` effectively does too.

use std::fmt;
use std::future::Future;

use chrono::{DateTime, Utc};
use oxidarr_cardigann::FilterCtx;
use oxidarr_cardigann::engine::extract;
use oxidarr_cardigann::model::Definition;
use oxidarr_core::Release;

use crate::builder::build_search_requests;
use crate::client::{HttpClient, HttpRequest};
use crate::decode::decode_body;
use crate::error::IndexerError;
use crate::indexer::Indexer;
use crate::login::{LoginError, authenticate, check_error_rules};
use crate::query::{SearchQuery, Settings};

/// An [`Indexer`] driven entirely by a parsed Cardigann [`Definition`].
///
/// Holds the definition, resolved user [`Settings`], the [`HttpClient`]
/// requests are executed through, and a clock hook (see
/// [`CardigannIndexer::with_clock`]) used to stamp the [`FilterCtx`] passed
/// to [`oxidarr_cardigann::engine::extract`].
pub struct CardigannIndexer<C: HttpClient> {
    def: Definition,
    settings: Settings,
    client: C,
    now_fn: fn() -> DateTime<Utc>,
}

impl<C: HttpClient> CardigannIndexer<C> {
    /// Builds a `CardigannIndexer` targeting `def`, authenticating and
    /// searching through `client`, stamping extracted releases with the
    /// real wall clock (`Utc::now`).
    #[must_use]
    pub fn new(def: Definition, settings: Settings, client: C) -> Self {
        Self::with_clock(def, settings, client, Utc::now)
    }

    /// Builds a `CardigannIndexer` with an injected clock, so tests can
    /// pin the instant [`FilterCtx::now`] carries (and, through it, any
    /// `dateparse`/`timeparse`/`timeago`/`fuzzytime` filter result that
    /// depends on "now") to a fixed value rather than the real wall clock.
    /// `now` is a plain `fn` pointer (not a generic `Fn`), matching the
    /// signature `Utc::now` itself already has.
    #[must_use]
    pub fn with_clock(
        def: Definition,
        settings: Settings,
        client: C,
        now: fn() -> DateTime<Utc>,
    ) -> Self {
        Self {
            def,
            settings,
            client,
            now_fn: now,
        }
    }

    /// Executes `req` and checks the response against `search.error`.
    ///
    /// A rejection ([`LoginError::Rejected`]) is retried exactly once: when
    /// `def.login` is set, [`authenticate`] runs again and `req` is
    /// reissued, whose own result (success or a second rejection) is
    /// returned as-is with no further retry. When `def.login` is absent,
    /// there is nothing to re-authenticate with, so the rejection is
    /// returned immediately. Any other error rule failure (a malformed
    /// selector — [`LoginError::Definition`]) is likewise never retried, and
    /// any transport failure propagates immediately.
    async fn execute_with_reauth(&self, req: HttpRequest) -> Result<String, IndexerError> {
        match self.execute_and_check(req.clone()).await {
            Ok(body) => Ok(body),
            Err(IndexerError::Login(LoginError::Rejected(_))) if self.def.login.is_some() => {
                authenticate(&self.client, &self.def, &self.settings).await?;
                self.execute_and_check(req).await
            }
            Err(other) => Err(other),
        }
    }

    /// Executes `req` and, on a 2xx-or-not response body, runs
    /// `def.search.error` rules over it via [`check_error_rules`].
    ///
    /// The body is decoded per `def.encoding` (via `decode_body`) rather
    /// than assumed to be UTF-8 — roughly 8% of the corpus declares a
    /// non-UTF-8 page encoding (`windows-1251` and similar), which a plain
    /// UTF-8-lossy decode would mangle.
    ///
    /// # Errors
    /// Returns [`IndexerError::Http`] on a transport failure, or
    /// [`IndexerError::Login`] when an error rule matches (or fails to
    /// compile).
    async fn execute_and_check(&self, req: HttpRequest) -> Result<String, IndexerError> {
        let resp = self.client.execute(req).await?;
        let body = decode_body(&resp.body, &self.def.encoding);
        check_error_rules(&self.def.search.error, &body).map_err(IndexerError::Login)?;
        Ok(body)
    }

    async fn search_inner(&self, q: &SearchQuery) -> Result<Vec<Release>, IndexerError> {
        // No eager `authenticate` call here — see this module's doc comment
        // for why. A rejected response is handled reactively, per request,
        // by `execute_with_reauth` below.
        let requests = build_search_requests(&self.def, q, &self.settings)?;
        let config = self.settings.resolved(&self.def);
        let ctx = FilterCtx {
            now: (self.now_fn)(),
            keywords: q.keywords(),
        };

        let mut releases = Vec::new();
        for req in requests {
            let body = self.execute_with_reauth(req).await?;
            releases.extend(extract(&self.def, &body, &config, &ctx)?);
        }
        Ok(releases)
    }
}

impl<C: HttpClient> Indexer for CardigannIndexer<C> {
    fn search(
        &self,
        q: &SearchQuery,
    ) -> impl Future<Output = Result<Vec<Release>, IndexerError>> + Send {
        self.search_inner(q)
    }
}

impl<C: HttpClient> fmt::Debug for CardigannIndexer<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CardigannIndexer")
            .field("definition", &self.def.id)
            .finish_non_exhaustive()
    }
}
