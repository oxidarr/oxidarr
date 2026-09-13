//! Pins `index.html`'s own mount div. `dioxus-web` 0.7.10 mounts by
//! *appending* into its root element (`dom.rs` — there is no
//! `set_inner_html("")` clearing it first), so any text placed inside
//! `#oxidarr-app` in the committed shell stays on screen, above the
//! mounted app, forever: a real browser against the committed bundle
//! measured `document.body.innerText` as `"Loading Oxidarr…\nEnter your
//! API key…"` — the loading text never went away. The mount div itself
//! must stay empty; a pre-wasm loading message, if kept at all, belongs in
//! a sibling element `assets/app.css` hides once `#oxidarr-app` is no
//! longer empty (see that file's own rule).
//!
//! The `id="oxidarr-app"` marker itself must survive untouched — both this
//! crate's own build and `crates/oxidarr-prowl/src/ui.rs`'s tests/CI grep
//! for it as proof a real `dx build` produced the served bundle.

#![allow(clippy::expect_used)]

use std::fs;

fn index_html() -> String {
    fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/index.html"))
        .expect("reading crates/oxidarr-ui/index.html")
}

#[test]
fn the_marker_id_survives() {
    let html = index_html();
    assert!(
        html.contains(r#"id="oxidarr-app""#),
        "expected the #oxidarr-app marker to survive, got:\n{html}"
    );
}

#[test]
fn the_mount_div_is_empty_not_carrying_loading_text() {
    let html = index_html();
    assert!(
        html.contains(r#"<div id="oxidarr-app"></div>"#),
        "expected an empty #oxidarr-app mount div (dioxus-web appends rather \
         than clearing it, so any text here would linger under the mounted app \
         forever), got:\n{html}"
    );
}
