#!/usr/bin/env bash
# Builds the real oxidarr-ui web bundle and copies it into
# crates/oxidarr-prowl/dist — the directory crates/oxidarr-prowl/src/ui.rs's
# `include_dir!` embeds at compile time behind the `ui` cargo feature. It
# lives inside oxidarr-prowl, not oxidarr-ui, so `cargo package` can ship it
# with the crate that actually consumes it — a path outside the package
# root (`../oxidarr-ui/dist`) can never be included in a published `.crate`,
# and even if it could, a `cargo install`-extracted crate has no sibling
# directory for it to live in.
#
# `dx build`'s own Dioxus.toml `out_dir` setting is not consulted for a web
# build (verified against dioxus-cli 0.7.10's own source): it always writes
# to target/dx/oxidarr-ui/<release|debug>/web/public, under the *workspace*
# target dir, regardless of that setting. This script copies from there
# itself rather than relying on dx to place the bundle in dist/ for us.
set -euo pipefail

cd "$(dirname "$0")/.."

# Deno also ships a binary called `dx`, and on a machine where Homebrew's
# bin directory precedes ~/.cargo/bin it wins the PATH lookup — producing a
# baffling "Unable to choose binary for build" from a tool that has nothing
# to do with Dioxus. Resolve an actual dioxus-cli instead of trusting the
# name. Override with DX=/path/to/dx if yours lives somewhere else.
#
# The candidate list also has to work where CARGO_HOME differs from
# $HOME/.cargo — the official rust:*-bookworm image is what proved this:
# there HOME=/root but CARGO_HOME=/usr/local/cargo, so `cargo install
# dioxus-cli` puts dx at /usr/local/cargo/bin/dx, which neither the
# $HOME/.cargo/bin guess nor a broken PATH search would find. Do not
# simplify this list back down to just the $HOME guess.
find_dx() {
  local candidate
  for candidate in \
    "${DX:-}" \
    "${CARGO_HOME:-$HOME/.cargo}/bin/dx" \
    "$HOME/.cargo/bin/dx" \
    $(type -aP dx 2>/dev/null || true); do
    [[ -n "$candidate" && -x "$candidate" ]] || continue
    if "$candidate" --version 2>/dev/null | grep -qi '^dioxus'; then
      printf '%s' "$candidate"
      return 0
    fi
  done
  return 1
}

if ! dx_bin=$(find_dx); then
  echo "error: no dioxus-cli found. Install it with 'cargo install dioxus-cli'," >&2
  echo "       or point DX at an existing one. Note that Deno ships its own" >&2
  echo "       unrelated 'dx' binary, which may be shadowing it on PATH." >&2
  exit 1
fi

profile=release
build_args=(build --release)
if [[ "${1:-}" == "--dev" ]]; then
  profile=debug
  build_args=(build)
fi

# `dx build` never cleans its own output directory between runs, so a
# previous build's now-unreferenced, content-hashed asset files (wasm/js/css
# all embed a hash in their filename) would otherwise sit alongside the
# fresh ones and get copied into dist/ below along with them.
rm -rf "target/dx/oxidarr-ui/$profile"

(cd crates/oxidarr-ui && "$dx_bin" "${build_args[@]}")

built="target/dx/oxidarr-ui/$profile/web/public"
if [[ ! -f "$built/index.html" ]]; then
  echo "error: expected a dx build output at $built/index.html, found none" >&2
  exit 1
fi

rm -rf crates/oxidarr-prowl/dist
mkdir -p crates/oxidarr-prowl/dist
cp -R "$built"/. crates/oxidarr-prowl/dist/

# dx only discovers (and content-hashes) assets reached through an `asset!()`
# call in Rust; it does not parse `url()` references inside a stylesheet. The
# two bundled webfonts are named only by assets/app.css, so dx never sees
# them — they are copied here under their plain, unhashed filenames, which is
# exactly what that stylesheet's `url("/assets/plex-*.woff2")` asks for.
cp crates/oxidarr-ui/assets/plex-sans.woff2 crates/oxidarr-ui/assets/plex-mono.woff2 \
  crates/oxidarr-prowl/dist/assets/

echo "wrote $(du -sh crates/oxidarr-prowl/dist | cut -f1) to crates/oxidarr-prowl/dist"
