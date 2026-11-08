#!/usr/bin/env bash
# Every shopping_text mutant must FAIL Verus; the real kernel must pass.
# Run: ~/.local/bin/verify-gate --mem 4G -- scripts/verus-mutants-shopping-text.sh
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
export VSTD_PATH="${VSTD_PATH:-$HOME/tools/verus/verus-x86-linux}"
verus="${VERUS_BIN:-$VSTD_PATH/verus}"

if ! "$verus" verification/verus/shopping_text.rs >/dev/null 2>&1; then
    echo "kernel fails: verification/verus/shopping_text.rs"
    exit 1
fi
echo "kernel verifies"

killed=0
total=0
for m in verification/verus/mutants/shopping_text/*.rs; do
    total=$((total + 1))
    if "$verus" "$m" >/dev/null 2>&1; then
        echo "SURVIVED $m"
    else
        killed=$((killed + 1))
        echo "killed $m"
    fi
done
echo "mutants: $killed/$total killed"
[ "$killed" -eq "$total" ] && [ "$total" -gt 0 ]
