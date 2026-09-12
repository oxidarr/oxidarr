//! The status screen: [`StatusView`] is the presentational half (plain
//! props, no context, no fetching — trivially SSR-snapshotted against a
//! fixture, per `tests/snapshots.rs`); [`Status`] is the routed container
//! that fetches from the shared `ApiClient` and feeds [`StatusView`] once
//! the data lands.
//!
//! Per this task's own brief: the server has no "definitions count"
//! endpoint, so "indexers configured" is derived from `indexers().len()`
//! rather than a dedicated count; and the per-indexer Torznab URL
//! (`{origin}/{id}/api`, matching `oxidarr_prowl`'s own route shape — see
//! that crate's torznab router) is built client-side from `origin` (the
//! browser's own `location().origin()` on `wasm32`; a fixed value from
//! [`Status`]'s own caller everywhere else) plus each indexer's `id`, with
//! a copy-to-clipboard button per row.

use dioxus::prelude::*;

use crate::app::{Session, SharedApi, handle_error};
use crate::dto::{IndexerResource, SystemStatus};

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

    let data = use_resource(move || async move {
        let status = api.read().system_status().await;
        let indexers = api.read().indexers().await;
        (status, indexers)
    });

    match &*data.read() {
        None => rsx! {
            p { "Loading…" }
        },
        Some((Ok(status), Ok(indexers))) => rsx! {
            StatusView {
                status: status.clone(),
                indexers: indexers.clone(),
                origin: origin.clone(),
            }
        },
        Some((Err(err), _) | (_, Err(err))) => {
            handle_error(err.clone(), &mut session);
            rsx! {}
        }
    }
}

/// The presentational half — see this module's own doc comment.
#[component]
pub fn StatusView(status: SystemStatus, indexers: Vec<IndexerResource>, origin: String) -> Element {
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
/// everywhere else, per this task's own brief ("clipboard via web-sys,
/// no-op native") — there is no clipboard to write to outside a browser.
#[cfg(target_arch = "wasm32")]
fn copy_to_clipboard(text: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.navigator().clipboard().write_text(text);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn copy_to_clipboard(_text: &str) {}
