//! The app shell: the router, the shared [`api::ApiClient`]/key-presence
//! state ([`SharedApi`]/[`Session`]), and the key-prompt/error-banner seam
//! every screen routes its [`UiError`]s through ([`handle_error`]).
//!
//! # Wiring
//!
//! [`App`] provides two pieces of context every routed screen reads via
//! `use_context`: [`SharedApi`] (the shared `ApiClient`, wrapped in a
//! `Signal` so it can be re-keyed once the user submits one — see
//! [`crate::components::key_prompt::KeyPrompt`]) and [`Session`] (whether
//! the key prompt should currently be showing, and the current banner
//! message, if any). [`App`] itself decides what to render based on
//! `Session::needs_key`: [`crate::components::key_prompt::KeyPrompt`] when
//! `true`, [`Route`]'s router otherwise — with
//! [`crate::components::banner::Banner`] overlaid whenever `Session::error`
//! is set, independent of which of those two is showing underneath.
//!
//! # The `Http` transport
//!
//! [`ApiClient`] is generic over its [`Http`] transport (`crate::api`'s own
//! doc comment). On `wasm32`, [`SharedApi`] is backed by the real
//! `crate::fetch::WasmHttp`; every other target — `cargo test
//! --workspace`, and a plain `cargo build` without
//! `--target wasm32-unknown-unknown` — needs *some* concrete type to
//! satisfy `ApiClient<H>`'s bound even though nothing there ever calls
//! `dioxus::launch`, so [`NativeHttp`] fills that role: a stub that always
//! fails, never actually reached at runtime on those targets.

use dioxus::prelude::*;

use crate::api::{ApiClient, UiError};
// `Http` is only named by the native `NativeHttp` impl below; importing it
// unconditionally leaves an unused import under the wasm cfg.
#[cfg(not(target_arch = "wasm32"))]
use crate::api::Http;
use crate::components::banner::Banner;
use crate::components::key_prompt::KeyPrompt;
use crate::components::layout::Layout;
use crate::key_store;
use crate::screens::applications::Applications;
use crate::screens::indexers::Indexers;
use crate::screens::search::Search;
use crate::screens::status::Status;

/// This app's four screens, nested under [`Layout`] so every route shares
/// the same nav. The nav's own *display* order —
/// Indexers/Applications/Search/Status, per this task's own brief — is
/// independent of this enum's declaration order: `/` (what a bare origin
/// request resolves to) is [`Status`], the most useful landing page.
#[derive(Routable, Clone, PartialEq, Debug)]
#[rustfmt::skip]
pub enum Route {
    #[layout(Layout)]
        #[route("/")]
        Status {},
        #[route("/indexers")]
        Indexers {},
        #[route("/applications")]
        Applications {},
        #[route("/search")]
        Search {},
}

/// A placeholder [`Http`] transport used only so [`App`] (and everything
/// it wires together) type-checks and runs on non-`wasm32` targets — see
/// this module's own doc comment.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct NativeHttp;

#[cfg(not(target_arch = "wasm32"))]
impl Http for NativeHttp {
    // This stub never actually awaits anything (it always fails
    // immediately), unlike every real `Http` implementation this trait
    // exists for — `async fn` here still matches the trait signature
    // exactly, so an allow is more honest than restructuring a stub around
    // a lint meant for production code (same pattern as `crate::api`'s own
    // `FakeHttp` test double). `unknown_lints` keeps the allow valid on
    // both the pinned 1.95 clippy and any newer driver that knows this
    // lint (see that same module's own comment).
    #[allow(unknown_lints, clippy::unused_async_trait_impl)]
    async fn request(
        &self,
        _method: &str,
        _path: &str,
        _key: Option<&str>,
        _body: Option<String>,
    ) -> Result<(u16, String), UiError> {
        Err(UiError::Network(
            "no HTTP transport is available on this target".to_string(),
        ))
    }
}

/// The [`Http`] transport [`SharedApi`] is generic over on this target —
/// [`crate::fetch::WasmHttp`] on `wasm32`, [`NativeHttp`] everywhere else.
#[cfg(target_arch = "wasm32")]
type ConcreteHttp = crate::fetch::WasmHttp;
#[cfg(not(target_arch = "wasm32"))]
type ConcreteHttp = NativeHttp;

#[cfg(target_arch = "wasm32")]
fn build_http() -> ConcreteHttp {
    crate::fetch::WasmHttp
}
#[cfg(not(target_arch = "wasm32"))]
fn build_http() -> ConcreteHttp {
    NativeHttp
}

/// The shared `ApiClient` every routed screen reads via
/// `use_context::<SharedApi>()`. Wrapped in a `Signal` (rather than provided
/// as a bare `ApiClient`) so [`App`] can re-key it in place once
/// [`crate::components::key_prompt::KeyPrompt`] collects one, without
/// tearing down and re-providing the whole context.
pub type SharedApi = Signal<ApiClient<ConcreteHttp>>;

/// Shared state every routed screen reads (and, via [`handle_error`],
/// writes) through context.
#[derive(Clone, Copy)]
pub struct Session {
    /// `true` shows [`crate::components::key_prompt::KeyPrompt`] instead of
    /// the router — set on startup with no stored key, and by
    /// [`handle_error`] on any [`UiError::Unauthorized`].
    pub needs_key: Signal<bool>,
    /// The current error banner's message, if any — shown by
    /// [`crate::components::banner::Banner`] regardless of whether
    /// `needs_key` is also set.
    pub error: Signal<Option<String>>,
}

/// What a given [`UiError`] means for a [`Session`] — pure, `dioxus`-free
/// mapping (no `Signal` involved), so it is unit-testable without a
/// running Dioxus runtime, unlike [`Session`] itself. [`handle_error`] is
/// the thin, `Session`-aware wrapper that actually applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ErrorEffect {
    /// The key was missing or wrong: clear it and reopen the key prompt.
    ReauthRequired,
    /// Show this message in the dismissible banner.
    ShowBanner(String),
}

/// See [`ErrorEffect`]'s own doc comment.
fn classify_error(err: UiError) -> ErrorEffect {
    match err {
        UiError::Unauthorized => ErrorEffect::ReauthRequired,
        UiError::Problem(message) => ErrorEffect::ShowBanner(message),
        UiError::Network(message) => ErrorEffect::ShowBanner(format!("Network error: {message}")),
        UiError::Decode(message) => {
            ErrorEffect::ShowBanner(format!("Unexpected response: {message}"))
        }
    }
}

/// Routes any screen's [`UiError`] into `session`: a `401` clears the
/// stored key ([`key_store::clear`]) and reopens the key prompt; every
/// other error shows its message in the dismissible banner. Called from
/// `crate::screens::status::Status` today; every later screen task
/// (indexers/applications/search) routes its own `UiError`s through the
/// same helper.
pub fn handle_error(err: UiError, session: &mut Session) {
    match classify_error(err) {
        ErrorEffect::ReauthRequired => {
            key_store::clear();
            session.needs_key.set(true);
        }
        ErrorEffect::ShowBanner(message) => session.error.set(Some(message)),
    }
}

/// The app's root component — see this module's own doc comment for how
/// its pieces fit together. Renders the `#oxidarr-app` marker div `dx
/// build`'s bundle mounts into (kept from this crate's earlier scaffold —
/// `crates/oxidarr-prowl/src/ui.rs`'s own tests select on it).
#[component]
pub fn App() -> Element {
    let mut api = use_context_provider(|| {
        Signal::new(ApiClient::new(
            String::new(),
            key_store::get(),
            build_http(),
        ))
    });
    let mut session = use_context_provider(|| Session {
        needs_key: Signal::new(key_store::get().is_none()),
        error: Signal::new(None),
    });

    rsx! {
        document::Stylesheet { href: asset!("/assets/app.css") }
        div { id: "oxidarr-app",
            if let Some(message) = session.error.read().clone() {
                Banner { message, ondismiss: move |()| session.error.set(None) }
            }
            if *session.needs_key.read() {
                KeyPrompt {
                    onsubmit: move |key: String| {
                        key_store::set(&key);
                        api.write().set_key(Some(key));
                        session.needs_key.set(false);
                    },
                }
            } else {
                Router::<Route> {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthorized_classifies_as_reauth_required() {
        assert_eq!(
            classify_error(UiError::Unauthorized),
            ErrorEffect::ReauthRequired
        );
    }

    #[test]
    fn a_problem_error_classifies_as_its_own_message_verbatim() {
        assert_eq!(
            classify_error(UiError::Problem("bad request".to_string())),
            ErrorEffect::ShowBanner("bad request".to_string())
        );
    }

    #[test]
    fn a_network_error_classifies_as_a_prefixed_banner_message() {
        assert_eq!(
            classify_error(UiError::Network("connection refused".to_string())),
            ErrorEffect::ShowBanner("Network error: connection refused".to_string())
        );
    }

    #[test]
    fn a_decode_error_classifies_as_a_prefixed_banner_message() {
        assert_eq!(
            classify_error(UiError::Decode("invalid json".to_string())),
            ErrorEffect::ShowBanner("Unexpected response: invalid json".to_string())
        );
    }
}
