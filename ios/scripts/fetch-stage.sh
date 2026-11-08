#!/usr/bin/env bash
# Copies the built Lucina stage bundle (packages/lucina-stage, `npm run build`) into the
# app. Only the renderer code: the model and manifest are private assets the app fetches
# at runtime from /v1/assets/* with the device token, never bundled.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$here/../Secretary/Resources/Stage/lucina-stage.standalone.js"
local="$repo/packages/lucina-stage/dist/lucina-stage.standalone.js"
if [[ -f "$local" ]]; then
    cp "$local" "$out"
else
    scp -q nezopt:/home/nez/Projects/SecretaryIOS/packages/lucina-stage/dist/lucina-stage.standalone.js "$out"
fi
echo "updated $out ($(wc -c < "$out") bytes)"
