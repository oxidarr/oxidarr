//! The app's persistent shell around every routed screen: the four-entry
//! nav (`crate::app::Route`'s own doc comment explains why the nav order —
//! Indexers/Applications/Search/Status — differs from the route enum's
//! declaration order) and the `Outlet` the router renders the active
//! screen into.

use dioxus::prelude::*;

use crate::app::Route;

/// `#[layout(Layout)]` on [`Route`] wraps every route in this component —
/// see `crate::app`'s own `Route` derive.
#[component]
pub fn Layout() -> Element {
    rsx! {
        nav {
            Link { to: Route::Indexers {}, "Indexers" }
            Link { to: Route::Applications {}, "Applications" }
            Link { to: Route::Search {}, "Search" }
            Link { to: Route::Status {}, "Status" }
        }
        main {
            Outlet::<Route> {}
        }
    }
}
