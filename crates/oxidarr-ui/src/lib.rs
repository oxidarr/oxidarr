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
//! all. [`fetch`] is the one `wasm32`-only module (the real `Http` transport,
//! backed by `gloo-net`); [`key_store`] is `cfg`-based instead of
//! `wasm32`-gated (a thread-local in-memory store natively, so its own
//! tests need no wasm target either). [`app`] wires the router, the shared
//! [`api::ApiClient`]/key-presence state, and the key-prompt/error-banner
//! seam together; [`components`] and [`screens`] hold the individual
//! views — each view's own props-driven rendering is what
//! `tests/snapshots.rs`'s SSR pins exercise, natively, per this workspace's
//! testing design.

pub mod api;
mod app;
pub mod components;
pub mod dto;
#[cfg(target_arch = "wasm32")]
mod fetch;
pub mod key_store;
pub mod screens;

pub use app::{App, Route};
