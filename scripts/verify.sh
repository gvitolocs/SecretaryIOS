#!/usr/bin/env bash
# Formal verification gate (docs/verification.md):
#   1. Stateright models  (verification/models: cargo test + the summary table), plus the
#      property / fault-injection tests of the pure kernels they model (crates/ink-sync) and
#      the deals kernel's differential tests on the pinned lucina-deals (verification/verus/diff/run-deals.sh)
#   2. TLC on every formal/**/*.cfg (each cfg's first line says MUST PASS or MUST FAIL)
#   3. Verus on every verification/verus/*.rs (top level only: subdirectories such as
#      verification/verus/diff/ hold plain-Rust differential tests, not Verus files)
# Exit 1 on any failure; a summary table at the end.
#
# Usage: scripts/verify.sh [--deep] [--only models|tlc|verus]
#   --deep   exhaustive Stateright bounds (feature `deep`) and the *_deep TLC cfgs
# Env: JAVA, TLA_JAR, VERUS_BIN (defaults: ~/tools/...), VERUS_DIR (default verification/verus),
#      LUCINA_POKEMON_DIR (the lucina-pokemon crate, for its kernel differential tests; skipped if absent),
#      CARGO_TARGET_DIR, TLC_WORKERS,
#      TLC_TIMEOUT (seconds per cfg, default 900), CARGO_JOBS (default 2).

# GATE-12 / PF-C046: proofs run on GitHub; a lane worker starts none.
. "$(dirname "$0")/lib/in-lane-worker.sh" || exit 75
if in_lane_worker; then
    echo "verify.sh: refused inside a lane worker (proofs run on GitHub)" >&2
    exit 75
fi

set -u

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

deep=0
only=""
while [ $# -gt 0 ]; do
    case "$1" in
        --deep) deep=1 ;;
        --only) only="${2:-}"; shift ;;
        -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

JAVA="${JAVA:-$HOME/tools/jdk-21.0.12.1+1/bin/java}"
TLA_JAR="${TLA_JAR:-$HOME/tools/tla/tla2tools.jar}"
# On nezopt every gate runs through verify-gate (one at a time, MemoryMax 12G, no swap): it holds
# /tmp/lucina-verify.lock on fd 9 and execs this script, so fd 9 points at the lock when it is ours.
if [ "$(readlink "/proc/$$/fd/9" 2>/dev/null)" != /tmp/lucina-verify.lock ] && { [ -x "$HOME/.local/bin/verify-gate" ] || command -v verify-gate >/dev/null 2>&1; }; then
    echo "verify.sh: not run through verify-gate; on nezopt use 'verify-gate' (or 'verify-gate -- scripts/verify.sh ...') so gates do not run in parallel and fill swap" >&2
fi

export VSTD_PATH="${VSTD_PATH:-$HOME/tools/verus/verus-x86-linux}"
VERUS_BIN="${VERUS_BIN:-$VSTD_PATH/verus}"
TLC_WORKERS="${TLC_WORKERS:-4}"
TLC_TIMEOUT="${TLC_TIMEOUT:-900}"
CARGO_JOBS="${CARGO_JOBS:-2}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"

work="$(mktemp -d "${TMPDIR:-/tmp}/verify.XXXXXX")"
trap 'rm -rf "$work"' EXIT

rows=()      # "section|name|expect|result|detail"
failed=0
row() { rows+=("$1|$2|$3|$4|$5"); case "$4" in PASS*) ;; *) failed=1 ;; esac; }
want() { [ -z "$only" ] || [ "$only" = "$1" ]; }

# ---- 1. Stateright -----------------------------------------------------------
if want models; then
    echo "== Stateright models (verification/models)"
    features=()
    [ "$deep" -eq 1 ] && features=(--features deep)
    start=$(date +%s)
    if cargo test -j"$CARGO_JOBS" -q -p verification-models "${features[@]}" >"$work/models-test.log" 2>&1; then
        row models "cargo test -p verification-models" pass "PASS" "$(( $(date +%s) - start ))s"
    else
        tail -40 "$work/models-test.log"
        row models "cargo test -p verification-models" pass "FAIL" "see the output above"
    fi
    # Lucina Ink sync: the real apply under permutations + duplicates (proptest) and
    # devices with crashes and torn writes over a flaky transport (docs/verification.md §6).
    start=$(date +%s)
    if cargo test -j"$CARGO_JOBS" -q -p ink-sync >"$work/ink-sync-test.log" 2>&1; then
        row models "cargo test -p ink-sync (property + fault injection)" pass "PASS" \
            "$(grep -oE '[0-9]+ passed' "$work/ink-sync-test.log" | awk '{n+=$1} END {print n}') tests, $(( $(date +%s) - start ))s"
    else
        tail -40 "$work/ink-sync-test.log"
        row models "cargo test -p ink-sync (property + fault injection)" pass "FAIL" "see the output above"
    fi
    # The Verus deals kernel against the real lucina-deals at the pinned revision (§14).
    start=$(date +%s)
    if verification/verus/diff/run-deals.sh >"$work/deals-diff.log" 2>&1; then
        row models "lucina-deals Verus differential tests (pinned source)" pass "PASS" "$(( $(date +%s) - start ))s"
    else
        tail -40 "$work/deals-diff.log"
        row models "lucina-deals Verus differential tests (pinned source)" pass "FAIL" "see the output above"
    fi
    # deploy-guard: the deploy model's fix as a tool (docs/verification.md §7), its rule and
    # today's incident replayed on real git repos.
    start=$(date +%s)
    if cargo test -j"$CARGO_JOBS" -q --manifest-path tools/deploy-guard/Cargo.toml >"$work/deploy-guard-test.log" 2>&1; then
        row models "cargo test deploy-guard (rule + git replay)" pass "PASS" \
            "$(grep -oE '[0-9]+ passed' "$work/deploy-guard-test.log" | awk '{n+=$1} END {print n}') tests, $(( $(date +%s) - start ))s"
    else
        tail -40 "$work/deploy-guard-test.log"
        row models "cargo test deploy-guard (rule + git replay)" pass "FAIL" "see the output above"
    fi
    # Lucina's Pokémon planner (Lucina repo): the differential tests that tie verification/verus/game_planner.rs
    # to the code (planned ROM routes against the kernel's step rule; the story requirement table).
    pk="${LUCINA_POKEMON_DIR:-$HOME/Projects/.worktrees/Lucina/game-link/rust/lucina-pokemon}"
    if [ -f "$pk/Cargo.toml" ]; then
        start=$(date +%s)
        if (cd "$pk" && CARGO_TARGET_DIR="$pk/target" cargo test -j"$CARGO_JOBS" -q --release -- \
            routes_only_enter_walkable_cells story_requirements_come_first) >"$work/pokemon-test.log" 2>&1; then
            row models "lucina-pokemon kernel differential tests" pass "PASS" "$(( $(date +%s) - start ))s"
        else
            tail -40 "$work/pokemon-test.log"
            row models "lucina-pokemon kernel differential tests" pass "FAIL" "see the output above"
        fi
    fi
    # The same cases as a table (states explored, depth, verdict per property).
    if cargo run -j"$CARGO_JOBS" -q -p verification-models --bin verify-models -- $([ "$deep" -eq 1 ] && echo --deep) \
        >"$work/models.txt" 2>&1; then
        row models "verify-models (table above)" pass "PASS" "$(grep -c '  PASS ' "$work/models.txt") cases"
    else
        row models "verify-models (table above)" pass "FAIL" "$(grep -c '  FAIL ' "$work/models.txt") case(s) failed"
    fi
    cat "$work/models.txt"
    echo
fi

# ---- 2. TLC -----------------------------------------------------------------------
# Every formal/**/*.cfg is found by the loop below, protocol 15 included
# (formal/retirement/{Reminders,Delivery,Telegram,Updates,Discord}.tla and their cfgs).
if want tlc; then
    echo "== TLC (formal/)"
    if [ ! -x "$JAVA" ] || [ ! -f "$TLA_JAR" ]; then
        row tlc "toolchain" "-" "FAIL" "java ($JAVA) or tla2tools.jar ($TLA_JAR) missing"
    else
        while IFS= read -r cfg; do
            dir="$(dirname "$cfg")"
            name="$(basename "$cfg" .cfg)"
            case "$name" in *_deep*) [ "$deep" -eq 1 ] || continue ;; esac
            # The spec: the longest *.tla in the directory whose name prefixes the cfg.
            spec=""
            for t in "$dir"/*.tla; do
                b="$(basename "$t" .tla)"
                case "$b" in *_TTrace_*) continue ;; esac
                case "$name" in "$b"|"$b"_*) [ ${#b} -gt ${#spec} ] && spec="$b" ;; esac
            done
            head1="$(head -1 "$cfg")"
            case "$head1" in
                *"MUST PASS"*) expect=pass ;;
                *"MUST FAIL"*) expect=violation ;;
                *) row tlc "$cfg" "?" "FAIL" "first line must say MUST PASS or MUST FAIL"; continue ;;
            esac
            if [ -z "$spec" ]; then
                row tlc "$cfg" "$expect" "FAIL" "no spec for this cfg"
                continue
            fi
            log="$work/$name.log"
            start=$(date +%s)
            (cd "$dir" && timeout "$TLC_TIMEOUT" "$JAVA" -XX:+UseParallelGC -cp "$TLA_JAR" tlc2.TLC \
                -config "$name.cfg" -workers "$TLC_WORKERS" -deadlock -noGenerateSpecTE \
                -metadir "$work/states-$name" "$spec.tla") >"$log" 2>&1
            rc=$?
            secs=$(( $(date +%s) - start ))
            states="$(grep -oE '[0-9,]+ distinct states found' "$log" | tail -1 | cut -d' ' -f1)"
            what="$(grep -oE 'Invariant [A-Za-z0-9_]+ is violated|Temporal properties were violated' "$log" | head -1)"
            if [ "$expect" = pass ] && [ "$rc" -eq 0 ]; then
                row tlc "$cfg" pass "PASS" "${states:-?} states, ${secs}s"
            elif [ "$expect" = violation ] && { [ "$rc" -eq 12 ] || [ "$rc" -eq 13 ]; }; then
                row tlc "$cfg" violation "PASS (violated as required)" "${what:-rc=$rc}, ${secs}s"
            else
                grep -E "Error|violated" "$log" | head -5
                row tlc "$cfg" "$expect" "FAIL" "rc=$rc ${what:-}, ${secs}s (log: TLC output above)"
            fi
            printf '  %-58s %s\n' "$cfg" "rc=$rc ${states:+$states states} ${secs}s"
        done < <(find formal -name '*.cfg' -not -path '*/states/*' | sort)
    fi
    echo
fi

# ---- 3. Verus ----------------------------------------------------------------------
if want verus; then
    VERUS_DIR="${VERUS_DIR:-verification/verus}"
    echo "== Verus ($VERUS_DIR)"
    shopt -s nullglob
    # Every top-level kernel, protocol 15 included (retirement_kernels.rs: outcome, status, lease, claims).
    vfiles=("$VERUS_DIR"/*.rs)
    shopt -u nullglob
    if [ ! -x "$VERUS_BIN" ]; then
        row verus "toolchain" "-" "FAIL" "verus not found at $VERUS_BIN"
    elif [ "${#vfiles[@]}" -eq 0 ]; then
        row verus "$VERUS_DIR" pass "FAIL" "no *.rs files"
    else
        for f in "${vfiles[@]}"; do
            start=$(date +%s)
            if out="$("$VERUS_BIN" "$f" 2>&1)"; then
                row verus "$f" pass "PASS" "$(echo "$out" | grep -oE '[0-9]+ verified, [0-9]+ errors' | tail -1), $(( $(date +%s) - start ))s"
            else
                echo "$out" | tail -20
                row verus "$f" pass "FAIL" "$(echo "$out" | grep -oE '[0-9]+ verified, [0-9]+ errors' | tail -1)"
            fi
        done
    fi
    echo
fi

# ---- summary -----------------------------------------------------------------------
printf '\n%-7s %-60s %-10s %-28s %s\n' section check expect result detail
printf '%.0s-' {1..130}; echo
for r in "${rows[@]}"; do
    IFS='|' read -r sec name exp res det <<<"$r"
    printf '%-7s %-60s %-10s %-28s %s\n' "$sec" "$name" "$exp" "$res" "$det"
done
echo
if [ "$failed" -eq 0 ]; then
    echo "VERIFY GREEN"
else
    echo "VERIFY FAILED"
fi
exit "$failed"
