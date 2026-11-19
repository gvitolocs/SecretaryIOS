#!/usr/bin/env bash
# scripts/tests/check-fast-queue-test.sh: scripts/check-fast runs as one job of the build queue's `long` class (HOST-11).
# A stub `bq` first on PATH prints our usage, records its argv to a file, and execs what follows the `--` with BQ_SLOT and
# BQ_SLOT_PID set to its own pid, as the real one does. cargo, df and the built tool are stubs too: nothing is built or queued.
#   (1) a plain run goes through bq once: class long, priority normal, and the inner run does not queue again;
#   (2) --merge-gate asks for priority gate;
#   (3) a run inside a bq job (the stub is an ANCESTOR of the launcher) makes no second call;
#   (4) BQ_SLOT set by hand, its BQ_SLOT_PID dead, or alive but not an ancestor, still queues;
#   (5) no bq anywhere (PATH without it, HOME in a temp dir): runs directly and the first line says so;
#   (6) a bq of the wrong program (the Google Cloud SDK's) is not taken.
# Last line: `check-fast-queue-test: pass`.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
base=${CHECK_FAST_QUEUE_TEST_TMP:-${TMPDIR:-/tmp}}
mkdir -p "$base"
tmp=$(mktemp -d "$base/check-fast-queue.XXXXXX")
sleeper=""
cleanup() {
    [ -z "$sleeper" ] || kill "$sleeper" 2>/dev/null || true
    rm -rf "$tmp"
}
trap cleanup EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
ncalls() { wc -l < "$1" | tr -d ' '; }

BQ_USAGE='usage: bq run --class short|long|proof [--lock PATH] [--cores N] [--mem 8G|none] [--exclusive] [--who NAME] [--priority gate|normal] [--max-wait SECONDS] [--keep-env NAME] -- cmd args...'
mkdir -p "$tmp/home" "$tmp/base" "$tmp/withbq" "$tmp/gcloud" "$tmp/build/release"
cat > "$tmp/withbq/bq" <<STUB
#!/usr/bin/env bash
if [ "\$#" -eq 0 ]; then
    echo '$BQ_USAGE'
    exit 2
fi
echo "\$*" >> "\$STUB_BQ_LOG"
while [ "\$1" != "--" ]; do shift; done
shift
export BQ_SLOT=long-0 BQ_SLOT_PID=\$\$
exec "\$@"
STUB
cat > "$tmp/gcloud/bq" <<'STUB'
#!/usr/bin/env bash
echo 'ERROR: (bq) You do not currently have an active account selected.' >&2
exit 1
STUB
printf '#!/usr/bin/env bash\necho Avail\necho 999G\n' > "$tmp/base/df"
printf '#!/usr/bin/env bash\nexit 0\n' > "$tmp/base/cargo"
printf '#!/usr/bin/env bash\necho "stub-ran $*"\n' > "$tmp/build/release/lucina-ci"
chmod +x "$tmp/withbq/bq" "$tmp/gcloud/bq" "$tmp/base/df" "$tmp/base/cargo" "$tmp/build/release/lucina-ci"
launcher=$root/scripts/check-fast
who=$(basename "$root")

# launch <name> <dirs first on PATH> <home> [env assignment...] -- <command...>
launch() {
    local name=$1 first=$2 home=$3
    shift 3
    local envs=()
    while [ "$#" -gt 0 ] && [ "$1" != -- ]; do envs+=("$1"); shift; done
    shift
    calls=$tmp/$name.calls
    : > "$calls"
    rc=0
    env -u BQ_SLOT -u BQ_SLOT_PID -u BQ_PRIORITY ${envs[@]+"${envs[@]}"} \
        HOME="$home" PATH="$first:/usr/bin:/bin" STUB_BQ_LOG="$calls" \
        SECRETARY_CI_TOOL_TARGET="$tmp/tool" SECRETARY_CI_TOOL_BUILD="$tmp/build" \
        "$@" > "$tmp/$name.out" 2> "$tmp/$name.err" || rc=$?
    out=$tmp/$name.out
    err=$tmp/$name.err
    [ "$rc" -eq 0 ] || fail "($name) exit $rc: $(cat "$err")"
    grep -qF "stub-ran check-fast" "$out" || fail "($name) the check never ran: out=$(cat "$out") err=$(cat "$err")"
}
withbq=$tmp/withbq:$tmp/base

# (1) a plain run: one call, class long, priority normal; the run inside the job does not queue again.
launch plain "$withbq" "$tmp/home" -- "$launcher" --base HEAD
[ "$(ncalls "$calls")" = 1 ] || fail "(1) expected exactly one queue call: $(cat "$calls")"
want="run --class long --priority normal --max-wait 3600 --keep-env GIT_CONFIG_COUNT --who check-fast:$who -- $launcher --base HEAD"
[ "$(cat "$calls")" = "$want" ] || fail "(1) the queue call is not what was asked for: $(cat "$calls")"
echo "ok: (1) a plain run queues once, class long, priority normal, and the inner run does not queue again"

# (2) the merge gate.
launch gate "$withbq" "$tmp/home" -- "$launcher" --base HEAD^1 --merge-gate
[ "$(ncalls "$calls")" = 1 ] || fail "(2) expected exactly one queue call: $(cat "$calls")"
grep -qF -- "run --class long --priority gate --max-wait 3600 " "$calls" || fail "(2) a merge gate asks for priority gate: $(cat "$calls")"
echo "ok: (2) --merge-gate asks for priority gate"

# (3) nested: the launcher runs as a descendant (not the same process) of a live bq job: no second call. `bash -c '...; exit'`
# keeps bash from exec'ing the launcher, so the stub's pid is an ancestor of it and not the launcher itself.
launch nested "$withbq" "$tmp/home" -- "$tmp/withbq/bq" run --class long -- bash -c '"$@"; exit $?' sh "$launcher" --base HEAD
[ "$(ncalls "$calls")" = 1 ] || fail "(3) a run inside a bq job queued again: $(cat "$calls")"
echo "ok: (3) a run inside a bq job makes no second queue call"

# (4) BQ_SLOT set by hand: a dead holder, then a live process that is not an ancestor. Both still queue.
dead=$(sh -c 'echo $$')
launch dead "$withbq" "$tmp/home" BQ_SLOT=short-0 "BQ_SLOT_PID=$dead" -- "$launcher" --base HEAD
[ "$(ncalls "$calls")" = 1 ] || fail "(4) BQ_SLOT with a dead BQ_SLOT_PID did not queue exactly once: $(cat "$calls")"
sleep 60 &
sleeper=$!
launch alive "$withbq" "$tmp/home" BQ_SLOT=short-0 "BQ_SLOT_PID=$sleeper" -- "$launcher" --base HEAD
[ "$(ncalls "$calls")" = 1 ] || fail "(4) BQ_SLOT naming a live non-ancestor did not queue exactly once: $(cat "$calls")"
launch noslotpid "$withbq" "$tmp/home" BQ_SLOT=short-0 -- "$launcher" --base HEAD
[ "$(ncalls "$calls")" = 1 ] || fail "(4) BQ_SLOT alone did not queue exactly once: $(cat "$calls")"
launch prio "$withbq" "$tmp/home" BQ_PRIORITY=gate -- "$launcher" --base HEAD
[ "$(ncalls "$calls")" = 1 ] || fail "(4) BQ_PRIORITY must not skip the queue: $(cat "$calls")"
echo "ok: (4) a hand-set BQ_SLOT (dead holder, live non-ancestor, no holder), or BQ_PRIORITY, still queues"

# (5) no bq anywhere: runs directly, and the first line of output says so.
launch none "$tmp/base" "$tmp/home" -- bash -c '"$@" 2>&1' sh "$launcher" --base HEAD
[ "$(ncalls "$calls")" = 0 ] || fail "(5) no queue exists, yet it was called: $(cat "$calls")"
first=$(head -n 1 "$out")
[ "$first" = "check-fast: no bq on this host; running without the queue" ] || fail "(5) the first line of output is: $first"
echo "ok: (5) without any bq the check runs directly and its first line says so"

# (6) the other program called bq: not taken, and the same line.
launch gcloud "$tmp/gcloud:$tmp/base" "$tmp/home" -- bash -c '"$@" 2>&1' sh "$launcher" --base HEAD
head -n 1 "$out" | grep -qF "no bq on this host" || fail "(6) a bq that is not ours was taken: $(cat "$out")"
grep -qF "active account" "$out" && fail "(6) the other bq's error reached the reader: $(cat "$out")"
echo "ok: (6) a bq on PATH that is not our queue is not taken"

echo "check-fast-queue-test: pass"
