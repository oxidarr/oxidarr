//! Shared domain types and traits for the Oxidarr stack.
//!
//! Pure domain layer: no I/O, no SQL, no HTTP. Depends on no other
//! oxidarr crate.

pub mod ids;
pub mod release;

pub use ids::{AppId, IndexerId, RemoteIndexerId};
pub use release::Release;
