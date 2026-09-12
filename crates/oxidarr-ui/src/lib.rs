//! Dioxus frontend for Oxidarr apps
//!
//! Rust-native web UI. Talks to the server exclusively through the public
//! `/api/v1` API, with no privileged access, so it doubles as a
//! compatibility test of that API.
//!
//! A library target (not just the `oxidarr-ui` binary `dx build` produces)
//! so [`App`] and the API client and view logic can be unit-tested
//! natively — `cargo test -p oxidarr-ui` needs no wasm target, per this
//! workspace's design (see `.local/plan-04-ui/design.md`'s "Testing"
//! decision). `src/main.rs` is the thin `dioxus::launch` entry point `dx`
//! builds for the web target.
//!
//! [`api`] and [`dto`] are wasm-independent by design: neither imports
//! `dioxus`, so both compile and their tests run on any native target,
//! proven by this crate's own `cargo test` needing no wasm toolchain at
//! all. [`fetch`] is the one `wasm32`-only module — see its own doc
//! comment for why it stays an empty placeholder for now.

use dioxus::prelude::*;

pub mod api;
pub mod dto;
#[cfg(target_arch = "wasm32")]
mod fetch;

/// The app's root component. Scaffolding only for now: a static shell with
/// a marker `id` later tasks' views replace piece by piece, and later
/// tests (SSR render-to-string snapshots, per the design doc) can select
/// on.
#[component]
pub fn App() -> Element {
    rsx! {
        div { id: "oxidarr-app", "Loading Oxidarr…" }
    }
}
