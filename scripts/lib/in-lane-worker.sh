# shellcheck shell=bash
# scripts/lib/in-lane-worker.sh: sourced, never run. One function, `in_lane_worker`: returns 0 when this process runs
# inside a lane worker, 1 when it does not. PF-C046 (GATE-12), coordinator decision 2026-11-19 (option b).
#
# The proof entry points (Verus, TLC, mutation runs) refuse to start inside a lane worker: proofs run on GitHub, and a
# worker that starts one costs the box an hour of CPU and RAM for a result nobody reads. Each entry point sources this
# file and exits 75 when `in_lane_worker` returns 0.
#
# The answer is the OR of three triggers, and none of them can be turned off from inside the worker:
#   1. `lane in-worker`, called ONLY by its absolute path (a worker's PATH starts with lane's shim directory and a worker
#      can edit PATH). Exit 0 = worker, 1 = not a worker, 2 = cannot tell. Until the subcommand is installed (LB-C063) the
#      call fails, which `lane in-worker --help` shows; then this trigger is skipped. When it is installed, only an exact
#      1 clears it; every other status counts as a worker. The status variable is set before it is read, so a call that
#      never ran is not read as "not a worker".
#   2. A walk up the parent chain from $$ to pid 1: any process whose /proc/<pid>/cgroup names a `lane-runner-*.scope`
#      or a `lane-*-<n>.scope` (an attempt scope) makes this a worker. A script started through `bq run` sits in its own
#      bq-*.scope, and only an ancestor carries the runner's scope, so /proc/self/cgroup alone is not enough. A step that
#      cannot be read (no stat, no cgroup, a parent that is not a number, a chain that does not end) counts as a worker.
#   3. LANE_WORKER=1 or LANE_PROOFS_REFUSED set. A second trigger only: unsetting them turns off nothing else.
# Triggers 1 and 2 are both asked when `lane` says 1: lane saying "not a worker" never overrides the walk.
#
# Both paths are arguments for the tests only: `in_lane_worker [lane_path [proc_root]]`, defaulting to
# /home/nez/.local/bin/lane and /proc. They are never read from the environment, so no variable can say "not a worker".
#
# Not guarded, on purpose: the 70 mutants.sh are left alone on purpose (coordinator 2026-11-19): they are frozen proof
# units, the harm is cost not safety, and lane's namespace fence hides the tools; do not add the guard to them.

in_lane_worker() {
    local lane=${1:-/home/nez/.local/bin/lane}
    local procroot=${2:-/proc}
    local st=2

    if [ "${LANE_WORKER:-}" = 1 ] || [ -n "${LANE_PROOFS_REFUSED:-}" ]; then
        return 0
    fi

    if [ -x "$lane" ] && "$lane" in-worker --help > /dev/null 2>&1; then
        st=2
        "$lane" in-worker > /dev/null 2>&1 || st=$?
        if [ "$st" -ne 1 ]; then
            return 0
        fi
    fi

    local p=$$ n=0 line stat rest ppid
    local re_runner='/lane-runner-[^/]*\.scope$'
    local re_attempt='/lane-[^/]*-[0-9]+\.scope$'
    while :; do
        n=$((n + 1))
        [ "$n" -le 128 ] || return 0
        [ -r "$procroot/$p/cgroup" ] || return 0
        while IFS= read -r line || [ -n "$line" ]; do
            if [[ $line =~ $re_runner ]] || [[ $line =~ $re_attempt ]]; then
                return 0
            fi
        done < "$procroot/$p/cgroup" 2> /dev/null || return 0
        [ "$p" = 1 ] && return 1
        [ -r "$procroot/$p/stat" ] || return 0
        stat=$(< "$procroot/$p/stat") 2> /dev/null || return 0
        case $stat in *') '*) ;; *) return 0 ;; esac
        rest=${stat##*) }
        rest=${rest#* }
        ppid=${rest%% *}
        case $ppid in '' | *[!0-9]*) return 0 ;; esac
        [ "$ppid" -ge 1 ] || return 0
        p=$ppid
    done
}
