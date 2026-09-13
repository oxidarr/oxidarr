//! The status screen: [`StatusView`] is the presentational half (plain
//! props, no context, no fetching — trivially SSR-snapshotted against a
//! fixture, per `tests/snapshots.rs`); [`Status`] is the routed container
//! that fetches from the shared `ApiClient` and feeds [`StatusView`] once
//! the data lands.
//!
//! "Indexers configured" is `indexers().len()` — actual, persisted
//! indexer rows. "Available definitions" is a genuinely different count:
//! `GET /api/v1/indexer/schema` (`ApiClient::indexer_schema`) *is* the
//! server's own definitions list — every Cardigann/Newznab/Torznab
//! definition an indexer could be added from — so this screen shows
//! `indexer_schema().len()` alongside it, not a placeholder for a
//! nonexistent endpoint. The per-indexer Torznab URL (`{origin}/{id}/api`,
//! matching `oxidarr_prowl`'s own route shape — see that crate's torznab
//! router) is built client-side from `origin` (the browser's own
//! `location().origin()` on `wasm32`; a fixed value from [`Status`]'s own
//! caller everywhere else) plus each indexer's `id`, with a
//! copy-to-clipboard button per row.

use dioxus::prelude::*;

use crate::api::UiError;
use crate::app::{Session, SharedApi, handle_error};
use crate::dto::{IndexerResource, SystemStatus};

/// True for an `Err(UiError::Unauthorized)` — used to decide whether a
/// later fetch in the same round trip should even be attempted. Pure (no
/// `dioxus` involvement), unlike [`Status`] itself, so it's natively
/// testable on its own.
#[must_use]
fn is_unauthorized<T>(result: &Result<T, UiError>) -> bool {
    matches!(result, Err(UiError::Unauthorized))
}

/// The routed, data-fetching half — see this module's own doc comment.
/// Not exercised by any native test (it needs a live `ApiClient`, which on
/// every non-`wasm32` target is the inert stub `crate::app`'s own doc
/// comment describes); `StatusView` below carries this screen's real test
/// coverage.
#[component]
pub fn Status() -> Element {
    let api = use_context::<SharedApi>();
    let mut session = use_context::<Session>();
    let origin = current_origin();

    // Cloning the client out of the `Signal` in this synchronous closure,
    // before the `async move` block even exists, avoids holding that
    // `Signal`'s read guard across an `.await` point — see
    // `crate::screens::indexers`'s own doc comment ("Avoiding a `Signal`
    // read guard held across `.await`") for exactly why
    // `api.read().system_status().await` directly (the guard spanning the
    // whole `.await`) is unsound. Each later fetch is also skipped once an
    // earlier one has already come back `Unauthorized` — the same `401`
    // would just repeat, and there is nothing a second or third attempt
    // could tell `handle_error` that the first one didn't.
    let data = use_resource(move || {
        let client = api.read().clone();
        async move {
            let status = client.system_status().await;
            let indexers = if is_unauthorized(&status) {
                Err(UiError::Unauthorized)
            } else {
                client.indexers().await
            };
            let schema = if is_unauthorized(&status) || is_unauthorized(&indexers) {
                Err(UiError::Unauthorized)
            } else {
                client.indexer_schema().await
            };
            (status, indexers, schema)
        }
    });

    match &*data.read() {
        None => rsx! {
            p { "Loading…" }
        },
        Some((Ok(status), Ok(indexers), Ok(schema))) => rsx! {
            StatusView {
                status: status.clone(),
                indexers: indexers.clone(),
                definitions_count: schema.len(),
                origin: origin.clone(),
            }
        },
        // `handle_error` writes to `session`'s own `Signal`s (a stored-key
        // clear plus `needs_key`/`error`) from inside this component's own
        // render body, not from a `use_effect` — normally a smell, since a
        // render-triggered side effect belongs in an effect that only
        // re-runs when its own dependencies change. This is safe *only*
        // because `Status` takes no props: with no props, Dioxus memoizes
        // this component and only re-renders it when a `Signal` this body
        // itself reads (here, `data`) changes — so this write fires once
        // per genuine error, not once per unrelated re-render. If `Status`
        // ever gains a prop, that memoization goes away and this must move
        // into a `use_effect` first. The identical invariant holds at
        // every other routed screen's own equivalent site
        // (`crate::screens::indexers::Indexers`, twice;
        // `crate::screens::applications::Applications`;
        // `crate::screens::search::Search`).
        Some((Err(err), _, _) | (_, Err(err), _) | (_, _, Err(err))) => {
            handle_error(err.clone(), &mut session);
            rsx! {}
        }
    }
}

/// The presentational half — see this module's own doc comment.
#[component]
pub fn StatusView(
    status: SystemStatus,
    indexers: Vec<IndexerResource>,
    definitions_count: usize,
    origin: String,
) -> Element {
    rsx! {
        div { id: "status-screen",
            h1 { "Status" }
            dl {
                dt { "App name" }
                dd { "{status.app_name}" }
                dt { "Instance name" }
                dd { "{status.instance_name}" }
                dt { "Version" }
                dd { "{status.version}" }
                dt { "Environment" }
                dd { if status.is_production { "Production" } else { "Development" } }
                dt { "Runtime" }
                dd { if status.is_docker { "Docker" } else { "Native" } }
                dt { "Started" }
                dd { "{status.start_time}" }
                dt { "App data" }
                dd { "{status.app_data}" }
                dt { "Authentication" }
                dd { "{status.authentication}" }
                dt { "URL base" }
                dd { "{status.url_base}" }
                dt { "Indexers configured" }
                dd { "{indexers.len()}" }
                dt { "Available definitions" }
                dd { "{definitions_count}" }
            }
            h2 { "Torznab URLs" }
            ul {
                for indexer in indexers {
                    {
                        let url = format!("{origin}/{}/api", indexer.id);
                        rsx! {
                            li { key: "{indexer.id}",
                                span { "{indexer.name}: " }
                                code { "{url}" }
                                button {
                                    r#type: "button",
                                    onclick: move |_| copy_to_clipboard(&url),
                                    "Copy"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The browser's own current origin (`scheme://host[:port]`, no trailing
/// slash) — used to build each row's full Torznab URL. `wasm32` asks the
/// real `window.location`; every other target (native `cargo test`, and
/// this module's own `StatusView`-only snapshot tests, which pass an
/// `origin` prop directly instead) has no browser to ask, so it returns a
/// fixed placeholder purely so [`Status`] compiles there too.
#[cfg(target_arch = "wasm32")]
fn current_origin() -> String {
    web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
fn current_origin() -> String {
    "http://oxidarr.local:9696".to_string()
}

/// Copies `text` to the system clipboard via `web_sys::Clipboard::write_text`
/// on `wasm32` — fire-and-forget: the returned `Promise` is dropped rather
/// than awaited, since this button has nothing useful to show for a
/// success/failure it can't easily surface anyway (browsers already show
/// their own permission prompt/toast for clipboard access). A no-op
/// everywhere else — there is no clipboard to write to outside a browser.
#[cfg(target_arch = "wasm32")]
fn copy_to_clipboard(text: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.navigator().clipboard().write_text(text);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn copy_to_clipboard(_text: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_unauthorized_is_true_only_for_that_exact_variant() {
        assert!(is_unauthorized::<()>(&Err(UiError::Unauthorized)));
        assert!(!is_unauthorized(&Ok(())));
        assert!(!is_unauthorized::<()>(&Err(UiError::Problem(
            "bad request".to_string()
        ))));
        assert!(!is_unauthorized::<()>(&Err(UiError::Network(
            "refused".to_string()
        ))));
        assert!(!is_unauthorized::<()>(&Err(UiError::Decode(
            "invalid json".to_string()
        ))));
    }
}
