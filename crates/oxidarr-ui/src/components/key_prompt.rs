//! The key prompt: shown whenever `crate::app::Session::needs_key` is
//! `true` — on first load with nothing in `crate::key_store`, or after any
//! screen's [`crate::api::UiError::Unauthorized`] clears a stale one (see
//! `crate::app::handle_error`).

use dioxus::prelude::*;

/// Collects one API key and reports it via `onsubmit` — storing it (in
/// `crate::key_store`) and wiring it into the shared `ApiClient` are the
/// caller's job (`crate::app::App`), not this component's: this is a pure
/// input, so it stays as easy to render in isolation (see
/// `tests/snapshots.rs`) as `crate::components::banner::Banner`.
#[component]
pub fn KeyPrompt(onsubmit: EventHandler<String>) -> Element {
    let mut value = use_signal(String::new);

    rsx! {
        div { id: "key-prompt",
            h1 { "Enter your API key" }
            p { "Oxidarr printed it at server startup." }
            form {
                onsubmit: move |event| {
                    event.prevent_default();
                    onsubmit.call(value.read().clone());
                },
                input {
                    r#type: "password",
                    placeholder: "API key",
                    value: "{value}",
                    oninput: move |event| value.set(event.value()),
                }
                button { r#type: "submit", class: "primary", "Continue" }
            }
        }
    }
}
