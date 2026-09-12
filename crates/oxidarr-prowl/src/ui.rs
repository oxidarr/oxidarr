//! Feature-gated web UI serving.
//!
//! Behind the `ui` cargo feature (off by default — see this crate's
//! `Cargo.toml`), this module embeds the `oxidarr-ui` web bundle at compile
//! time via [`include_dir`] and serves it: any `GET` request is answered
//! with the exact embedded file at that path if one exists (an asset —
//! `app.css`, the wasm binary, its JS glue), or the bundle's `index.html`
//! otherwise (a client-side SPA route, including `GET /` itself). Every
//! other HTTP method on a path this router doesn't otherwise recognize
//! still 404s — the same as before this feature existed — rather than
//! serving `index.html` for e.g. a stray `POST`.
//!
//! [`router`] returns a [`Router`] carrying only a fallback handler, no
//! named routes, so [`crate::server::app`] merges it in *after* the
//! Torznab and `/api/v1` routers: axum's `Router::merge` takes whichever
//! side defines a fallback when only one side does (merge semantics
//! verified against `axum` `v0.8.9`, `axum/src/routing/mod.rs`'s
//! `Router::merge`).
//!
//! That merge behavior, though, is exactly why this fallback is *not*
//! naturally scoped to "paths neither of those routers claimed": a merged
//! fallback is promoted app-wide (`RouterInner::call_with_state` tries the
//! concrete route table first, then the merged fallback, for every request
//! that doesn't match a concrete route — same file, same tag), and
//! `Router::nest`, which [`crate::api::api_router`] uses for `/api/v1`,
//! only flattens *concrete* routes with the prefix added; it does not
//! install a wildcard/passthrough route that would otherwise claim every
//! `/api/v1/*` sub-path ahead of this fallback. So a typo'd or
//! not-yet-implemented `/api/v1/*` endpoint, or a path merely *shaped*
//! like the Torznab route (`/{indexer_id}/api[...]`) that the Torznab
//! router's own exact two-segment route doesn't match, would otherwise
//! reach this fallback and get served the SPA shell as a `200` — [`serve`]
//! guards against exactly that with [`is_reserved_path`] before ever
//! consulting `dir`, pinned by `tests/ui_router.rs`'s
//! `an_unimplemented_api_v1_path_is_not_found_not_the_spa_shell` and this
//! module's own `is_reserved_path`-shaped tests below.
//!
//! # The embedded directory
//!
//! [`DIST`] embeds `$CARGO_MANIFEST_DIR/../oxidarr-ui/dist` — the output
//! directory `crates/oxidarr-ui/Dioxus.toml` configures for `dx build`.
//! That directory does not exist until `crates/oxidarr-ui` has actually
//! been built (`./scripts/build-ui.sh`, or `dx build` plus that script's
//! own copy step — see its comment for why the copy is needed at all); CI's
//! `ui` job runs it before building/testing this crate with `--features
//! ui`. There is no placeholder or stub shipped anywhere in this
//! repository for that directory (a build script fabricating one would
//! defeat the "is this the real bundle" check below and in CI) — enabling
//! `ui` without having built it first is a hard build failure: `build.rs`
//! catches it early with a one-line panic pointing at
//! `./scripts/build-ui.sh`; absent that, `include_dir!` itself fails the
//! same way but with a far less clear message (`"... is not a
//! directory"`). See this repository's own README.md ("Web UI" section)
//! for the full story.
//!
//! `include_dir!` gives cargo/rustc no signal that this directory's
//! *contents* ever changed — it only calls `tracked_path::path` (the API
//! that would trigger a recompile) behind its own `nightly` feature, which
//! this workspace doesn't enable (verified by reading
//! `Michael-F-Bryan/include_dir`'s `macros/src/lib.rs`, tag
//! `include_dir_macros-v0.7.4`). A stale compiled `oxidarr-prowl` (e.g.
//! restored from a CI cache) will keep serving whatever was embedded the
//! last time this file itself was compiled, even after a fresh `dx build`
//! writes new files here — CI's `ui` job works around this by touching
//! this file right after `dx build`, forcing rustc to reprocess the macro
//! (see `.github/workflows/ci.yml`).
//!
//! # Testing without a dist
//!
//! [`router_from_dir`] takes a `&'static Dir<'static>` rather than reading
//! [`DIST`] directly, so this module's own tests exercise the exact same
//! serving logic against `tests/fixtures/ui-bundle` — a small hand-written
//! bundle committed to this repo — without ever touching
//! `crates/oxidarr-ui/dist`. `tests/ui_router.rs` (an external integration
//! test, since it needs this crate's full [`crate::server::app`]
//! composition) instead proves route *precedence* against the real,
//! feature-gated [`router`] — see that file's own docs.

use std::path::Path;

use axum::Router;
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use include_dir::{Dir, File, include_dir};

/// The production web UI bundle, embedded at compile time. See the module
/// docs' "The embedded directory" section.
static DIST: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../oxidarr-ui/dist");

/// Builds the production web UI router: [`DIST`] served through
/// [`router_from_dir`]. Merge this into an existing [`Router`] — see the
/// module docs for why merge order matters and why this one never
/// conflicts with routes already present on the router it's merged into.
pub fn router() -> Router {
    router_from_dir(&DIST)
}

/// Builds a web UI router serving `dir`: see the module docs for the exact
/// serving rules. `pub(crate)` so this module's own tests can point it at
/// `tests/fixtures/ui-bundle` instead of [`DIST`], without exposing a
/// second, dist-shaped public entry point this crate has no other use for.
pub(crate) fn router_from_dir(dir: &'static Dir<'static>) -> Router {
    Router::new().fallback(move |method: Method, uri: Uri| async move { serve(dir, &method, &uri) })
}

/// The one handler behind [`router_from_dir`]'s fallback. `dir` is
/// `'static` (either [`DIST`] or a test's own fixture `Dir`), so this holds
/// no state of its own beyond the two request parts it's asked to decide
/// on. Plain (non-`async`) since every embedded byte is already resident in
/// the binary — nothing here ever awaits; [`router_from_dir`] wraps the
/// call in an async block only because axum's `Handler` trait requires a
/// `Future`-returning closure for `Router::fallback`.
fn serve(dir: &'static Dir<'static>, method: &Method, uri: &Uri) -> Response {
    if *method != Method::GET {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = uri.path();
    if is_reserved_path(path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = path.trim_start_matches('/');
    match dir.get_file(path).or_else(|| dir.get_file("index.html")) {
        Some(file) => file_response(file),
        // Only reachable if `dir` has no `index.html` at all — a
        // misbuilt/misplaced bundle, not a state any real `dx build`
        // output or `tests/fixtures/ui-bundle` leaves this in.
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// True for any path this crate's own API surface owns — see the module
/// docs for why this guard exists at all. Two families:
///
/// - `/api` or anything under `/api/` — the `/api/v1` control plane (and
///   any future sibling mounted alongside it under `/api`), whether or not
///   a concrete route under it actually matches.
/// - Anything *shaped* like the Torznab route, i.e. whose first two
///   non-empty path segments are `<anything>` then literally `api` —
///   `/{indexer_id}/api` itself never reaches this fallback (it's a
///   concrete two-segment route axum's `path_router` matches directly,
///   even when `{indexer_id}` fails to parse as an id — that's a `400`
///   from the `Path` extractor, not a fallback hit), but a sibling shape
///   with extra segments (`/123/api/extra`) or a non-numeric first segment
///   (`/notanumber/api`) does *not* match that concrete route and would
///   otherwise fall through here.
fn is_reserved_path(path: &str) -> bool {
    if path == "/api" || path.starts_with("/api/") {
        return true;
    }
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    matches!((segments.next(), segments.next()), (Some(_), Some("api")))
}

/// Renders `file`'s bytes verbatim with a best-effort content type (see
/// [`content_type_for`]).
fn file_response(file: &File<'static>) -> Response {
    (
        [(header::CONTENT_TYPE, content_type_for(file.path()))],
        file.contents().to_vec(),
    )
        .into_response()
}

/// A small extension→MIME match covering everything a Dioxus web bundle
/// ships — no new dependency pulled in just for this. Anything else falls
/// back to `application/octet-stream` rather than failing the request.
fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html",
        Some("css") => "text/css",
        Some("js") => "text/javascript",
        Some("wasm") => "application/wasm",
        Some("woff2") => "font/woff2",
        Some("map") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

#[cfg(all(test, feature = "ui"))]
mod tests {
    #![allow(clippy::unwrap_used)]

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use include_dir::include_dir;
    use tower::ServiceExt;

    use super::*;

    /// A hand-written fixture bundle, unrelated to and never depending on
    /// [`DIST`] — see the module docs' "Testing without a dist" section.
    static FIXTURE: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures/ui-bundle");

    fn get(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn root_serves_index_html() {
        let router = router_from_dir(&FIXTURE);

        let response = router.oneshot(get("/")).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert_eq!(content_type, "text/html");
        let body = body_text(response).await;
        assert!(body.contains(r#"id="oxidarr-app""#), "body was: {body}");
    }

    #[tokio::test]
    async fn a_known_asset_is_served_with_its_own_content_type() {
        let router = router_from_dir(&FIXTURE);

        let response = router.oneshot(get("/app.css")).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert_eq!(content_type, "text/css");
    }

    #[tokio::test]
    async fn an_unknown_path_falls_back_to_index_html() {
        let router = router_from_dir(&FIXTURE);

        let response = router.oneshot(get("/some/spa/route")).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(body.contains(r#"id="oxidarr-app""#), "body was: {body}");
    }

    #[tokio::test]
    async fn a_non_get_method_on_an_unmatched_path_still_404s() {
        let router = router_from_dir(&FIXTURE);
        let request = Request::builder()
            .method("POST")
            .uri("/some/spa/route")
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// `Router::merge` promotes this router's fallback app-wide once it's
    /// merged into `server::app()` (see the module docs' axum-source
    /// citation) — so it must never itself answer for a path the `/api/v1`
    /// control plane owns, typo'd or not, or an unmatched-shape sibling of
    /// the Torznab route would silently 200 as the SPA shell. These pin
    /// [`is_reserved_path`] directly against the isolated ui router (no
    /// other router mounted at all), proving the guard is unconditional —
    /// not merely "works because something else caught it first".
    #[tokio::test]
    async fn an_api_v1_path_is_not_found_not_served_as_the_spa() {
        let router = router_from_dir(&FIXTURE);

        let response = router.oneshot(get("/api/v1/does-not-exist")).await.unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_bare_api_path_is_not_found_not_served_as_the_spa() {
        let router = router_from_dir(&FIXTURE);

        let response = router.oneshot(get("/api")).await.unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_torznab_shaped_path_is_not_found_not_served_as_the_spa() {
        let router = router_from_dir(&FIXTURE);

        let response = router.oneshot(get("/123/api")).await.unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_torznab_shaped_path_with_extra_segments_is_not_found_not_served_as_the_spa() {
        let router = router_from_dir(&FIXTURE);

        let response = router.oneshot(get("/123/api/extra")).await.unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_non_numeric_segment_before_api_is_also_reserved() {
        let router = router_from_dir(&FIXTURE);

        let response = router.oneshot(get("/notanumber/api/extra")).await.unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
