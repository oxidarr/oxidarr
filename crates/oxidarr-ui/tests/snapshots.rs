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
use oxidarr_ui::dto::{
    ApplicationResource, Field, IndexerCapabilities, IndexerResource, SelectOption, SyncLevel,
    SystemStatus,
};
use oxidarr_ui::screens::applications::{
    ApplicationFormView, ApplicationListView, TestOutcome as ApplicationTestOutcome,
    TestOutcomeView as ApplicationTestOutcomeView,
};
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

fn fixture_application(id: i32, name: &str, implementation: &str) -> ApplicationResource {
    ApplicationResource {
        id,
        name: name.to_string(),
        implementation: implementation.to_string(),
        sync_level: SyncLevel::FullSync,
        fields: vec![
            Field {
                name: "baseUrl".to_string(),
                label: "baseUrl".to_string(),
                kind: "textbox".to_string(),
                value: Some(serde_json::Value::String(format!(
                    "http://{}.example",
                    implementation.to_lowercase()
                ))),
                select_options: None,
            },
            Field {
                name: "apiKey".to_string(),
                label: "apiKey".to_string(),
                kind: "textbox".to_string(),
                value: Some(serde_json::Value::String("secretkey".to_string())),
                select_options: None,
            },
        ],
        sync_error: None,
    }
}

/// [`ApplicationListView`] carries `EventHandler` props, same restriction as
/// [`indexer_list_view_html`] above — see this file's own module docs.
fn application_list_view_html(
    applications: Vec<ApplicationResource>,
    confirm_delete_id: Option<i32>,
    test_results: HashMap<i32, ApplicationTestOutcome>,
) -> String {
    thread_local! {
        static APPLICATIONS: RefCell<Vec<ApplicationResource>> = const { RefCell::new(Vec::new()) };
        static CONFIRM_DELETE_ID: RefCell<Option<i32>> = const { RefCell::new(None) };
        static TEST_RESULTS: RefCell<HashMap<i32, ApplicationTestOutcome>> = RefCell::new(HashMap::new());
    }

    fn root() -> Element {
        let applications = APPLICATIONS.with(|cell| cell.borrow().clone());
        let confirm_delete_id = CONFIRM_DELETE_ID.with(|cell| *cell.borrow());
        let test_results = TEST_RESULTS.with(|cell| cell.borrow().clone());
        rsx! {
            ApplicationListView {
                applications,
                confirm_delete_id,
                test_results,
                on_add: move |()| {},
                on_edit: move |_id: i32| {},
                on_delete_request: move |_id: i32| {},
                on_delete_confirm: move |_id: i32| {},
                on_delete_cancel: move |()| {},
                on_test: move |_id: i32| {},
            }
        }
    }

    APPLICATIONS.with(|cell| *cell.borrow_mut() = applications);
    CONFIRM_DELETE_ID.with(|cell| *cell.borrow_mut() = confirm_delete_id);
    TEST_RESULTS.with(|cell| *cell.borrow_mut() = test_results);
    let mut vdom = VirtualDom::new(root);
    vdom.rebuild_in_place();
    dioxus::ssr::render(&vdom)
}

/// See [`application_list_view_html`]'s own doc comment — same restriction,
/// same `thread_local` workaround, for [`ApplicationFormView`].
#[allow(clippy::too_many_arguments)]
fn application_form_view_html(
    implementation: &str,
    name: &str,
    base_url: &str,
    api_key: &str,
    sync_level: SyncLevel,
    errors: Vec<String>,
) -> String {
    thread_local! {
        static IMPLEMENTATION: RefCell<String> = const { RefCell::new(String::new()) };
        static NAME: RefCell<String> = const { RefCell::new(String::new()) };
        static BASE_URL: RefCell<String> = const { RefCell::new(String::new()) };
        static API_KEY: RefCell<String> = const { RefCell::new(String::new()) };
        static SYNC_LEVEL: RefCell<SyncLevel> = const { RefCell::new(SyncLevel::Disabled) };
        static ERRORS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    fn root() -> Element {
        let implementation = IMPLEMENTATION.with(|cell| cell.borrow().clone());
        let name = NAME.with(|cell| cell.borrow().clone());
        let base_url = BASE_URL.with(|cell| cell.borrow().clone());
        let api_key = API_KEY.with(|cell| cell.borrow().clone());
        let sync_level = SYNC_LEVEL.with(|cell| *cell.borrow());
        let errors = ERRORS.with(|cell| cell.borrow().clone());
        rsx! {
            ApplicationFormView {
                mode_label: "Add",
                name,
                implementation,
                base_url,
                api_key,
                sync_level,
                errors,
                test_result: None,
                on_name_change: move |_value: String| {},
                on_implementation_change: move |_value: String| {},
                on_base_url_change: move |_value: String| {},
                on_api_key_change: move |_value: String| {},
                on_sync_level_change: move |_value: String| {},
                on_test: move |()| {},
                on_save: move |()| {},
                on_cancel: move |()| {},
            }
        }
    }

    IMPLEMENTATION.with(|cell| *cell.borrow_mut() = implementation.to_string());
    NAME.with(|cell| *cell.borrow_mut() = name.to_string());
    BASE_URL.with(|cell| *cell.borrow_mut() = base_url.to_string());
    API_KEY.with(|cell| *cell.borrow_mut() = api_key.to_string());
    SYNC_LEVEL.with(|cell| *cell.borrow_mut() = sync_level);
    ERRORS.with(|cell| *cell.borrow_mut() = errors);
    let mut vdom = VirtualDom::new(root);
    vdom.rebuild_in_place();
    dioxus::ssr::render(&vdom)
}

/// See [`indexer_list_view_html`]'s own doc comment — same restriction, for
/// [`ApplicationTestOutcomeView`] (`Option<ApplicationTestOutcome>` alone
/// needs no `thread_local` juggling beyond what `banner_html` already does
/// for a `String`).
fn application_test_outcome_view_html(outcome: Option<ApplicationTestOutcome>) -> String {
    thread_local! {
        static OUTCOME: RefCell<Option<ApplicationTestOutcome>> = const { RefCell::new(None) };
    }

    fn root() -> Element {
        let outcome = OUTCOME.with(|cell| cell.borrow().clone());
        rsx! {
            ApplicationTestOutcomeView { outcome }
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

#[test]
fn applications_list_with_two_apps_one_carrying_a_sync_error_matches_its_snapshot() {
    let applications = vec![
        fixture_application(1, "My Sonarr", "Sonarr"),
        ApplicationResource {
            sync_error: Some(
                "unexpected status 500 from http://radarr.example/api/v3/system/status".to_string(),
            ),
            ..fixture_application(2, "My Radarr", "Radarr")
        },
    ];
    let html = application_list_view_html(applications, None, HashMap::new());
    let expected = fs::read_to_string(snapshot_path("applications_list.html"))
        .expect("reading tests/snapshots/applications_list.html");
    assert_eq!(html, expected.trim_end(), "rendered HTML was:\n{html}");
}

#[test]
fn applications_list_with_a_pending_delete_confirmation_matches_its_snapshot() {
    let applications = vec![fixture_application(1, "My Sonarr", "Sonarr")];
    let html = application_list_view_html(applications, Some(1), HashMap::new());
    let expected = fs::read_to_string(snapshot_path("applications_list_confirm_delete.html"))
        .expect("reading tests/snapshots/applications_list_confirm_delete.html");
    assert_eq!(html, expected.trim_end(), "rendered HTML was:\n{html}");
}

#[test]
fn applications_add_form_for_sonarr_matches_its_snapshot() {
    let html = application_form_view_html(
        "Sonarr",
        "My Sonarr",
        "http://sonarr.example",
        "secretkey",
        SyncLevel::FullSync,
        Vec::new(),
    );
    let expected = fs::read_to_string(snapshot_path("applications_form_sonarr.html"))
        .expect("reading tests/snapshots/applications_form_sonarr.html");
    assert_eq!(html, expected.trim_end(), "rendered HTML was:\n{html}");
}

#[test]
fn applications_add_form_for_radarr_matches_its_snapshot() {
    let html = application_form_view_html(
        "Radarr",
        "My Radarr",
        "http://radarr.example",
        "secretkey",
        SyncLevel::AddOnly,
        Vec::new(),
    );
    let expected = fs::read_to_string(snapshot_path("applications_form_radarr.html"))
        .expect("reading tests/snapshots/applications_form_radarr.html");
    assert_eq!(html, expected.trim_end(), "rendered HTML was:\n{html}");
}

#[test]
fn applications_form_with_validation_errors_matches_its_snapshot() {
    let html = application_form_view_html(
        "Sonarr",
        "",
        "not a url",
        "",
        SyncLevel::Disabled,
        vec![
            "Name is required".to_string(),
            "Base URL \"not a url\" is not a valid URL".to_string(),
            "API key is required".to_string(),
        ],
    );
    let expected = fs::read_to_string(snapshot_path("applications_form_invalid.html"))
        .expect("reading tests/snapshots/applications_form_invalid.html");
    assert_eq!(html, expected.trim_end(), "rendered HTML was:\n{html}");
}

#[test]
fn application_test_outcome_view_renders_nothing_when_no_test_has_run() {
    assert_eq!(application_test_outcome_view_html(None), "");
}

#[test]
fn application_test_outcome_view_renders_a_success_note() {
    assert_eq!(
        application_test_outcome_view_html(Some(ApplicationTestOutcome::Ok)),
        r#"<span class="test-result test-result-ok">Test succeeded</span>"#
    );
}

#[test]
fn application_test_outcome_view_renders_a_problem_message_on_failure() {
    assert_eq!(
        application_test_outcome_view_html(Some(ApplicationTestOutcome::Failed(
            "unexpected status 500 from http://sonarr.example/api/v3/system/status".to_string()
        ))),
        concat!(
            r#"<span class="test-result test-result-failed">"#,
            "unexpected status 500 from http://sonarr.example/api/v3/system/status</span>",
        )
    );
}
