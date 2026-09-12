//! `dx`'s web entry point: launches [`oxidarr_ui::App`] as the Dioxus web
//! renderer's root component. Dioxus 0.7's own scaffold generates exactly
//! this shape — a bare `fn main` calling `dioxus::launch` — for a new app's
//! binary target (verified against
//! `https://dioxuslabs.com/learn/0.7/tutorial/new_app`, 2026-09-12); the
//! app logic itself lives in `src/lib.rs` instead of inline here so it can
//! be unit-tested without the web/wasm target (see that file's own docs).

fn main() {
    dioxus::launch(oxidarr_ui::App);
}
