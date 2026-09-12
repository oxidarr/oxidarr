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

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use dioxus::prelude::*;
use oxidarr_ui::components::banner::Banner;
use oxidarr_ui::components::key_prompt::KeyPrompt;
use oxidarr_ui::dto::{Field, IndexerCapabilities, IndexerResource, SelectOption, SystemStatus};
use oxidarr_ui::screens::indexers::{
    IndexerFormView, IndexerListView, TestOutcome, TestOutcomeView,
};
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

/// [`IndexerListView`] carries `EventHandler` props, same restriction as
/// [`key_prompt_html`]/[`banner_html`] above — see this file's own module
/// docs for why that means a non-capturing `root` `fn` fed to
/// `VirtualDom::new`, with the actual fixture data threaded through via a
/// `thread_local`, rather than building `IndexerListViewProps` directly the
/// way `status_view_html` builds `StatusViewProps`.
fn indexer_list_view_html(
    indexers: Vec<IndexerResource>,
    confirm_delete_id: Option<i32>,
    test_results: HashMap<i32, TestOutcome>,
) -> String {
    thread_local! {
        static INDEXERS: RefCell<Vec<IndexerResource>> = const { RefCell::new(Vec::new()) };
        static CONFIRM_DELETE_ID: RefCell<Option<i32>> = const { RefCell::new(None) };
        static TEST_RESULTS: RefCell<HashMap<i32, TestOutcome>> = RefCell::new(HashMap::new());
    }

    fn root() -> Element {
        let indexers = INDEXERS.with(|cell| cell.borrow().clone());
        let confirm_delete_id = CONFIRM_DELETE_ID.with(|cell| *cell.borrow());
        let test_results = TEST_RESULTS.with(|cell| cell.borrow().clone());
        rsx! {
            IndexerListView {
                indexers,
                confirm_delete_id,
                test_results,
                on_add: move |()| {},
                on_edit: move |_id: i32| {},
                on_toggle_enable: move |_id: i32| {},
                on_delete_request: move |_id: i32| {},
                on_delete_confirm: move |_id: i32| {},
                on_delete_cancel: move |()| {},
                on_test: move |_id: i32| {},
            }
        }
    }

    INDEXERS.with(|cell| *cell.borrow_mut() = indexers);
    CONFIRM_DELETE_ID.with(|cell| *cell.borrow_mut() = confirm_delete_id);
    TEST_RESULTS.with(|cell| *cell.borrow_mut() = test_results);
    let mut vdom = VirtualDom::new(root);
    vdom.rebuild_in_place();
    dioxus::ssr::render(&vdom)
}

/// See [`indexer_list_view_html`]'s own doc comment — same restriction,
/// same `thread_local` workaround, for [`IndexerFormView`].
fn indexer_form_view_html(fields: Vec<Field>, values: HashMap<String, String>) -> String {
    thread_local! {
        static FIELDS: RefCell<Vec<Field>> = const { RefCell::new(Vec::new()) };
        static VALUES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    }

    fn root() -> Element {
        let fields = FIELDS.with(|cell| cell.borrow().clone());
        let values = VALUES.with(|cell| cell.borrow().clone());
        rsx! {
            IndexerFormView {
                mode_label: "Add".to_string(),
                definition_name: "example".to_string(),
                name: "My Indexer".to_string(),
                priority: 25,
                enable: true,
                fields,
                values,
                test_result: None,
                on_name_change: move |_value: String| {},
                on_priority_change: move |_value: String| {},
                on_enable_change: move |_value: bool| {},
                on_field_change: move |_change: (String, String)| {},
                on_test: move |()| {},
                on_save: move |()| {},
                on_cancel: move |()| {},
            }
        }
    }

    FIELDS.with(|cell| *cell.borrow_mut() = fields);
    VALUES.with(|cell| *cell.borrow_mut() = values);
    let mut vdom = VirtualDom::new(root);
    vdom.rebuild_in_place();
    dioxus::ssr::render(&vdom)
}

/// See [`indexer_list_view_html`]'s own doc comment — same restriction, for
/// [`TestOutcomeView`] (`Option<TestOutcome>` alone needs no `thread_local`
/// juggling beyond what `banner_html` already does for a `String`).
fn test_outcome_view_html(outcome: Option<TestOutcome>) -> String {
    thread_local! {
        static OUTCOME: RefCell<Option<TestOutcome>> = const { RefCell::new(None) };
    }

    fn root() -> Element {
        let outcome = OUTCOME.with(|cell| cell.borrow().clone());
        rsx! {
            TestOutcomeView { outcome }
        }
    }

    OUTCOME.with(|cell| *cell.borrow_mut() = outcome);
    let mut vdom = VirtualDom::new(root);
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

#[test]
fn indexers_list_with_a_disabled_indexer_and_a_sync_error_matches_its_snapshot() {
    let indexers = vec![
        fixture_indexer(1, "Example One"),
        IndexerResource {
            enable: false,
            sync_error: Some("sync failed: connection refused".to_string()),
            ..fixture_indexer(2, "Example Two")
        },
    ];
    let html = indexer_list_view_html(indexers, None, HashMap::new());
    let expected = fs::read_to_string(snapshot_path("indexers_list.html"))
        .expect("reading tests/snapshots/indexers_list.html");
    assert_eq!(html, expected.trim_end(), "rendered HTML was:\n{html}");
}

#[test]
fn indexers_list_with_a_pending_delete_confirmation_matches_its_snapshot() {
    let indexers = vec![fixture_indexer(1, "Example One")];
    let html = indexer_list_view_html(indexers, Some(1), HashMap::new());
    let expected = fs::read_to_string(snapshot_path("indexers_list_confirm_delete.html"))
        .expect("reading tests/snapshots/indexers_list_confirm_delete.html");
    assert_eq!(html, expected.trim_end(), "rendered HTML was:\n{html}");
}

#[test]
fn add_form_for_a_fixture_schema_entry_renders_its_select_options() {
    let fields = vec![
        Field {
            name: "cookie".to_string(),
            label: "Cookie".to_string(),
            kind: "textbox".to_string(),
            value: Some(serde_json::Value::String(String::new())),
            select_options: None,
        },
        Field {
            name: "freeleech".to_string(),
            label: "Freeleech only".to_string(),
            kind: "checkbox".to_string(),
            value: Some(serde_json::Value::Bool(false)),
            select_options: None,
        },
        Field {
            name: "sort".to_string(),
            label: "Sort by".to_string(),
            kind: "select".to_string(),
            value: Some(serde_json::Value::String("seeders".to_string())),
            select_options: Some(vec![
                SelectOption {
                    value: 0,
                    name: "seeders".to_string(),
                },
                SelectOption {
                    value: 1,
                    name: "created".to_string(),
                },
            ]),
        },
    ];
    let values: HashMap<String, String> = [
        ("cookie".to_string(), String::new()),
        ("freeleech".to_string(), "false".to_string()),
        ("sort".to_string(), "seeders".to_string()),
    ]
    .into_iter()
    .collect();

    let html = indexer_form_view_html(fields, values);
    let expected = fs::read_to_string(snapshot_path("indexers_add_form.html"))
        .expect("reading tests/snapshots/indexers_add_form.html");
    assert_eq!(html, expected.trim_end(), "rendered HTML was:\n{html}");
    assert!(
        html.contains("<option"),
        "expected select options to render, html was:\n{html}"
    );
}

#[test]
fn test_outcome_view_renders_nothing_when_no_test_has_run() {
    assert_eq!(test_outcome_view_html(None), "");
}

#[test]
fn test_outcome_view_renders_a_success_note() {
    assert_eq!(
        test_outcome_view_html(Some(TestOutcome::Ok)),
        r#"<span class="test-result test-result-ok">Test succeeded</span>"#
    );
}

#[test]
fn test_outcome_view_renders_a_problem_message_on_failure() {
    assert_eq!(
        test_outcome_view_html(Some(TestOutcome::Failed("bad request".to_string()))),
        r#"<span class="test-result test-result-failed">bad request</span>"#
    );
}
