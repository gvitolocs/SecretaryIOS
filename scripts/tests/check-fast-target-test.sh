#!/usr/bin/env bash
# scripts/tests/check-fast-target-test.sh: scripts/check-fast builds into ONE Cargo target PER WORKTREE under the check cache (GATE-24).
# The launcher is run from copies of itself at different worktree roots (the passwd-home assignment replaced by a fixed temp home, as in
# check-fast-queue-test.sh); there is no bq in that home or on PATH, so it runs directly. cargo and df are stubs and the installed
# tool is a stub that prints the CARGO_TARGET_DIR it was started with, so the run stops right after the target is resolved: nothing builds.
#   (1) the same root twice gives the same <cache>/wt-<hash> target; two roots give two different ones;
#   (2) the target exists and holds `.last-used`; nothing is said on stderr about the target;
#   (3) an inherited CARGO_TARGET_DIR outside the cache is kept and warned about; inside the cache it is kept silently;
#   (4) LUCINA_CHECK_ALLOW_TARGET=1 silences the warning, the target is still kept.
#   (5) the child sees CARGO_INCREMENTAL=0, and an explicitly inherited value is kept.
#   (6) LUCINA_AGENT_SHARED_TARGET equal (after readlink -f) to the inherited target: it is dropped and the worktree target is used, with one line; a different or absent target, or an empty variable, changes nothing.
# Last line: `check-fast-target-test: pass`.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
base=${CHECK_FAST_TARGET_TEST_TMP:-${TMPDIR:-/tmp}}
mkdir -p "$base"
tmp=$(mktemp -d "$base/check-fast-target.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

home=$tmp/home
cache=$tmp/cache
mkdir -p "$home/tool-build/release" "$cache" "$tmp/bin"
chmod 755 "$home" "$home/tool-build" "$home/tool-build/release"
printf '#!/usr/bin/env bash\necho "target=$CARGO_TARGET_DIR\nincremental=${CARGO_INCREMENTAL-unset}"\n' > "$home/tool-build/release/lucina-ci"
printf '#!/usr/bin/env bash\necho Avail\necho 999G\n' > "$tmp/bin/df"
printf '#!/usr/bin/env bash\nexit 0\n' > "$tmp/bin/cargo"
chmod +x "$home/tool-build/release/lucina-ci" "$tmp/bin/df" "$tmp/bin/cargo"

# make_root <name>: a copy of the launcher at <tmp>/<name>/scripts/check-fast with a fixed home.
make_root() {
    mkdir -p "$tmp/$1/scripts"
    awk -v h="$home" '
        /^qhome=\$\($/ { print "qhome=\"" h "\""; skip = 1; next }
        skip && /^\) \|\| qhome=""$/ { skip = 0; next }
        skip { next }
        { print }' "$root/scripts/check-fast" > "$tmp/$1/scripts/check-fast"
    chmod +x "$tmp/$1/scripts/check-fast"
}
make_root wt-a
make_root wt-b

# run_in <root name> [env assignment...]: leaves rc, out (the target line) and err.
run_in() {
    local r=$1
    shift
    rc=0
    env -u CARGO_TARGET_DIR -u CARGO_INCREMENTAL -u LUCINA_CHECK_ALLOW_TARGET ${@+"$@"} \
        PATH="$tmp/bin:/usr/bin:/bin" LUCINA_CHECK_CACHE="$cache" \
        SECRETARY_CI_TOOL_TARGET="$home/tool" SECRETARY_CI_TOOL_BUILD="$home/tool-build" \
        "$tmp/$r/scripts/check-fast" > "$tmp/out" 2> "$tmp/err" || rc=$?
    [ "$rc" -eq 0 ] || fail "$r: exit $rc: $(cat "$tmp/err")"
    out=$(grep '^target=' "$tmp/out" | head -1 | cut -d= -f2-)
    inc=$(grep '^incremental=' "$tmp/out" | head -1 | cut -d= -f2-)
    [ -n "$out" ] || fail "$r: the launcher printed no target: $(cat "$tmp/out")"
}
no_target_warning() { if grep -q 'CARGO_TARGET_DIR=' "$tmp/err"; then fail "$1: unexpected warning: $(cat "$tmp/err")"; fi; }

# (1) and (2)
run_in wt-a
a1=$out
no_target_warning "default a"
run_in wt-a
[ "$out" = "$a1" ] || fail "the same root gave two targets: $a1 and $out"
run_in wt-b
b1=$out
[ "$a1" != "$b1" ] || fail "two roots share the target $a1"
for t in "$a1" "$b1"; do
    case "$t" in
        "$cache"/wt-??????????) ;;
        *) fail "target $t is not <cache>/wt-<10 hex>" ;;
    esac
    [ -d "$t" ] || fail "$t was not created"
    [ -f "$t/.last-used" ] || fail "$t has no .last-used"
done
no_target_warning "default b"

# (3) an inherited target outside the cache warns and is kept; inside the cache it is kept silently
run_in wt-a CARGO_TARGET_DIR="$tmp/elsewhere"
[ "$out" = "$tmp/elsewhere" ] || fail "an inherited target was not kept: $out"
grep -q "warning: CARGO_TARGET_DIR=$tmp/elsewhere (inherited) is outside $cache" "$tmp/err" || fail "no warning for an outside target: $(cat "$tmp/err")"
[ -f "$tmp/elsewhere/.last-used" ] || fail "the kept target has no .last-used"
run_in wt-a CARGO_TARGET_DIR="$cache/mine"
[ "$out" = "$cache/mine" ] || fail "an inside target was not kept: $out"
no_target_warning "inside the cache"

# (4) LUCINA_CHECK_ALLOW_TARGET=1 silences it
run_in wt-a CARGO_TARGET_DIR="$tmp/elsewhere2" LUCINA_CHECK_ALLOW_TARGET=1
[ "$out" = "$tmp/elsewhere2" ] || fail "the allowed target was not kept: $out"
no_target_warning "allowed"

# (5) incremental compilation is off unless the caller says otherwise
run_in wt-a
[ "$inc" = 0 ] || fail "CARGO_INCREMENTAL is not 0 by default: $inc"
run_in wt-a CARGO_INCREMENTAL=1
[ "$inc" = 1 ] || fail "an inherited CARGO_INCREMENTAL was not kept: $inc"

# (6) the agent runner's shared target (LUCINA_AGENT_SHARED_TARGET) is not used: the check builds in its own per-worktree target
shared_line="the agent's shared target is not used"
mtime() { stat -c %Y "$1"; }
mkdir -p "$tmp/agent-shared"
touch -d '2020-01-01 00:00:00' "$tmp/agent-shared"
before=$(mtime "$tmp/agent-shared")
run_in wt-a CARGO_TARGET_DIR="$tmp/agent-shared" LUCINA_AGENT_SHARED_TARGET="$tmp/agent-shared"
[ "$out" = "$a1" ] || fail "the agent's shared target was not replaced by the worktree target: $out (want $a1)"
[ "$(mtime "$tmp/agent-shared")" = "$before" ] || fail "the shared target was touched"
[ ! -e "$tmp/agent-shared/.last-used" ] || fail "the shared target got a .last-used"
[ "$(grep -c "$shared_line" "$tmp/err")" = 1 ] || fail "want exactly one line about the shared target: $(cat "$tmp/err")"
grep -q "building in $a1\$" "$tmp/err" || fail "the line does not name the real target: $(cat "$tmp/err")"
no_target_warning "agent shared target"
# a different inherited target is still warned about and kept
mkdir -p "$tmp/other-shared"
run_in wt-a CARGO_TARGET_DIR="$tmp/other-shared" LUCINA_AGENT_SHARED_TARGET="$tmp/agent-shared"
[ "$out" = "$tmp/other-shared" ] || fail "a different inherited target was not kept: $out"
grep -q "warning: CARGO_TARGET_DIR=$tmp/other-shared (inherited) is outside" "$tmp/err" || fail "no warning for a different target: $(cat "$tmp/err")"
if grep -q "$shared_line" "$tmp/err"; then fail "the shared-target line was printed for a different target"; fi
# a symlink to the shared target counts as the same place
ln -s "$tmp/agent-shared" "$tmp/agent-link"
run_in wt-a CARGO_TARGET_DIR="$tmp/agent-link" LUCINA_AGENT_SHARED_TARGET="$tmp/agent-shared"
[ "$out" = "$a1" ] || fail "a symlink to the shared target was kept: $out"
[ "$(grep -c "$shared_line" "$tmp/err")" = 1 ] || fail "symlink: want one line: $(cat "$tmp/err")"
no_target_warning "symlink"
# the variable set but no inherited target: nothing changes
run_in wt-a LUCINA_AGENT_SHARED_TARGET="$tmp/agent-shared"
[ "$out" = "$a1" ] || fail "variable without an inherited target changed the target: $out"
if grep -q "$shared_line" "$tmp/err"; then fail "a line was printed without an inherited target"; fi
# the variable empty: nothing changes
run_in wt-a CARGO_TARGET_DIR="$tmp/elsewhere" LUCINA_AGENT_SHARED_TARGET=
[ "$out" = "$tmp/elsewhere" ] || fail "an empty variable changed the target: $out"
if grep -q "$shared_line" "$tmp/err"; then fail "a line was printed for an empty variable"; fi
grep -q "warning: CARGO_TARGET_DIR=$tmp/elsewhere (inherited)" "$tmp/err" || fail "empty variable: the warning is gone"

echo "check-fast-target-test: pass"
