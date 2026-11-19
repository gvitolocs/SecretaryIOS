# shellcheck shell=sh
# scripts/lib/in-lane-worker.sh: sourced, never run. One function, `in_lane_worker`: returns 0 when this process runs
# inside a lane worker, 1 when it does not. PF-C046 (GATE-12), coordinator decision 2026-11-19 (option b).
#
# WHAT THIS IS AND IS NOT. It is a fence on the named proof entry points against a casual unset: a worker that runs
# `scripts/verify.sh` (or another guarded script) by habit is stopped with exit 75. It is NOT a boundary against a worker
# that edits its worktree, runs Verus or TLC directly, or uses a namespace; lane's mount fence hides those tools.
#
# The proof entry points (Verus, TLC, mutation runs) refuse to start inside a lane worker: proofs run on GitHub, and a
# worker that starts one costs the box an hour of CPU and RAM for a result nobody reads. Each entry point sources this
# file BEFORE any bash-only line and exits 75 when `in_lane_worker` returns 0.
#
# POSIX only: no `[[ ]]`, no arrays, no `local`, no `$(< file)`, so `sh script.sh` (dash) gets the same answer as bash. A
# shell that cannot run the rest answers "worker" (the first lines below); the guard never falls through.
#
# The answer is the OR of these triggers. No variable turns a trigger off; the `lane` binary and the kernel's view of
# /proc are in the worker's uid and are not defended here:
#   1. `lane in-worker`, called ONLY by its absolute path (a worker's PATH starts with lane's shim directory and a worker
#      can edit PATH), under `/usr/bin/timeout 10`, by absolute path like lane (a PATH shim named `timeout` cannot skip the trigger; a
#      timeout, or a `timeout` that is missing or not executable, counts as a worker). Exit 0 = worker, 1 = not a worker, 2 = cannot
#      tell. Only an exact 1 stops this trigger from answering "worker" (triggers 2 and 3 are still asked).
#      If `lane in-worker --help` fails but `lane in-worker` itself returns 0, 1 or 2,
#      that result is used; any other status without a working --help means lane has no such subcommand and the trigger
#      is skipped. The status variable is set before it is read, so a call that never ran is not read as "not a worker".
#   2. A walk up the parent chain from $$ to pid 1: any process whose /proc/<pid>/cgroup names a `lane-runner-*.scope`
#      or a `lane-*-<n>.scope` (an attempt scope) makes this a worker. A script started through `bq run` sits in its own
#      bq-*.scope, and only an ancestor carries the runner's scope, so /proc/self/cgroup alone is not enough. A step that
#      cannot be read (no stat, no cgroup, a parent that is not a number, a chain that does not end) counts as a worker.
#      The one exception, asked BEFORE trigger 1: when the proc root itself is absent and `uname -s` (called as
#      /usr/bin/uname) is Darwin, the answer is "not a worker" (the Mac has no /proc and no lane worker; lane workers run
#      only on Linux), so mac-model-check.sh runs on the host it exists for. A proc root that exists but has no `self`
#      (a Linux view with procfs hidden or partial) is not this case: it stays a worker. macOS has no /usr/bin/timeout, and a
#      missing `timeout` counts as a worker, but on the Mac the Darwin branch answers first.
#   3. LANE_WORKER=1 or LANE_PROOFS_REFUSED set. A second trigger only: unsetting them turns off nothing else.
# Triggers 1 and 2 are both asked when `lane` says 1: lane saying "not a worker" never overrides the walk.
#
# The paths are arguments for the tests only: `in_lane_worker [lane_path [proc_root [uname_s [timeout_path]]]]`, defaulting to
# /home/nez/.local/bin/lane, /proc, `/usr/bin/uname -s` and /usr/bin/timeout.
# They are never read from the environment, so no variable can say "not a worker".
#
# Not guarded: the mutants.sh and verus-mutants-*.sh are left alone on purpose (coordinator 2026-11-19): they are frozen proof units, the harm
# is cost not safety, and lane's namespace fence hides the tools; do not add the guard to them.

if [ -z "${BASH_VERSION:-}" ] && ! (eval 'x=ab; case ${x##a} in b) exit 0 ;; esac; exit 1') 2> /dev/null; then
    # A shell that cannot run the rest of this file refuses.
    in_lane_worker() { return 0; }
else
    in_lane_worker() {
        _ilw_lane=${1:-/home/nez/.local/bin/lane}
        _ilw_proc=${2:-/proc}
        _ilw_uname=${3:-}
        _ilw_to=${4:-/usr/bin/timeout}

        if [ "${LANE_WORKER:-}" = 1 ] || [ -n "${LANE_PROOFS_REFUSED:-}" ]; then
            return 0
        fi

        if [ ! -d "$_ilw_proc" ]; then
            [ -n "$_ilw_uname" ] || _ilw_uname=$(/usr/bin/uname -s 2> /dev/null) || _ilw_uname=
            if [ "$_ilw_uname" = Darwin ]; then
                return 1
            fi
        fi

        if [ -x "$_ilw_lane" ]; then
            [ -x "$_ilw_to" ] || return 0
            _ilw_h=2
            "$_ilw_to" 10 "$_ilw_lane" in-worker --help > /dev/null 2>&1 && _ilw_h=0 || _ilw_h=$?
            [ "$_ilw_h" -ne 124 ] || return 0
            _ilw_st=2
            "$_ilw_to" 10 "$_ilw_lane" in-worker > /dev/null 2>&1 && _ilw_st=0 || _ilw_st=$?
            case $_ilw_st in
                1) ;;
                0 | 2 | 124) return 0 ;;
                *) [ "$_ilw_h" -ne 0 ] || return 0 ;;
            esac
        fi

        _ilw_p=$$
        _ilw_n=0
        while :; do
            _ilw_n=$((_ilw_n + 1))
            [ "$_ilw_n" -le 128 ] || return 0
            [ -r "$_ilw_proc/$_ilw_p/cgroup" ] || return 0
            while IFS= read -r _ilw_line || [ -n "$_ilw_line" ]; do
                _ilw_b=${_ilw_line##*/}
                case $_ilw_b in
                    lane-runner-*.scope) return 0 ;;
                    lane-*-*.scope)
                        _ilw_t=${_ilw_b%.scope}
                        _ilw_t=${_ilw_t##*-}
                        case $_ilw_t in '' | *[!0-9]*) ;; *) return 0 ;; esac
                        ;;
                esac
            done < "$_ilw_proc/$_ilw_p/cgroup" || return 0
            [ "$_ilw_p" = 1 ] && return 1
            [ -r "$_ilw_proc/$_ilw_p/stat" ] || return 0
            _ilw_stat=
            IFS= read -r _ilw_stat < "$_ilw_proc/$_ilw_p/stat" || [ -n "$_ilw_stat" ] || return 0
            case $_ilw_stat in *') '*) ;; *) return 0 ;; esac
            _ilw_rest=${_ilw_stat##*) }
            _ilw_rest=${_ilw_rest#* }
            _ilw_ppid=${_ilw_rest%% *}
            case $_ilw_ppid in '' | *[!0-9]*) return 0 ;; esac
            [ "$_ilw_ppid" -ge 1 ] || return 0
            _ilw_p=$_ilw_ppid
        done
    }
fi
