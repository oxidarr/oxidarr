//! One module per screen `crate::app::Route` routes to. [`applications`],
//! [`indexers`], and [`status`] are real screens; the remaining nav entry
//! (Search) is still stubbed directly in `crate::app` until its own task
//! lands.

pub mod applications;
pub mod indexers;
pub mod status;
