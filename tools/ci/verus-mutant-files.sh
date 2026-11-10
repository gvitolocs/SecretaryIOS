#!/usr/bin/env bash
# Every file in a mutant folder (verification/verus/mutants/<kernel>/*.rs) plants one bug in a copy of a Verus kernel, and
# Verus must refuse each one with a verification error. A file that fails to compile is reported STALE, not refused: a mutant
# that no longer parses proves nothing. Exit 0 only if every mutant is refused; 1 if any survived or went stale.
# Usage: VERUS=<verus> tools/ci/verus-mutant-files.sh verification/verus/mutants/<kernel>/
set -uo pipefail
dir=${1:?usage: verus-mutant-files.sh <mutant dir>}
verus=${VERUS:-/home/nez/tools/verus/verus-x86-linux/verus}
shopt -s nullglob
files=("${dir%/}"/*.rs)
[ "${#files[@]}" -gt 0 ] || { echo "no mutants in $dir"; exit 1; }
refused=0
for m in "${files[@]}"; do
    out=$("$verus" "$m" 2>&1)
    rc=$?
    errors=$(printf '%s\n' "$out" | sed -nE 's/.*verification results:: [0-9]+ verified, ([0-9]+) errors.*/\1/p' | tail -1)
    if [ "$rc" -eq 0 ]; then
        echo "SURVIVED $m"
    elif [ -n "$errors" ] && [ "$errors" -gt 0 ]; then
        echo "refused $m ($errors errors)"
        refused=$((refused + 1))
    else
        echo "STALE $m (exit $rc with no verification error: it does not compile)"
        printf '%s\n' "$out" | grep -m3 -E '^error' || true
    fi
done
echo "mutants: $refused/${#files[@]} refused"
[ "$refused" -eq "${#files[@]}" ]
