#!/usr/bin/env bash
# Verus on the functions that ship, not on a model of them (D-011, the same script as LucinaOS's). Such a file is plain
# Rust with its proof in `//@` lines. This script makes what Verus reads by mechanical steps only, and checks that undoing
# them gives the shipped text back:
#   `-> T //@ret`   becomes `-> (ret: T)`, the one thing Rust's syntax has no comment form for
#   `... //@omit`   is dropped (a derive Verus does not check, and that holds no logic)
#   `//@ proof`     becomes `proof`
# Run through verify-gate on nezopt (verify-gate -- scripts/verify-shipped.sh), or on nespc:
#   verus-remote . env VERUS=verus scripts/verify-shipped.sh
# Mutants that must fail: verification/*/mutants.sh.
# Run one file only: FILES=crates/secretary-server/src/voice_rules.rs scripts/verify-shipped.sh

# GATE-12 / PF-C046: proofs run on GitHub; a lane worker starts none.
. "$(dirname "$0")/lib/in-lane-worker.sh" || exit 75
if in_lane_worker; then
    echo "verify-shipped.sh: refused inside a lane worker (proofs run on GitHub)" >&2
    exit 75
fi

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
verus=${VERUS:-/home/nez/tools/verus/verus-x86-linux/verus}
files=(crates/lucina-vault/src/sync_rules.rs crates/secretary-server/src/voice_rules.rs)
if [ -n "${FILES:-}" ]; then read -r -a files <<< "$FILES"; fi
work=$(mktemp -d "${TMPDIR:-/tmp}/verify-shipped.XXXXXX")
trap 'rm -rf "$work"' EXIT
status=0
for f in "${files[@]}"; do
    out=$work/$(basename "$f")
    # The proof lines stay tagged, so that taking them out again can be compared with the shipped text.
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$root/$f")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body" > "$out"
    a=$(sed -E -e '/^ *\/\/@/d' -e '/\/\/@omit$/d' -e 's/ \/\/@ret$//' "$root/$f")
    b=$(printf '%s\n' "$body" | grep -vF '/*proof*/' | sed -E 's/-> \(ret: (.*)\)$/-> \1/')
    if [ "$a" != "$b" ]; then
        echo "FAIL $f: what Verus reads is not the shipped code"
        diff <(echo "$a") <(echo "$b") | head -20
        status=1
        continue
    fi
    if "$verus" "$out"; then echo "ok $f"; else echo "FAIL $f"; status=1; fi
done
exit "$status"
