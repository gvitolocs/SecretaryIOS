#!/usr/bin/env bash
# Builds the AR model (lucina.usdz + lucina-ar.json) from the live avatar and its clip library, on nezopt.
#   tools/lucina-usdz/run.sh [model_dir] [anims_dir] [out_dir]
# Defaults: the live model G and its clips. The output is private (derived from the model): copy both files
# into the server's SECRETARY_PRIVATE_ASSETS_DIR next to lucina.glb; never commit them.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
model="${1:-$HOME/scratch/lucina-avatar-g}"
anims="${2:-$HOME/scratch/lucina-anim/out/g}"
out="${3:-$model/ar}"
blender="${BLENDER:-$HOME/opt/blender/blender}"
mkdir -p "$out"
rm -rf "$out/tex"
"$blender" -b --factory-startup --python "$here/convert_usdz.py" -- \
    --glb "$model/lucina.glb" --manifest "$model/manifest.json" \
    --anims "$anims/lucina-anims.glb" --clips "$anims/clips.json" \
    --out "$out/lucina.usdz" --table "$out/lucina-ar.json" 2>&1 | grep -E '^\[usdz\]|Error|Traceback'
if [[ "${CHECK:-1}" == 1 ]]; then
    "$blender" -b --factory-startup --python "$here/render_check.py" -- "$out/lucina.usdz" "$out/check" 120 300 >/dev/null 2>&1
    echo "check renders: $out/check"
fi
ls -l "$out/lucina.usdz" "$out/lucina-ar.json"
