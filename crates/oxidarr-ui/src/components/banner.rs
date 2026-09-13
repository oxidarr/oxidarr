//! A dismissible error banner. Purely props-driven — `message` is already
//! the text to show (`crate::app::handle_error` is what decides *what*
//! message a given [`crate::api::UiError`] produces; this component has no
//! opinion on that) — so it renders identically whether the message came
//! from a `Problem`, `Network`, or `Decode` error.

use dioxus::prelude::*;

/// Renders `message` in a dismissible banner; `ondismiss` fires once, when
/// the dismiss button is clicked. Mounting/unmounting this component (an
/// `if let Some(message) = ...` in the caller, per `crate::app::App`) is
/// how "dismissible" is implemented — this component itself holds no
/// visibility state of its own.
#[component]
pub fn Banner(message: String, ondismiss: EventHandler<()>) -> Element {
    rsx! {
        div { class: "banner", role: "alert",
            span { "{message}" }
            button {
                r#type: "button",
                onclick: move |_| ondismiss.call(()),
                "Dismiss"
            }
        }
    }
}
