#!/usr/bin/env bash
# Copies the shopping item and section illustrations (assets/icons/{items,sections}/*.svg,
# docs/design/item-icons.md on main) into the app's ItemIcons asset catalog as vector
# images named item-<catalogId> and section-<sectionId> (ItemVisual.swift looks them up).
# Usage: ios/scripts/sync-item-icons.sh [icons-dir]   (default: <repo>/assets/icons, else
# nezopt's main checkout). Re-run after icons change on main; the output is committed.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
src="${1:-$repo/assets/icons}"
if [[ ! -d "$src/items" ]]; then
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    scp -qr nezopt:/home/nez/Projects/SecretaryIOS/assets/icons/items nezopt:/home/nez/Projects/SecretaryIOS/assets/icons/sections "$tmp/"
    src="$tmp"
fi
out="$here/../Secretary/Resources/ItemIcons.xcassets"
rm -rf "$out"
mkdir -p "$out"
printf '{ "info" : { "author" : "xcode", "version" : 1 } }\n' > "$out/Contents.json"
count=0
for kind in items sections; do
    prefix="${kind%s}"
    for svg in "$src/$kind"/*.svg; do
        id="$(basename "$svg" .svg)"
        set="$out/$prefix-$id.imageset"
        mkdir -p "$set"
        # The sources have only a viewBox; the asset compiler wants an intrinsic size (64 pt).
        sed 's/<svg /<svg width="64" height="64" /' "$svg" > "$set/$id.svg"
        cat > "$set/Contents.json" <<JSON
{
  "images" : [ { "filename" : "$id.svg", "idiom" : "universal" } ],
  "info" : { "author" : "xcode", "version" : 1 },
  "properties" : { "preserves-vector-representation" : true, "template-rendering-intent" : "original" }
}
JSON
        count=$((count + 1))
    done
done
echo "wrote $count icons to $out"
