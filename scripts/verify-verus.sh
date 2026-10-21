#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

export VSTD_PATH="${VSTD_PATH:-$HOME/tools/verus/verus-x86-linux}"
verus="${VERUS_BIN:-$VSTD_PATH/verus}"

shopt -s nullglob
files=(verification/verus/*.rs)
if [ "${#files[@]}" -eq 0 ]; then
    echo "error: no verification/verus/*.rs files found" >&2
    exit 1
fi

status=0
for file in "${files[@]}"; do
    echo "verifying $file"
    if ! "$verus" "$file"; then
        status=1
    fi
done
exit "$status"
