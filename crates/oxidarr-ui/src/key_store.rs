//! The configured API key's storage seam: `get`/`set`/`clear` over the
//! browser's `localStorage` on `wasm32` (via `gloo-storage`), and a
//! thread-local in-memory cell on every other target — the same `cfg`
//! shape `crate::fetch`'s own doc comment describes for the real `Http`
//! transport, so this module's own tests run natively, no wasm toolchain
//! needed.
//!
//! Nothing here namespaces per-instance (this UI only ever talks to the one
//! Oxidarr server it was served from) — the `wasm32` backend uses one fixed
//! `localStorage` key for that reason.

#[cfg(target_arch = "wasm32")]
mod imp {
    use gloo_storage::{LocalStorage, Storage};

    /// The `localStorage` key this module's `get`/`set`/`clear` all share.
    const STORAGE_KEY: &str = "oxidarr-api-key";

    /// Reads the stored key, or `None` if nothing is stored (including a
    /// stored value gloo/serde can't parse back as a plain string — never
    /// worth distinguishing from "not set" for this UI's one caller, the
    /// key prompt).
    pub(super) fn get() -> Option<String> {
        LocalStorage::get::<String>(STORAGE_KEY).ok()
    }

    /// Stores `key`, overwriting whatever was stored before.
    pub(super) fn set(key: &str) {
        // A write failure here (e.g. `localStorage` full/disabled) leaves
        // the previous value in place, which is the same "still asks for a
        // key" behavior this UI would show anyway had nothing been stored
        // at all — nothing to recover into instead.
        let _ = LocalStorage::set(STORAGE_KEY, key);
    }

    /// Removes the stored key, if any.
    pub(super) fn clear() {
        LocalStorage::delete(STORAGE_KEY);
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::cell::RefCell;

    thread_local! {
        static STORE: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    pub(super) fn get() -> Option<String> {
        STORE.with(|cell| cell.borrow().clone())
    }

    pub(super) fn set(key: &str) {
        STORE.with(|cell| *cell.borrow_mut() = Some(key.to_string()));
    }

    pub(super) fn clear() {
        STORE.with(|cell| *cell.borrow_mut() = None);
    }
}

/// Reads the currently configured API key, if one is stored.
#[must_use]
pub fn get() -> Option<String> {
    imp::get()
}

/// Stores `key` as the configured API key, overwriting any previous value.
pub fn set(key: &str) {
    imp::set(key);
}

/// Clears the configured API key — called on a `401` from any screen, per
/// `crate::app::handle_error`.
pub fn clear() {
    imp::clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test starts from a known "nothing stored" state: on native
    /// targets `STORE` is a `thread_local`, and `cargo test`'s default
    /// harness reuses OS threads across tests in its pool, so a value left
    /// behind by one test could otherwise leak into the next test that
    /// happens to land on the same thread.
    fn reset() {
        clear();
    }

    #[test]
    fn nothing_stored_reads_back_as_none() {
        reset();
        assert_eq!(get(), None);
    }

    #[test]
    fn a_stored_key_reads_back_the_same_value() {
        reset();
        set("s3cret");
        assert_eq!(get(), Some("s3cret".to_string()));
        reset();
    }

    #[test]
    fn setting_again_overwrites_the_previous_value() {
        reset();
        set("first");
        set("second");
        assert_eq!(get(), Some("second".to_string()));
        reset();
    }

    #[test]
    fn clear_removes_a_stored_key() {
        reset();
        set("s3cret");
        clear();
        assert_eq!(get(), None);
    }

    #[test]
    fn clearing_an_already_empty_store_is_a_harmless_no_op() {
        reset();
        clear();
        assert_eq!(get(), None);
    }
}
