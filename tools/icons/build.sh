#!/usr/bin/env bash
# Optimise the hand-written sources in assets/icons/src/{items,sections} into assets/icons/{items,sections}
# with svgo, then validate the results against the style guide (docs/design/item-icons.md).
# Usage: tools/icons/build.sh            (run from anywhere)
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
tools="$repo/tools/icons"
cargo build --release --quiet --manifest-path "$tools/Cargo.toml"
for kind in items sections; do
  src="$repo/assets/icons/src/$kind"
  compgen -G "$src/*.svg" >/dev/null || continue
  mkdir -p "$repo/assets/icons/$kind"
  # Drop outputs whose source is gone.
  for out in "$repo/assets/icons/$kind"/*.svg; do
    [ -e "$out" ] && [ ! -e "$src/$(basename "$out")" ] && rm -- "$out"
  done
  npx -y svgo@4.0.0 --config "$tools/svgo.config.mjs" --quiet -f "$src" -o "$repo/assets/icons/$kind"
done
"$tools/target/release/icons" validate "$repo/assets/icons/items" "$repo/assets/icons/sections"
