#!/usr/bin/env bash
# Fetch the Cardigann v11 indexer definitions used by the test corpus.
# These are third-party and unlicensed; they are never committed.
set -euo pipefail

dest="${1:-.definitions}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Fetching Prowlarr indexer definitions..."
curl -fsSL "https://github.com/Prowlarr/Indexers/archive/refs/heads/master.tar.gz" \
  -o "$tmp/indexers.tar.gz"
tar xzf "$tmp/indexers.tar.gz" -C "$tmp"

mkdir -p "$dest"
rm -rf "${dest:?}/v11"
cp -R "$tmp/Indexers-master/definitions/v11" "$dest/v11"

count=$(find "$dest/v11" -name '*.yml' | wc -l | tr -d ' ')
echo "Fetched $count definitions into $dest/v11"
