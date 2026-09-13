//! Fails fast, with an actionable message, when the `ui` feature is enabled
//! but `crates/oxidarr-prowl/dist` hasn't been built yet — instead of
//! leaving that to `src/ui.rs`'s `include_dir!` macro, whose own panic
//! (`"... is not a directory"`) gives no hint what to do about it.
//!
//! Only acts when Cargo sets `CARGO_FEATURE_UI` (i.e. the `ui` feature is
//! actually enabled for this build) — a no-op on every other build,
//! including the default, API-only one this crate normally ships as.
//!
//! Deliberately never creates or writes anything: there is no stub bundle
//! anywhere in this repository, on purpose — a build script fabricating
//! one here would defeat CI's own "is this the real, `dx`-built bundle"
//! check (`.github/workflows/ci.yml`'s `ui` job greps the served
//! `index.html` for a marker a stub would have to omit; see `src/ui.rs`'s
//! own module docs). Building the real bundle is `./scripts/build-ui.sh`'s
//! job, not this one's.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_UI");

    if std::env::var_os("CARGO_FEATURE_UI").is_none() {
        return;
    }

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let dist = Path::new(manifest_dir).join("dist");

    assert!(
        dist.is_dir(),
        "the `ui` feature embeds crates/oxidarr-prowl/dist at compile time, but that \
         directory does not exist yet. Run `./scripts/build-ui.sh` from the repo root \
         first, then retry this build — see README.md's \"Web UI\" section."
    );
}
