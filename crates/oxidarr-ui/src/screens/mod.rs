//! One module per screen `crate::app::Route` routes to. [`indexers`] and
//! [`status`] are real screens; the remaining two nav entries
//! (Applications/Search) are still stubbed directly in `crate::app` until
//! their own tasks land.

pub mod indexers;
pub mod status;
