#!/usr/bin/env bash

# GATE-12 / PF-C046: proofs run on GitHub; a lane worker starts none.
. "$(dirname "$0")/lib/in-lane-worker.sh" || exit 75
if in_lane_worker; then
    echo "verify-verus.sh: refused inside a lane worker (proofs run on GitHub)" >&2
    exit 75
fi

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
