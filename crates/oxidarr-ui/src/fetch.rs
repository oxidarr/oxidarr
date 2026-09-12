//! The real [`crate::api::Http`] implementation for the browser.
//!
//! Deliberately empty for now. This task (Plan 4's task 2) only needs
//! `crate::api`/`crate::dto` to be wasm-independent and natively testable —
//! see those modules' own doc comments and `.local/plan-04-ui/design.md`'s
//! "Testing" decision. The actual `fetch`-backed [`crate::api::Http`]
//! implementation (via `gloo-net` or raw `web-sys`) belongs to the later
//! shell task that wires a live [`crate::api::ApiClient`] into the running
//! Dioxus app: that task has a concrete caller to drive the design (which
//! crate, how errors surface, whether requests need aborting on unmount,
//! ...), which this one does not. Gated to `wasm32` purely so the module
//! path exists now and this crate keeps compiling for both native
//! (`cargo test`, no wasm target needed) and the `wasm32-unknown-unknown`
//! target `dx build` uses.
