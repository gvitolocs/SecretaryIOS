#!/usr/bin/env bash
# scripts/tests/in-lane-worker-test.sh: scripts/lib/in-lane-worker.sh (PF-C046, GATE-12) and the three proof entry points
# that source it. Everything is a fixture: a fake /proc tree whose pid chain starts at this script's own $$, stub `lane`
# programs at absolute paths of the fixture, and entry points started with LANE_WORKER=1 so that they stop at the guard and
# never reach Verus or TLC. The paths the helper reads are ARGUMENTS of `in_lane_worker` (never the environment), which is
# also what the cases at the bottom prove: no variable turns a worker answer off. The last line is `in-lane-worker-test: pass`.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
base=${IN_LANE_WORKER_TEST_TMP:-${TMPDIR:-$HOME/scratch/tmp}}
mkdir -p "$base"
tmp=$(mktemp -d "$base/in-lane-worker.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

# shellcheck source=../lib/in-lane-worker.sh
. "$root/scripts/lib/in-lane-worker.sh"

fails=0
fail() {
    echo "FAIL: $*" >&2
    fails=$((fails + 1))
}

# expect <worker|not> <label> <lane_path> <proc_root>: the answer of the helper, with every LANE_* variable unset.
expect() {
    local want=$1 label=$2 got
    if (
        unset LANE_WORKER LANE_PROOFS_REFUSED
        in_lane_worker "$3" "$4"
    ); then got=worker; else got=not; fi
    [ "$got" = "$want" ] || fail "$label: wanted $want, got $got"
}

# fake_proc <dir> <cgroup of $$> <cgroup of parent 1> <cgroup of parent 2>: $$ -> 91001 -> 91002 -> 1. A cgroup of "-" is an
# unreadable step (the file is not written). The command names hold spaces and a ")" to prove the parse is after the last ") ".
fake_proc() {
    local d=$1 p
    rm -rf "$d"
    mkdir -p "$d/$$" "$d/91001" "$d/91002" "$d/1"
    printf '%s (bash) S 91001 1 1 0 -1\n' "$$" > "$d/$$/stat"
    printf '91001 (bq run (x) y) S 91002 1 1 0 -1\n' > "$d/91001/stat"
    printf '91002 (a b) S 1 1 1 0 -1\n' > "$d/91002/stat"
    printf '1 (init) S 0 1 1 0 -1\n' > "$d/1/stat"
    local i=2
    for p in "$$" 91001 91002; do
        [ "${!i}" = "-" ] || printf '0::%s\n' "${!i}" > "$d/$p/cgroup"
        i=$((i + 1))
    done
    printf '0::/init.scope\n' > "$d/1/cgroup"
}

clean=/user.slice/user-1000.slice/session-3.scope

# --- 1. the walk (no lane at all: the path does not exist)
nolane=$tmp/no-such-lane
fake_proc "$tmp/p1" "$clean" "$clean" "$clean"
expect not "clean chain" "$nolane" "$tmp/p1"

# the runner's scope on a GRANDPARENT, the script's own cgroup a bq scope: the case /proc/self/cgroup alone misses
fake_proc "$tmp/p2" /user.slice/bq-4242.scope /user.slice/bq-wrap-1.scope /user.slice/lane-runner-ab12cd.scope
expect worker "runner scope on a grandparent, own cgroup a bq scope" "$nolane" "$tmp/p2"

fake_proc "$tmp/p3" /user.slice/lane-runner-ab12cd.scope "$clean" "$clean"
expect worker "runner scope on the script itself" "$nolane" "$tmp/p3"

fake_proc "$tmp/p4" "$clean" /user.slice/lane-attempt-4521.scope "$clean"
expect worker "attempt scope lane-<name>-<n>.scope" "$nolane" "$tmp/p4"

fake_proc "$tmp/p5" "$clean" /user.slice/lane-notes.scope "$clean"
expect not "a lane-*.scope without a trailing number is not an attempt scope" "$nolane" "$tmp/p5"

# unreadable steps count as workers
fake_proc "$tmp/p6" "$clean" "-" "$clean"
expect worker "unreadable cgroup of a parent" "$nolane" "$tmp/p6"
fake_proc "$tmp/p7" "$clean" "$clean" "$clean"
rm "$tmp/p7/91001/stat"
expect worker "unreadable stat of a parent" "$nolane" "$tmp/p7"
fake_proc "$tmp/p8" "$clean" "$clean" "$clean"
printf 'garbage\n' > "$tmp/p8/91002/stat"
expect worker "stat without a parent field" "$nolane" "$tmp/p8"
fake_proc "$tmp/p9" "$clean" "$clean" "$clean"
printf '91002 (a b) S notapid 1 1 0 -1\n' > "$tmp/p9/91002/stat"
expect worker "parent that is not a number" "$nolane" "$tmp/p9"
fake_proc "$tmp/p10" "$clean" "$clean" "$clean"
printf '91002 (a b) S 91002 1 1 0 -1\n' > "$tmp/p10/91002/stat"
expect worker "a parent chain that never reaches pid 1" "$nolane" "$tmp/p10"

# --- 2. stub lane at an absolute path of the fixture
stub_lane() { # <path> <exit code of `in-worker`> <help works: 1|0>
    cat > "$1" << EOS
#!/bin/sh
if [ "\$1" = in-worker ] && [ "\$2" = --help ]; then exit $([ "$3" = 1 ] && echo 0 || echo 2); fi
if [ "\$1" = in-worker ]; then exit $2; fi
exit 9
EOS
    chmod 0755 "$1"
}
for code in 0 1 2 7; do
    stub_lane "$tmp/lane$code" "$code" 1
done
expect worker "lane in-worker exits 0 (clean tree)" "$tmp/lane0" "$tmp/p1"
expect not "lane in-worker exits 1 (clean tree)" "$tmp/lane1" "$tmp/p1"
expect worker "lane in-worker exits 2: cannot tell" "$tmp/lane2" "$tmp/p1"
expect worker "lane in-worker exits 7: anything but 1" "$tmp/lane7" "$tmp/p1"
expect worker "lane says 1 but an ancestor holds the runner's scope" "$tmp/lane1" "$tmp/p2"

# subcommand missing (LB-C063 not installed): --help fails, the walk decides
stub_lane "$tmp/lane-old" 0 0
expect not "subcommand missing, clean tree: the walk says not" "$tmp/lane-old" "$tmp/p1"
expect worker "subcommand missing, runner scope on a grandparent: the walk says worker" "$tmp/lane-old" "$tmp/p2"
# a lane that is not there at all
expect worker "no lane, runner scope on a grandparent" "$nolane" "$tmp/p2"

# --- 3. variables are a second trigger, and cannot clear anything
if ! (
    export LANE_WORKER=1
    in_lane_worker "$nolane" "$tmp/p1"
); then fail "LANE_WORKER=1 on a clean tree"; fi
if ! (
    export LANE_PROOFS_REFUSED=1
    in_lane_worker "$nolane" "$tmp/p1"
); then fail "LANE_PROOFS_REFUSED set on a clean tree"; fi
for v in "LANE_WORKER=0" "LANE_WORKER=" "LANE_PROOFS_REFUSED=" "LANE_ANYTHING=0"; do
    if ! (
        unset LANE_WORKER LANE_PROOFS_REFUSED
        export "${v?}"
        in_lane_worker "$nolane" "$tmp/p2"
    ); then fail "variable $v turned a worker (runner scope on a grandparent) into not-a-worker"; fi
    if ! (
        unset LANE_WORKER LANE_PROOFS_REFUSED
        export "${v?}"
        in_lane_worker "$tmp/lane0" "$tmp/p1"
    ); then fail "variable $v turned lane's exit 0 into not-a-worker"; fi
done
if ! (
    unset LANE_WORKER LANE_PROOFS_REFUSED
    in_lane_worker "$nolane" "$tmp/p2"
); then fail "with the variables unset the walk must still say worker"; fi
# the paths are arguments, never the environment: a variable that names them changes nothing
if ! (
    export LANE_PATH="$tmp/lane1" PROC_ROOT="$tmp/p1" PROCROOT="$tmp/p1" LANE="$tmp/lane1"
    in_lane_worker "$nolane" "$tmp/p2"
); then fail "an environment variable replaced a path argument"; fi

# --- 4. the three entry points stop at the guard, exit 75, with the message, and never reach a prover
entry() { # <script path under root>
    local f=$root/$1 name out code
    name=$(basename "$1")
    [ -f "$f" ] || { fail "$1 is missing"; return; }
    grep -q 'lib/in-lane-worker.sh' "$f" || fail "$1 does not source the helper"
    code=0
    out=$(cd "$tmp" && LANE_WORKER=1 VERUS=/nonexistent/verus bash "$f" 2>&1 > /dev/null) || code=$?
    [ "$code" = 75 ] || fail "$1: exit $code, wanted 75: $out"
    [ "$out" = "$name: refused inside a lane worker (proofs run on GitHub)" ] || fail "$1: message was: $out"
}
for e in scripts/verify.sh scripts/verify-shipped.sh scripts/verify-verus.sh; do
    entry "$e"
done

# the mutants.sh stay untouched on purpose (coordinator 2026-11-19)
while IFS= read -r m; do
    if grep -q 'in-lane-worker' "$m"; then fail "$m carries the guard; the mutants.sh are frozen on purpose"; fi
done < <(find "$root/verification" "$root/crates" "$root/scripts" \( -name mutants.sh -o -name "verus-mutants-*.sh" \) 2> /dev/null)

if [ "$fails" -ne 0 ]; then
    echo "in-lane-worker-test: $fails failure(s)" >&2
    exit 1
fi
echo "in-lane-worker-test: pass"
