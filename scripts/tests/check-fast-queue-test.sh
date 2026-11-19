#!/usr/bin/env bash
# scripts/tests/check-fast-queue-test.sh: scripts/check-fast runs as one job of the build queue's `long` class (HOST-11).
# The queue is looked up as $HOME/.local/bin/bq first (if that file exists it MUST be ours, else the check fails), and a `bq`
# on PATH only when it does not exist; no environment variable names or skips it. A stub `bq` prints our usage, records its
# argv to a file, and execs what follows the `--` with BQ_SLOT and BQ_SLOT_PID set to its own pid, as the real one does.
# cargo, df and the built tool are stubs too: nothing is built or queued.
#   (1) a plain run goes through bq once: class long, priority normal, and the inner run does not queue again;
#   (2) --merge-gate asks for priority gate;
#   (3) a run inside a bq job (the stub is an ANCESTOR of the launcher) makes no second call;
#   (4) BQ_SLOT set by hand, its BQ_SLOT_PID dead, or alive but not an ancestor, still queues;
#   (5) no bq anywhere (PATH without it, HOME in a temp dir): runs directly and the first line says so;
#   (6) a bq of the wrong program (the Google Cloud SDK's) on PATH, none in HOME, is not taken;
#   (7) $HOME/.local/bin/bq exists but is not ours: the check fails and runs nothing;
#   (8) an ours-looking `bq` first on PATH while $HOME/.local/bin/bq exists: the HOME one is used;
#   (9) a variable that names a queue (LUCINA_BQ) is not read: it neither supplies a queue nor skips one.
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
mkdir -p "$tmp/home/.local/bin" "$tmp/nohome" "$tmp/base" "$tmp/pathbq" "$tmp/gcloud" "$tmp/build/release"
# make_stub <file> <tag>: a queue in our usage's shape. The tag is the first word of what it records.
make_stub() {
    cat > "$1" <<STUB
#!/usr/bin/env bash
if [ "\$#" -eq 0 ]; then
    echo '$BQ_USAGE'
    exit 2
fi
echo "$2 \$*" >> "\$STUB_BQ_LOG"
while [ "\$1" != "--" ]; do shift; done
shift
export BQ_SLOT=long-0 BQ_SLOT_PID=\$\$
exec "\$@"
STUB
    chmod +x "$1"
}
make_stub "$tmp/home/.local/bin/bq" home
make_stub "$tmp/pathbq/bq" path
cat > "$tmp/gcloud/bq" <<'STUB'
#!/usr/bin/env bash
echo 'ERROR: (bq) You do not currently have an active account selected.' >&2
exit 1
STUB
chmod +x "$tmp/gcloud/bq"
printf '#!/usr/bin/env bash\necho Avail\necho 999G\n' > "$tmp/base/df"
printf '#!/usr/bin/env bash\nexit 0\n' > "$tmp/base/cargo"
printf '#!/usr/bin/env bash\necho "stub-ran $*"\n' > "$tmp/build/release/lucina-ci"
chmod +x "$tmp/base/df" "$tmp/base/cargo" "$tmp/build/release/lucina-ci"
launcher=$root/scripts/check-fast
who=$(basename "$root")

# launch <name> <dirs first on PATH> <home> [env assignment...] -- <command...>; leaves rc, out, err, calls.
launch() {
    local name=$1 first=$2 home=$3
    shift 3
    local envs=()
    while [ "$#" -gt 0 ] && [ "$1" != -- ]; do envs+=("$1"); shift; done
    shift
    calls=$tmp/$name.calls
    : > "$calls"
    rc=0
    env -u BQ_SLOT -u BQ_SLOT_PID -u BQ_PRIORITY -u LUCINA_BQ ${envs[@]+"${envs[@]}"} \
        HOME="$home" PATH="$first:/usr/bin:/bin" STUB_BQ_LOG="$calls" \
        SECRETARY_CI_TOOL_TARGET="$tmp/tool" SECRETARY_CI_TOOL_BUILD="$tmp/build" \
        "$@" > "$tmp/$name.out" 2> "$tmp/$name.err" || rc=$?
    out=$tmp/$name.out
    err=$tmp/$name.err
}
# ran_ok: the last launch exited 0 and the check ran.
ran_ok() {
    [ "$rc" -eq 0 ] || fail "($1) exit $rc: $(cat "$err")"
    grep -qF "stub-ran check-fast" "$out" || fail "($1) the check never ran: out=$(cat "$out") err=$(cat "$err")"
}
home=$tmp/home

# (1) a plain run: one call, class long, priority normal; the run inside the job does not queue again.
launch plain "$tmp/base" "$home" -- "$launcher" --base HEAD
ran_ok plain
[ "$(ncalls "$calls")" = 1 ] || fail "(1) expected exactly one queue call: $(cat "$calls")"
want="home run --class long --priority normal --max-wait 3600 --keep-env GIT_CONFIG_COUNT --who check-fast:$who -- $launcher --base HEAD"
[ "$(cat "$calls")" = "$want" ] || fail "(1) the queue call is not what was asked for: $(cat "$calls")"
echo "ok: (1) a plain run queues once, class long, priority normal, and the inner run does not queue again"

# (2) the merge gate.
launch gate "$tmp/base" "$home" -- "$launcher" --base HEAD^1 --merge-gate
ran_ok gate
[ "$(ncalls "$calls")" = 1 ] || fail "(2) expected exactly one queue call: $(cat "$calls")"
grep -qF -- "run --class long --priority gate --max-wait 3600 " "$calls" || fail "(2) a merge gate asks for priority gate: $(cat "$calls")"
echo "ok: (2) --merge-gate asks for priority gate"

# (3) nested: the launcher runs as a descendant (not the same process) of a live bq job: no second call. `bash -c '...; exit'`
# keeps bash from exec'ing the launcher, so the stub's pid is an ancestor of it and not the launcher itself.
launch nested "$tmp/base" "$home" -- "$home/.local/bin/bq" run --class long -- bash -c '"$@"; exit $?' sh "$launcher" --base HEAD
ran_ok nested
[ "$(ncalls "$calls")" = 1 ] || fail "(3) a run inside a bq job queued again: $(cat "$calls")"
echo "ok: (3) a run inside a bq job makes no second queue call"

# (4) BQ_SLOT set by hand: a dead holder, then a live process that is not an ancestor. Both still queue.
dead=$(sh -c 'echo $$')
launch dead "$tmp/base" "$home" BQ_SLOT=short-0 "BQ_SLOT_PID=$dead" -- "$launcher" --base HEAD
ran_ok dead
[ "$(ncalls "$calls")" = 1 ] || fail "(4) BQ_SLOT with a dead BQ_SLOT_PID did not queue exactly once: $(cat "$calls")"
sleep 60 &
sleeper=$!
launch alive "$tmp/base" "$home" BQ_SLOT=short-0 "BQ_SLOT_PID=$sleeper" -- "$launcher" --base HEAD
ran_ok alive
[ "$(ncalls "$calls")" = 1 ] || fail "(4) BQ_SLOT naming a live non-ancestor did not queue exactly once: $(cat "$calls")"
launch noslotpid "$tmp/base" "$home" BQ_SLOT=short-0 -- "$launcher" --base HEAD
ran_ok noslotpid
[ "$(ncalls "$calls")" = 1 ] || fail "(4) BQ_SLOT alone did not queue exactly once: $(cat "$calls")"
launch prio "$tmp/base" "$home" BQ_PRIORITY=gate -- "$launcher" --base HEAD
ran_ok prio
[ "$(ncalls "$calls")" = 1 ] || fail "(4) BQ_PRIORITY must not skip the queue: $(cat "$calls")"
echo "ok: (4) a hand-set BQ_SLOT (dead holder, live non-ancestor, no holder), or BQ_PRIORITY, still queues"

# (5) no bq anywhere: runs directly, and the first line of output says so.
launch none "$tmp/base" "$tmp/nohome" -- bash -c '"$@" 2>&1' sh "$launcher" --base HEAD
ran_ok none
[ "$(ncalls "$calls")" = 0 ] || fail "(5) no queue exists, yet it was called: $(cat "$calls")"
first=$(head -n 1 "$out")
[ "$first" = "check-fast: no bq on this host; running without the queue" ] || fail "(5) the first line of output is: $first"
echo "ok: (5) without any bq the check runs directly and its first line says so"

# (6) the other program called bq, on PATH, with none in HOME: not taken, and the same line.
launch gcloud "$tmp/gcloud:$tmp/base" "$tmp/nohome" -- bash -c '"$@" 2>&1' sh "$launcher" --base HEAD
ran_ok gcloud
head -n 1 "$out" | grep -qF "no bq on this host" || fail "(6) a bq that is not ours was taken: $(cat "$out")"
if grep -qF "active account" "$out"; then fail "(6) the other bq's error reached the reader: $(cat "$out")"; fi
echo "ok: (6) a bq on PATH that is not our queue is not taken"

# (7) $HOME/.local/bin/bq exists and is not ours: the check fails with a message and runs nothing, never a direct run.
mkdir -p "$tmp/badhome/.local/bin"
cp "$tmp/gcloud/bq" "$tmp/badhome/.local/bin/bq"
launch badhome "$tmp/pathbq:$tmp/base" "$tmp/badhome" -- "$launcher" --base HEAD
[ "$rc" -ne 0 ] || fail "(7) a bq in HOME that is not ours let the check run: $(cat "$out") $(cat "$err")"
if grep -qF "stub-ran" "$out"; then fail "(7) the check ran: $(cat "$out")"; fi
[ "$(ncalls "$calls")" = 0 ] || fail "(7) a queue was called: $(cat "$calls")"
grep -qF "not our build queue" "$err" || fail "(7) the refusal does not say why: $(cat "$err")"
echo "ok: (7) a \$HOME/.local/bin/bq that is not ours fails the check and runs nothing"

# (8) an ours-looking bq first on PATH does not displace $HOME/.local/bin/bq.
launch pathfirst "$tmp/pathbq:$tmp/base" "$home" -- "$launcher" --base HEAD
ran_ok pathfirst
[ "$(ncalls "$calls")" = 1 ] || fail "(8) expected exactly one queue call: $(cat "$calls")"
grep -q "^home " "$calls" || fail "(8) the bq in HOME was not the one used: $(cat "$calls")"
# and a PATH bq is used when HOME has none
launch pathonly "$tmp/pathbq:$tmp/base" "$tmp/nohome" -- "$launcher" --base HEAD
ran_ok pathonly
grep -q "^path " "$calls" || fail "(8) with no bq in HOME the one on PATH is used: $(cat "$calls")"
echo "ok: (8) \$HOME/.local/bin/bq comes first; a bq on PATH only when HOME has none"

# (9) LUCINA_BQ is not read: pointing it at an ours-looking program neither supplies a queue nor skips the real one.
mkdir -p "$tmp/other"
make_stub "$tmp/other/bq" other
launch envbq "$tmp/base" "$tmp/nohome" LUCINA_BQ="$tmp/other/bq" -- bash -c '"$@" 2>&1' sh "$launcher" --base HEAD
ran_ok envbq
[ "$(ncalls "$calls")" = 0 ] || fail "(9) LUCINA_BQ supplied a queue: $(cat "$calls")"
launch envbq2 "$tmp/base" "$home" LUCINA_BQ="$tmp/other/bq" -- "$launcher" --base HEAD
ran_ok envbq2
grep -q "^home " "$calls" || fail "(9) LUCINA_BQ displaced the queue in HOME: $(cat "$calls")"
echo "ok: (9) LUCINA_BQ is not read"

echo "check-fast-queue-test: pass"
