#!/usr/bin/env bash
# Render contact sheets of the optimised icons and the A/B evidence.
# Usage: tools/icons/sheets.sh [out-dir]   (default ~/scratch/icon-pilot/sheets)
# Per set (items, sections):
#   <set>-B-light-1x.png   chosen style, spec tiles (#f16a6f / #6eb8ae), true 58 px size
#   <set>-B-light-2x.png   the same at 2x zoom
#   <set>-B-dark-web.png   dark page, spec + web tiles (#ff3b6b / #14cfc1), with the web's drop shadow
#   <set>-A-light-1x.png   rejected variant A (1.5 px ink outline on every shape)
#   <set>-A2-light-1x.png  rejected variant A' (1.5 px ink outline around the silhouette only)
# plus contrast-{B,A,A2}.txt from `icons contrast`.
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
bin="$repo/tools/icons/target/release/icons"
out="${1:-$HOME/scratch/icon-pilot/sheets}"
mkdir -p "$out"
cat="$repo/assets/catalog/catalog.v1.json"
for set in items sections; do
  dir="$repo/assets/icons/$set"
  "$bin" sheet --catalog "$cat" --cols 5 --out "$out/$set-B-light-1x.png" "$dir"
  "$bin" sheet --catalog "$cat" --cols 5 --scale 2 --out "$out/$set-B-light-2x.png" "$dir"
  "$bin" sheet --catalog "$cat" --tiles all --cols 3 --theme dark --shadow --out "$out/$set-B-dark-web.png" "$dir"
  "$bin" sheet --catalog "$cat" --cols 5 --outline --out "$out/$set-A-light-1x.png" "$dir"
  "$bin" sheet --catalog "$cat" --cols 5 --silhouette --out "$out/$set-A2-light-1x.png" "$dir"
done
"$bin" contrast "$repo/assets/icons/items" "$repo/assets/icons/sections" > "$out/contrast-B.txt" || echo "contrast gate failed, see $out/contrast-B.txt"
"$bin" contrast --outline "$repo/assets/icons/items" "$repo/assets/icons/sections" > "$out/contrast-A.txt" || true
"$bin" contrast --silhouette "$repo/assets/icons/items" "$repo/assets/icons/sections" > "$out/contrast-A2.txt" || true
ls "$out"
