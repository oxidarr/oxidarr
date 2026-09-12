//! SSR render-to-string pins for this crate's views — the "views get SSR
//! snapshot pins" half of `.local/plan-04-ui/design.md`'s testing design.
//! No wasm toolchain needed: `dioxus::ssr::render` walks a [`VirtualDom`]
//! built and progressed entirely in-process (verified by this file's own
//! `cargo test -p oxidarr-ui` run, no `--target wasm32-unknown-unknown`),
//! and this crate's `Cargo.toml` pulls `dioxus`'s `ssr` feature in only as
//! a `[dev-dependencies]` addition — it adds no runtime dependency
//! (`dioxus-ssr` itself carries no `tokio`, confirmed via `cargo tree -p
//! oxidarr-ui`), so it never reaches the `wasm32` bundle `dx build`
//! produces.
//!
//! # Snapshot format: inline literals vs. committed files
//!
//! Small, single-purpose components ([`KeyPrompt`], [`Banner`]) are pinned
//! as inline `assert_eq!` literals in this file — the whole point of a
//! small fixed string is that a reviewer can read the expected HTML
//! directly in the diff that changes it. [`StatusView`], the one screen
//! this task ships, renders a multi-row table-like list whose HTML is long
//! enough that an inline literal would bury the interesting part (the
//! per-indexer rows) in indentation noise; that one is pinned against a
//! committed file instead, `tests/snapshots/status.html` — read side by
//! side with a diff tool, a changed row is easy to see, unlike a changed
//! substring inside one giant `assert_eq!` argument. Later screen tasks
//! (indexers/applications/search) follow the same rule: one screen, one
//! `tests/snapshots/<screen>.html`.
//!
//! # Constructing a [`VirtualDom`] for a props-driven component
//!
//! [`VirtualDom::new`] takes a bare `fn() -> Element` — not a capturing
//! closure — so a component whose props include an `EventHandler` (which
//! itself needs an active Dioxus runtime to construct, since
//! `Callback::new` calls `Runtime::current()`) can't have its props built
//! *before* the `VirtualDom` exists. [`key_prompt_html`] and
//! [`banner_html`] work around that the same way this crate's own
//! `dioxus-router` dependency's `tests/via_ssr/*.rs` do: wrap the
//! component call in a plain (non-capturing) `fn` item passed to
//! `VirtualDom::new`, so the `EventHandler`-producing closures inside the
//! `rsx!` call are only evaluated once `rebuild_in_place` is already
//! running the tree inside its own runtime. [`StatusView`]'s own props
//! (`SystemStatus`/`Vec<IndexerResource>`/`String` — no `EventHandler`
//! anywhere) carry no such restriction, so `status_view_html` builds
//! `StatusViewProps` directly and passes it to `VirtualDom::new_with_props`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

use dioxus::prelude::*;
use oxidarr_ui::components::banner::Banner;
use oxidarr_ui::components::key_prompt::KeyPrompt;
use oxidarr_ui::dto::{IndexerCapabilities, IndexerResource, SystemStatus};
use oxidarr_ui::screens::status::{StatusView, StatusViewProps};

fn key_prompt_html() -> String {
    fn root() -> Element {
        rsx! {
            KeyPrompt { onsubmit: move |_: String| {} }
        }
    }
    let mut vdom = VirtualDom::new(root);
    vdom.rebuild_in_place();
    dioxus::ssr::render(&vdom)
}

fn banner_html(message: &str) -> String {
    // `root` can't capture `message` (see this file's own module docs), so
    // it's threaded through as a `static` the wrapper `fn` reads instead —
    // fine for a single-threaded test, and it keeps `root` a genuine
    // capture-free `fn` item.
    thread_local! {
        static MESSAGE: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    }

    fn root() -> Element {
        let message = MESSAGE.with(|cell| cell.borrow().clone());
        rsx! {
            Banner { message, ondismiss: move |()| {} }
        }
    }

    MESSAGE.with(|cell| *cell.borrow_mut() = message.to_string());
    let mut vdom = VirtualDom::new(root);
    vdom.rebuild_in_place();
    dioxus::ssr::render(&vdom)
}

fn fixture_status() -> SystemStatus {
    SystemStatus {
        app_name: "Oxidarr".to_string(),
        instance_name: "Oxidarr".to_string(),
        version: "0.0.0".to_string(),
        is_production: true,
        is_docker: false,
        start_time: "2024-01-01T00:00:00+00:00".to_string(),
        app_data: "/config".to_string(),
        authentication: "apikey".to_string(),
        url_base: String::new(),
    }
}

fn fixture_indexer(id: i32, name: &str) -> IndexerResource {
    IndexerResource {
        id,
        name: name.to_string(),
        implementation: "Cardigann".to_string(),
        definition_name: "example".to_string(),
        description: String::new(),
        language: String::new(),
        enable: true,
        priority: 25,
        capabilities: IndexerCapabilities::default(),
        fields: Vec::new(),
        sync_error: None,
    }
}

fn status_view_html() -> String {
    let props = StatusViewProps {
        status: fixture_status(),
        indexers: vec![
            fixture_indexer(1, "Example One"),
            fixture_indexer(2, "Example Two"),
        ],
        origin: "http://oxidarr.local:9696".to_string(),
    };
    let mut vdom = VirtualDom::new_with_props(StatusView, props);
    vdom.rebuild_in_place();
    dioxus::ssr::render(&vdom)
}

fn snapshot_path(name: &str) -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/snapshots")).join(name)
}

#[test]
fn key_prompt_renders_with_no_key() {
    assert_eq!(
        key_prompt_html(),
        concat!(
            r#"<div id="key-prompt">"#,
            "<h1>Enter your API key</h1>",
            "<p>Oxidarr printed it at server startup.</p>",
            "<form>",
            r#"<input type="password" placeholder="API key" value=""/>"#,
            r#"<button type="submit">Continue</button>"#,
            "</form>",
            "</div>",
        )
    );
}

#[test]
fn banner_renders_a_problem_errors_message() {
    assert_eq!(
        banner_html("database is corrupt"),
        concat!(
            r#"<div class="banner" role="alert">"#,
            "<span>database is corrupt</span>",
            r#"<button type="button">Dismiss</button>"#,
            "</div>",
        )
    );
}

#[test]
fn status_screen_with_a_fixture_status_and_two_indexers_matches_its_snapshot() {
    let html = status_view_html();
    let expected = fs::read_to_string(snapshot_path("status.html"))
        .expect("reading tests/snapshots/status.html");
    assert_eq!(html, expected.trim_end(), "rendered HTML was:\n{html}");
}
