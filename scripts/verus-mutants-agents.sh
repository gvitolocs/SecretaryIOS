#!/usr/bin/env bash
# Lucina Agents kernels: each must verify, and every one of its mutants
# (verification/verus/mutants/<kernel>/*.rs) must FAIL Verus.
# Run: ~/.local/bin/verify-gate --mem 4G -- scripts/verus-mutants-agents.sh
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
export VSTD_PATH="${VSTD_PATH:-$HOME/tools/verus/verus-x86-linux}"
verus="${VERUS_BIN:-$VSTD_PATH/verus}"

killed=0
total=0
for kernel in agent_handoff acp_turn_watch; do
    if ! "$verus" "verification/verus/$kernel.rs" >/dev/null 2>&1; then
        echo "kernel fails: verification/verus/$kernel.rs"
        exit 1
    fi
    echo "kernel verifies: $kernel"
    for m in verification/verus/mutants/"$kernel"/*.rs; do
        total=$((total + 1))
        if "$verus" "$m" >/dev/null 2>&1; then
            echo "SURVIVED $m"
        else
            killed=$((killed + 1))
            echo "killed $m"
        fi
    done
done
echo "mutants: $killed/$total killed"
[ "$killed" -eq "$total" ] && [ "$total" -gt 0 ]
