#!/usr/bin/env bash
# scripts/tests/check-fast-target-test.sh: scripts/check-fast builds into ONE Cargo target PER WORKTREE under the check cache (GATE-24).
# The launcher is run from copies of itself at different worktree roots (the passwd-home assignment replaced by a fixed temp home, as in
# check-fast-queue-test.sh); there is no bq in that home or on PATH, so it runs directly. cargo and df are stubs and the installed
# tool is a stub that prints the CARGO_TARGET_DIR it was started with, so the run stops right after the target is resolved: nothing builds.
#   (1) the same root twice gives the same <cache>/wt-<hash> target; two roots give two different ones;
#   (2) the target exists and holds `.last-used`; nothing is said on stderr about the target;
#   (3) an inherited CARGO_TARGET_DIR outside the cache is kept and warned about; inside the cache it is kept silently;
#   (4) LUCINA_CHECK_ALLOW_TARGET=1 silences the warning, the target is still kept.
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
printf '#!/usr/bin/env bash\necho "target=$CARGO_TARGET_DIR"\n' > "$home/tool-build/release/lucina-ci"
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
    env -u CARGO_TARGET_DIR -u LUCINA_CHECK_ALLOW_TARGET ${@+"$@"} \
        PATH="$tmp/bin:/usr/bin:/bin" LUCINA_CHECK_CACHE="$cache" \
        SECRETARY_CI_TOOL_TARGET="$home/tool" SECRETARY_CI_TOOL_BUILD="$home/tool-build" \
        "$tmp/$r/scripts/check-fast" > "$tmp/out" 2> "$tmp/err" || rc=$?
    [ "$rc" -eq 0 ] || fail "$r: exit $rc: $(cat "$tmp/err")"
    out=$(grep '^target=' "$tmp/out" | head -1 | cut -d= -f2-)
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

echo "check-fast-target-test: pass"
