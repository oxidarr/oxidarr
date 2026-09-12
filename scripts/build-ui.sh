#!/usr/bin/env bash
# Builds the real oxidarr-ui web bundle and copies it into
# crates/oxidarr-ui/dist — the directory crates/oxidarr-prowl/src/ui.rs's
# `include_dir!` embeds at compile time behind the `ui` cargo feature.
#
# `dx build`'s own Dioxus.toml `out_dir` setting is not consulted for a web
# build (verified against dioxus-cli 0.7.10's own source): it always writes
# to target/dx/oxidarr-ui/<release|debug>/web/public, under the *workspace*
# target dir, regardless of that setting. This script copies from there
# itself rather than relying on dx to place the bundle in dist/ for us.
set -euo pipefail

cd "$(dirname "$0")/.."

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

(cd crates/oxidarr-ui && dx "${build_args[@]}")

built="target/dx/oxidarr-ui/$profile/web/public"
if [[ ! -f "$built/index.html" ]]; then
  echo "error: expected a dx build output at $built/index.html, found none" >&2
  exit 1
fi

rm -rf crates/oxidarr-ui/dist
mkdir -p crates/oxidarr-ui/dist
cp -R "$built"/. crates/oxidarr-ui/dist/

echo "wrote $(du -sh crates/oxidarr-ui/dist | cut -f1) to crates/oxidarr-ui/dist"
