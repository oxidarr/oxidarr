//! `dx`'s web entry point: launches [`oxidarr_ui::App`] as the Dioxus web
//! renderer's root component. The app logic itself lives in `src/lib.rs`
//! instead of inline here so it can be unit-tested without the web/wasm
//! target (see that file's own docs).
//!
//! `rootname("oxidarr-app")` tells `dioxus_web` to mount into the
//! `<div id="oxidarr-app">` this crate's own `index.html` shell provides,
//! instead of the `dioxus_web::Config` default of `id="main"` — that shell
//! div (not anything [`oxidarr_ui::App`] renders) is what makes the real,
//! built bundle's `index.html` carry the `id="oxidarr-app"` marker
//! `crates/oxidarr-prowl/src/ui.rs`'s tests and CI's `ui` job check for, so
//! it must match between the two files.

fn main() {
    dioxus::LaunchBuilder::new()
        .with_cfg(dioxus::web::Config::new().rootname("oxidarr-app"))
        .launch(oxidarr_ui::App);
}
