//! Small, reusable, props-driven views shared across screens — each one
//! plain enough that `tests/snapshots.rs` pins its SSR rendering as an
//! inline literal (see that file's own module docs for why small
//! components get inline literals and screens get committed `.html`
//! files).

pub mod banner;
pub mod key_prompt;
pub mod layout;
